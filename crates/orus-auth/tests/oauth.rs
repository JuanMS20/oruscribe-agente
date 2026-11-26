//! Contrato de `oauth.rs` contra un servidor loopback propio.

use std::io::{BufRead, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;

use chrono::{Datelike, Utc};
use orus_auth::{Config, Error, Metadata, discover, refresh};

fn leer_peticion(flujo: &mut TcpStream) -> String {
    let mut lector = std::io::BufReader::new(flujo.try_clone().expect("clona el socket"));
    let mut cabeza = String::new();
    loop {
        let mut linea = String::new();
        lector.read_line(&mut linea).expect("lee la cabeza");
        if linea == "\r\n" {
            break;
        }
        cabeza.push_str(&linea);
    }
    let largo = cabeza
        .lines()
        .find_map(|linea| {
            let valor = linea.to_ascii_lowercase();
            let valor = valor.strip_prefix("content-length:")?;
            valor.trim().parse::<usize>().ok()
        })
        .unwrap_or(0);
    let mut cuerpo = vec![0; largo];
    lector.read_exact(&mut cuerpo).expect("lee el cuerpo");
    format!("{cabeza}\r\n{}", String::from_utf8_lossy(&cuerpo))
}

fn servidor(status: u16, cuerpo: String) -> (String, thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("puerto libre");
    let direccion = listener.local_addr().expect("dirección local");
    let servidor = thread::spawn(move || {
        let (mut flujo, _) = listener.accept().expect("conexión del cliente");
        let peticion = leer_peticion(&mut flujo);
        let respuesta = format!(
            "HTTP/1.1 {status} OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{cuerpo}",
            cuerpo.len()
        );
        flujo
            .write_all(respuesta.as_bytes())
            .expect("escribe la respuesta");
        peticion
    });
    (format!("http://{direccion}"), servidor)
}

fn config(base: &str) -> Config {
    Config {
        metadata: Metadata {
            token_endpoint: format!("{base}/token"),
            ..Metadata::default()
        },
        client_id: "cliente".to_owned(),
        ..Config::default()
    }
}

#[tokio::test]
async fn discover_reads_metadata() {
    let (base, servidor) = servidor(
        200,
        r#"{"authorization_endpoint":"http://x/a","token_endpoint":"http://x/t"}"#.to_owned(),
    );
    let metadata = discover(&reqwest::Client::new(), &format!("{base}/"))
        .await
        .expect("lee la metadata");
    assert_eq!(metadata.token_endpoint, "http://x/t");
    let peticion = servidor.join().expect("el servidor termina");
    let peticion_minusculas = peticion.to_ascii_lowercase();
    assert!(
        peticion.starts_with("GET /.well-known/oauth-authorization-server HTTP/1.1")
            && peticion_minusculas.contains("accept: application/json"),
        "{peticion}"
    );
}

#[tokio::test]
async fn discover_rejects_missing_endpoints() {
    let (base, servidor) = servidor(200, r#"{"issuer":"http://x"}"#.to_owned());
    let error = discover(&reqwest::Client::new(), &base)
        .await
        .expect_err("sin endpoints no hay metadata");
    assert_eq!(
        error.to_string(),
        "el servidor OAuth no publica los endpoints necesarios"
    );
    servidor.join().expect("el servidor termina");
}

#[tokio::test]
async fn refresh_rotates_token() {
    let (base, servidor) = servidor(
        200,
        r#"{"access_token":"nuevo","refresh_token":"rotado","token_type":"bearer","scope":"openid","expires_in":3600}"#.to_owned(),
    );
    let token = refresh(&reqwest::Client::new(), config(&base), "viejo")
        .await
        .expect("renueva");
    assert_eq!(token.access_token, "nuevo");
    assert_eq!(token.refresh_token, "rotado");
    assert_eq!(token.token_type, "bearer");
    assert_eq!(token.scope, "openid");
    assert!(token.expiry > Utc::now().fixed_offset());
    let peticion = servidor.join().expect("el servidor termina");
    assert!(peticion.starts_with("POST /token HTTP/1.1"), "{peticion}");
    let peticion = peticion.to_ascii_lowercase();
    assert!(peticion.contains("content-type: application/x-www-form-urlencoded"));
    assert!(peticion.contains("grant_type=refresh_token"), "{peticion}");
    assert!(peticion.contains("refresh_token=viejo"), "{peticion}");
    assert!(peticion.contains("client_id=cliente"), "{peticion}");
}

#[tokio::test]
async fn refresh_keeps_old_refresh_token() {
    let (base, servidor) = servidor(200, r#"{"access_token":"nuevo"}"#.to_owned());
    let token = refresh(&reqwest::Client::new(), config(&base), "viejo")
        .await
        .expect("renueva");
    assert_eq!(token.refresh_token, "viejo");
    servidor.join().expect("el servidor termina");
}

#[tokio::test]
async fn refresh_rejects_with_sentinel() {
    let (base, servidor) = servidor(401, "token vencido".to_owned());
    let error = refresh(&reqwest::Client::new(), config(&base), "viejo")
        .await
        .expect_err("el 401 es un rechazo");
    assert!(matches!(error, Error::RefreshRejected(ref cuerpo) if cuerpo == "token vencido"));
    assert_eq!(
        error.to_string(),
        "el servidor rechazó renovar la sesión: token vencido"
    );
    servidor.join().expect("el servidor termina");
}

#[tokio::test]
async fn refresh_without_expires_in_keeps_zero_expiry() {
    let (base, servidor) = servidor(200, r#"{"access_token":"nuevo"}"#.to_owned());
    let token = refresh(&reqwest::Client::new(), config(&base), "viejo")
        .await
        .expect("renueva");
    assert_eq!(token.expiry.year(), 1, "fecha cero, no el pasado");
    assert!(!token.expired(Utc::now().fixed_offset()));
    servidor.join().expect("el servidor termina");
}

/// Cuerpo de la renovación con el `expires_in` pedido.
fn cuerpo_renovacion(expires_in: i64) -> String {
    let clave = "access_token";
    format!(r#"{{"{clave}":"nuevo","expires_in":{expires_in}}}"#)
}

/// Mismo defecto que #74 en el canje: la renovación corre en cada token
/// vencido, así que una respuesta rota tumba el proceso a mitad de un turno.
#[tokio::test]
async fn refresh_clamps_expires_in_enorme() {
    let (base, servidor) = servidor(200, cuerpo_renovacion(i64::MAX));
    let token = refresh(&reqwest::Client::new(), config(&base), "viejo")
        .await
        .expect("la renovación no debe hacer panic con un expires_in enorme");
    let ahora = Utc::now().fixed_offset();
    assert!(token.expiry > ahora, "la expiración quedó en el pasado");
    assert!(
        token.expiry <= ahora + chrono::Duration::days(366),
        "expires_in enorme sin acotar: {}",
        token.expiry
    );
    servidor.join().expect("el servidor termina");
}

#[tokio::test]
async fn refresh_expires_in_negativo_queda_en_cero() {
    let (base, servidor) = servidor(200, cuerpo_renovacion(-5));
    let token = refresh(&reqwest::Client::new(), config(&base), "viejo")
        .await
        .expect("renueva");
    assert_eq!(token.expiry.year(), 1, "fecha cero, no el pasado");
    assert!(!token.expired(Utc::now().fixed_offset()));
    servidor.join().expect("el servidor termina");
}

#[tokio::test]
async fn refresh_empty_token_errors() {
    let error = refresh(&reqwest::Client::new(), config("http://127.0.0.1:9"), "")
        .await
        .expect_err("sin token no se llama al servidor");
    assert!(matches!(error, Error::NoRefreshToken));
    assert_eq!(error.to_string(), "no hay token de refresco");
}
