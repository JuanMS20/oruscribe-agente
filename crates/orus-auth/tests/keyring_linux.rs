#![cfg(target_os = "linux")]

use std::fs;
use std::os::unix::fs::PermissionsExt;

use chrono::DateTime;
use orus_auth::{SecretStore, Token, new_store};
use serial_test::serial;

#[test]
#[ignore = "requiere gnome-keyring desbloqueado; correr con dbus-run-session"]
#[serial]
fn linux_usa_secret_service() {
    // target y usuario únicos: si el llavero está bloqueado, `set` falla con
    // el motivo real (`NoStorageAccess`) en vez de caer al archivo.
    let clave = SecretStore::new("oruscribe-linux-ci", "prueba");
    clave
        .set("valor")
        .expect("el llavero debe estar disponible y desbloqueado");
    assert_eq!(
        clave.get().expect("leer del llavero").as_deref(),
        Some("valor")
    );
    clave.delete().expect("borrar del llavero");

    temp_env::with_var_unset("ORUSCRIBE_ALLOW_FILE_SECRETS", || {
        let dir = tempfile::tempdir().expect("carpeta temporal");
        let store = new_store(dir.path());
        let esperado = Token {
            access_token: "acceso-keyring".to_owned(),
            refresh_token: "refresco".to_owned(),
            token_type: "Bearer".to_owned(),
            scope: "openid".to_owned(),
            expiry: DateTime::parse_from_rfc3339("2026-11-12T10:00:00-05:00")
                .expect("fecha RFC3339"),
        };

        store
            .save(esperado.clone())
            .expect("guardar la sesión en el llavero");
        assert_eq!(store.load().expect("leer la sesión del llavero"), esperado);
        assert!(
            !dir.path().join("session.json").exists(),
            "no debe crear session.json"
        );
        store.clear().expect("borrar la sesión del llavero");
    });
}

/// El caso del reporte: un Linux sin llavero. La sesión tiene que caer al
/// archivo 0600 en vez de decir que no se puede guardar; si el entorno sí
/// tiene llavero, esta prueba no aplica y la anterior cubre ese camino.
#[test]
#[serial]
fn sin_llavero_la_sesion_cae_al_archivo_0600() {
    let sonda = SecretStore::new("oruscribe-linux-sonda", "sonda");
    if sonda.set("x").is_ok() {
        let _ = sonda.delete();
        eprintln!("se omite sin_llavero_la_sesion_cae_al_archivo_0600: el llavero está disponible");
        return;
    }
    temp_env::with_var_unset("ORUSCRIBE_ALLOW_FILE_SECRETS", || {
        let dir = tempfile::tempdir().expect("carpeta temporal");
        let store = new_store(dir.path());
        let token = Token {
            access_token: "acceso-archivo".to_owned(),
            refresh_token: "refresco".to_owned(),
            token_type: "Bearer".to_owned(),
            scope: "openid".to_owned(),
            expiry: DateTime::parse_from_rfc3339("2026-11-12T10:00:00-05:00")
                .expect("fecha RFC3339"),
        };

        store
            .save(token)
            .expect("sin llavero la sesión debe caer al archivo");

        let ruta = dir.path().join("session.json");
        let modo = fs::metadata(&ruta)
            .expect("session.json")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(modo, 0o600, "el archivo de sesión quedó con {modo:o}");
        assert_eq!(
            store.load().expect("leer la sesión").access_token,
            "acceso-archivo"
        );
        store.clear().expect("borrar la sesión");
        assert!(!ruta.exists(), "clear dejó el archivo de sesión");
    });
}
