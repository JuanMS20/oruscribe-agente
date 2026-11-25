#![cfg(target_os = "linux")]

//! El bus D-Bus existe pero está caído: `clear` debe propagar el error.
//!
//! Archivo aparte a propósito: keyring 4 inicializa el store una sola vez
//! por proceso (LazyLock). Si esta prueba compartiera binario con las del
//! llavero real, la inicialización fallida quedaría cacheada para las demás.

use std::os::unix::net::UnixListener;

use chrono::DateTime;
use orus_auth::{Error, Token, new_store};
use serial_test::serial;

fn token_de_prueba() -> Token {
    Token {
        access_token: "acceso".to_owned(),
        refresh_token: "refresco".to_owned(),
        token_type: "Bearer".to_owned(),
        scope: "openid".to_owned(),
        expiry: DateTime::parse_from_rfc3339("2026-11-12T10:00:00-05:00").expect("fecha RFC3339"),
    }
}

/// El socket del bus existe pero nadie atiende (daemon caído): no se pudo
/// hablar con el llavero. `clear` no puede responder Ok porque el token
/// puede seguir ahí y reaparecer cuando el bus vuelva.
#[test]
#[serial]
fn clear_con_bus_caido_propaga_error() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let socket = dir.path().join("bus-falso");
    // Un socket ligado y sin listener: connect da «connection refused».
    drop(UnixListener::bind(&socket).expect("ligar el socket falso"));
    let address = format!("unix:path={}", socket.display());
    temp_env::with_var("DBUS_SESSION_BUS_ADDRESS", Some(address.as_str()), || {
        temp_env::with_var_unset("ORUSCRIBE_ALLOW_FILE_SECRETS", || {
            let store = new_store(dir.path());
            store
                .save(token_de_prueba())
                .expect("sin bus la sesión cae al archivo");
            let error = store
                .clear()
                .expect_err("el bus caído no puede dar Ok: el token puede seguir en el llavero");
            assert!(matches!(error, Error::Unavailable(_)), "{error:?}");
        });
    });
}

#[test]
#[serial]
fn respaldo_escrito_con_bus_caido_se_lee_al_arrancar() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let socket = dir.path().join("bus-rechazado");
    drop(UnixListener::bind(&socket).expect("ligar el socket falso"));
    let address = format!("unix:path={}", socket.display());
    temp_env::with_var("DBUS_SESSION_BUS_ADDRESS", Some(address.as_str()), || {
        temp_env::with_var_unset("ORUSCRIBE_ALLOW_FILE_SECRETS", || {
            let store = new_store(dir.path());
            let esperado = token_de_prueba();
            store
                .save(esperado.clone())
                .expect("guardar con el bus caído");
            let cargado = store
                .load()
                .expect("la sesión que el propio guardado puso en el respaldo debe leerse");
            assert_eq!(cargado.access_token, esperado.access_token);
            assert_eq!(cargado.refresh_token, esperado.refresh_token);
        });
    });
}
