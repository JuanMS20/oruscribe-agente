//! Escritura de tareas, mensajes, metas, captura, avisos y contexto.
//!
//! Los
//! adaptadores decodifican, validan lo mismo que el contrato antes de pedir el cliente y
//! delegan en `Backend`; el orden y los textos son los del contrato.

use std::sync::Arc;

use axum::Extension;
use axum::body::Body;
use axum::extract::State as AxumState;
use axum::http::StatusCode;
use axum::response::Response;
use chrono::{DateTime, Duration, Local, NaiveDate, TimeZone, Utc};
use orus_app::{App, DigestSource, GoalInput, PeriodInput, SessionError, TaskInput, parse_status};
use serde::Deserialize;
use serde_json::json;

use crate::{ErrorRuta, decode, write_json};

/// La lectura de `git log` de una carpeta local.
///
/// `orus-api` no depende de `orus-org` (D161/D180): la implementación real
/// delega en `orus_app::git_log_con_plazo` y los tests inyectan un fake.
pub trait GitLog: Send + Sync {
    fn log(&self, dir: &str, since: Option<DateTime<Utc>>) -> Result<DigestSource, SessionError>;

    /// Igual que `log`, con el plazo que la petición le da al git para que
    /// mate su propio proceso. La implementación por defecto sirve a las
    /// costuras de prueba, que no manejan procesos.
    fn log_con_plazo(
        &self,
        dir: &str,
        since: Option<DateTime<Utc>>,
        _plazo: std::time::Duration,
    ) -> Result<DigestSource, SessionError> {
        self.log(dir, since)
    }
}

/// La implementación real que cablea `Api::new`/`Api::with_hub`.
pub(crate) struct GitLogReal;

impl GitLog for GitLogReal {
    fn log(&self, dir: &str, since: Option<DateTime<Utc>>) -> Result<DigestSource, SessionError> {
        self.log_con_plazo(dir, since, PLAZO_GIT)
    }

    fn log_con_plazo(
        &self,
        dir: &str,
        since: Option<DateTime<Utc>>,
        plazo: std::time::Duration,
    ) -> Result<DigestSource, SessionError> {
        orus_app::git_log_con_plazo(dir, since, plazo)
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CuerpoEstado {
    id: String,
    status: String,
    note: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CuerpoComentario {
    task: String,
    update: String,
    body: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CuerpoMensaje {
    room: String,
    to: String,
    body: String,
    #[serde(rename = "replyTo")]
    reply_to: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CuerpoSala {
    room: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CuerpoIds {
    ids: Vec<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CuerpoCaptura {
    folder: String,
    since: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CuerpoAplicar {
    digest: String,
    accept: Vec<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CuerpoAviso {
    #[serde(rename = "type")]
    kind: String,
    message: String,
    team: String,
    user: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CuerpoReaccion {
    message: String,
    emoji: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CuerpoContexto {
    focus: String,
    mood: String,
    notes: String,
}

fn ok() -> Response {
    write_json(StatusCode::OK, &json!({"ok": true}))
}

/// `POST /api/tasks`: el adaptador no valida título ni
/// descripción; el error del backend queda en 500
pub(crate) async fn crear_tarea(
    AxumState(app): AxumState<Arc<App>>,
    body: Body,
) -> Result<Response, ErrorRuta> {
    let entrada: TaskInput = decode(body).await?;
    let cliente = app.client().await?;
    let tarea = cliente.create_task(&entrada).await?;
    Ok(write_json(
        StatusCode::OK,
        &json!({"ok": true, "task": tarea}),
    ))
}

/// `POST /api/tasks/status`: el estado se parsea antes de
/// pedir el cliente; uno desconocido es 500, no 400.
pub(crate) async fn cambiar_estado(
    AxumState(app): AxumState<Arc<App>>,
    body: Body,
) -> Result<Response, ErrorRuta> {
    let cuerpo: CuerpoEstado = decode(body).await?;
    let estado =
        parse_status(&cuerpo.status).map_err(|error| SessionError::Tool(error.to_string()))?;
    let cliente = app.client().await?;
    let progreso = cliente
        .set_task_status(&cuerpo.id, estado, &cuerpo.note)
        .await?;
    Ok(write_json(
        StatusCode::OK,
        &json!({"ok": true, "progress": progreso}),
    ))
}

/// `POST /api/tasks/files/comment`: el contrato no devuelve el
/// `update_id`.
pub(crate) async fn comentar_avance(
    AxumState(app): AxumState<Arc<App>>,
    body: Body,
) -> Result<Response, ErrorRuta> {
    let cuerpo: CuerpoComentario = decode(body).await?;
    let cliente = app.client().await?;
    cliente
        .comment_update(&cuerpo.task, &cuerpo.update, &cuerpo.body)
        .await?;
    Ok(ok())
}

/// `POST /api/messages`: valida `body`/`room`/`to` antes de
/// pedir el cliente y no recorta ninguno de los tres.
pub(crate) async fn enviar_mensaje(
    AxumState(app): AxumState<Arc<App>>,
    body: Body,
) -> Result<Response, ErrorRuta> {
    let cuerpo: CuerpoMensaje = decode(body).await?;
    if cuerpo.body.is_empty() {
        return Err(ErrorRuta::Invalido("el mensaje está vacío".to_owned()));
    }
    if cuerpo.room.is_empty() && cuerpo.to.is_empty() {
        return Err(ErrorRuta::Invalido("falta la sala o la persona".to_owned()));
    }
    let cliente = app.client().await?;
    if !cuerpo.to.is_empty() && cuerpo.room.is_empty() {
        cliente
            .send_direct(&cuerpo.to, &cuerpo.body, &cuerpo.reply_to)
            .await?;
    } else {
        cliente
            .send(&cuerpo.room, &cuerpo.body, &cuerpo.reply_to)
            .await?;
    }
    Ok(ok())
}

/// `POST /api/rooms/read`.
pub(crate) async fn marcar_sala(
    AxumState(app): AxumState<Arc<App>>,
    body: Body,
) -> Result<Response, ErrorRuta> {
    let cuerpo: CuerpoSala = decode(body).await?;
    let cliente = app.client().await?;
    cliente.mark_room_read(&cuerpo.room).await?;
    Ok(ok())
}

/// `POST /api/alerts/read`: sin ids se pasa `[]`, no null.
pub(crate) async fn marcar_alertas(
    AxumState(app): AxumState<Arc<App>>,
    body: Body,
) -> Result<Response, ErrorRuta> {
    let cuerpo: CuerpoIds = decode(body).await?;
    let cliente = app.client().await?;
    cliente.mark_alerts_read(&cuerpo.ids).await?;
    Ok(ok())
}

/// `POST /api/mentions/read`: sin ids se pasa `[]`, no null.
pub(crate) async fn marcar_menciones(
    AxumState(app): AxumState<Arc<App>>,
    body: Body,
) -> Result<Response, ErrorRuta> {
    let cuerpo: CuerpoIds = decode(body).await?;
    let cliente = app.client().await?;
    cliente.mark_mentions_read(&cuerpo.ids).await?;
    Ok(ok())
}

/// `POST /api/goals`: el adaptador no valida el título; el
/// backend responde 500 si falta
pub(crate) async fn crear_meta(
    AxumState(app): AxumState<Arc<App>>,
    body: Body,
) -> Result<Response, ErrorRuta> {
    let entrada: GoalInput = decode(body).await?;
    let cliente = app.client().await?;
    let meta = cliente.create_goal(entrada).await?;
    Ok(write_json(
        StatusCode::OK,
        &json!({"ok": true, "goal": meta}),
    ))
}

/// `POST /api/periods`.
pub(crate) async fn crear_periodo(
    AxumState(app): AxumState<Arc<App>>,
    body: Body,
) -> Result<Response, ErrorRuta> {
    let entrada: PeriodInput = decode(body).await?;
    let cliente = app.client().await?;
    let periodo = cliente.create_period(&entrada).await?;
    Ok(write_json(
        StatusCode::OK,
        &json!({"ok": true, "period": periodo}),
    ))
}

/// `POST /api/capture/propose`: `git_log` corre antes de
/// pedir el cliente, así que una carpeta inválida da 500 y no 401. El `folder`
/// original se pasa tal cual; solo la validación lo recorta.
pub(crate) async fn proponer_captura(
    AxumState(app): AxumState<Arc<App>>,
    Extension(git_log): Extension<Arc<dyn GitLog>>,
    body: Body,
) -> Result<Response, ErrorRuta> {
    let cuerpo: CuerpoCaptura = decode(body).await?;
    if cuerpo.folder.trim().is_empty() {
        return Err(ErrorRuta::Invalido(
            "falta la carpeta del repositorio".to_owned(),
        ));
    }
    let (since_utc, since_texto) = if cuerpo.since.is_empty() {
        // El contrato resta siete días al reloj local; `Duration::days` es 168 h fijas y
        // puede diferir una hora en el cambio de horario (D181).
        let local = Local::now() - Duration::days(7);
        (
            local.with_timezone(&Utc),
            local.format("%Y-%m-%d").to_string(),
        )
    } else {
        let fecha = NaiveDate::parse_from_str(&cuerpo.since, "%Y-%m-%d")
            .map_err(|_| ErrorRuta::Invalido("la fecha no es válida".to_owned()))?;
        let medianoche = fecha
            .and_hms_opt(0, 0, 0)
            .ok_or_else(|| ErrorRuta::Invalido("la fecha no es válida".to_owned()))?;
        (
            Utc.from_utc_datetime(&medianoche),
            fecha.format("%Y-%m-%d").to_string(),
        )
    };
    // `git_log` ejecuta `git` y es síncrona: va a un hilo bloqueante con
    // plazo para no detener el runtime ni colgar la petición para siempre,
    // y sigue ocurriendo antes de pedir el cliente.
    let source = git_con_plazo(git_log, cuerpo.folder, Some(since_utc), PLAZO_GIT).await?;
    let cliente = app.client().await?;
    let digest = cliente.propose_digest(&since_texto, &[source]).await?;
    // `Digest` directo serializaría `candidates: null`; El contrato usa `[]` y deja
    // `skipped` como `null` cuando el servidor no lo mandó (D176).
    Ok(write_json(
        StatusCode::OK,
        &json!({
            "digest_id": digest.id,
            "expires_at": digest.expires_at,
            "candidates": digest.candidates.unwrap_or_default(),
            "skipped": digest.skipped,
            "next_step": digest.next_step,
        }),
    ))
}

/// Plazo de `git log` en capture/propose: un git colgado (una unidad de red
/// que no responde, un repo enorme) no puede dejar la petición esperando
/// para siempre.
const PLAZO_GIT: std::time::Duration = std::time::Duration::from_secs(30);

/// `git_log` en un hilo bloqueante, con plazo y cancelación. El plazo es
/// parámetro para que las pruebas lo ejerciten sin esperar 30 s.
async fn git_con_plazo(
    git_log: Arc<dyn GitLog>,
    folder: String,
    since: Option<DateTime<Utc>>,
    plazo: std::time::Duration,
) -> Result<DigestSource, SessionError> {
    let mut tarea =
        tokio::task::spawn_blocking(move || git_log.log_con_plazo(&folder, since, plazo));
    match tokio::time::timeout(plazo, &mut tarea).await {
        Ok(resultado) => resultado.map_err(|error| SessionError::Tool(error.to_string()))?,
        Err(_vencido) => {
            // El git ya recibió este plazo y se mata solo: `abort` no puede
            // interrumpir un hilo bloqueante, solo evita esperar al join.
            tarea.abort();
            Err(SessionError::Tool(format!(
                "git no terminó dentro del plazo de {plazo:?} leyendo la carpeta"
            )))
        }
    }
}

/// `POST /api/capture/apply`: un digest o `accept` vacío lo
/// rechaza el backend con 500, no 400.
pub(crate) async fn aplicar_captura(
    AxumState(app): AxumState<Arc<App>>,
    body: Body,
) -> Result<Response, ErrorRuta> {
    let cuerpo: CuerpoAplicar = decode(body).await?;
    let cliente = app.client().await?;
    let result = cliente.apply_digest(&cuerpo.digest, &cuerpo.accept).await?;
    Ok(write_json(
        StatusCode::OK,
        &json!({"ok": true, "result": result}),
    ))
}

/// `POST /api/alerts/send`: tipo y destino se validan
/// antes del cliente; el mensaje vacío lo rechaza el backend con 500.
pub(crate) async fn enviar_aviso(
    AxumState(app): AxumState<Arc<App>>,
    body: Body,
) -> Result<Response, ErrorRuta> {
    let cuerpo: CuerpoAviso = decode(body).await?;
    if !matches!(
        cuerpo.kind.as_str(),
        "blocker" | "misalignment" | "milestone" | "info"
    ) {
        return Err(ErrorRuta::Invalido("tipo de aviso desconocido".to_owned()));
    }
    if cuerpo.team.is_empty() && cuerpo.user.is_empty() {
        return Err(ErrorRuta::Invalido("elige a quién va el aviso".to_owned()));
    }
    let cliente = app.client().await?;
    cliente
        .send_alert(&cuerpo.kind, &cuerpo.message, &cuerpo.team, &cuerpo.user)
        .await?;
    Ok(ok())
}

/// `POST /api/messages/react`: no valida; el backend
/// responde 500 con `reacción incompleta`.
pub(crate) async fn reaccionar(
    AxumState(app): AxumState<Arc<App>>,
    body: Body,
) -> Result<Response, ErrorRuta> {
    let cuerpo: CuerpoReaccion = decode(body).await?;
    let cliente = app.client().await?;
    cliente.react(&cuerpo.message, &cuerpo.emoji).await?;
    Ok(ok())
}

/// `POST /api/context`: no valida; el backend responde 500
/// con `no hay nada que actualizar`.
pub(crate) async fn actualizar_contexto(
    AxumState(app): AxumState<Arc<App>>,
    body: Body,
) -> Result<Response, ErrorRuta> {
    let cuerpo: CuerpoContexto = decode(body).await?;
    let cliente = app.client().await?;
    cliente
        .update_context(&cuerpo.focus, &cuerpo.mood, &cuerpo.notes)
        .await?;
    Ok(ok())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    use chrono::TimeZone;

    use super::*;

    struct GitLento {
        llamado: AtomicBool,
    }

    impl GitLog for GitLento {
        fn log(
            &self,
            _dir: &str,
            _since: Option<DateTime<Utc>>,
        ) -> Result<DigestSource, SessionError> {
            self.llamado.store(true, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_secs(2));
            Ok(DigestSource {
                name: "repo".to_owned(),
                items: None,
                truncated: false,
            })
        }
    }

    /// Issue #94: un git que no responde corta la petición con error dentro
    /// del plazo en vez de colgarla para siempre.
    #[tokio::test]
    async fn git_colgado_se_corta_por_plazo() {
        let git = Arc::new(GitLento {
            llamado: AtomicBool::new(false),
        });
        let inicio = std::time::Instant::now();
        let resultado = git_con_plazo(
            Arc::clone(&git) as Arc<dyn GitLog>,
            "C:/repo".to_owned(),
            Some(
                Utc.with_ymd_and_hms(2026, 11, 1, 0, 0, 0)
                    .single()
                    .expect("fecha"),
            ),
            std::time::Duration::from_millis(200),
        )
        .await;
        assert!(
            inicio.elapsed() < std::time::Duration::from_secs(1),
            "la petición esperó a git: {:?}",
            inicio.elapsed()
        );
        let error = resultado.expect_err("git colgado debe cortar por plazo");
        assert!(error.to_string().contains("plazo"), "{error}");
        assert!(git.llamado.load(Ordering::SeqCst), "git_log sí se llamó");
    }

    /// Un git que solo se detiene con su propio plazo: comprueba que la
    /// petición le pasa el plazo en vez de conformarse con abortar el join.
    struct GitConPlazo {
        plazo: Mutex<Option<std::time::Duration>>,
    }

    impl GitLog for GitConPlazo {
        fn log(
            &self,
            _dir: &str,
            _since: Option<DateTime<Utc>>,
        ) -> Result<DigestSource, SessionError> {
            Err(SessionError::Tool(
                "git sin plazo: no se puede matar".to_owned(),
            ))
        }

        fn log_con_plazo(
            &self,
            _dir: &str,
            _since: Option<DateTime<Utc>>,
            plazo: std::time::Duration,
        ) -> Result<DigestSource, SessionError> {
            *self.plazo.lock().expect("plazo") = Some(plazo);
            std::thread::sleep(plazo / 2);
            Err(SessionError::Tool(format!(
                "git no respondió en {} ms; se mató el proceso",
                plazo.as_millis()
            )))
        }
    }

    /// Issue #129: el git recibe el plazo de la petición y se mata solo;
    /// abortar el `JoinHandle` no interrumpe un hilo bloqueante.
    #[tokio::test]
    async fn el_plazo_llega_al_git_para_que_mate_el_proceso() {
        let git = Arc::new(GitConPlazo {
            plazo: Mutex::new(None),
        });
        let plazo = std::time::Duration::from_millis(600);
        let inicio = std::time::Instant::now();
        let error = git_con_plazo(
            Arc::clone(&git) as Arc<dyn GitLog>,
            "C:/repo".to_owned(),
            None,
            plazo,
        )
        .await
        .expect_err("un git colgado debe dar error");

        assert_eq!(
            *git.plazo.lock().expect("plazo"),
            Some(plazo),
            "la petición tiene que pasar su plazo al git"
        );
        assert!(
            error.to_string().contains("se mató el proceso"),
            "el error debe venir del git que se mató: {error}"
        );
        assert!(
            inicio.elapsed() < plazo,
            "el git se detuvo por su plazo, no por el timeout externo: {:?}",
            inicio.elapsed()
        );
    }
}
