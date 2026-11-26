#![cfg(target_os = "linux")]

use orus_auth::{Error, SecretStore};
use serial_test::serial;

#[test]
#[serial]
fn secret_store_falls_back_to_file() {
    temp_env::with_var(
        "DBUS_SESSION_BUS_ADDRESS",
        Some("unix:path=/nonexistent"),
        || {
            let dir = tempfile::tempdir().expect("carpeta temporal");
            let store =
                SecretStore::with_file_fallback("oruscribe-agent-test", "clave", dir.path());

            store.set("secreto").expect("guardar en archivo");
            assert_eq!(
                store.get().expect("leer del archivo").as_deref(),
                Some("secreto")
            );

            let archivo = dir.path().join("secret-clave.json");
            assert!(archivo.exists(), "debe existir el respaldo");
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                let modo = std::fs::metadata(&archivo).expect("metadata").mode() & 0o777;
                assert_eq!(modo, 0o600);
            }

            store.delete().expect("borrar la clave");
            assert!(!archivo.exists());
            assert!(
                matches!(store.get(), Err(Error::Unavailable(_))),
                "sin llavero y sin archivo, el error debe ser real"
            );
        },
    );
}

#[test]
#[serial]
fn secret_store_usa_el_archivo_principal_cuando_se_permite() {
    temp_env::with_var("ORUSCRIBE_ALLOW_FILE_SECRETS", Some("1"), || {
        temp_env::with_var(
            "DBUS_SESSION_BUS_ADDRESS",
            Some("unix:path=/nonexistent"),
            || {
                let raiz = tempfile::tempdir().expect("carpeta temporal");
                let dir = raiz.path().join("nueva").join("datos");
                let store =
                    SecretStore::with_file_fallback("oruscribe-agent-test", "clave-archivo", &dir);

                assert!(
                    matches!(store.get(), Ok(None)),
                    "sin archivo todavía no hay clave, y eso no es un fallo"
                );
                store
                    .set("secreto")
                    .expect("guardar en archivo aunque la carpeta no exista");
                assert_eq!(
                    store.get().expect("leer del archivo").as_deref(),
                    Some("secreto")
                );

                let archivo = dir.join("secret-clave-archivo.json");
                assert!(archivo.exists(), "debe existir el archivo principal");

                store.delete().expect("borrar la clave");
                assert!(!archivo.exists());
                assert!(matches!(store.get(), Ok(None)));
            },
        );
    });
}

#[test]
#[serial]
fn secret_store_sin_llavero_borra_respaldo_y_devuelve_ok() {
    temp_env::with_var(
        "DBUS_SESSION_BUS_ADDRESS",
        Some("unix:path=/nonexistent"),
        || {
            let dir = tempfile::tempdir().expect("carpeta temporal");
            let store = SecretStore::with_file_fallback(
                "oruscribe-agent-test",
                "clave-borrado",
                dir.path(),
            );
            let archivo = dir.path().join("secret-clave-borrado.json");

            store.set("temporal").expect("guardar en el respaldo");
            assert!(archivo.exists());

            store
                .delete()
                .expect("sin llavero, borrar el respaldo no es error");
            assert!(!archivo.exists());
        },
    );
}
