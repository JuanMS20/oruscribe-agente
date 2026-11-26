//! Caché de avatares por organización: port.
//!
//! Las fotos de perfil no vienen en las respuestas del backend MCP. El contrato las
//! pedía a Supabase REST con el JWT de la sesión, las cacheaba en
//! `avatars.json` por organización y las inyectaba en salas, mensajes, equipo
//! y estado. Este módulo es esa caché y su
//! refresco; la inyección vive en `state()` y en el borde HTTP.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use orus_org::types::{MemberInfo, Person};
use serde::{Deserialize, Serialize};

use crate::sync::lock;

/// Cinco minutos de caché y un minuto de espera tras un intento fallido
const REFRESCAR_CADA: Duration = Duration::from_secs(5 * 60);
const REINTENTAR_TRAS: Duration = Duration::from_secs(60);
const TIMEOUT_PETICION: Duration = Duration::from_secs(10);
/// La lista de perfiles no puede pasar de un mebibyte.
const MAX_LISTA: usize = 1 << 20;

/// La copia en disco: `id -> dirección de foto`, por organización
/// Una cuenta distinta en el mismo equipo no debe ver
/// las caras de la anterior.
#[derive(Serialize, Deserialize)]
struct CopiaDisco {
    org: String,
    urls: HashMap<String, String>,
}

#[derive(Default)]
struct Interior {
    urls: HashMap<String, String>,
    org: String,
    leida_disco: bool,
    ocupado: bool,
    fresca: Option<Instant>,
    ultimo_intento: Option<Instant>,
}

/// La caché de fotos de la organización actual.
pub(crate) struct Avatares {
    interior: Mutex<Interior>,
}

impl Default for Avatares {
    fn default() -> Self {
        Self::new()
    }
}

impl Avatares {
    pub(crate) fn new() -> Self {
        Self {
            interior: Mutex::new(Interior::default()),
        }
    }

    /// La copia vigente para inyectar, sin esperar a la red. Si.
    /// toca, arranca el refresco en segundo plano.
    pub(crate) fn mapa(
        self: &Arc<Self>,
        org: &str,
        base: &str,
        anon: &str,
        token: &str,
    ) -> HashMap<String, String> {
        let mut interior = lock(&self.interior);
        if interior.org != org {
            // #125: cambiar de organización no puede seguir sirviendo las
            // caras de la anterior ni su marca de frescura. Volver a leer el
            // disco recupera las fotos guardadas de esta org; `aplicar` ya
            // descarta lo que llegue tarde de la anterior.
            interior.urls.clear();
            interior.fresca = None;
            interior.ultimo_intento = None;
            interior.leida_disco = false;
            interior.org = org.to_owned();
        }
        if !interior.leida_disco {
            interior.leida_disco = true;
            if let Some((_, urls)) = leer_disco(org) {
                interior.urls = urls;
            }
        }
        if interior.ocupado
            || interior
                .fresca
                .is_some_and(|fue| fue.elapsed() < REFRESCAR_CADA)
            || interior
                .ultimo_intento
                .is_some_and(|fue| fue.elapsed() < REINTENTAR_TRAS)
        {
            return interior.urls.clone();
        }
        // Un token del MCP (`syn_...`) no es un JWT de Supabase: la consulta
        // de perfiles devuelve PGRST301 y la ventana se queda con las
        // iniciales sin decir por qué.
        if org.is_empty() || base.is_empty() || token.matches('.').count() != 2 {
            return interior.urls.clone();
        }
        // Sin runtime no hay dónde poner el refresco: se devuelve la copia y
        // el próximo `mapa` en un contexto async lo intenta.
        if tokio::runtime::Handle::try_current().is_err() {
            return interior.urls.clone();
        }
        interior.ocupado = true;
        interior.ultimo_intento = Some(Instant::now());
        let (este, org, base, anon, token) = (
            Arc::clone(self),
            org.to_owned(),
            base.to_owned(),
            anon.to_owned(),
            token.to_owned(),
        );
        tokio::spawn(async move {
            match pedir(&base, &anon, &token, &org).await {
                Ok(urls) => este.aplicar(&org, urls),
                Err(_) => este.liberar(),
            }
        });
        interior.urls.clone()
    }

    /// Aplica lo que trajo la red. Una organización distinta mientras se
    /// pedían las fotos no se mezcla: el intento quedó contado igual
    fn aplicar(&self, org: &str, urls: HashMap<String, String>) {
        let mut interior = lock(&self.interior);
        interior.ocupado = false;
        if interior.org != org || urls.is_empty() {
            return;
        }
        interior.fresca = Some(Instant::now());
        interior.urls = urls.clone();
        drop(interior);
        let _ = guardar_disco(org, &urls);
    }

    /// Un fallo de red conserva las caras que ya estaban.
    fn liberar(&self) {
        lock(&self.interior).ocupado = false;
    }
}

/// Una persona cuyo avatar puede rellenarse desde la caché.
pub trait ConFoto {
    fn id(&self) -> &str;
    fn sin_foto(&self) -> bool;
    fn poner_foto(&mut self, url: String);
}

impl ConFoto for Person {
    fn id(&self) -> &str {
        &self.id
    }
    fn sin_foto(&self) -> bool {
        self.avatar_url.as_deref().unwrap_or("").is_empty()
    }
    fn poner_foto(&mut self, url: String) {
        self.avatar_url = Some(url);
    }
}

impl ConFoto for MemberInfo {
    fn id(&self) -> &str {
        &self.id
    }
    fn sin_foto(&self) -> bool {
        self.avatar_url.as_deref().unwrap_or("").is_empty()
    }
    fn poner_foto(&mut self, url: String) {
        self.avatar_url = Some(url);
    }
}

/// Rellena la foto que el backend MCP no trae con la de la caché de avatares
pub fn con_avatar<T: ConFoto + ?Sized>(quien: &mut T, urls: &HashMap<String, String>) {
    if quien.sin_foto()
        && let Some(url) = urls.get(quien.id())
    {
        quien.poner_foto(url.clone());
    }
}

/// `GET {base}/rest/v1/users?select=id,avatar_url&organization_id=eq.<org>`
/// con el JWT de la sesión y la llave anónima.
async fn pedir(
    base: &str,
    anon: &str,
    token: &str,
    org: &str,
) -> Result<HashMap<String, String>, String> {
    let url = format!(
        "{}/rest/v1/users?select=id,avatar_url&organization_id=eq.{}",
        base.trim_end_matches('/'),
        org
    );
    let cliente = reqwest::Client::builder()
        .timeout(TIMEOUT_PETICION)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| error.to_string())?;
    let respuesta = cliente
        .get(&url)
        .bearer_auth(token)
        .header("apikey", anon)
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if !respuesta.status().is_success() {
        return Err(format!(
            "supabase respondió {} al pedir las fotos de perfil",
            respuesta.status().as_u16()
        ));
    }
    let mut respuesta = respuesta;
    let mut cuerpo: Vec<u8> = Vec::new();
    while let Some(trozo) = respuesta.chunk().await.map_err(|error| error.to_string())? {
        if cuerpo.len() + trozo.len() > MAX_LISTA {
            return Err("la lista de fotos de perfil pesa más de un mebibyte".to_owned());
        }
        cuerpo.extend_from_slice(&trozo);
    }
    #[derive(Deserialize)]
    struct Fila {
        id: String,
        #[serde(default)]
        avatar_url: String,
    }
    let filas: Vec<Fila> = serde_json::from_slice(&cuerpo)
        .map_err(|error| format!("la lista de fotos de perfil no se pudo leer: {error}"))?;
    Ok(filas
        .into_iter()
        .filter(|fila| !fila.avatar_url.is_empty())
        .map(|fila| (fila.id, fila.avatar_url))
        .collect())
}

/// `avatars.json` en la carpeta de datos.
fn ruta_disco() -> PathBuf {
    orus_config::data_dir().join("avatars.json")
}

fn leer_disco(org: &str) -> Option<(String, HashMap<String, String>)> {
    let data = std::fs::read(ruta_disco()).ok()?;
    let copia = serde_json::from_slice::<CopiaDisco>(&data).ok()?;
    if copia.org != org || copia.urls.is_empty() {
        return None;
    }
    Some((copia.org, copia.urls))
}

/// Escritura atómica con `tmp` y renombrado.
fn guardar_disco(org: &str, urls: &HashMap<String, String>) -> std::io::Result<()> {
    if org.is_empty() || urls.is_empty() {
        return Ok(());
    }
    let dir = orus_config::data_dir();
    let mut constructor = std::fs::DirBuilder::new();
    constructor.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        constructor.mode(0o700);
    }
    constructor.create(&dir)?;
    let data = serde_json::to_vec(&CopiaDisco {
        org: org.to_owned(),
        urls: urls.clone(),
    })
    .map_err(std::io::Error::other)?;
    let ruta = ruta_disco();
    let tmp = ruta.with_extension("json.tmp");
    std::fs::write(&tmp, data)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, &ruta)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    #[serial]
    fn disco_se_guarda_y_filtra_por_organizacion() {
        let dir = tempfile::tempdir().expect("carpeta temporal");
        let ruta = dir.path().to_string_lossy().into_owned();
        temp_env::with_var("ORUSCRIBE_DATA_DIR", Some(ruta.as_str()), || {
            let mut urls = HashMap::new();
            urls.insert("u1".to_owned(), "https://ejemplo/1.png".to_owned());
            guardar_disco("org-a", &urls).expect("guardar");

            let (_, leidas) = leer_disco("org-a").expect("leer la misma org");
            assert_eq!(
                leidas.get("u1").map(|v| v.as_str()),
                Some("https://ejemplo/1.png")
            );
            assert!(
                leer_disco("org-b").is_none(),
                "las caras de otra org no se mezclan"
            );
        });
    }

    #[test]
    #[serial]
    fn aplicar_descarta_otra_organizacion() {
        let avatares = Arc::new(Avatares::new());
        avatares.mapa("org-a", "", "", "");
        let mut urls = HashMap::new();
        urls.insert("u1".to_owned(), "https://ejemplo/1.png".to_owned());
        avatares.aplicar("org-b", urls);
        assert!(
            avatares.mapa("org-a", "", "", "").is_empty(),
            "lo que llegó tarde de otra org no entra"
        );
    }

    #[test]
    #[serial]
    fn cambio_de_org_recarga_las_fotos_y_no_sirve_las_de_la_anterior() {
        let dir = tempfile::tempdir().expect("carpeta temporal");
        let ruta = dir.path().to_string_lossy().into_owned();
        temp_env::with_var("ORUSCRIBE_DATA_DIR", Some(ruta.as_str()), || {
            let avatares = Arc::new(Avatares::new());
            // Primero A: `mapa` fija la org y su refresco llena la caché.
            assert!(avatares.mapa("org-a", "", "", "").is_empty());
            let mut urls_a = HashMap::new();
            urls_a.insert("u-a".to_owned(), "https://ejemplo/a.png".to_owned());
            avatares.aplicar("org-a", urls_a);
            assert_eq!(
                avatares
                    .mapa("org-a", "", "", "")
                    .get("u-a")
                    .map(String::as_str),
                Some("https://ejemplo/a.png")
            );

            // El disco vuelve a tener las fotos de B, guardadas en otra sesión.
            let mut urls_b = HashMap::new();
            urls_b.insert("u-b".to_owned(), "https://ejemplo/b.png".to_owned());
            guardar_disco("org-b", &urls_b).expect("guardar fotos de B");

            let vistos = avatares.mapa("org-b", "", "", "");
            assert_eq!(
                vistos.get("u-b").map(String::as_str),
                Some("https://ejemplo/b.png"),
                "el cambio de org debe releer las fotos guardadas: {vistos:?}"
            );
            assert!(
                !vistos.contains_key("u-a"),
                "la org B no puede ver las caras de la A: {vistos:?}"
            );
        });
    }

    /// Un servidor de una sola respuesta, para no depender de otro crate.
    async fn servidor_una_vez(
        cuerpo: &'static str,
    ) -> (String, tokio::sync::oneshot::Receiver<u32>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let dir = listener.local_addr().unwrap().to_string();
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let mut visitas = 0u32;
            if let Ok((mut socket, _)) = listener.accept().await {
                let mut buffer = [0u8; 4096];
                let _ = socket.read(&mut buffer).await;
                let respuesta = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    cuerpo.len(),
                    cuerpo
                );
                let _ = socket.write_all(respuesta.as_bytes()).await;
                visitas += 1;
            }
            let _ = tx.send(visitas);
        });
        (format!("http://{dir}"), rx)
    }

    #[tokio::test]
    async fn pedir_descarta_filas_sin_foto() {
        let (base, _visitas) = servidor_una_vez(
            r#"[{"id":"u1","avatar_url":"https://ejemplo/1.png"},{"id":"u2","avatar_url":""}]"#,
        )
        .await;
        let urls = pedir(&base, "anon", "a.b.c", "org").await.expect("pedir");
        assert_eq!(urls.len(), 1, "la fila sin foto no entra: {urls:?}");
        assert_eq!(
            urls.get("u1").map(|v| v.as_str()),
            Some("https://ejemplo/1.png")
        );
    }

    #[tokio::test]
    #[serial]
    async fn token_del_mcp_no_dispara_la_consulta() {
        let dir = tempfile::tempdir().expect("carpeta temporal");
        let ruta = dir.path().to_string_lossy().into_owned();
        // `mapa` lee la caché de disco al arrancar: sin aislar el data dir,
        // otro test del archivo puede cambiar ORUSCRIBE_DATA_DIR en medio.
        temp_env::async_with_vars([("ORUSCRIBE_DATA_DIR", Some(ruta.as_str()))], async {
            let (base, mut visitas) = servidor_una_vez("[]").await;
            let avatares = Arc::new(Avatares::new());
            let urls = avatares.mapa("org", &base, "anon", "syn_secreto");
            assert!(urls.is_empty());
            // El canal avisa cuando alguien consultó: esperar con plazo
            // demuestra que no llegó nada, no que todavía no había
            // llegado (issue #98: el try_recv inmediato no puede fallar).
            let llego = tokio::time::timeout(Duration::from_millis(500), &mut visitas).await;
            assert!(
                llego.is_err(),
                "nadie debía consultar: el token del MCP no es un JWT"
            );
            // Control positivo: con un JWT la consulta sí sale y el mismo
            // canal la ve; sin él, el plazo de arriba podría ser vacuo.
            let urls = avatares.mapa("org", &base, "anon", "a.b.c");
            assert!(urls.is_empty());
            let vistas = tokio::time::timeout(Duration::from_secs(5), &mut visitas)
                .await
                .expect("con un JWT la consulta sí sale")
                .expect("canal de visitas");
            assert_eq!(vistas, 1, "una sola consulta");
        })
        .await;
    }
}
