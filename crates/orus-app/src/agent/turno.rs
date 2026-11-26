//! El contrato del turno y su cableado con el arnés.

use std::sync::Arc;

use async_trait::async_trait;
use orus_agent::{CancelToken, Risk, ToolError};
use orus_core::ToolCall;
use orus_harness::sessions::Session;
use orus_harness::{ApproveRiskFn, ErrorTurno, TurnoReal};
use orus_org::{JsonObject, OrgError, ToolSpec};

use crate::approvals::{APROBACION_VENCIDA, ApprovalError, ApprovalQueue};
use crate::sesion::{Backend, SessionError};
use crate::telemetria::Telemetry;

use super::Emisor;

pub use orus_harness::Turno;

/// La costura del turno: el real usa el arnés y los tests uno falso.
#[async_trait]
pub trait TurnRunner: Send + Sync {
    async fn run(
        &self,
        turno: Turno,
        session: &mut Session,
        cancel: CancelToken,
        emitir: Emisor,
    ) -> Result<(), ErrorTurno>;
}

/// El turno real, con la cola de aprobaciones de la ventana como único
/// `Approve`.
pub struct HarnessRunner {
    approvals: Arc<ApprovalQueue>,
}

impl HarnessRunner {
    pub fn new(approvals: Arc<ApprovalQueue>) -> Self {
        Self { approvals }
    }
}

#[async_trait]
impl TurnRunner for HarnessRunner {
    async fn run(
        &self,
        turno: Turno,
        session: &mut Session,
        cancel: CancelToken,
        emitir: Emisor,
    ) -> Result<(), ErrorTurno> {
        // `correr_turno_real` agrega `turno.mensaje`; el estado ya lo agregó al
        // historial. Se quita la copia para no mandarle dos veces el mensaje.
        let pendiente = if session
            .messages
            .last()
            .is_some_and(|mensaje| mensaje.role == "user" && mensaje.content == turno.mensaje)
        {
            session.messages.pop()
        } else {
            None
        };
        let base = session.messages.len();
        let real = TurnoReal {
            datos: &turno,
            approve: Some(aprobar_con(Arc::clone(&self.approvals), cancel.clone())),
            cancel: Some(cancel),
        };
        let resultado =
            orus_harness::correr_turno_real(&real, session, None, move |evento| emitir(evento))
                .await;
        // Si el turno falló antes de agregar el mensaje, se devuelve al
        // historial: el estado lo tenía y no se puede perder.
        if session.messages.len() == base
            && let Some(mensaje) = pendiente
        {
            session.messages.push(mensaje);
        }
        resultado
    }
}

/// El puente entre el `Backend` de la app y el que espera el arnés
/// El arnés no conoce `SessionError`: el texto viaja en
/// `OrgError::Tool`, salvo los errores de transporte, que conservan su
/// variante para que `mcptools` devuelva el cupo (#113, #148). No se duplica
/// el puente en `orus-api`.
pub(crate) struct AdaptadorMcp {
    pub(crate) backend: Arc<dyn Backend>,
    pub(crate) telemetria: Arc<Telemetry>,
}

#[async_trait]
impl orus_harness::mcptools::Backend for AdaptadorMcp {
    async fn tool_specs(&self) -> orus_org::Result<Vec<ToolSpec>> {
        self.backend
            .tool_specs()
            .await
            .map_err(|error| OrgError::Tool {
                tool: "tool_specs".to_owned(),
                message: error.to_string(),
            })
    }

    async fn call(&self, nombre: &str, args: JsonObject) -> orus_org::Result<String> {
        match self.backend.call_tool(nombre, args).await {
            Ok(texto) => Ok(texto),
            Err(error) => {
                self.reportar_fallo(nombre, &error);
                Err(match error {
                    // El error del cliente MCP viaja entero, con su variante: un
                    // `Call` de transporte no llegó al servidor y `mcptools`
                    // reembolsa la lectura (#148). Antes se aplanaba a `Tool` y un
                    // blip quemaba una de las 15.
                    SessionError::Org(error) => error,
                    // La conexión al abrir la sesión también es reembolsable
                    // (#113).
                    SessionError::Connect(_) => OrgError::NotConnected,
                    // Un error que el servidor SÍ respondió sigue siendo `Tool` y
                    // gasta el cupo (#94).
                    other => OrgError::Tool {
                        tool: nombre.to_owned(),
                        message: other.to_string(),
                    },
                })
            }
        }
    }
}

impl AdaptadorMcp {
    fn reportar_fallo(&self, nombre: &str, error: &SessionError) {
        let (kind, nivel) = match error {
            SessionError::Auth | SessionError::LoggedOut => ("auth", "error"),
            SessionError::Connect(_) | SessionError::Org(OrgError::NotConnected) => {
                ("connect", "warning")
            }
            SessionError::Org(OrgError::Call { .. } | OrgError::Timeout { .. }) => {
                ("transport", "error")
            }
            SessionError::Org(OrgError::RateLimited { .. }) => ("rate_limit", "warning"),
            _ => ("tool", "warning"),
        };
        self.telemetria.report(
            "mcp.tool",
            nivel,
            &format!("{nombre}: {error}"),
            serde_json::json!({ "mcp_tool": nombre, "kind": kind }),
        );
    }
}

/// La costura síncrona de `ApproveFn` no puede frenar el runtime: se espera en
/// `block_in_place` cuando hay varios hilos, o en un hilo dedicado.
fn aprobar_con(approvals: Arc<ApprovalQueue>, cancel: CancelToken) -> ApproveRiskFn {
    Arc::new(move |call, riesgo| pedir_aprobacion(Arc::clone(&approvals), call, riesgo, &cancel))
}

fn pedir_aprobacion(
    approvals: Arc<ApprovalQueue>,
    call: &ToolCall,
    riesgo: Option<Risk>,
    cancel: &CancelToken,
) -> Result<bool, ToolError> {
    let call = call.clone();
    let cancel = cancel.clone();
    let trabajo = move || -> Result<bool, ToolError> {
        match approvals.request(&call, riesgo.unwrap_or(Risk::Read), &cancel) {
            Ok(ok) => Ok(ok),
            // La tarjeta venció: se corta el turno y el error nombra la
            // aprobación, no al agente (#95).
            Err(ApprovalError::Vencida) => {
                cancel.cancelar();
                Err(ToolError::Message(APROBACION_VENCIDA.to_owned()))
            }
            Err(error) => Err(ToolError::Message(error.to_string())),
        }
    };
    match tokio::runtime::Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(trabajo)
        }
        _ => std::thread::spawn(trabajo)
            .join()
            .map_err(|_| ToolError::Message("la aprobación se cortó".to_owned()))?,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    use orus_agent::Tool as _;
    use orus_harness::mcptools::{McpTool, RegistroLecturas};
    use orus_org::{Me, MessagesPage, Room, Task};
    use serde_json::json;

    use super::*;
    use crate::sesion::SessionError;

    /// Un backend al que le falla la red las 20 primeras llamadas y después
    /// responde: reproduce el blip que quemaba el cupo de lecturas (#113).
    struct BackendConBlip {
        llamadas: AtomicU32,
    }

    #[async_trait]
    impl Backend for BackendConBlip {
        async fn me(&self) -> Result<Me, SessionError> {
            Err(SessionError::Tool("no usado".to_owned()))
        }

        async fn tasks(&self) -> Result<Vec<Task>, SessionError> {
            Err(SessionError::Tool("no usado".to_owned()))
        }

        async fn rooms(&self) -> Result<Vec<Room>, SessionError> {
            Err(SessionError::Tool("no usado".to_owned()))
        }

        async fn messages(&self, _: &str, _: i64, _: i64) -> Result<MessagesPage, SessionError> {
            Err(SessionError::Tool("no usado".to_owned()))
        }

        async fn send(&self, _: &str, _: &str, _: &str) -> Result<(), SessionError> {
            Err(SessionError::Tool("no usado".to_owned()))
        }

        async fn tool_specs(&self) -> Result<Vec<ToolSpec>, SessionError> {
            Ok(Vec::new())
        }

        async fn call_tool(&self, _: &str, _: JsonObject) -> Result<String, SessionError> {
            if self.llamadas.fetch_add(1, Ordering::SeqCst) < 20 {
                Err(SessionError::Connect("sin red".to_owned()))
            } else {
                Ok("resultado del servidor".to_owned())
            }
        }

        async fn doorbell(&self) -> Result<orus_org::doorbell::Config, SessionError> {
            Err(SessionError::Tool("no usado".to_owned()))
        }
    }

    /// #113: un blip de red no puede gastar el cupo de lecturas. El adaptador
    /// conserva la variante de conexión y `mcptools` devuelve la reserva.
    #[tokio::test]
    async fn un_blip_de_red_no_gasta_el_cupo_de_lecturas() {
        let backend = Arc::new(BackendConBlip {
            llamadas: AtomicU32::new(0),
        });
        let herramienta = McpTool::new(
            Some(Arc::new(AdaptadorMcp {
                backend,
                telemetria: Telemetry::new(false, "test", None),
            })),
            ToolSpec {
                name: "get_my_tasks".to_owned(),
                description: "tareas".to_owned(),
                schema: json!({"type": "object"}),
            },
            &RegistroLecturas::new(),
        );

        for _ in 0..20 {
            herramienta
                .run(json!({}))
                .await
                .expect_err("la red falla durante el blip");
        }
        for _ in 0..15 {
            herramienta
                .run(json!({}))
                .await
                .expect("la ventana sigue entera cuando vuelve la red");
        }
        let error = herramienta
            .run(json!({}))
            .await
            .expect_err("la dieciséis con red cae como sondeo");
        assert!(
            error.to_string().contains("parece sondeo"),
            "el cupo se agotó antes de tiempo: {error}"
        );
    }

    /// Lo que responde el `Backend`: el transporte caído pasa por el mismo
    /// `org_error` que usa `Cliente`, o el servidor rechaza la herramienta.
    #[derive(Clone, Copy)]
    enum Fallo {
        Transporte,
        Herramienta,
    }

    /// El backend de los tests de #148: falla como en producción y después
    /// responde, para ver si la ventana se gastó durante el blip.
    struct BackendQueFalla {
        fallo: Fallo,
        llamadas: AtomicU32,
    }

    impl BackendQueFalla {
        fn nuevo(fallo: Fallo) -> Self {
            Self {
                fallo,
                llamadas: AtomicU32::new(0),
            }
        }
    }

    #[async_trait]
    impl Backend for BackendQueFalla {
        async fn me(&self) -> Result<Me, SessionError> {
            Err(SessionError::Tool("no usado".to_owned()))
        }

        async fn tasks(&self) -> Result<Vec<Task>, SessionError> {
            Err(SessionError::Tool("no usado".to_owned()))
        }

        async fn rooms(&self) -> Result<Vec<Room>, SessionError> {
            Err(SessionError::Tool("no usado".to_owned()))
        }

        async fn messages(&self, _: &str, _: i64, _: i64) -> Result<MessagesPage, SessionError> {
            Err(SessionError::Tool("no usado".to_owned()))
        }

        async fn send(&self, _: &str, _: &str, _: &str) -> Result<(), SessionError> {
            Err(SessionError::Tool("no usado".to_owned()))
        }

        async fn tool_specs(&self) -> Result<Vec<ToolSpec>, SessionError> {
            Ok(Vec::new())
        }

        async fn call_tool(&self, _: &str, _: JsonObject) -> Result<String, SessionError> {
            let llamada = self.llamadas.fetch_add(1, Ordering::SeqCst);
            match self.fallo {
                Fallo::Transporte if llamada < 20 => Err(SessionError::desde_org(OrgError::Call {
                    tool: "get_my_tasks".to_owned(),
                    source: Box::new(rmcp::service::ServiceError::TransportClosed),
                })),
                Fallo::Transporte => Ok("resultado del servidor".to_owned()),
                Fallo::Herramienta => Err(SessionError::Tool(
                    "el servidor rechazó la lectura".to_owned(),
                )),
            }
        }

        async fn doorbell(&self) -> Result<orus_org::doorbell::Config, SessionError> {
            Err(SessionError::Tool("no usado".to_owned()))
        }
    }

    fn herramienta(backend: Arc<dyn Backend>) -> McpTool {
        McpTool::new(
            Some(Arc::new(AdaptadorMcp {
                backend,
                telemetria: Telemetry::new(false, "test", None),
            })),
            ToolSpec {
                name: "get_my_tasks".to_owned(),
                description: "tareas".to_owned(),
                schema: json!({"type": "object"}),
            },
            &RegistroLecturas::new(),
        )
    }

    /// #148: el error de transporte sale de `org_error` como en `Cliente`, el
    /// adaptador no lo aplana a `Tool` y `mcptools` devuelve el cupo. Veinte
    /// fallos seguidos dejan enteras las 15 lecturas de la ventana.
    #[tokio::test]
    async fn el_transporte_caido_conserva_la_variante_y_no_gasta_el_cupo() {
        let adaptador = AdaptadorMcp {
            backend: Arc::new(BackendQueFalla::nuevo(Fallo::Transporte)),
            telemetria: Telemetry::new(false, "test", None),
        };
        let error =
            orus_harness::mcptools::Backend::call(&adaptador, "get_my_tasks", JsonObject::new())
                .await
                .expect_err("el transporte está caído");
        assert!(
            matches!(error, OrgError::Call { .. }),
            "el adaptador aplanó el error del transporte: {error:?}"
        );

        let herramienta = herramienta(Arc::new(BackendQueFalla::nuevo(Fallo::Transporte)));
        for _ in 0..20 {
            herramienta
                .run(json!({}))
                .await
                .expect_err("el transporte falla durante el blip");
        }
        for _ in 0..15 {
            herramienta
                .run(json!({}))
                .await
                .expect("la ventana sigue entera cuando vuelve el transporte");
        }
        let error = herramienta
            .run(json!({}))
            .await
            .expect_err("la dieciséis con transporte cae como sondeo");
        assert!(
            error.to_string().contains("parece sondeo"),
            "el cupo se agotó antes de tiempo: {error}"
        );
    }

    /// #94: lo que el servidor SÍ respondió sigue gastando la ventana; no se
    /// reembolsa un error de herramienta.
    #[tokio::test]
    async fn un_error_de_herramienta_si_gasta_el_cupo_de_lecturas() {
        let herramienta = herramienta(Arc::new(BackendQueFalla::nuevo(Fallo::Herramienta)));
        for _ in 0..15 {
            herramienta
                .run(json!({}))
                .await
                .expect_err("la herramienta falla");
        }
        let error = herramienta
            .run(json!({}))
            .await
            .expect_err("la dieciséis ya es sondeo");
        assert!(
            error.to_string().contains("parece sondeo"),
            "un error de herramienta no gastó el cupo: {error}"
        );
    }
}
