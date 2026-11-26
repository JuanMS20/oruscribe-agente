//! Aprobaciones de la ventana: la cola que la persona contesta.
//!
//! La cola va detrás de candados porque
//! `request` espera sin bloquear a `answer`.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use orus_agent::{CancelToken, HERRAMIENTA_PROPOSE_PLAN, Risk};
use orus_core::ToolCall;
use orus_tools::permissions;
use serde::{Deserialize, Serialize};

use crate::sync::lock;

/// Cada cuánto se mira el token mientras se espera la respuesta.
const SONDEO_CANCELACION: Duration = Duration::from_millis(50);
/// El plazo de la ventana. Tiene que ser menor que
/// `DURACION_TURNO` (300 s): si el turno vence primero, el error culpa al
/// agente por lento cuando lo que venció fue la tarjeta sin respuesta (#95).
const ESPERA_POR_DEFECTO: Duration = Duration::from_secs(240);

/// El error del turno cuando la tarjeta de aprobación venció sin respuesta:
/// nombra la aprobación, no al agente (#95).
pub(crate) const APROBACION_VENCIDA: &str =
    "la aprobación venció sin respuesta; el turno se detuvo";

/// Una acción que el agente quiere correr y no puede hasta que la persona
/// diga que sí. `rule_pattern` solo viaja si la llamada admite una regla
/// recordada.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentApproval {
    pub id: String,
    pub tool: String,
    pub action: String,
    pub text: String,
    pub at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub rule_pattern: String,
}

/// Los fallos del flujo de aprobación: la regla no se pudo guardar tras el
/// sí, o la tarjeta venció sin respuesta (#95).
#[derive(Debug, thiserror::Error)]
pub enum ApprovalError {
    #[error("la acción se aprobó, pero no se pudo recordar: {0}")]
    Recordar(#[source] permissions::Error),
    /// La tarjeta venció sin respuesta: no es una negación de la persona ni
    /// una cancelación del turno, y el error tiene que nombrarla.
    #[error("la aprobación venció sin respuesta")]
    Vencida,
}

/// La aprobación pendiente más el canal donde llega la respuesta.
struct Pending {
    info: AgentApproval,
    tx: Sender<bool>,
}

/// Cola de aprobaciones que la ventana pinta y contesta.
pub struct ApprovalQueue {
    seq: AtomicU64,
    pending: Mutex<Vec<Pending>>,
    wait: Duration,
    /// Cuántas tarjetas vencieron sin respuesta. El turno lo consulta para
    /// que su error nombre la aprobación y no al agente (#95).
    vencidas: AtomicU64,
}

impl ApprovalQueue {
    pub fn new() -> Self {
        Self::with_wait(ESPERA_POR_DEFECTO)
    }

    /// El plazo es configurable para las pruebas; en producción son 4 min.
    pub fn with_wait(wait: Duration) -> Self {
        Self {
            seq: AtomicU64::new(0),
            pending: Mutex::new(Vec::new()),
            wait,
            vencidas: AtomicU64::new(0),
        }
    }

    /// Tarjetas que vencieron sin respuesta desde que existe la cola.
    pub fn vencidas(&self) -> u64 {
        self.vencidas.load(Ordering::SeqCst)
    }

    /// Encola la llamada y espera. Un plazo agotado, una cancelación o un
    /// cierre de sesión cuentan como no.
    /// El vencimiento propio se distingue de la negación con
    /// [`ApprovalError::Vencida`] para que el error del turno nombre la
    /// aprobación y no al agente (#95).
    ///
    /// Es síncrona porque es la forma de [`orus_agent::ApproveFn`]; el borde
    /// que conecta el turno la corre en un hilo de bloqueo.
    pub fn request(
        &self,
        call: &ToolCall,
        risk: Risk,
        cancel: &CancelToken,
    ) -> Result<bool, ApprovalError> {
        // Una ejecución cancelada no vuelve a encolar.
        if cancel.esta_cancelado() {
            return Ok(false);
        }

        let (tx, rx) = mpsc::channel::<bool>();
        let info = {
            let mut pendientes = lock(&self.pending);
            if cancel.esta_cancelado() {
                return Ok(false);
            }
            let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
            // La casilla «Recordar» solo existe si la llamada admite regla.
            let pattern = permissions::eligible(&call.name, &call.args, risk).unwrap_or_default();
            let info = AgentApproval {
                id: format!("ap-{seq}"),
                tool: call.name.clone(),
                action: nombre_accion(&call.name),
                // El contrato guarda `string(c.Args)`, el JSON crudo; acá `args` ya
                // es un valor JSON, así que se serializa compacto.
                text: serde_json::to_string(&call.args).unwrap_or_default(),
                at: Utc::now(),
                rule_pattern: pattern,
            };
            pendientes.push(Pending {
                info: info.clone(),
                tx,
            });
            info
        };

        let plazo = Instant::now() + self.wait;
        let mut vencida = false;
        loop {
            if cancel.esta_cancelado() {
                break;
            }
            let restante = plazo.saturating_duration_since(Instant::now());
            if restante.is_zero() {
                vencida = true;
                break;
            }
            match rx.recv_timeout(restante.min(SONDEO_CANCELACION)) {
                Ok(ok) => return Ok(ok),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }

        self.quitar(&info.id);
        // Si la persona contestó justo al vencer, gana su respuesta.
        if let Ok(ok) = rx.try_recv() {
            return Ok(ok);
        }
        if vencida {
            self.vencidas.fetch_add(1, Ordering::SeqCst);
            return Err(ApprovalError::Vencida);
        }
        Ok(false)
    }

    /// Contesta una aprobación. Un id que no existe no hace nada: la ventana
    /// puede llegar tarde y eso no puede aprobar otra acción
    pub fn answer(&self, id: &str, ok: bool, remember: bool) -> Result<(), ApprovalError> {
        let pendiente = {
            let mut pendientes = lock(&self.pending);
            let Some(posicion) = pendientes.iter().position(|p| p.info.id == id) else {
                return Ok(());
            };
            let pendiente = pendientes.remove(posicion);
            let _ = pendiente.tx.send(ok);
            pendiente
        };
        if !ok || !remember || pendiente.info.rule_pattern.is_empty() {
            return Ok(());
        }
        permissions::remember(&pendiente.info.tool, &pendiente.info.rule_pattern, "")
            .map(|_| ())
            .map_err(ApprovalError::Recordar)
    }

    /// Una copia de lo que la ventana tiene que pintar.
    pub fn pending(&self) -> Vec<AgentApproval> {
        lock(&self.pending)
            .iter()
            .map(|pendiente| pendiente.info.clone())
            .collect()
    }

    /// Cierre de sesión: vacía la cola y niega todo. No lo llama la cancelación del turno, que solo corta el turno.
    pub fn cancel_all(&self) {
        let pendientes = std::mem::take(&mut *lock(&self.pending));
        for pendiente in pendientes {
            let _ = pendiente.tx.send(false);
        }
    }

    fn quitar(&self, id: &str) {
        lock(&self.pending).retain(|pendiente| pendiente.info.id != id);
    }
}

impl Default for ApprovalQueue {
    fn default() -> Self {
        Self::new()
    }
}

/// Traduce la herramienta a lo que la persona lee en la tarjeta. Una
/// herramienta nueva se muestra por su nombre técnico.
pub fn nombre_accion(tool: &str) -> String {
    let accion = match tool {
        "create_task" => "Crear una tarea",
        "update_task_status" => "Cambiar el estado de una tarea",
        "comment_task_update" => "Comentar un avance de una tarea",
        "upload_task_file" => "Subir un archivo a una tarea",
        "attach_task_files" => "Adjuntar archivos a una tarea",
        "create_goal" => "Crear una meta",
        "create_period" => "Crear un periodo",
        "send_chat_message" => "Enviar un mensaje a una sala",
        "upload_chat_file" => "Subir un archivo a una sala",
        HERRAMIENTA_PROPOSE_PLAN => "Ejecutar este plan",
        "mark_chat_read" => "Marcar mensajes como leídos",
        "mark_alerts_read" => "Marcar avisos como leídos",
        "apply_work_digest" => "Aplicar una puesta al día",
        "propose_work_digest" => "Preparar una puesta al día",
        "local_list_dir" => "Listar una carpeta",
        "local_read_file" => "Leer un archivo",
        "local_search_text" => "Buscar texto en archivos",
        "local_write_file" => "Crear o editar un archivo",
        "local_edit_file" => "Editar un archivo",
        "local_run_command" => "Ejecutar un programa",
        _ => return tool.to_owned(),
    };
    accion.to_owned()
}
