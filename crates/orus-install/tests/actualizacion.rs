//! Extremo a extremo del actualizador: manifiesto firmado con llaves
//! desechables, servidor de loopback, descarga con aviso de bytes, extracción
//! del paquete y swap.
//!
//! No toca la red real ni el ejecutable de la suite: el destino es un archivo
//! temporal y las llaves se generan en el test.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};

use orus_install::update::paquete::{NOMBRE_BINARIO, extraer_binario};
use orus_install::update::{Keys, Manifest, check_with_keys, download_con, responde_version, swap};

// ---------------------------------------------------------------------------
// Servidor local: una respuesta fija por ruta y cierre.
// ---------------------------------------------------------------------------

struct Servidor {
    base: String,
    parar: Arc<AtomicBool>,
    hilo: Option<thread::JoinHandle<()>>,
}

impl Servidor {
    /// Monta el servidor con las rutas que arma la clausura: así el manifiesto
    /// puede nombrar la URL de descarga del propio loopback.
    fn nuevo(rutas: impl FnOnce(&str) -> Vec<(&'static str, Vec<u8>)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("puerto local");
        listener
            .set_nonblocking(true)
            .expect("listener no bloqueante");
        let direccion = listener.local_addr().expect("dirección local");
        let base = format!("http://{direccion}");
        let mapa: HashMap<String, Vec<u8>> = rutas(&base)
            .into_iter()
            .map(|(ruta, cuerpo)| (ruta.to_owned(), cuerpo))
            .collect();
        let parar = Arc::new(AtomicBool::new(false));
        let parar_hilo = Arc::clone(&parar);
        let hilo = thread::spawn(move || {
            while !parar_hilo.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        // El listener es no bloqueante para poder parar; el
                        // socket aceptado tiene que serlo de lectura o la
                        // respuesta se corta antes de tiempo.
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

fn atender(stream: &mut TcpStream, rutas: &HashMap<String, Vec<u8>>) {
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
    let (estado, cuerpo) = match rutas.get(ruta) {
        Some(cuerpo) => (200, cuerpo.as_slice()),
        None => (404, &b""[..]),
    };
    let motivo = if estado == 200 { "OK" } else { "Not Found" };
    let cabecera = format!(
        "HTTP/1.1 {estado} {motivo}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        cuerpo.len()
    );
    let _ = stream.write_all(cabecera.as_bytes());
    let _ = stream.write_all(cuerpo);
    let _ = stream.flush();
}

// ---------------------------------------------------------------------------
// Utilidades
// ---------------------------------------------------------------------------

fn sha256(cuerpo: &[u8]) -> String {
    hex::encode(Sha256::digest(cuerpo))
}

/// Arma un `.tar.gz` con las entradas dadas, todas modo 0755.
fn paquete_tar_gz(entradas: &[(&str, &[u8])]) -> Vec<u8> {
    let comprimido = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut constructor = tar::Builder::new(comprimido);
    for (nombre, cuerpo) in entradas {
        let mut cabecera = tar::Header::new_gnu();
        cabecera.set_mode(0o755);
        cabecera.set_size(cuerpo.len() as u64);
        cabecera.set_cksum();
        constructor
            .append_data(&mut cabecera, nombre, *cuerpo)
            .expect("entrada del tar");
    }
    constructor
        .into_inner()
        .expect("cerrar el tar")
        .finish()
        .expect("cerrar el gzip")
}

/// Arma un `.tar.gz` con una entrada cuyo nombre se escribe crudo en la
/// cabecera: el constructor de `tar` rechaza rutas absolutas y con `..`, y
/// acá hace falta justamente una que apunte fuera.
fn paquete_con_ruta_cruda(nombre: &[u8], cuerpo: &[u8]) -> Vec<u8> {
    let comprimido = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut constructor = tar::Builder::new(comprimido);
    let mut cabecera = tar::Header::new_gnu();
    cabecera.set_mode(0o755);
    cabecera.set_size(cuerpo.len() as u64);
    {
        let gnu = cabecera.as_gnu_mut().expect("cabecera gnu");
        gnu.name[..nombre.len()].copy_from_slice(nombre);
    }
    cabecera.set_cksum();
    constructor
        .append(&cabecera, cuerpo)
        .expect("entrada cruda del tar");
    constructor
        .into_inner()
        .expect("cerrar el tar")
        .finish()
        .expect("cerrar el gzip")
}

fn firmar(semilla: u8, cuerpo: &[u8]) -> (String, Vec<u8>) {
    let llave = SigningKey::from_bytes(&[semilla; 32]);
    let publica = STANDARD.encode(llave.verifying_key().to_bytes());
    let firma = STANDARD.encode(llave.sign(cuerpo).to_bytes()).into_bytes();
    (publica, firma)
}

fn manifiesto(url: &str, sha: &str) -> Vec<u8> {
    serde_json::to_vec(&Manifest {
        version: "9.9.9".to_owned(),
        windows: url.to_owned(),
        sha256: sha.to_owned(),
        linux: url.to_owned(),
        linux_sha256: sha.to_owned(),
        ..Manifest::default()
    })
    .expect("manifiesto")
}

// ---------------------------------------------------------------------------
// Extracción del paquete
// ---------------------------------------------------------------------------

#[test]
fn extraer_deja_los_bytes_y_el_modo_del_binario() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let paquete = dir.path().join("paquete.tar.gz");
    std::fs::write(
        &paquete,
        paquete_tar_gz(&[
            ("install.sh", b"#!/bin/sh\necho hola\n"),
            (NOMBRE_BINARIO, b"soy el binario nuevo"),
        ]),
    )
    .expect("paquete");

    let destino = dir.path().join("nuevo");
    extraer_binario(&paquete, &destino).expect("extraer");

    assert_eq!(
        std::fs::read(&destino).expect("binario extraído"),
        b"soy el binario nuevo",
        "el contenido no es el de la entrada del binario"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let modo = std::fs::metadata(&destino)
            .expect("metadatos")
            .permissions()
            .mode();
        assert_eq!(
            modo & 0o777,
            0o755,
            "el binario tiene que quedar ejecutable"
        );
    }
}

#[test]
fn extraer_sin_el_binario_es_un_error_que_lo_nombra() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let paquete = dir.path().join("paquete.tar.gz");
    std::fs::write(
        &paquete,
        paquete_tar_gz(&[("install.sh", b"#!/bin/sh\necho hola\n")]),
    )
    .expect("paquete");
    let destino = dir.path().join("nuevo");

    let error = extraer_binario(&paquete, &destino).expect_err("falta el binario");

    let texto = error.to_string();
    assert!(
        texto.contains(NOMBRE_BINARIO),
        "el error no nombra el binario que falta: {texto}"
    );
    assert!(!destino.exists(), "no puede quedar un destino vacío");
}

#[test]
fn extraer_un_gzip_roto_es_un_error_claro() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let paquete = dir.path().join("paquete.tar.gz");
    std::fs::write(&paquete, b"esto no es un gzip").expect("paquete");
    let destino = dir.path().join("nuevo");

    let error = extraer_binario(&paquete, &destino).expect_err("gzip roto");

    let texto = error.to_string();
    assert!(
        texto.contains("paquete de actualización está roto"),
        "el error no explica que el paquete está roto: {texto}"
    );
    assert!(!destino.exists(), "no puede quedar un destino a medias");
}

/// Una entrada que apunta fuera del destino (ruta absoluta en el tar) se
/// escribe igual en `destino`: la ruta del archivo no la elige el paquete.
#[test]
fn extraer_no_usa_las_rutas_del_tar() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let paquete = dir.path().join("paquete.tar.gz");
    std::fs::write(
        &paquete,
        paquete_con_ruta_cruda(b"/etc/oruscribe-agente", b"binario"),
    )
    .expect("paquete");
    let destino = dir.path().join("nuevo");

    extraer_binario(&paquete, &destino).expect("extraer");

    assert_eq!(std::fs::read(&destino).expect("binario"), b"binario");
    assert!(
        !dir.path().join("etc").exists(),
        "el tar creó una carpeta a partir de su propia ruta"
    );
}

// ---------------------------------------------------------------------------
// Extremo a extremo
// ---------------------------------------------------------------------------

/// El flujo que hace `apply_update`: `check` con llaves desechables, descarga
/// con aviso de bytes, extracción del paquete (Linux), verificación del
/// binario extraído antes de pisar nada y swap.
#[tokio::test]
async fn el_flujo_completo_deja_el_binario_nuevo() {
    // Un script que contesta `--version` en Linux; en el resto de sistemas no
    // hay forma de fabricar un ejecutable que corra, así que el artefacto es
    // un archivo opaco y la verificación se salta.
    #[cfg(unix)]
    let binario: Vec<u8> =
        b"#!/bin/sh\n[ \"$1\" = \"--version\" ] && { echo 9.9.9; exit 0; }\nexit 1\n".to_vec();
    #[cfg(not(unix))]
    let binario: Vec<u8> = b"exe nuevo".to_vec();

    #[cfg(unix)]
    let descarga = paquete_tar_gz(&[(NOMBRE_BINARIO, &binario)]);
    #[cfg(not(unix))]
    let descarga = binario.clone();

    #[cfg(unix)]
    let ruta = "/paquete.tar.gz";
    #[cfg(not(unix))]
    let ruta = "/x.exe";

    let servidor = Servidor::nuevo(|base| {
        let cuerpo_manifiesto = manifiesto(&format!("{base}{ruta}"), &sha256(&descarga));
        let (_, firma) = firmar(7, &cuerpo_manifiesto);
        vec![
            ("/latest.json", cuerpo_manifiesto),
            ("/latest.json.sig", firma),
            (ruta, descarga.clone()),
        ]
    });
    let (publica, _) = firmar(7, b"cualquier cosa");
    let llaves = Keys::from_base64(&[publica.as_str()]).expect("llave desechable");
    let http = reqwest::Client::new();

    let info = check_with_keys(&http, &servidor.url("/latest.json"), "0.1.0", &llaves)
        .await
        .expect("check");
    assert!(info.available, "el manifiesto tiene que anunciar la nueva");

    let dir = tempfile::tempdir().expect("carpeta temporal");
    let paquete = dir.path().join("app.new.pkg");
    let mut avisos: Vec<(u64, Option<u64>)> = Vec::new();
    download_con(
        &http,
        &info.url,
        &info.sha256,
        &paquete,
        &mut |recibidos, total| avisos.push((recibidos, total)),
    )
    .await
    .expect("descarga");
    assert_eq!(
        avisos.last().copied(),
        Some((descarga.len() as u64, Some(descarga.len() as u64))),
        "la descarga tiene que avisar del tamaño completo"
    );

    let target = dir
        .path()
        .join(if cfg!(windows) { "app.exe" } else { "app" });
    std::fs::write(&target, b"viejo").expect("binario viejo");

    #[cfg(unix)]
    {
        let nuevo = dir.path().join("app.new");
        extraer_binario(&paquete, &nuevo).expect("extraer");
        let estado = std::process::Command::new(&nuevo)
            .arg("--version")
            .status()
            .expect("ejecutar el binario nuevo");
        assert!(
            estado.success(),
            "el binario extraído no responde --version"
        );
        swap(&nuevo, &target).expect("swap");
    }
    #[cfg(not(unix))]
    {
        swap(&paquete, &target).expect("swap");
    }

    assert_eq!(
        std::fs::read(&target).expect("destino"),
        binario,
        "el destino no quedó con los bytes nuevos"
    );
}

/// El binario que no contesta `--version` no llega al swap: en Linux se
/// comprueba sobre el `.new` extraído, antes de pisar el que corre, y el
/// original queda intacto.
#[test]
fn un_binario_que_no_responde_version_no_pisa_el_original() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let paquete = dir.path().join("paquete.tar.gz");
    std::fs::write(
        &paquete,
        paquete_tar_gz(&[(NOMBRE_BINARIO, b"esto no es un ejecutable")]),
    )
    .expect("paquete");
    let target = dir.path().join("app");
    std::fs::write(&target, b"original").expect("binario original");
    let nuevo = dir.path().join("app.new");

    extraer_binario(&paquete, &nuevo).expect("extraer");
    assert!(
        !responde_version(&nuevo),
        "un archivo de texto no puede contestar --version"
    );
    // El flujo real pregunta antes del swap y, si no contesta, no reemplaza.
    if responde_version(&nuevo) {
        swap(&nuevo, &target).expect("swap");
    }

    assert_eq!(
        std::fs::read(&target).expect("original"),
        b"original",
        "se pisó el original con un binario que no arranca"
    );
}

/// La otra cara: un binario que sí contesta `--version` pasa la comprobación.
#[cfg(unix)]
#[test]
fn un_binario_que_responde_version_pasa_la_comprobacion() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let guion = b"#!/bin/sh\necho 9.9.9\nexit 0\n";
    let paquete = dir.path().join("paquete.tar.gz");
    std::fs::write(&paquete, paquete_tar_gz(&[(NOMBRE_BINARIO, guion)])).expect("paquete");
    let nuevo = dir.path().join("app.new");

    extraer_binario(&paquete, &nuevo).expect("extraer");

    assert!(
        responde_version(&nuevo),
        "el binario extraído tiene que contestar --version"
    );
}

/// El target es un archivo que existe: después del swap el contenido es el
/// del binario nuevo y no una mezcla. Sirve de guarda contra un `apply` que
/// escribe encima sin reemplazar.
#[test]
fn el_swap_reemplaza_entero_el_destino() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let target = dir.path().join("app");
    let nuevo = dir.path().join("app.new");
    std::fs::write(&target, b"viejo y mas largo").expect("viejo");
    std::fs::write(&nuevo, b"nuevo").expect("nuevo");

    swap(&nuevo, &target).expect("swap");

    assert_eq!(std::fs::read(&target).expect("destino"), b"nuevo");
    assert!(!Path::new(&nuevo).exists(), "el .new quedó en disco");
}
