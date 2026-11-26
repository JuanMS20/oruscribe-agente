//! Formato en disco de las conversaciones guardadas.
//!
//! Cada frente elige su
//! carpeta y su prefijo; la versión, la escritura atómica y la validación
//! viven aquí una sola vez.

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Local, NaiveDate, Utc};
use orus_config::clean_path;
use orus_core::Message;
use serde::de::{Deserializer, IgnoredAny, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Sube cuando el formato guardado cambia de forma incompatible.
pub const VERSION: i64 = 1;

/// Última versión escrita por este proceso, por ruta (#131). Un `Session`
/// recién construido trae un `updated` nuevo: eso no prueba que otro proceso
/// haya tocado el archivo. Solo comparamos contra lo que nosotros escribimos.
type VersionEscrita = (i64, DateTime<Utc>);
type Escritas = Mutex<HashMap<PathBuf, VersionEscrita>>;

static ESCRITAS: OnceLock<Escritas> = OnceLock::new();

fn escritas() -> &'static Escritas {
    ESCRITAS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Una conversación guardada. El registro es la fuente de verdad: si el
/// proceso muere, la próxima corrida sigue desde aquí.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    #[serde(rename = "version", default)]
    pub version: i64,
    #[serde(rename = "id", default)]
    pub id: String,
    #[serde(rename = "title", default)]
    pub title: String,
    #[serde(rename = "cwd", default, skip_serializing_if = "String::is_empty")]
    pub cwd: String,
    #[serde(rename = "model", default, skip_serializing_if = "String::is_empty")]
    pub model: String,
    #[serde(rename = "created", default = "tiempo_cero")]
    pub created: DateTime<Utc>,
    #[serde(rename = "updated", default = "tiempo_cero")]
    pub updated: DateTime<Utc>,
    /// Usa `orus_core::Message`, que conserva las claves en mayúscula.
    /// Cambiar esa forma es un cambio incompatible y obliga a subir `VERSION`.
    #[serde(rename = "messages", default)]
    pub messages: Vec<Message>,
    #[serde(rename = "tokens_in", default, skip_serializing_if = "es_cero_i64")]
    pub tokens_in: i64,
    #[serde(rename = "tokens_out", default, skip_serializing_if = "es_cero_i64")]
    pub tokens_out: i64,
    /// Lo estimado a gastar por esta conversación, sumando todas sus corridas.
    #[serde(rename = "cost_usd", default, skip_serializing_if = "es_cero_f64")]
    pub cost_usd: f64,
}

/// Lo que una lista necesita sin cargar los mensajes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Info {
    #[serde(rename = "id")]
    pub id: String,
    #[serde(rename = "title")]
    pub title: String,
    #[serde(rename = "cwd", skip_serializing_if = "String::is_empty")]
    pub cwd: String,
    #[serde(rename = "created")]
    pub created: DateTime<Utc>,
    #[serde(rename = "updated")]
    pub updated: DateTime<Utc>,
    /// Cantidad de mensajes, no los mensajes.
    #[serde(rename = "messages")]
    pub messages: usize,
}

#[derive(Deserialize)]
struct Metadatos {
    #[serde(default)]
    version: i64,
    #[serde(default)]
    id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    cwd: String,
    #[serde(default = "tiempo_cero")]
    created: DateTime<Utc>,
    #[serde(default = "tiempo_cero")]
    updated: DateTime<Utc>,
    #[serde(default)]
    messages: Conteo,
}

#[derive(Default)]
struct Conteo(usize);

impl<'de> Deserialize<'de> for Conteo {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitante(usize);

        impl<'de> Visitor<'de> for Visitante {
            type Value = Conteo;

            fn expecting(&self, formato: &mut fmt::Formatter<'_>) -> fmt::Result {
                formato.write_str("una lista de mensajes")
            }

            fn visit_seq<A: SeqAccess<'de>>(mut self, mut seq: A) -> Result<Conteo, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {
                    self.0 += 1;
                }
                Ok(Conteo(self.0))
            }
        }

        deserializer.deserialize_seq(Visitante(0))
    }
}

/// Lee y escribe sesiones en una carpeta. El prefijo permite que la ventana
/// conserve sus nombres históricos `c-<id>.json`; la terminal no usa prefijo.
#[derive(Debug, Clone, Default)]
pub struct Store {
    pub dir: PathBuf,
    pub prefix: String,
}

/// Errores del formato de sesiones. Los textos son parte del contrato.
#[derive(Debug, Error)]
pub enum SessionsError {
    #[error("la sesión no tiene un id válido")]
    InvalidId,

    #[error("no hay sesión para guardar")]
    MissingSession,

    #[error("sesión guardada ilegible: {0}")]
    Unreadable(#[source] serde_json::Error),

    #[error("la sesión usa la versión {encontrada}; este arnés entiende la {esperada}")]
    UnsupportedVersion { encontrada: i64, esperada: i64 },

    #[error("la sesión guardada no coincide con el id pedido")]
    IdMismatch,

    #[error("no hay ninguna sesión guardada")]
    NoSessions,

    #[error("no hay ninguna sesión guardada para {0}")]
    NoSessionsForCwd(String),

    #[error("la sesión cambió en disco desde que se cargó: vuelve a cargarla antes de guardar")]
    Conflicto,

    #[error(transparent)]
    Json(#[from] serde_json::Error),

    /// `os.ReadFile` del contrato devuelve `*PathError` con la ruta; `fs::read` no.
    #[error("open {}: {source}", path.display())]
    Open {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Crea una sesión vacía. El id sale del reloj en nanosegundos, así dos
/// corridas seguidas nunca comparten archivo.
pub fn new(cwd: &str, model: &str) -> Session {
    let ahora = Utc::now();
    // Fuera del rango 1678-2262 no hay nanosegundos representables; a esa
    // altura el id cae a segundos y sigue siendo válido.
    let id = ahora
        .timestamp_nanos_opt()
        .unwrap_or_else(|| ahora.timestamp());
    Session {
        version: VERSION,
        id: id.to_string(),
        title: String::new(),
        cwd: cwd.to_string(),
        model: model.to_string(),
        created: ahora,
        updated: ahora,
        messages: Vec::new(),
        tokens_in: 0,
        tokens_out: 0,
        cost_usd: 0.0,
    }
}

/// Un id de fuera no puede salirse de la carpeta: solo dígitos y hasta 32.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 32 && id.bytes().all(|byte| byte.is_ascii_digit())
}

/// Convierte el nombre de un archivo de sesión en su id: quita `.json`, quita
/// el prefijo si lo hay y exige un id válido. No recorta ni toca el disco.
pub fn id_de_archivo<'a>(nombre: &'a str, prefix: &str) -> Option<&'a str> {
    let sin_json = nombre.strip_suffix(".json")?;
    let id = if prefix.is_empty() {
        sin_json
    } else {
        sin_json.strip_prefix(prefix)?
    };
    valid_id(id).then_some(id)
}

impl Store {
    /// Devuelve el archivo de un id, validándolo primero.
    pub fn path(&self, id: &str) -> Result<PathBuf, SessionsError> {
        if !valid_id(id) {
            return Err(SessionsError::InvalidId);
        }
        Ok(self.dir.join(format!("{}{id}.json", self.prefix)))
    }

    /// Escribe la sesión entera y la reemplaza de una vez: un corte a mitad de
    /// la escritura nunca deja una conversación a medias. Antes de pisar,
    /// compara la versión y el `updated` que había al cargar: dos procesos con
    /// el mismo id no pueden perderse mensajes en silencio (#131).
    pub fn save(&self, ses: Option<&mut Session>) -> Result<(), SessionsError> {
        let Some(ses) = ses else {
            return Err(SessionsError::MissingSession);
        };
        if !valid_id(&ses.id) {
            return Err(SessionsError::InvalidId);
        }
        if ses.version == 0 {
            ses.version = VERSION;
        }
        let destino = self.path(&ses.id)?;
        // Se compara contra la última versión que este proceso escribió. Un
        // `Session` nuevo no es un cambio ajeno: la app lo reconstruye en cada
        // guardado del mismo turno (agent.rs::sesion_actual, #131).
        let esperado = escritas()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&destino)
            .copied()
            .unwrap_or((ses.version, ses.updated));
        // Una slice nil se vuelve `[]` antes de escribir. Un `Vec` de
        // Rust siempre se serializa como `[]`: no hace falta normalizar.
        ses.updated = Utc::now();
        if ses.title.is_empty() {
            ses.title = title(&ses.messages);
        }
        // Un archivo del formato viejo (sin `version`) se deja pisar: es lo
        // que hace `migrate`. Solo compara una sesión del formato actual.
        if let Ok(datos) = fs::read(&destino)
            && let Ok(actual) = serde_json::from_slice::<Session>(&datos)
        {
            if actual.version > VERSION {
                return Err(SessionsError::UnsupportedVersion {
                    encontrada: actual.version,
                    esperada: VERSION,
                });
            }
            if actual.version == VERSION && (actual.version, actual.updated) != esperado {
                return Err(SessionsError::Conflicto);
            }
        }
        crear_dir(&self.dir)?;
        let datos = serde_json::to_vec(&*ses)?;
        escribir_atomico(&self.dir, &self.prefix, &destino, &datos)?;
        escritas()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(destino, (ses.version, ses.updated));
        Ok(())
    }

    /// Lee una sesión. Un archivo de otra versión se rechaza en vez de
    /// interpretarlo mal.
    pub fn load(&self, id: &str) -> Result<Session, SessionsError> {
        let ruta = self.path(id)?;
        let datos = fs::read(&ruta).map_err(|source| SessionsError::Open {
            path: ruta.clone(),
            source,
        })?;
        let ses: Session = serde_json::from_slice(&datos).map_err(SessionsError::Unreadable)?;
        if ses.version != VERSION {
            return Err(SessionsError::UnsupportedVersion {
                encontrada: ses.version,
                esperada: VERSION,
            });
        }
        if ses.id != id {
            return Err(SessionsError::IdMismatch);
        }
        Ok(ses)
    }

    fn metadatos(&self, ruta: &Path, id: &str) -> Option<Info> {
        let archivo = fs::File::open(ruta).ok()?;
        let meta: Metadatos = serde_json::from_reader(io::BufReader::new(archivo)).ok()?;
        (meta.version == VERSION && meta.id == id).then_some(Info {
            id: meta.id,
            title: meta.title,
            cwd: meta.cwd,
            created: meta.created,
            updated: meta.updated,
            messages: meta.messages.0,
        })
    }

    /// Devuelve las sesiones legibles, de la más nueva a la más vieja.
    pub fn list(&self) -> Result<Vec<Info>, SessionsError> {
        let entradas = match fs::read_dir(&self.dir) {
            Ok(entradas) => entradas,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut salida = Vec::new();
        for entrada in entradas {
            let entrada = entrada?;
            // Igual que `DirEntry.IsDir`: si el tipo no se puede leer,
            // se trata como archivo y el `load` decide si se salta.
            if entrada.file_type().is_ok_and(|tipo| tipo.is_dir()) {
                continue;
            }
            let Some(nombre) = entrada.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Some(id) = id_de_archivo(&nombre, &self.prefix) else {
                continue;
            };
            let Some(info) = self.metadatos(&entrada.path(), id) else {
                continue;
            };
            salida.push(info);
        }
        salida.sort_by_key(|info| std::cmp::Reverse(info.updated));
        Ok(salida)
    }

    /// La sesión más nueva, para `--continue`.
    pub fn latest(&self) -> Result<Session, SessionsError> {
        let lista = self.list()?;
        let Some(primera) = lista.first() else {
            return Err(SessionsError::NoSessions);
        };
        self.load(&primera.id)
    }

    /// Borra una sesión. Un archivo ausente no es error.
    pub fn delete(&self, id: &str) -> Result<(), SessionsError> {
        let ruta = self.path(id)?;
        match fs::remove_file(ruta) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    /// Borra cada sesión de este almacén y deja los demás archivos en paz.
    pub fn clear(&self) -> Result<(), SessionsError> {
        let entradas = match fs::read_dir(&self.dir) {
            Ok(entradas) => entradas,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        for entrada in entradas {
            let entrada = entrada?;
            if entrada.file_type().is_ok_and(|tipo| tipo.is_dir()) {
                continue;
            }
            let Some(nombre) = entrada.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            // Reutiliza el mismo filtro que `list`: extensión, prefijo e id
            // numérico. Un `c-apuntes.json` no es una sesión y no se borra.
            if id_de_archivo(&nombre, &self.prefix).is_none() {
                continue;
            }
            match fs::remove_file(entrada.path()) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }
}

/// Usa la primera pregunta como nombre. Sin pregunta, una fecha, para que una
/// lista nunca muestre una fila vacía.
pub fn title(msgs: &[Message]) -> String {
    for mensaje in msgs {
        if mensaje.role != "user" {
            continue;
        }
        let texto = mensaje.content.trim();
        if texto.is_empty() {
            continue;
        }
        let mut runas = texto.chars();
        let corto: String = runas.by_ref().take(60).collect();
        if runas.next().is_some() {
            return format!("{corto}…");
        }
        return texto.to_string();
    }
    format!("Sesión del {}", Local::now().format("%-d/%-m/%Y"))
}

/// Compara dos carpetas como lo hace el sistema: Windows sin distinguir
/// mayúsculas, el resto exacto.
pub fn same_cwd(a: &str, b: &str) -> bool {
    let a = clean_path(Path::new(a));
    let b = clean_path(Path::new(b));
    if cfg!(windows) {
        // Aproximación de igualdad sin distinguir mayúsculas para rutas de Windows.
        a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    } else {
        a == b
    }
}

// El cero de time.Time se escribe "0001-01-01T00:00:00Z".
fn tiempo_cero() -> DateTime<Utc> {
    let naive = NaiveDate::from_ymd_opt(1, 1, 1).and_then(|fecha| fecha.and_hms_opt(0, 0, 0));
    match naive {
        Some(naive) => DateTime::from_naive_utc_and_offset(naive, Utc),
        None => DateTime::UNIX_EPOCH,
    }
}

fn es_cero_i64(valor: &i64) -> bool {
    *valor == 0
}

fn es_cero_f64(valor: &f64) -> bool {
    *valor == 0.0
}

// Crea el temporal con 0600 en Unix, el equivalente a `os.CreateTemp` con el
// patrón del contrato: `s-*.tmp` sin prefijo y `{prefix}*.tmp` con prefijo. El
// sufijo mezcla reloj de pared, pid y un contador local: otro proceso que
// reutilice el pid no deja cien restos que bloqueen el guardado (#131).
fn crear_temporal(dir: &Path, prefix: &str) -> io::Result<(PathBuf, fs::File)> {
    let base = if prefix.is_empty() { "s-" } else { prefix };
    let nano = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duracion| duracion.as_nanos())
        .unwrap_or_default();
    for intento in 0..100 {
        let nombre = format!("{base}{}-{nano}-{intento}.tmp", std::process::id());
        let ruta = dir.join(nombre);
        match abrir_privado(&ruta) {
            Ok(archivo) => return Ok((ruta, archivo)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no se pudo crear un archivo temporal único",
    ))
}

fn abrir_privado(ruta: &Path) -> io::Result<fs::File> {
    let mut opciones = fs::OpenOptions::new();
    opciones.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opciones.mode(0o600);
    }
    opciones.open(ruta)
}

fn crear_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        let mut constructor = fs::DirBuilder::new();
        constructor.recursive(true).mode(0o700);
        constructor.create(dir)
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(dir)
    }
}

fn escribir_atomico(
    dir: &Path,
    prefix: &str,
    destino: &Path,
    datos: &[u8],
) -> Result<(), SessionsError> {
    let (temporal, mut archivo) = crear_temporal(dir, prefix)?;
    // Sin el sync_all, un corte de energía puede dejar el temporal vacío y el
    // rename lo pondría encima de la conversación buena. El contrato no sincroniza
    // Esto agrega durabilidad, no cambia el formato.
    let escritura = archivo.write_all(datos).and_then(|()| archivo.sync_all());
    // El contrato devuelve el error de Close; Rust cierra al soltar el File y no expone
    // ese error. La escritura, que es la que puede fallar por datos, sí se
    // revisa, y el temporal se borra ante cualquier fallo.
    drop(archivo);
    if let Err(error) = escritura {
        let _ = fs::remove_file(&temporal);
        return Err(error.into());
    }
    if let Err(error) = fs::rename(&temporal, destino) {
        let _ = fs::remove_file(&temporal);
        return Err(error.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mensaje(role: &str, content: &str) -> Message {
        Message {
            role: role.to_string(),
            content: content.to_string(),
            ..Message::default()
        }
    }

    #[test]
    fn guarda_carga_lista_y_borra() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store {
            dir: dir.path().to_path_buf(),
            prefix: "c-".to_string(),
        };
        let mut ses = new(r"C:\proyecto", "modelo");
        ses.messages = vec![
            mensaje("user", "arregla el test"),
            mensaje("assistant", "voy"),
        ];
        ses.tokens_in = 4;
        ses.tokens_out = 2;
        store.save(Some(&mut ses)).unwrap();
        assert!(
            dir.path().join(format!("c-{}.json", ses.id)).exists(),
            "el archivo no quedó con el prefijo"
        );

        let cargada = store.load(&ses.id).unwrap();
        assert_eq!(cargada.title, "arregla el test");
        assert_eq!(cargada.messages.len(), 2);
        assert_eq!((cargada.tokens_in, cargada.tokens_out), (4, 2));

        let lista = store.list().unwrap();
        assert_eq!(lista.len(), 1);
        assert_eq!(lista[0].id, ses.id);
        assert_eq!(lista[0].messages, 2);

        store.delete(&ses.id).unwrap();
        assert!(
            store.load(&ses.id).is_err(),
            "la sesión borrada todavía se carga"
        );
    }

    #[test]
    fn save_reemplaza_la_anterior_y_no_deja_temporales() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store {
            dir: dir.path().to_path_buf(),
            prefix: "c-".to_string(),
        };
        let mut ses = new("", "m");
        store.save(Some(&mut ses)).unwrap();
        ses.messages = vec![mensaje("user", "la última")];
        store.save(Some(&mut ses)).unwrap();

        let cargada = store.load(&ses.id).unwrap();
        assert_eq!(cargada.messages, ses.messages);
        let temporales: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entrada| entrada.unwrap().file_name())
            .filter(|nombre| nombre.to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(temporales.is_empty(), "quedaron temporales: {temporales:?}");
    }

    #[test]
    fn rechaza_ids_y_versiones_invalidas() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store {
            dir: dir.path().to_path_buf(),
            prefix: String::new(),
        };
        for id in ["", "..", "../x", "c-1", "123456789012345678901234567890123"] {
            assert!(store.load(id).is_err(), "Load({id:?}) no dio error");
        }
        let cwd = tempfile::tempdir().unwrap();
        let mut ses = new(&cwd.path().to_string_lossy(), "m");
        ses.version = VERSION + 1;
        store.save(Some(&mut ses)).unwrap();
        assert!(
            store.load(&ses.id).is_err(),
            "una versión futura se cargó como si fuera la actual"
        );
    }

    #[test]
    fn el_json_usa_las_claves_del_go_y_omite_los_vacios() {
        let mut ses = new("", "modelo");
        ses.id = "123".to_string();
        ses.messages = vec![mensaje("user", "hola")];
        ses.title = title(&ses.messages);
        let valor = serde_json::to_value(&ses).unwrap();
        assert_eq!(valor["version"], 1);
        assert_eq!(valor["id"], "123");
        assert_eq!(valor["title"], "hola");
        assert_eq!(valor["model"], "modelo");
        assert!(valor.get("cwd").is_none());
        assert!(valor.get("tokens_in").is_none());
        assert!(valor.get("tokens_out").is_none());
        assert!(valor.get("cost_usd").is_none());
        assert_eq!(valor["messages"][0]["Role"], "user");
        assert_eq!(valor["messages"][0]["Content"], "hola");

        // Un archivo guardado por una versión anterior: offset en la fecha, campos omitidos.
        let archivo_previo = r#"{"version":1,"id":"7","title":"t","cwd":"C:\\x","created":"2026-11-11T10:00:00.123-05:00","updated":"2026-11-11T10:00:00.123-05:00","messages":[],"tokens_in":1,"cost_usd":0.5}"#;
        let ses: Session = serde_json::from_str(archivo_previo).unwrap();
        assert_eq!(ses.cwd, r"C:\x");
        assert_eq!(ses.tokens_in, 1);
        assert_eq!(ses.tokens_out, 0);
        assert_eq!(ses.cost_usd, 0.5);
        assert!(ses.model.is_empty());
        assert!(ses.messages.is_empty());
    }

    #[test]
    fn title_recorta_a_sesenta_runas_y_usa_la_fecha_sin_pregunta() {
        let largo = "á".repeat(61);
        let msgs = vec![
            mensaje("assistant", "no"),
            mensaje("user", "   "),
            mensaje("user", &largo),
        ];
        let recortado = title(&msgs);
        assert_eq!(recortado.chars().count(), 61);
        assert_eq!(recortado, format!("{}…", "á".repeat(60)));

        let exacto = "b".repeat(60);
        assert_eq!(title(&[mensaje("user", &exacto)]), exacto);

        let sin_pregunta = title(&[mensaje("assistant", "hola")]);
        assert!(sin_pregunta.starts_with("Sesión del "));
        assert_eq!(sin_pregunta.matches('/').count(), 2);
    }

    #[test]
    fn id_de_archivo_quita_json_y_prefijo_y_exige_digitos() {
        let casos = [
            ("c-123.json", "c-", Some("123")),
            ("123.json", "", Some("123")),
            ("nota.txt", "", None),
            ("c-abc.json", "c-", None),
            ("x-123.json", "c-", None),
            ("123.json", "c-", None),
        ];
        for (nombre, prefix, esperado) in casos {
            assert_eq!(
                id_de_archivo(nombre, prefix),
                esperado,
                "id_de_archivo({nombre:?}, {prefix:?})"
            );
        }
    }

    #[test]
    fn clear_solo_borra_los_archivos_del_prefijo() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store {
            dir: dir.path().to_path_buf(),
            prefix: "c-".to_string(),
        };
        let mut ses = new("", "m");
        store.save(Some(&mut ses)).unwrap();
        let otro = dir.path().join("nota.txt");
        fs::write(&otro, b"no tocar").unwrap();
        store.clear().unwrap();
        assert!(otro.exists(), "Clear borró un archivo ajeno");
    }

    #[test]
    fn guardar_rechaza_un_cambio_ajeno_en_disco() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store {
            dir: dir.path().to_path_buf(),
            prefix: "c-".to_string(),
        };
        let mut ses = new("", "m");
        ses.messages = vec![mensaje("user", "mia")];
        store.save(Some(&mut ses)).unwrap();

        // Otro proceso cargó la misma sesión, agregó un mensaje y guardó.
        let mut ajena = store.load(&ses.id).unwrap();
        ajena.messages = vec![mensaje("user", "ajena")];
        ajena.updated = ses.updated + chrono::Duration::seconds(1);
        fs::write(
            store.path(&ses.id).unwrap(),
            serde_json::to_vec(&ajena).unwrap(),
        )
        .unwrap();

        assert!(
            store.save(Some(&mut ses)).is_err(),
            "se pisó un cambio ajeno sin comparar la versión"
        );
        let guardada = store.load(&ses.id).unwrap();
        assert_eq!(
            guardada.messages, ajena.messages,
            "el guardado cabreó la conversación del otro proceso"
        );
    }

    #[test]
    fn guardar_sobre_un_formato_viejo_no_da_conflicto() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store {
            dir: dir.path().to_path_buf(),
            prefix: "c-".to_string(),
        };
        let mut ses = new("", "m");
        // Un archivo del formato viejo: sin `version`, con `at` en vez de
        // `updated`. `migrate` lo reescribe con el formato compartido.
        let viejo = format!(
            r#"{{"id":"{}","title":"vieja","at":"2026-11-01T10:00:00Z","messages":[]}}"#,
            ses.id
        );
        fs::write(store.path(&ses.id).unwrap(), viejo).unwrap();

        store
            .save(Some(&mut ses))
            .expect("un archivo viejo no es un cambio concurrente");
        assert_eq!(store.load(&ses.id).unwrap().version, VERSION);
    }

    #[test]
    fn clear_no_borra_json_que_no_es_una_sesion() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store {
            dir: dir.path().to_path_buf(),
            prefix: "c-".to_string(),
        };
        let mut ses = new("", "m");
        store.save(Some(&mut ses)).unwrap();
        let apuntes = dir.path().join("c-apuntes.json");
        fs::write(&apuntes, b"no tocar").unwrap();

        store.clear().unwrap();
        assert!(
            apuntes.exists(),
            "clear borró c-apuntes.json: no era un id de sesión"
        );
        assert!(
            store.load(&ses.id).is_err(),
            "clear no borró la sesión real"
        );
    }

    #[test]
    fn lista_cuenta_mensajes_sin_deserializar_imagenes() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store {
            dir: dir.path().to_path_buf(),
            prefix: "c-".to_string(),
        };
        let ses = new("", "m");
        let datos = serde_json::json!({
            "version": VERSION,
            "id": ses.id,
            "title": "con adjunto",
            "created": ses.created,
            "updated": ses.updated,
            "messages": [{
                "Role": "user",
                "Content": "x",
                "Images": [{"MIME": "image/png", "Data": "AAAA", "Preview": 7}]
            }]
        });
        fs::write(
            store.path(&ses.id).unwrap(),
            serde_json::to_vec(&datos).unwrap(),
        )
        .unwrap();
        assert!(store.load(&ses.id).is_err(), "el mensaje inválido cargó");

        let lista = store.list().unwrap();
        assert_eq!(lista.len(), 1);
        assert_eq!(
            (lista[0].title.as_str(), lista[0].messages),
            ("con adjunto", 1)
        );
    }

    #[test]
    fn guarda_aunque_queden_temporales_del_mismo_pid() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store {
            dir: dir.path().to_path_buf(),
            prefix: "c-".to_string(),
        };
        // Restos de una encarnación anterior con el mismo pid. El rango cubre
        // cualquier posición del contador del proceso actual.
        for n in 0..200 {
            let ruta = dir.path().join(format!("c-{}-{n}.tmp", std::process::id()));
            fs::write(ruta, b"resto").unwrap();
        }
        let mut ses = new("", "m");
        store
            .save(Some(&mut ses))
            .expect("cien temporales viejos no pueden bloquear el guardado");
        assert_eq!(store.load(&ses.id).unwrap().id, ses.id);
    }
}
