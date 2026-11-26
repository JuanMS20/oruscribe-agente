//! Contrato de PKCE, registro y canje contra un servidor loopback propio.

use std::io::{BufRead, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread::{self, JoinHandle};

use chrono::{Datelike, Utc};
use orus_auth::{
    Config, Error, Metadata, authorize_url, challenge, exchange, new_state, new_verifier, register,
};

const REDIRECT: &str = "http://127.0.0.1:47823/oauth_callback";

fn leer_peticion(flujo: &mut TcpStream) -> String {
    let mut lector = std::io::BufReader::new(flujo.try_clone().unwrap());
    let mut cabeza = String::new();
    let mut largo = 0;
    loop {
        let mut linea = String::new();
        lector.read_line(&mut linea).unwrap();
        if linea == "\r\n" {
            break;
        }
        if let Some(valor) = linea.to_ascii_lowercase().strip_prefix("content-length:") {
            largo = valor.trim().parse().unwrap();
        }
        cabeza.push_str(&linea);
    }
    let mut cuerpo = vec![0; largo];
    lector.read_exact(&mut cuerpo).unwrap();
    format!("{cabeza}\r\n{}", String::from_utf8_lossy(&cuerpo))
}

fn servidor(status: u16, cuerpo: String) -> (String, JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let hilo = thread::spawn(move || {
        let (mut flujo, _) = listener.accept().unwrap();
        let peticion = leer_peticion(&mut flujo);
        let respuesta = format!(
            "HTTP/1.1 {status} OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{cuerpo}",
            cuerpo.len()
        );
        flujo.write_all(respuesta.as_bytes()).unwrap();
        peticion
    });
    (base, hilo)
}

fn partes(peticion: &str) -> (&str, &str) {
    peticion.split_once("\r\n\r\n").unwrap()
}

fn url_segura(valor: &str) -> bool {
    valor
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn config(base: &str) -> Config {
    Config {
        metadata: Metadata {
            authorization_endpoint: format!("{base}/authorize"),
            token_endpoint: format!("{base}/token"),
            ..Metadata::default()
        },
        client_id: "cliente".into(),
        redirect_uri: REDIRECT.into(),
        ..Config::default()
    }
}

#[test]
fn verifier_is_43_url_safe() {
    let primero = new_verifier().unwrap();
    let segundo = new_verifier().unwrap();
    assert_eq!(primero.len(), 43);
    assert!(url_segura(&primero), "{primero}");
    assert_ne!(primero, segundo);
}

#[test]
fn challenge_matches_rfc7636() {
    assert_eq!(
        challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    );
}

#[test]
fn state_is_16_bytes() {
    let estado = new_state().unwrap();
    assert_eq!(estado.len(), 22, "16 bytes en base64url");
    assert!(url_segura(&estado), "{estado}");
}

#[test]
fn authorize_url_matches_go_order() {
    let mut config = config("http://127.0.0.1:9");
    config.scopes = vec!["openid".into(), "email".into(), "offline_access".into()];
    assert_eq!(
        authorize_url(&config, "estado", "reto").unwrap(),
        "http://127.0.0.1:9/authorize?client_id=cliente&code_challenge=reto&code_challenge_method=S256&redirect_uri=http%3A%2F%2F127.0.0.1%3A47823%2Foauth_callback&response_type=code&scope=openid+email+offline_access&state=estado"
    );
    config.scopes.clear();
    assert!(
        !authorize_url(&config, "estado", "reto")
            .unwrap()
            .contains("scope=")
    );
}

#[tokio::test]
async fn register_returns_client_id() {
    let (base, hilo) = servidor(201, r#"{"client_id":"cliente-123"}"#.into());
    let metadata = Metadata {
        registration_endpoint: format!("{base}/register"),
        ..Metadata::default()
    };
    let id = register(&reqwest::Client::new(), &metadata, REDIRECT, "Oruscribe")
        .await
        .unwrap();
    assert_eq!(id, "cliente-123");
    let peticion = hilo.join().unwrap();
    let (cabeza, cuerpo) = partes(&peticion);
    assert!(cabeza.starts_with("POST /register HTTP/1.1"), "{cabeza}");
    let cabeceras = cabeza.to_ascii_lowercase();
    assert!(
        cabeceras.contains("content-type: application/json")
            && cabeceras.contains("accept: application/json"),
        "{cabeza}"
    );
    assert_eq!(
        cuerpo,
        r#"{"client_name":"Oruscribe","grant_types":["authorization_code","refresh_token"],"redirect_uris":["http://127.0.0.1:47823/oauth_callback"],"response_types":["code"],"token_endpoint_auth_method":"none"}"#
    );
}

#[tokio::test]
async fn register_rejects_missing_endpoint() {
    let error = register(
        &reqwest::Client::new(),
        &Metadata::default(),
        REDIRECT,
        "Oruscribe",
    )
    .await
    .unwrap_err();
    assert!(matches!(error, Error::RegisterEndpoint));
    assert_eq!(
        error.to_string(),
        "el servidor OAuth no permite registrar la app"
    );
}

#[tokio::test]
async fn exchange_rotates_token() {
    let (base, hilo) = servidor(
        200,
        r#"{"access_token":"nuevo","refresh_token":"rotado","token_type":"bearer","scope":"openid","expires_in":3600}"#.into(),
    );
    let token = exchange(
        &reqwest::Client::new(),
        &config(&base),
        "codigo",
        "verificador",
    )
    .await
    .unwrap();
    assert_eq!(token.access_token, "nuevo");
    assert_eq!(token.refresh_token, "rotado");
    assert_eq!(token.token_type, "bearer");
    assert_eq!(token.scope, "openid");
    assert!(token.expiry > Utc::now().fixed_offset());
    let peticion = hilo.join().unwrap();
    let (cabeza, cuerpo) = partes(&peticion);
    assert!(cabeza.starts_with("POST /token HTTP/1.1"), "{cabeza}");
    assert!(
        cabeza
            .to_ascii_lowercase()
            .contains("content-type: application/x-www-form-urlencoded"),
        "{cabeza}"
    );
    assert!(
        cabeza
            .to_ascii_lowercase()
            .contains("accept: application/json"),
        "{cabeza}"
    );
    assert_eq!(
        cuerpo,
        "client_id=cliente&code=codigo&code_verifier=verificador&grant_type=authorization_code&redirect_uri=http%3A%2F%2F127.0.0.1%3A47823%2Foauth_callback"
    );
}

/// Cuerpo del canje con el `expires_in` pedido (`None` lo omite).
fn cuerpo_canje(expires_in: Option<i64>) -> String {
    let clave = "access_token";
    match expires_in {
        Some(valor) => format!(r#"{{"{clave}":"nuevo","expires_in":{valor}}}"#),
        None => format!(r#"{{"{clave}":"nuevo"}}"#),
    }
}

/// #74: un `expires_in` enorme del servidor no puede tumbar el canje.
/// `chrono::Duration::seconds` hace panic por encima de `i64::MAX/1000`.
#[tokio::test]
async fn exchange_clamps_expires_in_enorme() {
    let (base, hilo) = servidor(200, cuerpo_canje(Some(i64::MAX)));
    let token = exchange(
        &reqwest::Client::new(),
        &config(&base),
        "codigo",
        "verificador",
    )
    .await
    .expect("el canje no debe hacer panic con un expires_in enorme");
    let ahora = Utc::now().fixed_offset();
    assert!(token.expiry > ahora, "la expiración quedó en el pasado");
    assert!(
        token.expiry <= ahora + chrono::Duration::days(366),
        "expires_in enorme sin acotar: {}",
        token.expiry
    );
    hilo.join().unwrap();
}

/// Sin `expires_in` positivo la expiración queda en el cero, no en el
/// pasado: renovar en bucle sería peor.
#[tokio::test]
async fn exchange_sin_expires_in_positivo_queda_en_cero() {
    for expires_in in [Some(0), Some(-5), None] {
        let cuerpo = cuerpo_canje(expires_in);
        let (base, hilo) = servidor(200, cuerpo.clone());
        let token = exchange(
            &reqwest::Client::new(),
            &config(&base),
            "codigo",
            "verificador",
        )
        .await
        .expect("canje sin expires_in positivo");
        assert_eq!(token.expiry.year(), 1, "fecha cero para {cuerpo}");
        assert!(!token.expired(Utc::now().fixed_offset()));
        hilo.join().unwrap();
    }
}

#[tokio::test]
async fn exchange_rejects_error_body() {
    let (base, hilo) = servidor(400, "invalid_grant".into());
    let error = exchange(
        &reqwest::Client::new(),
        &config(&base),
        "codigo",
        "verificador",
    )
    .await
    .unwrap_err();
    assert!(
        matches!(error, Error::Exchange(ref cuerpo) if cuerpo == "400 Bad Request invalid_grant"),
        "{error:?}"
    );
    assert_eq!(
        error.to_string(),
        "intercambio del código: 400 Bad Request invalid_grant"
    );
    hilo.join().unwrap();
}
