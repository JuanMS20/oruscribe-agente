use std::path::Path;

use chrono::{DateTime, Duration};
use orus_auth::{Error, FileStore, Store, Token, file_secrets_allowed, new_store};
use serial_test::serial;

fn temporales(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .expect("leer la carpeta")
        .map(|entrada| {
            entrada
                .expect("entrada de la carpeta")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|nombre| nombre.ends_with(".tmp"))
        .collect()
}

fn token() -> Token {
    Token {
        access_token: "acceso".to_owned(),
        refresh_token: "refresco".to_owned(),
        token_type: "Bearer".to_owned(),
        scope: "openid".to_owned(),
        expiry: DateTime::parse_from_rfc3339("2026-11-12T10:00:00-05:00").expect("fecha RFC3339"),
    }
}

#[test]
fn token_expires_with_one_minute_slack() {
    let now = DateTime::parse_from_rfc3339("2026-11-12T10:00:00Z").expect("ahora RFC3339");

    let mut candidato = token();
    candidato.expiry = now + Duration::seconds(30);
    assert!(candidato.expired(now));

    candidato.expiry = now + Duration::seconds(90);
    assert!(!candidato.expired(now));

    candidato.expiry = DateTime::parse_from_rfc3339("0001-01-01T00:00:00Z").expect("fecha cero");
    assert!(!candidato.expired(now));
}

#[test]
fn file_store_roundtrip() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let store = FileStore::from_data_dir(dir.path());
    let esperado = token();

    store.save(esperado.clone()).expect("guardar la sesión");
    assert_eq!(store.load().expect("leer la sesión"), esperado);

    store.clear().expect("borrar la sesión");
    assert!(matches!(store.load(), Err(Error::NoSession)));
    store.clear().expect("borrar sin sesión no falla");
}

#[test]
fn save_twice_loads_last() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let store = FileStore::from_data_dir(dir.path());
    let primero = token();
    let mut segundo = token();
    segundo.access_token = "segundo".to_owned();

    store.save(primero).expect("guardar el primero");
    store.save(segundo.clone()).expect("guardar el segundo");

    assert_eq!(store.load().expect("leer la sesión"), segundo);
}

#[test]
fn save_leaves_no_temp() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let store = FileStore::from_data_dir(dir.path());

    store.save(token()).expect("guardar la sesión");

    let restantes = temporales(dir.path());
    assert!(restantes.is_empty(), "quedaron temporales: {restantes:?}");
}

#[test]
fn clear_removes_legacy_and_unique_temps() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let store = FileStore::from_data_dir(dir.path());
    std::fs::write(dir.path().join("session.json.tmp"), b"token viejo").expect("temporal fijo");
    std::fs::write(dir.path().join("session.json.x.tmp"), b"token viejo").expect("temporal único");
    let nota = dir.path().join("nota.txt");
    std::fs::write(&nota, b"no tocar").expect("archivo ajeno");

    store.clear().expect("borrar sin sesión no falla");

    assert!(!dir.path().join("session.json.tmp").exists());
    assert!(!dir.path().join("session.json.x.tmp").exists());
    assert!(nota.exists(), "clear borró un archivo ajeno");
}

#[test]
fn clear_borra_todos_los_temporales_bajo_el_candado() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let store = FileStore::from_data_dir(dir.path());
    let pid_actual = std::process::id();
    let ajeno = dir
        .path()
        .join(format!("session.json.{}-0-1.tmp", pid_actual + 1));
    let propio = dir
        .path()
        .join(format!("session.json.{pid_actual}-0-2.tmp"));
    let legado = dir.path().join("session.json.tmp");
    std::fs::write(&ajeno, b"token sintetico ajeno").expect("temporal ajeno");
    std::fs::write(&propio, b"token sintetico propio").expect("temporal propio");
    std::fs::write(&legado, b"token sintetico viejo").expect("temporal fijo");

    store.clear().expect("borrar la sesión");

    assert!(
        !ajeno.exists(),
        "con candado entre procesos no queda escritura viva: clear debe borrar el temporal ajeno"
    );
    assert!(!propio.exists(), "clear no borró el temporal propio");
    assert!(!legado.exists(), "clear no borró el temporal fijo");
}

/// #133: otro proceso tenía un temporal ya escrito cuando corrió `clear`;
/// su `rename` posterior volvía a crear `session.json` con el token en claro.
#[test]
fn clear_no_resucita_sesion_por_un_rename_en_vuelo() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let store = FileStore::from_data_dir(dir.path());
    let destino = dir.path().join("session.json");
    let destino_hilo = destino.clone();
    let temporal = dir
        .path()
        .join(format!("session.json.{}-0-1.tmp", std::process::id() + 1));
    store.save(token()).expect("guardar la sesión");
    std::fs::write(&temporal, serde_json::to_vec(&token()).expect("serializar"))
        .expect("temporal del otro proceso");
    let (señal, espera) = std::sync::mpsc::channel();

    let escritor = std::thread::spawn(move || {
        let _ = espera.recv();
        let _ = std::fs::rename(&temporal, &destino_hilo);
    });
    store.clear().expect("borrar la sesión");
    let _ = señal.send(());
    escritor.join().expect("el escritor no debe fallar");

    assert!(
        !destino.exists(),
        "el logout se deshizo: el rename en vuelo recreó session.json"
    );
}

/// #133: un reloj atrasado deja una fecha de modificación futura. Ese
/// temporal ajeno seguía en disco con el token sin cifrar tras cada logout.
#[test]
fn clear_borra_temporal_ajeno_con_fecha_futura() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let store = FileStore::from_data_dir(dir.path());
    let ajeno = dir
        .path()
        .join(format!("session.json.{}-0-1.tmp", std::process::id() + 1));
    std::fs::write(&ajeno, b"token sintetico con fecha futura").expect("temporal ajeno");
    let futuro = std::time::SystemTime::now() + std::time::Duration::from_secs(3600);
    std::fs::File::options()
        .write(true)
        .open(&ajeno)
        .expect("abrir el temporal")
        .set_modified(futuro)
        .expect("poner fecha futura");

    store.clear().expect("borrar la sesión");

    assert!(
        !ajeno.exists(),
        "clear dejó el token sin cifrar de un temporal con mtime futuro"
    );
}

/// #75: el temporal de un proceso que murió a mitad de la escritura trae el
/// token en claro y no puede sobrevivir al logout para siempre. Un temporal
/// ajeno viejo (fecha de modificación antigua) es un resto abandonado: una
/// escritura atómica viva dura milisegundos.
#[test]
fn clear_borra_temporal_ajeno_abandonado() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let store = FileStore::from_data_dir(dir.path());
    let pid_actual = std::process::id();
    let ajeno = dir
        .path()
        .join(format!("session.json.{}-0-1.tmp", pid_actual + 1));
    std::fs::write(&ajeno, b"token de un proceso muerto").expect("temporal ajeno");
    // Una hora atrás: el proceso ya no puede estar escribiendo.
    let viejo = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    std::fs::File::options()
        .write(true)
        .open(&ajeno)
        .expect("abrir el temporal")
        .set_modified(viejo)
        .expect("envejecer el temporal");

    store.clear().expect("borrar la sesión");

    assert!(
        !ajeno.exists(),
        "clear dejó el token en claro del proceso muerto"
    );
}

#[test]
fn two_stores_saving_at_once_leave_a_valid_session() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let store_a = FileStore::from_data_dir(dir.path());
    let store_b = FileStore::from_data_dir(dir.path());
    let mut primero = token();
    primero.access_token = "primero".to_owned();
    let mut segundo = token();
    segundo.access_token = "segundo".to_owned();

    std::thread::scope(|scope| {
        let a = scope.spawn(|| store_a.save(primero));
        let b = scope.spawn(|| store_b.save(segundo));
        a.join()
            .expect("el hilo A no debe fallar")
            .expect("guardar A");
        b.join()
            .expect("el hilo B no debe fallar")
            .expect("guardar B");
    });

    let cargado = store_a.load().expect("leer la sesión");
    assert!(
        cargado.access_token == "primero" || cargado.access_token == "segundo",
        "se guardó un token a medias: {:?}",
        cargado.access_token
    );
}

#[test]
fn file_store_removes_tmp_on_failed_rename() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    std::fs::create_dir(dir.path().join("session.json")).expect("carpeta en el destino");
    let store = FileStore::from_data_dir(dir.path());

    store.save(token()).expect_err("el rename debe fallar");
    assert!(!dir.path().join("session.json.tmp").exists());
    let restantes = temporales(dir.path());
    assert!(
        !restantes
            .iter()
            .any(|nombre| nombre.starts_with("session.json.")),
        "quedaron temporales: {restantes:?}"
    );
}

#[test]
#[serial]
fn new_store_respects_file_secrets() {
    let dir = tempfile::tempdir().expect("carpeta temporal");

    temp_env::with_var("ORUSCRIBE_ALLOW_FILE_SECRETS", Some("1"), || {
        assert!(file_secrets_allowed());
        let store = new_store(dir.path());
        store.save(token()).expect("guardar en archivo");
        assert!(dir.path().join("session.json").exists());
    });
    temp_env::with_var("ORUSCRIBE_ALLOW_FILE_SECRETS", Some("0"), || {
        assert!(!file_secrets_allowed());
    });
    temp_env::with_var_unset("ORUSCRIBE_ALLOW_FILE_SECRETS", || {
        assert!(!file_secrets_allowed());
    });
}

#[cfg(target_os = "linux")]
#[test]
#[serial]
fn linux_store_falls_back_to_file() {
    temp_env::with_var(
        "DBUS_SESSION_BUS_ADDRESS",
        Some("unix:path=/nonexistent"),
        || {
            temp_env::with_var_unset("ORUSCRIBE_ALLOW_FILE_SECRETS", || {
                let dir = tempfile::tempdir().expect("carpeta temporal");
                let store = new_store(dir.path());
                let esperado = token();

                store.save(esperado.clone()).expect("guardar en archivo");
                assert_eq!(store.load().expect("leer del archivo"), esperado);

                let archivo = dir.path().join("session.json");
                assert!(archivo.exists(), "debe existir el respaldo");
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    let modo = std::fs::metadata(&archivo).expect("metadata").mode() & 0o777;
                    assert_eq!(modo, 0o600);
                }

                store.clear().expect("borrar la sesión");
                assert!(!archivo.exists());
                assert!(
                    matches!(store.load(), Err(Error::Unavailable(_))),
                    "sin llavero y sin archivo, el error debe ser real"
                );
            });
        },
    );
}

#[cfg(target_os = "linux")]
#[test]
#[serial]
fn linux_store_sin_llavero_borra_respaldo_y_devuelve_ok() {
    temp_env::with_var(
        "DBUS_SESSION_BUS_ADDRESS",
        Some("unix:path=/nonexistent"),
        || {
            temp_env::with_var_unset("ORUSCRIBE_ALLOW_FILE_SECRETS", || {
                let dir = tempfile::tempdir().expect("carpeta temporal");
                let store = new_store(dir.path());
                let archivo = dir.path().join("session.json");

                store.save(token()).expect("guardar en el respaldo");
                assert!(archivo.exists());

                store
                    .clear()
                    .expect("sin llavero, borrar el respaldo no es error");
                assert!(!archivo.exists());
            });
        },
    );
}
