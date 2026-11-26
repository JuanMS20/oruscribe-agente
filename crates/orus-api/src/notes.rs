//! Bloc de notas local: `GET`, `POST` y `DELETE /api/notes`.
//!
//! El archivo vive en `orus_config::data_dir()/notes.json`, es un mapa por
//! dueño (`user_id@org_slug`) y nunca sale del equipo. La tienda es global y relee el archivo en
//! cada operación para no pisar cambios de otro proceso.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};

use axum::body::Body;
use axum::extract::{Query, State as AxumState};
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use orus_app::App;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{ErrorRuta, decode, write_err, write_json};

const SIN_IDENTIFICADOR: &str = "la nota no tiene identificador";
const FALTA_IDENTIFICADOR: &str = "falta el identificador";
/// El texto del error de sesión cerrada más el sufijo.
const SIN_SESION: &str = "no has iniciado sesión: inicia sesión para escribir en el bloc";

/// Una nota personal.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Nota {
    id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    body: String,
    /// `time.Time` del contrato acepta el campo ausente como cero; acá el cero es
    /// `MIN_UTC`, el mismo que usa `orus-tools` para el cero. Solo
    /// importa al leer un archivo viejo incompleto.
    #[serde(default = "fecha_cero")]
    updated_at: DateTime<Utc>,
}

fn fecha_cero() -> DateTime<Utc> {
    DateTime::<Utc>::MIN_UTC
}

/// El cuerpo del `POST`: la UI y el caso inmune mandan `{id,title,body}`, sin
/// `updated_at`. El valor del
/// cliente se ignora: el servidor fija `Utc::now`
/// `null` deja el campo vacío, igual que `encoding/json` con un `string`.
#[derive(Default, Deserialize)]
#[serde(default)]
struct EntradaNota {
    #[serde(deserialize_with = "texto_o_vacio")]
    id: String,
    #[serde(deserialize_with = "texto_o_vacio")]
    title: String,
    #[serde(deserialize_with = "texto_o_vacio")]
    body: String,
}

/// El contrato deserializa `null` en un `string` sin error y deja el cero.
fn texto_o_vacio<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
}

/// `GET`/`DELETE` usan `?id=`; el resto de los parámetros se ignora.
#[derive(Default, Deserialize)]
#[serde(default)]
pub(crate) struct Consulta {
    id: Option<String>,
}

/// La tienda en memoria, con el mismo alcance que el `notes` del contrato.
#[derive(Default)]
struct Tienda {
    notas: BTreeMap<String, Vec<Nota>>,
}

static TIENDA: LazyLock<Mutex<Tienda>> = LazyLock::new(|| Mutex::new(Tienda::default()));

/// El contrato no envenena el mutex: si otro hilo falló, se sigue usando el estado.
fn tienda() -> MutexGuard<'static, Tienda> {
    TIENDA
        .lock()
        .unwrap_or_else(|envenenado| envenenado.into_inner())
}

fn ruta_notas() -> PathBuf {
    orus_config::data_dir().join("notes.json")
}

fn ruta_candado() -> PathBuf {
    orus_config::data_dir().join("notes.json.lock")
}

static CONTADOR_TEMPORAL: AtomicU64 = AtomicU64::new(0);

fn con_candado<T>(operacion: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    crear_carpeta(&orus_config::data_dir())?;
    let archivo = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(ruta_candado())
        .map_err(|error| error.to_string())?;
    archivo.lock().map_err(|error| error.to_string())?;
    operacion()
}

/// El formato actual es un mapa
/// por dueño; una lista suelta es el formato viejo y se asigna al primer dueño
/// con sesión, para no perder lo ya escrito.
impl Tienda {
    fn cargar(&mut self, owner: &str) -> Result<(), String> {
        let datos = match std::fs::read(ruta_notas()) {
            Ok(datos) => datos,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.notas.clear();
                return Ok(());
            }
            Err(error) => return Err(error.to_string()),
        };
        let valor: serde_json::Value =
            serde_json::from_slice(&datos).map_err(|error| error.to_string())?;
        match valor {
            serde_json::Value::Array(_) => {
                let viejas: Vec<Nota> =
                    serde_json::from_value(valor).map_err(|error| error.to_string())?;
                self.notas.clear();
                if !owner.is_empty() && !viejas.is_empty() {
                    self.notas.insert(owner.to_owned(), viejas);
                    self.guardar()?;
                }
                Ok(())
            }
            serde_json::Value::Object(_) => {
                self.notas = serde_json::from_value(valor).map_err(|error| error.to_string())?;
                Ok(())
            }
            serde_json::Value::Null => {
                self.notas.clear();
                Ok(())
            }
            _ => Err("notes.json no tiene el formato esperado".to_owned()),
        }
    }

    /// `.tmp` + rename, carpeta 0700
    /// y archivo 0600. En Windows los permisos POSIX no aplican.
    fn guardar(&self) -> Result<(), String> {
        let carpeta = orus_config::data_dir();
        crear_carpeta(&carpeta)?;
        let datos = serde_json::to_vec_pretty(&self.notas).map_err(|error| error.to_string())?;
        let destino = ruta_notas();
        let mut temporal = destino.as_os_str().to_os_string();
        temporal.push(format!(
            ".tmp-{}-{}",
            std::process::id(),
            CONTADOR_TEMPORAL.fetch_add(1, Ordering::Relaxed)
        ));
        let temporal = PathBuf::from(temporal);
        escribir_privado(&temporal, &datos)?;
        std::fs::rename(&temporal, &destino).map_err(|error| error.to_string())
    }
}

fn crear_carpeta(carpeta: &Path) -> Result<(), String> {
    let mut constructor = std::fs::DirBuilder::new();
    constructor.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        constructor.mode(0o700);
    }
    constructor
        .create(carpeta)
        .map_err(|error| error.to_string())
}

fn escribir_privado(ruta: &Path, datos: &[u8]) -> Result<(), String> {
    use std::io::Write as _;

    let mut opciones = std::fs::OpenOptions::new();
    opciones.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opciones.mode(0o600);
    }
    let mut archivo = opciones.open(ruta).map_err(|error| error.to_string())?;
    archivo
        .write_all(datos)
        .and_then(|()| archivo.sync_all())
        .map_err(|error| error.to_string())
}

/// Copia y ordena por `updated_at`
/// descendente.
fn todas(owner: &str) -> Result<Vec<Nota>, String> {
    let mut tienda = tienda();
    con_candado(|| {
        tienda.cargar(owner)?;
        let mut lista = tienda.notas.get(owner).cloned().unwrap_or_default();
        lista.sort_by_key(|nota| std::cmp::Reverse(nota.updated_at));
        Ok(lista)
    })
}

/// Reemplaza por `id` o agrega, y
/// siempre fija la hora del servidor.
fn guardar_nota(owner: &str, entrada: EntradaNota) -> Result<Nota, String> {
    if owner.is_empty() {
        return Err("sin sesión no se puede escribir en el bloc".to_owned());
    }
    let mut tienda = tienda();
    con_candado(|| {
        tienda.cargar(owner)?;
        let nota = Nota {
            id: entrada.id,
            title: entrada.title,
            body: entrada.body,
            updated_at: Utc::now(),
        };
        let lista = tienda.notas.entry(owner.to_owned()).or_default();
        match lista.iter_mut().find(|actual| actual.id == nota.id) {
            Some(actual) => *actual = nota.clone(),
            None => lista.push(nota.clone()),
        }
        tienda.guardar()?;
        Ok(nota)
    })
}

/// Borra por `id` y guarda aunque no
/// hubiera nada que borrar.
fn borrar_nota(owner: &str, id: &str) -> Result<(), String> {
    if owner.is_empty() {
        return Err("sin sesión no se puede escribir en el bloc".to_owned());
    }
    let mut tienda = tienda();
    con_candado(|| {
        tienda.cargar(owner)?;
        if let Some(lista) = tienda.notas.get_mut(owner) {
            lista.retain(|nota| nota.id != id);
        }
        tienda.guardar()
    })
}

/// `App.notesOwner`: sin usuario no hay dueño; dos cuentas
/// en el mismo equipo no comparten el bloc.
fn dueno(app: &App) -> String {
    let usuario = app.me().map(|me| me.user_id).unwrap_or_default();
    if usuario.is_empty() {
        return String::new();
    }
    format!("{usuario}@{}", app.settings_snapshot().org_slug)
}

/// `/api/notes`, registrada sin método El `id` se
/// valida antes de mirar la sesión en `POST` y `DELETE`.
pub(crate) async fn notas(
    AxumState(app): AxumState<Arc<App>>,
    method: Method,
    Query(consulta): Query<Consulta>,
    body: Body,
) -> Response {
    match method {
        Method::GET => {
            let owner = dueno(&app);
            // El IO del bloc va a un hilo bloqueante: no frena el worker
            // async que atiende el resto de la API (#95).
            let resultado = tokio::task::spawn_blocking(move || todas(&owner))
                .await
                .map_err(|error| error.to_string())
                .and_then(|resultado| resultado);
            match resultado {
                Ok(lista) => write_json(StatusCode::OK, &json!({ "notes": lista })),
                Err(error) => write_err(ErrorRuta::Lectura(error)),
            }
        }
        Method::POST => {
            let entrada: EntradaNota = match decode(body).await {
                Ok(entrada) => entrada,
                Err(error) => return write_err(error),
            };
            if entrada.id.is_empty() {
                return write_json(
                    StatusCode::BAD_REQUEST,
                    &json!({ "error": SIN_IDENTIFICADOR }),
                );
            }
            let owner = dueno(&app);
            if owner.is_empty() {
                return write_json(StatusCode::UNAUTHORIZED, &json!({ "error": SIN_SESION }));
            }
            let resultado = tokio::task::spawn_blocking(move || guardar_nota(&owner, entrada))
                .await
                .map_err(|error| error.to_string())
                .and_then(|resultado| resultado);
            match resultado {
                Ok(nota) => write_json(StatusCode::OK, &nota),
                Err(error) => write_err(ErrorRuta::Lectura(error)),
            }
        }
        Method::DELETE => {
            let id = consulta.id.unwrap_or_default();
            if id.is_empty() {
                return write_json(
                    StatusCode::BAD_REQUEST,
                    &json!({ "error": FALTA_IDENTIFICADOR }),
                );
            }
            let owner = dueno(&app);
            if owner.is_empty() {
                return write_json(StatusCode::UNAUTHORIZED, &json!({ "error": SIN_SESION }));
            }
            let resultado = tokio::task::spawn_blocking(move || borrar_nota(&owner, &id))
                .await
                .map_err(|error| error.to_string())
                .and_then(|resultado| resultado);
            match resultado {
                Ok(()) => write_json(StatusCode::OK, &json!({ "ok": true })),
                Err(error) => write_err(ErrorRuta::Lectura(error)),
            }
        }
        // El contrato contesta 405 sin cuerpo.
        _ => StatusCode::METHOD_NOT_ALLOWED.into_response(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use async_trait::async_trait;
    use orus_app::{App, Backend, Connector, Refresh, SessionError};
    use orus_auth::{Error as AuthError, Store, Token};
    use orus_config::Settings;

    use super::*;

    struct StoreVacio;

    impl Store for StoreVacio {
        fn save(&self, _: Token) -> Result<(), AuthError> {
            Ok(())
        }

        fn load(&self) -> Result<Token, AuthError> {
            Err(AuthError::NoSession)
        }

        fn clear(&self) -> Result<(), AuthError> {
            Ok(())
        }
    }

    struct SinRed;

    #[async_trait]
    impl Connector for SinRed {
        async fn open(&self, _: &str, _: &str, _: &str) -> Result<Arc<dyn Backend>, SessionError> {
            Err(SessionError::Connect("sin red".to_owned()))
        }
    }

    #[async_trait]
    impl Refresh for SinRed {
        async fn refresh(&self, _: &Token) -> Result<Token, SessionError> {
            Err(SessionError::RefreshRejected)
        }
    }

    fn app_de_notas() -> Arc<App> {
        Arc::new(App::new(
            Box::new(StoreVacio),
            Settings::default(),
            Arc::new(SinRed),
            Arc::new(SinRed),
        ))
    }

    /// #95: el IO del bloc no puede correr en el worker async. En un runtime
    /// de un solo hilo, un testigo que solo marca «libre» cuando el handler
    /// ya empezó distingue el IO síncrono (el handler no tiene puntos de
    /// espera y el testigo nunca corre) del `spawn_blocking` (el worker queda
    /// libre mientras el hilo bloqueante lee).
    #[tokio::test]
    async fn get_notas_deja_libre_el_worker_async() {
        const MARCA: &str = "ORUSCRIBE_NOTAS_TEST_HIJO";
        if std::env::var_os(MARCA).is_none() {
            let base = tempfile::tempdir().expect("carpeta temporal");
            let exe = std::env::current_exe().expect("binario de tests");
            let salida = std::process::Command::new(exe)
                .args([
                    "--exact",
                    "notes::tests::get_notas_deja_libre_el_worker_async",
                    "--test-threads=1",
                ])
                .env(MARCA, "1")
                .env("ORUSCRIBE_DATA_DIR", base.path().join("datos"))
                .output()
                .expect("correr el test aislado");
            assert!(
                salida.status.success(),
                "el test aislado falló en su proceso: {}",
                String::from_utf8_lossy(&salida.stderr)
            );
            let stdout = String::from_utf8_lossy(&salida.stdout);
            assert!(
                stdout.contains("1 passed"),
                "el hijo no ejecutó el test: {stdout}"
            );
            return;
        }

        let app = app_de_notas();
        let empezado = Arc::new(AtomicBool::new(false));
        let libre = Arc::new(AtomicBool::new(false));
        let fin = Arc::new(AtomicBool::new(false));
        let handler = tokio::spawn({
            let app = Arc::clone(&app);
            let empezado = Arc::clone(&empezado);
            let fin = Arc::clone(&fin);
            async move {
                empezado.store(true, Ordering::SeqCst);
                let respuesta = notas(
                    AxumState(app),
                    Method::GET,
                    Query(Consulta::default()),
                    Body::empty(),
                )
                .await;
                fin.store(true, Ordering::SeqCst);
                respuesta
            }
        });
        let testigo = tokio::spawn({
            let empezado = Arc::clone(&empezado);
            let libre = Arc::clone(&libre);
            let fin = Arc::clone(&fin);
            async move {
                while !fin.load(Ordering::SeqCst) {
                    if empezado.load(Ordering::SeqCst) {
                        libre.store(true, Ordering::SeqCst);
                    }
                    tokio::task::yield_now().await;
                }
            }
        });
        let respuesta = handler.await.expect("el handler termina");
        testigo.await.expect("el testigo termina");
        assert_eq!(respuesta.status(), StatusCode::OK);
        assert!(
            libre.load(Ordering::SeqCst),
            "el IO de notas bloqueó el worker async"
        );
    }
}
