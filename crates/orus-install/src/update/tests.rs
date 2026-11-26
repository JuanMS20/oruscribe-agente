//! Tests portados y
//! con un servidor HTTP local.
//! No tocan la red real ni el ejecutable de la suite.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};

use super::{
    ApplyLock, Error, Keys, Manifest, TRUSTED_KEYS_B64, check, check_with_keys, download, swap,
};

#[cfg(windows)]
use super::rollback;
#[cfg(windows)]
use super::swap::intercambiar;

// ---------------------------------------------------------------------------
// Servidor local mínimo: una ruta por archivo, respuesta fija y cierre.
// ---------------------------------------------------------------------------

struct Ruta {
    estado: u16,
    cuerpo: Vec<u8>,
    cortada: bool,
}

impl Ruta {
    fn ok(cuerpo: Vec<u8>) -> Self {
        Self {
            estado: 200,
            cuerpo,
            cortada: false,
        }
    }

    fn estado(estado: u16) -> Self {
        Self {
            estado,
            cuerpo: Vec::new(),
            cortada: false,
        }
    }

    fn cortada(cuerpo: Vec<u8>) -> Self {
        Self {
            estado: 200,
            cuerpo,
            cortada: true,
        }
    }
}

struct Servidor {
    base: String,
    parar: Arc<AtomicBool>,
    hilo: Option<thread::JoinHandle<()>>,
}

impl Servidor {
    fn nuevo(rutas: Vec<(&str, Ruta)>) -> Self {
        let mapa: HashMap<String, Ruta> = rutas
            .into_iter()
            .map(|(ruta, respuesta)| (ruta.to_owned(), respuesta))
            .collect();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let direccion = listener.local_addr().unwrap();
        let parar = Arc::new(AtomicBool::new(false));
        let parar_hilo = Arc::clone(&parar);
        let hilo = thread::spawn(move || {
            while !parar_hilo.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        // El listener es no bloqueante para poder parar el hilo;
                        // el socket aceptado debe quedar bloqueante o la lectura
                        // de la petición puede adelantarse y cerrar sin respuesta.
                        let _ = stream.set_nonblocking(false);
                        atender(&mut stream, &mapa);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            base: format!("http://{direccion}"),
            parar,
            hilo: Some(hilo),
        }
    }

    fn url(&self, ruta: &str) -> String {
        format!("{}{ruta}", self.base)
    }
}

impl Drop for Servidor {
    fn drop(&mut self) {
        self.parar.store(true, Ordering::Release);
        if let Some(hilo) = self.hilo.take() {
            let _ = hilo.join();
        }
    }
}

fn atender(stream: &mut TcpStream, rutas: &HashMap<String, Ruta>) {
    let mut lector = BufReader::new(&*stream);
    let mut primera = String::new();
    if lector.read_line(&mut primera).is_err() {
        return;
    }
    loop {
        let mut linea = String::new();
        match lector.read_line(&mut linea) {
            Ok(0) | Err(_) => break,
            Ok(_) if linea == "\r\n" || linea == "\n" => break,
            Ok(_) => {}
        }
    }
    drop(lector);

    let ruta = primera.split_whitespace().nth(1).unwrap_or("/");
    let Some(respuesta) = rutas.get(ruta) else {
        escribir(stream, 404, b"", false);
        return;
    };
    escribir(
        stream,
        respuesta.estado,
        &respuesta.cuerpo,
        respuesta.cortada,
    );
}

fn escribir(stream: &mut TcpStream, estado: u16, cuerpo: &[u8], cortada: bool) {
    let motivo = match estado {
        200 => "OK",
        302 => "Found",
        404 => "Not Found",
        413 => "Payload Too Large",
        _ => "Internal Server Error",
    };
    let largo = if cortada {
        cuerpo.len() + 100
    } else {
        cuerpo.len()
    };
    let cabecera = format!(
        "HTTP/1.1 {estado} {motivo}\r\nContent-Length: {largo}\r\nConnection: close\r\n\r\n"
    );
    let _ = stream.write_all(cabecera.as_bytes());
    let _ = stream.write_all(cuerpo);
    let _ = stream.flush();
}

// ---------------------------------------------------------------------------
// Utilidades
// ---------------------------------------------------------------------------

fn sha256_hex(cuerpo: &[u8]) -> String {
    hex::encode(Sha256::digest(cuerpo))
}

fn cuerpo_manifiesto(descarga: &str, sha: &str) -> Vec<u8> {
    cuerpo_manifiesto_con_notas(descarga, sha, "")
}

fn cuerpo_manifiesto_con_notas(descarga: &str, sha: &str, notas: &str) -> Vec<u8> {
    serde_json::to_vec(&Manifest {
        version: "9.9.9".to_owned(),
        notes: notas.to_owned(),
        windows: descarga.to_owned(),
        sha256: sha.to_owned(),
        linux: descarga.to_owned(),
        linux_sha256: sha.to_owned(),
    })
    .unwrap()
}

fn servidor_manifiesto(cuerpo: Vec<u8>, firma: Option<Vec<u8>>) -> Servidor {
    let mut rutas = vec![("/latest.json", Ruta::ok(cuerpo))];
    if let Some(firma) = firma {
        rutas.push(("/latest.json.sig", Ruta::ok(firma)));
    }
    Servidor::nuevo(rutas)
}

fn publica(semilla: u8) -> String {
    STANDARD.encode(
        SigningKey::from_bytes(&[semilla; 32])
            .verifying_key()
            .to_bytes(),
    )
}

fn firma(semilla: u8, cuerpo: &[u8]) -> Vec<u8> {
    STANDARD
        .encode(
            SigningKey::from_bytes(&[semilla; 32])
                .sign(cuerpo)
                .to_bytes(),
        )
        .into_bytes()
}

fn mensaje(error: Error) -> String {
    error.to_string()
}

fn llaves(semillas: &[u8]) -> Keys {
    let publicas: Vec<String> = semillas.iter().map(|semilla| publica(*semilla)).collect();
    let textos: Vec<&str> = publicas.iter().map(String::as_str).collect();
    Keys::from_base64(&textos).unwrap()
}

// ---------------------------------------------------------------------------
// Chequeo del manifiesto
// ---------------------------------------------------------------------------

#[tokio::test]
async fn resultado_sin_url_omite_latest_notas_url_y_sha() {
    let info = check(&reqwest::Client::new(), "", "0.5.4").await.unwrap();
    assert!(!info.configured);
    assert!(!info.available);
    assert_eq!(info.current, "0.5.4");
    let valor = serde_json::to_value(&info).unwrap();
    assert_eq!(
        valor,
        serde_json::json!({
            "configured": false,
            "available": false,
            "current": "0.5.4",
        })
    );
}

#[tokio::test]
async fn resultado_disponible_lleva_las_siete_claves() {
    let descarga = Servidor::nuevo(vec![("/x.exe", Ruta::ok(b"exe nuevo".to_vec()))]);
    let cuerpo = b"exe nuevo";
    let manifiesto = servidor_manifiesto(
        cuerpo_manifiesto_con_notas(&descarga.url("/x.exe"), &sha256_hex(cuerpo), "notas nuevas"),
        None,
    );

    let info = check_with_keys(
        &reqwest::Client::new(),
        &manifiesto.url("/latest.json"),
        "0.1.0",
        &Keys::empty(),
    )
    .await
    .unwrap();

    let valor = serde_json::to_value(&info).unwrap();
    assert_eq!(
        valor,
        serde_json::json!({
            "configured": true,
            "available": true,
            "current": "0.1.0",
            "latest": "9.9.9",
            "notes": "notas nuevas",
            "url": descarga.url("/x.exe"),
            "sha256": sha256_hex(cuerpo),
        })
    );
}

#[tokio::test]
async fn check_rechaza_manifiesto_con_huella_rota() {
    let letras = "z".repeat(64);
    for sha in ["", "abc", letras.as_str()] {
        let descarga = Servidor::nuevo(vec![("/x.exe", Ruta::ok(b"exe".to_vec()))]);
        let manifiesto = servidor_manifiesto(cuerpo_manifiesto(&descarga.url("/x.exe"), sha), None);
        let error = check_with_keys(
            &reqwest::Client::new(),
            &manifiesto.url("/latest.json"),
            "0.1.0",
            &Keys::empty(),
        )
        .await
        .unwrap_err();
        assert_eq!(
            mensaje(error),
            "el manifiesto no trae una huella válida para esta plataforma",
            "huella {sha:?}"
        );
    }
}

#[tokio::test]
async fn check_rechaza_descarga_de_otro_host() {
    let manifiesto = servidor_manifiesto(
        cuerpo_manifiesto("https://evil.example/x.exe", &"a".repeat(64)),
        None,
    );
    let error = check_with_keys(
        &reqwest::Client::new(),
        &manifiesto.url("/latest.json"),
        "0.1.0",
        &Keys::empty(),
    )
    .await
    .unwrap_err();
    let texto = mensaje(error);
    assert!(texto.contains("evil.example"), "{texto}");
    assert_eq!(
        texto,
        "la descarga no viene de un sitio permitido: evil.example"
    );
}

#[tokio::test]
async fn check_acepta_manifiesto_bueno() {
    let cuerpo = b"exe nuevo";
    let descarga = Servidor::nuevo(vec![("/x.exe", Ruta::ok(cuerpo.to_vec()))]);
    let manifiesto = servidor_manifiesto(
        cuerpo_manifiesto(&descarga.url("/x.exe"), &sha256_hex(cuerpo)),
        None,
    );

    let info = check_with_keys(
        &reqwest::Client::new(),
        &manifiesto.url("/latest.json"),
        "0.1.0",
        &Keys::empty(),
    )
    .await
    .unwrap();

    assert!(info.available);
    assert_eq!(info.latest, "9.9.9");
    assert_eq!(info.url, descarga.url("/x.exe"));
}

#[tokio::test]
async fn check_version_nueva_sin_descarga_es_error() {
    let manifiesto = servidor_manifiesto(cuerpo_manifiesto("", ""), None);
    let error = check_with_keys(
        &reqwest::Client::new(),
        &manifiesto.url("/latest.json"),
        "0.1.0",
        &Keys::empty(),
    )
    .await
    .unwrap_err();
    let texto = mensaje(error);
    assert!(texto.contains("9.9.9"), "{texto}");
    assert_eq!(
        texto,
        "hay una versión nueva (9.9.9), pero todavía no está disponible para este sistema"
    );
}

#[tokio::test]
async fn check_misma_version_sin_descarga_esta_al_dia() {
    let manifiesto = servidor_manifiesto(cuerpo_manifiesto("", ""), None);
    let info = check_with_keys(
        &reqwest::Client::new(),
        &manifiesto.url("/latest.json"),
        "9.9.9",
        &Keys::empty(),
    )
    .await
    .unwrap();
    assert!(!info.available);
    assert_eq!(info.latest, "9.9.9");
    assert!(info.url.is_empty());
    assert!(info.sha256.is_empty());
}

#[tokio::test]
async fn check_rechaza_descarga_fuera_del_repositorio() {
    let casos = [
        (
            "https://github.com/JuanMS20/oruscribe-agente-descargas/releases/download/v9.9.9/Oruscribe.Agente.exe",
            true,
        ),
        (
            "https://github.com/otro/repo/releases/download/v9.9.9/Oruscribe.Agente.exe",
            false,
        ),
        (
            "https://github.com/JuanMS20/oruscribe-agente-descargas/../otro/Oruscribe.Agente.exe",
            false,
        ),
    ];
    for (descarga, valida) in casos {
        let manifiesto = servidor_manifiesto(cuerpo_manifiesto(descarga, &"a".repeat(64)), None);
        let resultado = check_with_keys(
            &reqwest::Client::new(),
            &manifiesto.url("/latest.json"),
            "0.1.0",
            &Keys::empty(),
        )
        .await;
        assert_eq!(resultado.is_ok(), valida, "{descarga}: {resultado:?}");
    }
}

#[tokio::test]
async fn check_servidor_413_da_el_codigo() {
    let manifiesto = Servidor::nuevo(vec![("/latest.json", Ruta::estado(413))]);
    let error = check_with_keys(
        &reqwest::Client::new(),
        &manifiesto.url("/latest.json"),
        "0.1.0",
        &Keys::empty(),
    )
    .await
    .unwrap_err();
    assert_eq!(mensaje(error), "el servidor de versiones respondió 413");
}

#[tokio::test]
async fn check_manifiesto_de_mas_de_un_mib_no_se_entiende() {
    let notas = "x".repeat(2 << 20);
    let manifiesto = servidor_manifiesto(
        cuerpo_manifiesto_con_notas(
            "https://github.com/JuanMS20/oruscribe-agente-descargas/releases/download/v9.9.9/x.exe",
            &"a".repeat(64),
            &notas,
        ),
        None,
    );
    let error = check_with_keys(
        &reqwest::Client::new(),
        &manifiesto.url("/latest.json"),
        "0.1.0",
        &Keys::empty(),
    )
    .await
    .unwrap_err();
    let texto = mensaje(error);
    assert!(
        texto.starts_with("el manifiesto no se entiende: "),
        "{texto}"
    );
}

// ---------------------------------------------------------------------------
// Firma Ed25519
// ---------------------------------------------------------------------------

#[test]
fn llaves_embebidas_estan_bien_formadas() {
    let keys = Keys::embedded().unwrap();
    assert_eq!(keys.len(), 2, "principal y respaldo");
    let decodificadas: Vec<Vec<u8>> = TRUSTED_KEYS_B64
        .iter()
        .map(|texto| STANDARD.decode(texto.as_bytes()).unwrap())
        .collect();
    for llave in &decodificadas {
        assert_eq!(llave.len(), 32);
    }
    assert_ne!(decodificadas[0], decodificadas[1]);
}

#[test]
fn llave_mal_formada_no_se_ignora() {
    for mala in ["no es base64!", "AAAA"] {
        let error = Keys::from_base64(&[mala]).unwrap_err();
        assert_eq!(
            mensaje(error),
            "una llave pública embebida está mal formada",
            "{mala}"
        );
    }
}

#[tokio::test]
async fn check_acepta_manifiesto_firmado() {
    let cuerpo = cuerpo_manifiesto(
        "https://github.com/JuanMS20/oruscribe-agente-descargas/releases/download/v9.9.9/x.exe",
        &"a".repeat(64),
    );
    let manifiesto = servidor_manifiesto(cuerpo.clone(), Some(firma(1, &cuerpo)));
    let keys = llaves(&[1]);

    let info = check_with_keys(
        &reqwest::Client::new(),
        &manifiesto.url("/latest.json"),
        "0.1.0",
        &keys,
    )
    .await
    .unwrap();

    assert!(info.available);
    assert_eq!(info.latest, "9.9.9");
}

#[tokio::test]
async fn check_acepta_firma_de_la_llave_de_respaldo() {
    let cuerpo = cuerpo_manifiesto(
        "https://github.com/JuanMS20/oruscribe-agente-descargas/releases/download/v9.9.9/x.exe",
        &"a".repeat(64),
    );
    let manifiesto = servidor_manifiesto(cuerpo.clone(), Some(firma(2, &cuerpo)));
    let keys = llaves(&[1, 2]);

    let info = check_with_keys(
        &reqwest::Client::new(),
        &manifiesto.url("/latest.json"),
        "0.1.0",
        &keys,
    )
    .await
    .unwrap();

    assert!(info.available);
}

#[tokio::test]
async fn check_rechaza_manifiesto_cambiado_despues_de_firmar() {
    let original = cuerpo_manifiesto(
        "https://github.com/JuanMS20/oruscribe-agente-descargas/releases/download/v9.9.9/x.exe",
        &"a".repeat(64),
    );
    let cambiado = cuerpo_manifiesto_con_notas(
        "https://github.com/JuanMS20/oruscribe-agente-descargas/releases/download/v9.9.9/x.exe",
        &"a".repeat(64),
        "otra cosa",
    );
    let manifiesto = servidor_manifiesto(cambiado, Some(firma(1, &original)));
    let keys = llaves(&[1]);

    let error = check_with_keys(
        &reqwest::Client::new(),
        &manifiesto.url("/latest.json"),
        "0.1.0",
        &keys,
    )
    .await
    .unwrap_err();

    assert_eq!(
        mensaje(error),
        "el manifiesto no está firmado por Oruscribe; no se actualiza"
    );
}

#[tokio::test]
async fn check_rechaza_manifiesto_sin_firma() {
    let cuerpo = cuerpo_manifiesto(
        "https://github.com/JuanMS20/oruscribe-agente-descargas/releases/download/v9.9.9/x.exe",
        &"a".repeat(64),
    );
    let manifiesto = servidor_manifiesto(cuerpo, None);
    let keys = llaves(&[1]);

    let error = check_with_keys(
        &reqwest::Client::new(),
        &manifiesto.url("/latest.json"),
        "0.1.0",
        &keys,
    )
    .await
    .unwrap_err();

    assert_eq!(
        mensaje(error),
        "el manifiesto no está firmado por Oruscribe; no se actualiza"
    );
}

#[tokio::test]
async fn check_rechaza_firma_de_otra_llave() {
    let cuerpo = cuerpo_manifiesto(
        "https://github.com/JuanMS20/oruscribe-agente-descargas/releases/download/v9.9.9/x.exe",
        &"a".repeat(64),
    );
    let manifiesto = servidor_manifiesto(cuerpo.clone(), Some(firma(2, &cuerpo)));
    let keys = llaves(&[1]);

    let error = check_with_keys(
        &reqwest::Client::new(),
        &manifiesto.url("/latest.json"),
        "0.1.0",
        &keys,
    )
    .await
    .unwrap_err();

    assert_eq!(
        mensaje(error),
        "el manifiesto no está firmado por Oruscribe; no se actualiza"
    );
}

#[tokio::test]
async fn check_sin_llaves_no_exige_firma() {
    let cuerpo = cuerpo_manifiesto(
        "https://github.com/JuanMS20/oruscribe-agente-descargas/releases/download/v9.9.9/x.exe",
        &"a".repeat(64),
    );
    let manifiesto = servidor_manifiesto(cuerpo, None);

    let info = check_with_keys(
        &reqwest::Client::new(),
        &manifiesto.url("/latest.json"),
        "0.1.0",
        &Keys::empty(),
    )
    .await
    .unwrap();

    assert!(info.available);
}

#[tokio::test]
async fn check_produccion_exige_firma() {
    let cuerpo = cuerpo_manifiesto(
        "https://github.com/JuanMS20/oruscribe-agente-descargas/releases/download/v9.9.9/x.exe",
        &"a".repeat(64),
    );
    let manifiesto = servidor_manifiesto(cuerpo, None);

    let error = check(
        &reqwest::Client::new(),
        &manifiesto.url("/latest.json"),
        "0.1.0",
    )
    .await
    .unwrap_err();

    assert_eq!(
        mensaje(error),
        "el manifiesto no está firmado por Oruscribe; no se actualiza"
    );
}

#[tokio::test]
async fn check_rechaza_firma_que_no_es_base64() {
    let cuerpo = cuerpo_manifiesto(
        "https://github.com/JuanMS20/oruscribe-agente-descargas/releases/download/v9.9.9/x.exe",
        &"a".repeat(64),
    );
    let manifiesto = servidor_manifiesto(cuerpo, Some(b"no es base64!".to_vec()));
    let keys = llaves(&[1]);

    let error = check_with_keys(
        &reqwest::Client::new(),
        &manifiesto.url("/latest.json"),
        "0.1.0",
        &keys,
    )
    .await
    .unwrap_err();

    assert_eq!(
        mensaje(error),
        "el manifiesto no está firmado por Oruscribe; no se actualiza"
    );
}

#[tokio::test]
async fn check_rechaza_firma_de_mas_de_cuatro_kib() {
    let cuerpo = cuerpo_manifiesto(
        "https://github.com/JuanMS20/oruscribe-agente-descargas/releases/download/v9.9.9/x.exe",
        &"a".repeat(64),
    );
    // La firma válida va después de más de 4 KiB de espacios: el tope de
    // lectura corta antes de llegar a ella y el `trim` solo ve espacios.
    // Con la firma delante y relleno detrás, el rechazo venía de la
    // verificación y el test pasaba igual sin LIMITE_FIRMA (issue #98).
    let mut grande = vec![b' '; (4 << 10) + 1];
    grande.extend(firma(1, &cuerpo));
    let manifiesto = servidor_manifiesto(cuerpo, Some(grande));
    let keys = llaves(&[1]);

    let error = check_with_keys(
        &reqwest::Client::new(),
        &manifiesto.url("/latest.json"),
        "0.1.0",
        &keys,
    )
    .await
    .unwrap_err();

    assert_eq!(
        mensaje(error),
        "el manifiesto no está firmado por Oruscribe; no se actualiza"
    );
}

// ---------------------------------------------------------------------------
// Descarga
// ---------------------------------------------------------------------------

#[tokio::test]
async fn download_rechaza_sin_huella() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("nuevo.exe");
    let error = download(
        &reqwest::Client::new(),
        "http://127.0.0.1:9/x.exe",
        "",
        &dest,
    )
    .await
    .unwrap_err();
    assert_eq!(
        mensaje(error),
        "el manifiesto no trae una huella válida para esta plataforma"
    );
    assert!(!dest.exists());
}

#[tokio::test]
async fn download_rechaza_host_no_permitido() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("nuevo.exe");
    let error = download(
        &reqwest::Client::new(),
        "https://evil.example/x.exe",
        &"a".repeat(64),
        &dest,
    )
    .await
    .unwrap_err();
    assert_eq!(
        mensaje(error),
        "la descarga no viene de un sitio permitido: evil.example"
    );
    assert!(!dest.exists());
}

#[tokio::test]
async fn download_borra_el_archivo_con_huella_mala() {
    let servidor = Servidor::nuevo(vec![("/x.exe", Ruta::ok(b"exe".to_vec()))]);
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("nuevo.exe");

    let error = download(
        &reqwest::Client::new(),
        &servidor.url("/x.exe"),
        &"0".repeat(64),
        &dest,
    )
    .await
    .unwrap_err();

    assert_eq!(
        mensaje(error),
        "la descarga no coincide con la firma del manifiesto"
    );
    assert!(!dest.exists());
}

#[tokio::test]
async fn download_conserva_el_archivo_con_huella_buena() {
    let cuerpo = b"exe nuevo";
    let servidor = Servidor::nuevo(vec![("/x.exe", Ruta::ok(cuerpo.to_vec()))]);
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("nuevo.exe");

    download(
        &reqwest::Client::new(),
        &servidor.url("/x.exe"),
        &sha256_hex(cuerpo),
        &dest,
    )
    .await
    .unwrap();

    assert_eq!(std::fs::read(&dest).unwrap().as_slice(), &cuerpo[..]);
}

#[tokio::test]
async fn download_crea_la_carpeta_destino() {
    let servidor = Servidor::nuevo(vec![("/x.exe", Ruta::ok(b"exe".to_vec()))]);
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("sub").join("nuevo.exe");

    download(
        &reqwest::Client::new(),
        &servidor.url("/x.exe"),
        &sha256_hex(b"exe"),
        &dest,
    )
    .await
    .unwrap();

    assert!(dest.exists());
}

#[tokio::test]
async fn download_parcial_se_borra() {
    let cuerpo = b"exe nuevo";
    let servidor = Servidor::nuevo(vec![("/x.exe", Ruta::cortada(cuerpo.to_vec()))]);
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("nuevo.exe");

    let resultado = download(
        &reqwest::Client::new(),
        &servidor.url("/x.exe"),
        &sha256_hex(cuerpo),
        &dest,
    )
    .await;

    assert!(resultado.is_err(), "una descarga cortada no puede pasar");
    assert!(!dest.exists(), "no puede quedar un archivo a medias");
}

// ---------------------------------------------------------------------------
// Swap y candado
// ---------------------------------------------------------------------------

#[test]
fn swap_reemplaza_el_target() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("app");
    let descargado = dir.path().join("app.new");
    std::fs::write(&target, b"viejo").unwrap();
    std::fs::write(&descargado, b"nuevo").unwrap();

    swap(&descargado, &target).unwrap();

    assert_eq!(std::fs::read(&target).unwrap().as_slice(), &b"nuevo"[..]);
}

/// En Windows el swap va por dos renames y deja `target.old` como copia de
/// rescate: el rollback depende de ese archivo. Sin esta aserción, una
/// regresión que deja de producir el .old queda verde (issue #98).
#[cfg(windows)]
#[test]
fn swap_conserva_el_old_y_el_rollback_lo_restaura() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("app.exe");
    let descargado = dir.path().join("app.exe.new");
    std::fs::write(&target, b"viejo").unwrap();
    std::fs::write(&descargado, b"nuevo").unwrap();

    swap(&descargado, &target).unwrap();

    let old = dir.path().join("app.exe.old");
    assert_eq!(
        std::fs::read(&old).unwrap().as_slice(),
        &b"viejo"[..],
        "el swap no conservó la copia .old con el contenido viejo"
    );
    assert_eq!(std::fs::read(&target).unwrap().as_slice(), &b"nuevo"[..]);

    // La cadena completa de rescate: el .old que dejó el swap vuelve a su
    // sitio y la copia nueva queda como .failed para poder mirarla.
    rollback(&target).unwrap();
    assert_eq!(std::fs::read(&target).unwrap().as_slice(), &b"viejo"[..]);
    let failed = dir.path().join("app.exe.failed");
    assert_eq!(std::fs::read(&failed).unwrap().as_slice(), &b"nuevo"[..]);
}

#[cfg(windows)]
#[test]
fn swap_con_restauracion_fallida_explica_donde_queda_la_copia() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("app.exe");
    let descargado = dir.path().join("app.exe.new");
    let old = dir.path().join("app.exe.old");
    std::fs::write(&target, b"viejo").unwrap();
    std::fs::write(&descargado, b"nuevo").unwrap();

    let error = intercambiar(&descargado, &target, |origen, destino| {
        if origen == descargado || (origen == old && destino == target) {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "bloqueado",
            ))
        } else {
            std::fs::rename(origen, destino)
        }
    })
    .expect_err("si fallan los dos renombres tiene que fallar el swap");

    assert!(error.to_string().contains("bloqueado"), "{error}");
    assert!(error.to_string().contains("restaurar"), "{error}");
    assert!(error.to_string().contains("app.exe.old"), "{error}");
    assert!(!target.exists(), "la app se quedó sin ejecutable");
    assert!(old.exists(), "la copia de rescate no se conservó");
}

#[test]
fn swap_conserva_el_target_si_falta_el_descargado() {
    let dir = tempfile::tempdir().unwrap();
    let target: PathBuf = dir.path().join("app");
    std::fs::write(&target, b"viejo").unwrap();

    let resultado = swap(&dir.path().join("no-existe"), &target);

    assert!(resultado.is_err(), "swap sin descarga no puede pasar");
    assert_eq!(std::fs::read(&target).unwrap().as_slice(), &b"viejo"[..]);
}

#[test]
fn candado_una_actualizacion_a_la_vez() {
    let candado = ApplyLock::new();
    let guardia = candado.try_acquire().unwrap();
    assert!(candado.try_acquire().is_none());
    drop(guardia);
    assert!(candado.try_acquire().is_some());
}
