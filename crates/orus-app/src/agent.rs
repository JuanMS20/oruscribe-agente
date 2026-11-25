//! Estado de la conversación del agente y su turno.
//!
//! Port de `internal/app/agent.go:539-1001,1003-1113`: historial, pasos,
//! tokens, costo, generación y cancelación. El turno real vive en
//! `orus-harness`; acá solo se conecta (D80).

mod app;
mod imagenes;
mod secretos;
mod tipos;
mod turno;
mod validacion;
mod vista;

pub use app::ProveedorFn;
pub use imagenes::normalizar_imagenes;
pub use secretos::{AgentSecrets, NoSecrets, Secrets, WakeLog};
pub(crate) use tipos::EstadoAgente;
pub use tipos::{AgentError, AgentInput, AgentStep};
pub use turno::{HarnessRunner, TurnRunner, Turno};
pub(crate) use vista::reconstruir_pasos;
pub use vista::{AgentImage, AgentMessage, AgentProbe, AgentView, JournalEntryView};

use std::any::Any;
use std::collections::HashSet;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use orus_agent::{CancelToken, ProviderError, RunnerError, costo_estimado};
use orus_core::{Event, Image, Message};
use orus_harness::ErrorTurno;
pub use orus_harness::Toolset;
use orus_harness::sessions::{self, Session};
use serde_json::json;

use crate::approvals::{APROBACION_VENCIDA, ApprovalQueue};
use crate::conversations::Conversations;
use crate::sync::lock;
use crate::telemetria::Telemetry;

const MAX_HISTORIAL: usize = 200;
const MAX_PASOS: usize = 100;
const MAX_BORRADOR: usize = 256 << 10;
const MAX_MODELO: usize = 200;
const DURACION_TURNO: Duration = Duration::from_secs(600);
/// Margen para que el runner vuelque lo parcial cuando vence `turn_timeout`.
const PLAZO_DE_GRACIA: Duration = Duration::from_secs(5);

/// El callback de eventos que recibe cada turno.
pub type Emisor = Arc<dyn Fn(Event) + Send + Sync>;

type FinAgente = Arc<dyn Fn(u64, i64, Option<String>) + Send + Sync>;

/// El dueño único del estado de la conversación.
pub struct Agent {
    pub(crate) conversations: Conversations,
    pub(crate) approvals: Arc<ApprovalQueue>,
    pub(crate) runner: Arc<dyn TurnRunner>,
    // `Arc` para que el callback `'static` de eventos actualice el estado vivo.
    pub(crate) estado: Arc<Mutex<EstadoAgente>>,
    turn_timeout: Duration,
    on_finish: Option<FinAgente>,
    /// Barrera de prueba: se llama en el hueco entre el chequeo de `busy` y el
    /// borrado en disco de `delete_conversation`. Solo la arma el test de la
    /// carrera con el timbre.
    #[doc(hidden)]
    pub(crate) delete_gap: Option<Arc<dyn Fn() + Send + Sync>>,
    #[doc(hidden)]
    pub(crate) save_gap: Option<Arc<dyn Fn() + Send + Sync>>,
    #[doc(hidden)]
    pub(crate) open_gap: Option<Arc<dyn Fn() + Send + Sync>>,
    /// Conversaciones borradas mientras un turno las tenía tomadas. El
    /// guardado final no debe recrearlas (`agent.go:862-876`).
    pub(crate) borradas: Arc<Mutex<HashSet<String>>>,
    telemetry: Option<Arc<Telemetry>>,
}

/// Suelta el turno aunque el runner entre en pánico: sin esto `busy` queda en
/// true para siempre y todo envío responde 409 hasta reiniciar.
struct LiberaTurno<'a> {
    agente: &'a Agent,
    generacion: u64,
}

impl Drop for LiberaTurno<'_> {
    fn drop(&mut self) {
        let mut estado = lock(&self.agente.estado);
        if estado.generation == self.generacion {
            estado.busy = false;
            estado.cancel = None;
        }
    }
}

/// Lo que `preparar` dejó listo: el turno ya tomó el agente y el estado ya
/// tiene el mensaje. Es propiedad porque `empezar` lo mueve a otra tarea.
struct Preparado {
    turno: Turno,
    sesion: Session,
    generacion: u64,
    cancel: CancelToken,
    tasa_in: f64,
    tasa_out: f64,
    uso: Arc<Mutex<(i64, i64)>>,
    emitir: Emisor,
}

impl Agent {
    /// `maxMensaje` (`agent.go:30`): el tope del texto de un envío.
    pub const MAX_MENSAJE: usize = 80_000;

    /// Recorta y valida el texto del envío (`agent.go:545-552`). La ruta y
    /// `preparar` comparten este dueño para no repetir el tope ni el texto.
    /// Con imágenes, el texto puede ir vacío: solo-imagen es un turno válido.
    pub fn validar_mensaje_con_imagenes(
        texto: &str,
        hay_imagenes: bool,
    ) -> Result<&str, AgentError> {
        let mensaje = texto.trim();
        if mensaje.is_empty() && !hay_imagenes {
            return Err(AgentError::Invalid(
                "escribe un mensaje o adjunta una imagen".to_owned(),
            ));
        }
        if mensaje.chars().count() > Self::MAX_MENSAJE {
            return Err(AgentError::Invalid(format!(
                "el mensaje no puede tener más de {} caracteres",
                Self::MAX_MENSAJE
            )));
        }
        Ok(mensaje)
    }

    pub fn new(
        conversations: Conversations,
        approvals: Arc<ApprovalQueue>,
        runner: Arc<dyn TurnRunner>,
    ) -> Self {
        Self {
            conversations,
            approvals,
            runner,
            estado: Arc::new(Mutex::new(EstadoAgente::default())),
            turn_timeout: DURACION_TURNO,
            on_finish: None,
            delete_gap: None,
            save_gap: None,
            open_gap: None,
            borradas: Arc::new(Mutex::new(HashSet::new())),
            telemetry: None,
        }
    }

    /// Conecta la telemetría: los turnos fallidos se reportan con su causa y
    /// los pasos del turno quedan como migas del siguiente error.
    pub fn with_telemetry(mut self, telemetry: Arc<Telemetry>) -> Self {
        self.telemetry = Some(telemetry);
        self
    }

    fn miga(&self, categoria: &str, mensaje: &str) {
        if let Some(telemetry) = &self.telemetry {
            telemetry.breadcrumb(categoria, mensaje);
        }
    }

    /// Cambia el plazo del turno; los tests usan uno corto.
    pub fn with_turn_timeout(mut self, timeout: Duration) -> Self {
        self.turn_timeout = timeout;
        self
    }

    /// El aviso de fin de turno que usa el despertar (`agent.go:995-1000`).
    pub fn with_on_finish<F>(mut self, on_finish: F) -> Self
    where
        F: Fn(u64, i64, Option<String>) + Send + Sync + 'static,
    {
        self.on_finish = Some(Arc::new(on_finish));
        self
    }

    /// Instala la barrera de prueba del borrado. Producción nunca la arma.
    #[doc(hidden)]
    pub fn with_delete_gap<F>(mut self, gap: F) -> Self
    where
        F: Fn() + Send + Sync + 'static,
    {
        self.delete_gap = Some(Arc::new(gap));
        self
    }

    #[doc(hidden)]
    pub fn with_save_gap<F>(mut self, gap: F) -> Self
    where
        F: Fn() + Send + Sync + 'static,
    {
        self.save_gap = Some(Arc::new(gap));
        self
    }

    #[doc(hidden)]
    pub fn with_open_gap<F>(mut self, gap: F) -> Self
    where
        F: Fn() + Send + Sync + 'static,
    {
        self.open_gap = Some(Arc::new(gap));
        self
    }

    /// Toma el agente y deja el turno listo para correr. El chequeo de
    /// `busy||setup` y la marca de ocupado ocurren bajo el mismo candado: dos
    /// envíos simultáneos no pueden pasar los dos (`agentArrancar`,
    /// `agent.go:927-944`).
    fn preparar(&self, texto: &str, mut turno: Turno) -> Result<Preparado, AgentError> {
        let hay_imagenes = !turno.imagenes.is_empty();
        let mensaje;
        let tasa_in = turno.cost_input_per_mtok;
        let tasa_out = turno.cost_output_per_mtok;
        let (generacion, cancel, sesion) = {
            let mut estado = lock(&self.estado);
            if estado.busy || estado.setup {
                return Err(AgentError::Busy);
            }
            mensaje = Self::validar_mensaje_con_imagenes(texto, hay_imagenes)?.to_owned();
            let antes = estado.clone();
            if estado.conversation_id.is_empty() {
                estado.conversation_id = self.conversations.new_id();
                estado.created = Some(Utc::now());
                estado.tokens_in = 0;
                estado.tokens_out = 0;
                estado.cost_usd = 0.0;
            }
            estado.history.push(Message {
                role: "user".to_owned(),
                content: mensaje.clone(),
                images: hay_imagenes.then(|| turno.imagenes.clone()),
                ..Message::default()
            });
            estado.busy = true;
            estado.error.clear();
            estado.draft.clear();
            estado.model = turno.modelo.clone();
            let cancel = CancelToken::new();
            estado.cancel = Some(cancel.clone());
            let mut sesion = Session {
                version: sessions::VERSION,
                id: estado.conversation_id.clone(),
                title: sessions::title(&estado.history),
                model: estado.model.clone(),
                created: estado.created.unwrap_or_else(Utc::now),
                updated: Utc::now(),
                messages: estado.history.clone(),
                tokens_in: estado.tokens_in,
                tokens_out: estado.tokens_out,
                cost_usd: estado.cost_usd,
                cwd: String::new(),
            };
            if !self.borrada(&sesion.id)
                && let Err(error) = self.conversations.save(&mut sesion)
            {
                *estado = antes;
                return Err(AgentError::Save(error.to_string()));
            }
            (estado.generation, cancel, sesion)
        };
        turno.mensaje = mensaje;
        let uso = Arc::new(Mutex::new((0i64, 0i64)));
        let emitir = self.emisor(generacion, Arc::clone(&uso));
        Ok(Preparado {
            turno,
            sesion,
            generacion,
            cancel,
            tasa_in,
            tasa_out,
            uso,
            emitir,
        })
    }

    /// Corre un turno que ya tomó el agente (`agentCorrer`,
    /// `agent.go:949-1001`).
    async fn correr(&self, preparado: Preparado) -> Result<(), AgentError> {
        let Preparado {
            turno,
            mut sesion,
            generacion,
            cancel,
            tasa_in,
            tasa_out,
            uso,
            emitir,
        } = preparado;
        let _libera = LiberaTurno {
            agente: self,
            generacion,
        };
        let proveedor = if turno.cerebro { "org" } else { "propio" };
        let modelo = turno.modelo.clone();
        let inicio = Instant::now();
        self.miga(
            "agent.turn",
            &format!(
                "inicio modelo={modelo} modo={} proveedor={proveedor}",
                turno.modo
            ),
        );
        let vencidas_antes = self.approvals.vencidas();
        // El bloque suelta el préstamo mutable de `sesion` antes de leer los
        // mensajes: `tokio::time::timeout` descartaría el futuro al vencer y
        // con él lo parcial, como `context.WithTimeout` no hace en Go.
        let fin = {
            // #95: el pánico del runner no se traga: se vuelve error visible
            // y el guardado de abajo sigue ocurriendo con lo que haya.
            let carrera = AtrapaPanico(self.runner.run(turno, &mut sesion, cancel.clone(), emitir));
            tokio::pin!(carrera);
            tokio::select! {
                resultado = &mut carrera => match resultado {
                    Ok(resultado) => Fin::Turno(resultado),
                    Err(panico) => Fin::Panico(panico),
                },
                () = tokio::time::sleep(self.turn_timeout) => {
                    let esperando_aprobacion = !self.approvals.pending().is_empty();
                    cancel.cancelar();
                    // Margen para que el runner devuelva `sesion.messages` con
                    // lo parcial. Si no termina, se descarta el futuro y el
                    // turno sigue siendo un tiempo agotado.
                    let _ = tokio::time::timeout(PLAZO_DE_GRACIA, &mut carrera).await;
                    Fin::Tiempo { esperando_aprobacion }
                }
            }
        };
        let aprobacion_vencida = self.approvals.vencidas() > vencidas_antes;
        let reporte = reporte_del_fin(&fin, &cancel, aprobacion_vencida);
        let error_turno = error_del_turno(fin, &cancel, aprobacion_vencida);
        let (prompt, completion) = {
            let uso = lock(&uso);
            *uso
        };
        let transcurrido_ms = u64::try_from(inicio.elapsed().as_millis()).unwrap_or(u64::MAX);
        match (&reporte, &error_turno) {
            (Some(reporte), _) => {
                let extra = {
                    let estado = lock(&self.estado);
                    json!({
                        "kind": reporte.kind,
                        "status": reporte.status,
                        "provider": proveedor,
                        "model": modelo,
                        "steps": estado.steps.len(),
                        "last_tools": ultimas_herramientas(&estado.steps),
                        "history": estado.history.len(),
                        "usage_in": prompt,
                        "usage_out": completion,
                        "elapsed_ms": transcurrido_ms,
                    })
                };
                self.miga(
                    "agent.turn",
                    &format!("falló ({}) tras {transcurrido_ms} ms", reporte.kind),
                );
                if let Some(telemetry) = &self.telemetry {
                    telemetry.report(reporte.categoria, reporte.nivel, &reporte.mensaje, extra);
                }
            }
            (None, Some(error)) => {
                self.miga("agent.turn", &format!("terminó sin reporte: {error}"));
            }
            (None, None) => {
                self.miga("agent.turn", &format!("ok en {transcurrido_ms} ms"));
            }
        }
        let para_guardar = {
            let mut estado = lock(&self.estado);
            if estado.generation != generacion {
                let cancelado = cancel.esta_cancelado();
                drop(estado);
                return if cancelado {
                    Err(AgentError::Cancelled)
                } else {
                    Ok(())
                };
            }
            estado.history = recortar_historial(std::mem::take(&mut sesion.messages));
            estado.cost_usd += costo_estimado(prompt, completion, tasa_in, tasa_out);
            estado.cancel = None;
            estado.draft.clear();
            estado.error = error_turno
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default();
            if self.borrada(&estado.conversation_id) {
                reiniciar(&mut estado);
                None
            } else {
                sesion_actual(&estado)
            }
        };
        if let Some(mut sesion) = para_guardar {
            if let Some(gap) = &self.save_gap {
                gap();
            }
            if let Err(error) = self.conversations.save(&mut sesion) {
                let mut estado = lock(&self.estado);
                if let Some(telemetry) = &self.telemetry {
                    telemetry.report(
                        "agent.save",
                        "error",
                        &format!("no se pudo guardar la conversación: {error}"),
                        serde_json::Value::Null,
                    );
                }
                if estado.generation == generacion {
                    let aviso = format!("no se pudo guardar la conversación: {error}");
                    estado.error = if estado.error.is_empty() {
                        aviso
                    } else {
                        format!("{} · {aviso}", estado.error)
                    };
                }
            }
            if lock(&self.estado).generation != generacion {
                let _ = self.conversations.delete(&sesion.id);
            }
        }
        let vigente = {
            let mut estado = lock(&self.estado);
            if estado.generation == generacion {
                estado.busy = false;
                estado.cancel = None;
            }
            estado.generation == generacion
        };
        if vigente && let Some(on_finish) = &self.on_finish {
            on_finish(
                generacion,
                prompt.saturating_add(completion),
                error_turno.as_ref().map(ToString::to_string),
            );
        }
        match error_turno {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// Manda un texto y espera el turno (`AgentSend`/`agentArrancar`/
    /// `agentCorrer`, `agent.go:539-691,927-1001`).
    pub async fn send(&self, texto: &str, turno: Turno) -> Result<(), AgentError> {
        let preparado = self.preparar(texto, turno)?;
        self.correr(preparado).await
    }

    /// Reserva el agente y lanza el turno sin esperarlo. La ruta responde 200
    /// solo después de que `preparar` marcó `busy` bajo el candado: si otro
    /// envío ganó, devuelve `Busy` y la ruta lo pinta 409 (`agentArrancar`,
    /// `agent.go:927-944`).
    pub fn empezar(self: &Arc<Self>, texto: &str, turno: Turno) -> Result<(), AgentError> {
        self.empezar_con(texto, turno, |turno| Box::pin(async move { Ok(turno) }))
    }

    pub fn empezar_con(
        self: &Arc<Self>,
        texto: &str,
        turno: Turno,
        preparar: impl FnOnce(Turno) -> Pin<Box<dyn Future<Output = Result<Turno, AgentError>> + Send>>
        + Send
        + 'static,
    ) -> Result<(), AgentError> {
        let mut preparado = self.preparar(texto, turno)?;
        let agente = Arc::clone(self);
        tokio::spawn(async move {
            let generacion = preparado.generacion;
            let _libera = LiberaTurno {
                agente: &agente,
                generacion,
            };
            let cancel = preparado.cancel.clone();
            let resultado = tokio::select! {
                resultado = preparar(preparado.turno) => resultado,
                () = cancel.esperar() => Err(AgentError::Cancelled),
            };
            match resultado {
                Ok(turno) => {
                    preparado.turno = turno;
                    let _ = agente.correr(preparado).await;
                }
                Err(error) => agente.fallar_preparacion(generacion, error),
            }
        });
        Ok(())
    }

    fn fallar_preparacion(&self, generacion: u64, error: AgentError) {
        let texto = error.to_string();
        self.miga("agent.turn", &format!("preparación falló: {texto}"));
        if let Some(telemetry) = &self.telemetry
            && matches!(
                error,
                AgentError::Turno(_)
                    | AgentError::Save(_)
                    | AgentError::Conversaciones(_)
                    | AgentError::Sesion(_)
            )
            && !matches!(
                error,
                AgentError::Sesion(crate::sesion::SessionError::LoggedOut)
            )
        {
            telemetry.report(
                "agent.setup",
                "error",
                &texto,
                json!({ "phase": "prepare" }),
            );
        }
        let vigente = {
            let mut estado = lock(&self.estado);
            if estado.generation != generacion {
                return;
            }
            estado.error = texto.clone();
            estado.busy = false;
            estado.cancel = None;
            true
        };
        if vigente && let Some(on_finish) = &self.on_finish {
            on_finish(generacion, 0, Some(texto));
        }
    }

    fn emisor(&self, generacion: u64, uso: Arc<Mutex<(i64, i64)>>) -> Emisor {
        let estado = Arc::clone(&self.estado);
        let telemetry = self.telemetry.clone();
        Arc::new(move |evento| {
            let mut estado = lock(&estado);
            if estado.generation != generacion {
                return;
            }
            let miga = |texto: String| {
                if let Some(telemetry) = &telemetry {
                    telemetry.breadcrumb("agent.tool", &texto);
                }
            };
            match evento {
                Event::Delta { text } => {
                    if estado.draft.len() < MAX_BORRADOR {
                        estado.draft.push_str(&text);
                    }
                }
                Event::Answer { text } => {
                    estado.draft.clear();
                    agregar_paso(&mut estado, "answer", String::new(), text);
                }
                Event::ToolCall { tool, args } => {
                    miga(format!("{tool} solicitada"));
                    agregar_paso(&mut estado, "tool_call", tool, args);
                }
                Event::ToolResult { tool, text } => {
                    miga(format!(
                        "{tool} respondió ({} caracteres)",
                        text.chars().count()
                    ));
                    agregar_paso(&mut estado, "tool_result", tool, text);
                }
                Event::Denied { tool } => {
                    miga(format!("{tool} denegada"));
                    agregar_paso(&mut estado, "denied", tool, String::new());
                }
                Event::Usage {
                    prompt_tokens,
                    completion_tokens,
                    ..
                } => {
                    let prompt = tokens_i64(prompt_tokens);
                    let completion = tokens_i64(completion_tokens);
                    estado.tokens_in = estado.tokens_in.saturating_add(prompt);
                    estado.tokens_out = estado.tokens_out.saturating_add(completion);
                    let mut uso = lock(&uso);
                    uso.0 = uso.0.saturating_add(prompt);
                    uso.1 = uso.1.saturating_add(completion);
                }
                Event::Start { .. } | Event::Done { .. } | Event::Error { .. } => {}
            }
        })
    }

    /// Corta el turno en curso (`AgentCancel`, `agent.go:695-701`).
    pub fn cancel(&self) {
        let cancel = lock(&self.estado).cancel.clone();
        if let Some(cancel) = cancel {
            cancel.cancelar();
        }
    }

    /// Cierre de sesión: corta, sube la generación y borra todo
    /// (`agentLimpiar`, `agent.go:889-925`).
    pub fn clear(&self) {
        {
            let mut estado = lock(&self.estado);
            if let Some(cancel) = &estado.cancel {
                cancel.cancelar();
            }
            estado.generation = estado.generation.wrapping_add(1);
            estado.busy = false;
            reiniciar(&mut estado);
        }
        self.approvals.cancel_all();
    }

    /// Borra la conversación sin tocar la generación (`AgentReset`,
    /// `agent.go:878-887`).
    pub fn reset(&self) -> Result<(), AgentError> {
        let mut estado = lock(&self.estado);
        if estado.busy {
            return Err(AgentError::Busy);
        }
        reiniciar(&mut estado);
        Ok(())
    }

    /// Reinicia solo si no arrancó un turno mientras se borraba en disco. El
    /// primer chequeo de `busy` de `delete_conversation` no basta: suelta el
    /// candado, y el timbre puede tomar el agente en ese hueco (`agent.go:862-876`).
    pub(crate) fn reset_sin_chequeo(&self) {
        let mut estado = lock(&self.estado);
        if estado.busy {
            return;
        }
        reiniciar(&mut estado);
    }

    /// Reinicia solo si la conversación abierta sigue siendo `id` y ningún
    /// turno la tomó mientras se borraba. El candado se toma una vez para que
    /// un `open` o un `empezar` no se cuelen entre la comparación y el reinicio
    /// (`conversations.go:112-116`).
    pub(crate) fn reset_if_current(&self, id: &str) {
        let mut estado = lock(&self.estado);
        if estado.busy {
            return;
        }
        if estado.conversation_id == id {
            reiniciar(&mut estado);
        }
    }

    /// Toma el agente para una operación exclusiva, como deshacer. `send` la
    /// respeta y devuelve `Busy`; el `Drop` de [`Reserva`] la suelta
    /// (`reservarAgente`/`liberarAgente`, `agent.go:249-266`).
    pub(crate) fn reservar(&self) -> Result<Reserva<'_>, AgentError> {
        let mut estado = lock(&self.estado);
        if estado.busy || estado.setup {
            return Err(AgentError::Busy);
        }
        estado.setup = true;
        Ok(Reserva { agent: self })
    }

    /// Guarda la foto actual; sin id o historial no hay nada que guardar
    /// (`conversations.go:191-230`).
    pub fn save(&self) -> Result<(), AgentError> {
        let mut sesion = {
            let estado = lock(&self.estado);
            if self.borrada(&estado.conversation_id) {
                None
            } else {
                sesion_actual(&estado)
            }
        };
        match sesion.as_mut() {
            Some(sesion) => self
                .conversations
                .save(sesion)
                .map_err(|error| AgentError::Save(error.to_string())),
            None => Ok(()),
        }
    }

    /// Una conversación borrada durante su turno no se vuelve a guardar.
    pub(crate) fn borrada(&self, id: &str) -> bool {
        !id.is_empty() && lock(&self.borradas).contains(id)
    }

    pub fn busy(&self) -> bool {
        lock(&self.estado).busy
    }

    /// `busy || setup`: una reserva exclusiva también cuenta como ejecución en
    /// curso. La ruta de envío lo mira antes de pedir red (`agent.go:545-552`).
    pub fn ocupado(&self) -> bool {
        let estado = lock(&self.estado);
        estado.busy || estado.setup
    }

    pub fn history(&self) -> Vec<Message> {
        lock(&self.estado).history.clone()
    }

    /// El historial que consume la vista en cada sondeo. Costura propia para
    /// no clonar el base64 completo de las imágenes que ya tienen miniatura
    /// (#95); `history` conserva el archivo completo para el proveedor.
    pub(crate) fn history_vista(&self) -> Vec<Message> {
        let estado = lock(&self.estado);
        estado
            .history
            .iter()
            .map(|mensaje| Message {
                role: mensaje.role.clone(),
                content: mensaje.content.clone(),
                images: mensaje.images.as_ref().map(|imagenes| {
                    imagenes
                        .iter()
                        .map(|imagen| Image {
                            // Con miniatura, la ventana pinta la miniatura:
                            // el base64 completo no se clona en cada sondeo.
                            data: if imagen.preview.is_empty() {
                                imagen.data.clone()
                            } else {
                                String::new()
                            },
                            mime: imagen.mime.clone(),
                            preview: imagen.preview.clone(),
                        })
                        .collect()
                }),
                tool_calls: mensaje.tool_calls.clone(),
                tool_call_id: mensaje.tool_call_id.clone(),
            })
            .collect()
    }

    pub fn steps(&self) -> Vec<AgentStep> {
        lock(&self.estado).steps.clone()
    }

    pub fn tokens(&self) -> (i64, i64) {
        let estado = lock(&self.estado);
        (estado.tokens_in, estado.tokens_out)
    }

    pub fn cost_usd(&self) -> f64 {
        lock(&self.estado).cost_usd
    }

    pub fn error(&self) -> String {
        lock(&self.estado).error.clone()
    }

    pub fn draft(&self) -> String {
        lock(&self.estado).draft.clone()
    }

    pub fn conversation_id(&self) -> Option<String> {
        let estado = lock(&self.estado);
        if estado.conversation_id.is_empty() {
            None
        } else {
            Some(estado.conversation_id.clone())
        }
    }

    pub fn created(&self) -> Option<DateTime<Utc>> {
        lock(&self.estado).created
    }

    pub fn generation(&self) -> u64 {
        lock(&self.estado).generation
    }

    pub fn approvals(&self) -> Arc<ApprovalQueue> {
        Arc::clone(&self.approvals)
    }
}

enum Fin {
    Turno(Result<(), ErrorTurno>),
    Tiempo { esperando_aprobacion: bool },
    Panico(String),
}

/// Lo que se manda a telemetría cuando un turno falla por una causa que la
/// persona no provocó (cancelar o dejar vencer una aprobación no cuentan).
struct ReporteTurno {
    categoria: &'static str,
    nivel: &'static str,
    kind: &'static str,
    status: Option<u16>,
    mensaje: String,
}

fn reporte_de_proveedor(error: &ProviderError, mensaje: String) -> ReporteTurno {
    let kind = match error.status {
        401 | 403 => "auth",
        429 => "rate_limit",
        500.. => "server",
        _ if error.es_sin_respuesta() => "empty_response",
        _ if error.source.is_some() => "transport",
        0 => "local",
        _ => "client",
    };
    ReporteTurno {
        categoria: "agent.chat",
        nivel: "error",
        kind,
        status: (error.status != 0).then_some(error.status),
        mensaje,
    }
}

fn reporte_del_error(error: &ErrorTurno) -> Option<ReporteTurno> {
    let simple = |categoria, nivel, kind| {
        Some(ReporteTurno {
            categoria,
            nivel,
            kind,
            status: None,
            mensaje: error.to_string(),
        })
    };
    match error {
        ErrorTurno::Proveedor(proveedor)
        | ErrorTurno::Turno(RunnerError::Proveedor(proveedor))
        | ErrorTurno::TurnoYSesiones {
            turno: RunnerError::Proveedor(proveedor),
            ..
        } => Some(reporte_de_proveedor(proveedor, error.to_string())),
        ErrorTurno::Turno(RunnerError::Cancelado) => None,
        ErrorTurno::Turno(
            RunnerError::TopeDePasos
            | RunnerError::VentanaLlena { .. }
            | RunnerError::PresupuestoAgotado { .. },
        ) => simple("agent.limit", "warning", "limit"),
        ErrorTurno::Herramientas(_) => simple("agent.tools", "error", "tools"),
        ErrorTurno::Sesiones(_) | ErrorTurno::TurnoYSesiones { .. } | ErrorTurno::Diario(_) => {
            simple("agent.session", "error", "session")
        }
        ErrorTurno::Instrucciones(_) | ErrorTurno::ModoDesconocido(_) => {
            simple("agent.setup", "error", "setup")
        }
    }
}

fn reporte_del_fin(
    fin: &Fin,
    cancel: &CancelToken,
    aprobacion_vencida: bool,
) -> Option<ReporteTurno> {
    if aprobacion_vencida {
        return None;
    }
    match fin {
        Fin::Panico(panico) => Some(ReporteTurno {
            categoria: "agent.panic",
            nivel: "error",
            kind: "panic",
            status: None,
            mensaje: format!("el turno entró en pánico: {panico}"),
        }),
        Fin::Tiempo {
            esperando_aprobacion: true,
        } => None,
        Fin::Tiempo { .. } => Some(ReporteTurno {
            categoria: "agent.timeout",
            nivel: "error",
            kind: "timeout",
            status: None,
            mensaje: "el turno del agente superó su plazo".to_owned(),
        }),
        Fin::Turno(Ok(())) => None,
        Fin::Turno(Err(_)) if cancel.esta_cancelado() => None,
        Fin::Turno(Err(error)) => reporte_del_error(error),
    }
}

fn ultimas_herramientas(pasos: &[AgentStep]) -> Vec<&str> {
    let mut nombres: Vec<&str> = pasos
        .iter()
        .rev()
        .filter(|paso| paso.kind == "tool_call")
        .take(5)
        .map(|paso| paso.tool.as_str())
        .collect();
    nombres.reverse();
    nombres
}

fn error_del_turno(fin: Fin, cancel: &CancelToken, aprobacion_vencida: bool) -> Option<AgentError> {
    match fin {
        // El pánico gana: es el fallo real del turno (#95).
        Fin::Panico(panico) => Some(AgentError::Turno(format!(
            "el turno entró en pánico: {panico}"
        ))),
        // Si el turno vence esperando una tarjeta sin respuesta, el error
        // nombra la aprobación, no al agente (#95).
        Fin::Tiempo {
            esperando_aprobacion,
        } => Some(if esperando_aprobacion || aprobacion_vencida {
            AgentError::Turno(APROBACION_VENCIDA.to_owned())
        } else {
            AgentError::TimedOut
        }),
        Fin::Turno(Ok(())) => {
            if aprobacion_vencida {
                Some(AgentError::Turno(APROBACION_VENCIDA.to_owned()))
            } else {
                cancel.esta_cancelado().then_some(AgentError::Cancelled)
            }
        }
        Fin::Turno(Err(error)) => Some(if aprobacion_vencida {
            AgentError::Turno(APROBACION_VENCIDA.to_owned())
        } else if cancel.esta_cancelado() {
            AgentError::Cancelled
        } else {
            match &error {
                ErrorTurno::Turno(RunnerError::Cancelado) => AgentError::Cancelled,
                _ => AgentError::Turno(error.to_string()),
            }
        }),
    }
}

/// Envuelve el futuro del runner para que un pánico no se trague: el unwind
/// se convierte en un error visible y el guardado del turno sigue ocurriendo
/// (#95). Tras el pánico el futuro no se vuelve a poller.
struct AtrapaPanico<F>(F);

impl<F: Future> Future for AtrapaPanico<F> {
    type Output = Result<F::Output, String>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // SAFETY: proyección pin: el futuro interno queda pineado en el sitio
        // y nunca se mueve fuera de esta envoltura.
        let futuro = unsafe { self.map_unchecked_mut(|envoltura| &mut envoltura.0) };
        match std::panic::catch_unwind(AssertUnwindSafe(|| futuro.poll(cx))) {
            Ok(Poll::Pending) => Poll::Pending,
            Ok(Poll::Ready(valor)) => Poll::Ready(Ok(valor)),
            Err(payload) => Poll::Ready(Err(texto_de_panico(payload.as_ref()))),
        }
    }
}

fn texto_de_panico(payload: &(dyn Any + Send)) -> String {
    if let Some(texto) = payload.downcast_ref::<&str>() {
        (*texto).to_owned()
    } else if let Some(texto) = payload.downcast_ref::<String>() {
        texto.clone()
    } else {
        "pánico sin mensaje".to_owned()
    }
}

fn reiniciar(estado: &mut EstadoAgente) {
    estado.conversation_id.clear();
    estado.created = None;
    estado.history.clear();
    estado.tokens_in = 0;
    estado.tokens_out = 0;
    estado.cost_usd = 0.0;
    estado.steps.clear();
    estado.error.clear();
    estado.draft.clear();
    estado.cancel = None;
}

fn sesion_actual(estado: &EstadoAgente) -> Option<Session> {
    if estado.conversation_id.is_empty() || estado.history.is_empty() {
        return None;
    }
    Some(Session {
        version: sessions::VERSION,
        id: estado.conversation_id.clone(),
        title: sessions::title(&estado.history),
        model: estado.model.clone(),
        created: estado.created.unwrap_or_else(Utc::now),
        updated: Utc::now(),
        messages: estado.history.clone(),
        tokens_in: estado.tokens_in,
        tokens_out: estado.tokens_out,
        cost_usd: estado.cost_usd,
        cwd: String::new(),
    })
}

/// Deja los últimos 40 mensajes; conserva el prefijo `system` y no arranca en
/// un `tool` huérfano (`agent.go:1076-1100`).
fn recortar_historial(mensajes: Vec<Message>) -> Vec<Message> {
    if mensajes.len() <= MAX_HISTORIAL {
        return mensajes;
    }
    let prefijo = mensajes
        .iter()
        .take_while(|mensaje| mensaje.role == "system")
        .count();
    let mut corte = mensajes.len() - MAX_HISTORIAL;
    if corte < prefijo {
        corte = prefijo;
    }
    while corte < mensajes.len() && mensajes[corte].role == "tool" {
        corte += 1;
    }
    let mut salida = Vec::with_capacity(prefijo + mensajes.len() - corte);
    salida.extend_from_slice(&mensajes[..prefijo]);
    salida.extend_from_slice(&mensajes[corte..]);
    salida
}

fn agregar_paso(estado: &mut EstadoAgente, kind: &str, tool: String, text: String) {
    estado.steps.push(AgentStep {
        at: Utc::now(),
        kind: kind.to_owned(),
        tool,
        text: vista::recortar_paso(&text),
    });
    if estado.steps.len() > MAX_PASOS {
        let sobra = estado.steps.len() - MAX_PASOS;
        estado.steps.drain(..sobra);
    }
}

fn tokens_i64(tokens: Option<u64>) -> i64 {
    tokens
        .and_then(|tokens| i64::try_from(tokens).ok())
        .unwrap_or(0)
}

/// Libera la reserva del agente al salir de `agent_undo` aunque haya error.
pub(crate) struct Reserva<'a> {
    agent: &'a Agent,
}

impl Drop for Reserva<'_> {
    fn drop(&mut self) {
        lock(&self.agent.estado).setup = false;
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use async_trait::async_trait;

    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use orus_core::Image;
    use orus_harness::Toolset;

    use super::imagenes::{MAX_IMAGEN_BYTES, MAX_IMAGENES, MAX_PREVIEW_BYTES};
    use super::validacion::host_de;
    use super::*;

    struct Nunca;

    #[async_trait]
    impl TurnRunner for Nunca {
        async fn run(
            &self,
            _: Turno,
            _: &mut Session,
            _: CancelToken,
            _: Emisor,
        ) -> Result<(), ErrorTurno> {
            Ok(())
        }
    }

    #[test]
    fn la_reserva_rechaza_la_segunda_y_el_drop_la_suelta() {
        let agent = Agent::new(
            Conversations::new(),
            Arc::new(ApprovalQueue::new()),
            Arc::new(Nunca),
        );

        let primera = agent.reservar().expect("la primera reserva entra");
        assert!(
            matches!(agent.reservar(), Err(AgentError::Busy)),
            "la segunda reserva tiene que dar Busy"
        );

        drop(primera);
        let segunda = agent
            .reservar()
            .expect("el Drop de la reserva tiene que soltarla");
        drop(segunda);
    }

    #[test]
    fn agent_probe_omite_reply_y_error_vacios() {
        let vacio = serde_json::to_value(AgentProbe::default()).expect("el probe serializa");
        assert_eq!(
            vacio,
            serde_json::json!({"ok": false, "ms": 0}),
            "ok y ms viajan siempre; reply y error vacíos no"
        );

        let lleno = serde_json::to_value(AgentProbe {
            ok: true,
            ms: 12,
            reply: "ok".to_owned(),
            error: String::new(),
        })
        .expect("el probe lleno serializa");
        assert_eq!(lleno["reply"], "ok");
        assert!(
            lleno.get("error").is_none(),
            "error vacío no debe aparecer: {lleno}"
        );
    }

    #[test]
    fn host_de_conserva_puerto_y_minusculas() {
        assert_eq!(host_de("https://API.Ejemplo:443/v1"), "api.ejemplo:443");
        assert_eq!(
            host_de("http://usuario:clave@127.0.0.1:8080/v1"),
            "127.0.0.1:8080"
        );
        assert_eq!(host_de("api.ejemplo/v1"), "");
        assert_eq!(host_de(""), "");
        assert_eq!(
            host_de("https://evil.example\\@good.example/v1"),
            "evil.example:443",
            "la barra invertida no puede esconder el host detrás de un usuario aparente"
        );
    }

    #[test]
    fn agent_input_ausente_y_null_valen_cero() {
        let vacio: AgentInput = serde_json::from_str("{}").expect("{} es un AgentInput");
        assert_eq!(vacio, AgentInput::default());

        let solo_modelo: AgentInput =
            serde_json::from_str(r#"{"model":null}"#).expect("null vale cero");
        assert!(solo_modelo.model.is_empty());

        let nulo: AgentInput = serde_json::from_str(
            r#"{
                "kind": null, "base_url": null, "model": null, "mode": null,
                "api_key": null, "folders": null, "programs": null,
                "wake_enabled": null, "wake_per_minute": null,
                "wake_per_day": null, "wake_tokens_per_day": null,
                "voice_stt_url": null, "voice_stt_model": null,
                "voice_tts_url": null, "voice_tts_model": null,
                "voice_tts_voice": null
            }"#,
        )
        .expect("todos los campos aceptan null");
        assert_eq!(nulo, AgentInput::default());
    }

    /// PNG de 1x1 transparente, el archivo real más pequeño que se puede mandar
    /// (`agent_images_test.go:13`).
    const PNG_MINIMO: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAFgwJ/lU7c5wAAAABJRU5ErkJggg==";

    fn error_de_imagenes(resultado: Result<Vec<Image>, AgentError>) -> String {
        match resultado {
            Err(AgentError::Invalid(texto)) => texto,
            otro => panic!("se esperaba Invalid, llegó {otro:?}"),
        }
    }

    fn imagen_png() -> AgentImage {
        AgentImage {
            mime: "image/png".to_owned(),
            data: PNG_MINIMO.to_owned(),
            preview: String::new(),
        }
    }

    /// `relleno(n)` del Go: la firma PNG más `n` bytes de relleno.
    fn relleno(n: usize) -> String {
        let mut datos = b"\x89PNG\r\n\x1a\n".to_vec();
        datos.resize(datos.len() + n, 0);
        STANDARD.encode(datos)
    }

    #[test]
    fn normalizar_imagenes_valida_el_contenido() {
        let imagenes = normalizar_imagenes(vec![imagen_png()]).expect("la imagen válida entra");
        assert_eq!(
            imagenes,
            vec![Image {
                mime: "image/png".to_owned(),
                data: PNG_MINIMO.to_owned(),
                preview: String::new(),
            }]
        );

        assert_eq!(
            error_de_imagenes(normalizar_imagenes(vec![AgentImage {
                mime: "image/png".to_owned(),
                data: STANDARD.encode(b"no soy un png"),
                preview: String::new(),
            }])),
            "el archivo no es la imagen que dice ser",
            "un archivo que dice ser PNG pero no lo es debe rechazarse"
        );
        assert_eq!(
            error_de_imagenes(normalizar_imagenes(vec![AgentImage {
                mime: "image/tiff".to_owned(),
                data: PNG_MINIMO.to_owned(),
                preview: String::new(),
            }])),
            "la imagen debe ser PNG, JPEG, GIF o WebP",
            "un tipo que los proveedores no hablan debe rechazarse"
        );

        let previa = format!("data:image/png;base64,{PNG_MINIMO}");
        let con_previa = normalizar_imagenes(vec![AgentImage {
            preview: previa.clone(),
            ..imagen_png()
        }])
        .expect("la miniatura válida entra");
        assert_eq!(con_previa[0].preview, previa);
        assert_eq!(
            error_de_imagenes(normalizar_imagenes(vec![AgentImage {
                preview: "data:text/html;base64,PGI+aG9sYTwvYj4=".to_owned(),
                ..imagen_png()
            }])),
            "la vista previa de la imagen no es válida",
            "una vista previa que no es imagen debe rechazarse"
        );
        assert_eq!(
            error_de_imagenes(normalizar_imagenes(vec![imagen_png(); MAX_IMAGENES + 1])),
            "puedes adjuntar hasta 4 imágenes por mensaje",
            "más de cuatro imágenes debe rechazarse"
        );

        // Los topes de peso se miden sobre los bytes decodificados, no sobre el
        // base64: si se colara una imagen de 3 MB, el proveedor cobraría el
        // turno entero y el cuerpo HTTP podría pasar del límite.
        assert_eq!(
            error_de_imagenes(normalizar_imagenes(vec![AgentImage {
                data: relleno(MAX_IMAGEN_BYTES + 1),
                ..imagen_png()
            }])),
            "cada imagen puede pesar hasta 2 MB después de comprimirla"
        );
        let cuatro = vec![
            AgentImage {
                data: relleno(1_700_000),
                ..imagen_png()
            };
            4
        ];
        let error = error_de_imagenes(normalizar_imagenes(cuatro));
        assert!(
            error.contains("6 MB"),
            "cuatro imágenes de 1,7 MB deben pasar de 6 MB en total: {error}"
        );
    }

    #[test]
    fn normalizar_imagenes_bordes_de_base64_y_miniatura() {
        assert_eq!(
            normalizar_imagenes(Vec::new()).expect("sin imágenes no hay nada que validar"),
            Vec::new()
        );
        assert_eq!(
            error_de_imagenes(normalizar_imagenes(vec![AgentImage {
                data: String::new(),
                ..imagen_png()
            }])),
            "la imagen no llegó completa; vuelve a adjuntarla"
        );
        assert_eq!(
            error_de_imagenes(normalizar_imagenes(vec![AgentImage {
                data: "no es base64".to_owned(),
                ..imagen_png()
            }])),
            "la imagen no llegó completa; vuelve a adjuntarla"
        );
        assert_eq!(
            error_de_imagenes(normalizar_imagenes(vec![AgentImage {
                data: relleno(0),
                preview: format!("data:image/png;base64,{}", "A".repeat(MAX_PREVIEW_BYTES)),
                ..imagen_png()
            }])),
            "la vista previa de la imagen es demasiado grande"
        );
        assert_eq!(
            error_de_imagenes(normalizar_imagenes(vec![AgentImage {
                data: relleno(0),
                preview: "image/png;base64,AAAA".to_owned(),
                ..imagen_png()
            }])),
            "la vista previa de la imagen no es válida"
        );
    }

    fn turno_de_prueba() -> Turno {
        Turno {
            base_url: String::new(),
            modelo: "gpt-prueba".to_owned(),
            modo: "ask".to_owned(),
            cwd: String::new(),
            folders: Vec::new(),
            cerebro: false,
            mensaje: String::new(),
            imagenes: Vec::new(),
            clave: String::new(),
            proveedor: None,
            programas: Vec::new(),
            toolset: Toolset::Full,
            max_steps: 0,
            no_agents_md: false,
            no_skills: false,
            yes: false,
            allow_exec: false,
            es_tty: false,
            memoria: None,
            org_tools: Vec::new(),
            org_note: String::new(),
            window_tokens: 0,
            cost_input_per_mtok: 0.0,
            cost_output_per_mtok: 0.0,
        }
    }

    fn agente_de_prueba() -> (Agent, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("carpeta temporal");
        let conversaciones = Conversations {
            store: sessions::Store {
                dir: dir.path().to_path_buf(),
                prefix: "c-".to_owned(),
            },
        };
        (
            Agent::new(
                conversaciones,
                Arc::new(ApprovalQueue::new()),
                Arc::new(Nunca),
            ),
            dir,
        )
    }

    /// Solo-imagen es un turno válido y el historial guarda el archivo completo
    /// (`agentArrancar`, `agent.go:927-946`).
    #[test]
    fn preparar_acepta_solo_imagen_y_la_deja_en_el_historial() {
        let imagen = Image {
            mime: "image/png".to_owned(),
            data: PNG_MINIMO.to_owned(),
            preview: format!("data:image/png;base64,{PNG_MINIMO}"),
        };
        let mut turno = turno_de_prueba();
        turno.imagenes = vec![imagen.clone()];

        let (agente, _dir) = agente_de_prueba();
        let preparado = agente
            .preparar("  ", turno)
            .expect("una imagen sin texto se puede enviar");

        assert_eq!(preparado.sesion.messages[0].content, "");
        assert_eq!(
            preparado.sesion.messages[0].images,
            Some(vec![imagen]),
            "el historial guarda la imagen normalizada"
        );
    }

    #[test]
    fn preparar_sin_texto_ni_imagenes_sigue_siendo_invalido() {
        let (agente, _dir) = agente_de_prueba();
        let error = agente
            .preparar("   ", turno_de_prueba())
            .err()
            .expect("sin texto ni imágenes no entra");
        assert!(
            matches!(&error, AgentError::Invalid(texto) if texto == "escribe un mensaje o adjunta una imagen"),
            "error = {error:?}"
        );
    }

    #[test]
    fn preparar_con_imagen_igual_rechaza_mas_de_80000_caracteres() {
        let mut turno = turno_de_prueba();
        turno.imagenes = vec![Image {
            mime: "image/png".to_owned(),
            data: PNG_MINIMO.to_owned(),
            preview: String::new(),
        }];
        let (agente, _dir) = agente_de_prueba();
        let error = agente
            .preparar(&"a".repeat(Agent::MAX_MENSAJE + 1), turno)
            .err()
            .expect("el tope de texto sigue valiendo con imágenes");
        assert!(
            matches!(&error, AgentError::Invalid(texto) if texto == "el mensaje no puede tener más de 80000 caracteres"),
            "error = {error:?}"
        );
    }

    /// #95: el sondeo de la vista no puede clonar el base64 completo de una
    /// imagen que ya tiene miniatura; el historial real sí lo conserva.
    #[test]
    fn la_vista_no_clona_el_base64_completo_cuando_hay_miniatura() {
        let (agente, _dir) = agente_de_prueba();
        let mut turno = turno_de_prueba();
        turno.imagenes = vec![Image {
            mime: "image/png".to_owned(),
            data: "A".repeat(100_000),
            preview: format!("data:image/png;base64,{PNG_MINIMO}"),
        }];
        let _preparado = agente
            .preparar("mira", turno)
            .expect("el turno con imagen entra");

        let vista = agente.history_vista();
        let imagen = &vista[0].images.as_ref().expect("la imagen viaja")[0];
        assert!(
            imagen.data.is_empty(),
            "la vista clonó el base64 completo teniendo miniatura"
        );
        assert!(!imagen.preview.is_empty(), "la miniatura se conserva");
        assert_eq!(
            agente.history()[0].images.as_ref().expect("la imagen")[0]
                .data
                .len(),
            100_000,
            "el historial real conserva el archivo completo para el proveedor"
        );
    }

    /// Sin miniatura la ventana pinta el archivo completo: la vista no puede
    /// descartarlo (#95).
    #[test]
    fn la_vista_conserva_el_archivo_sin_miniatura() {
        let (agente, _dir) = agente_de_prueba();
        let mut turno = turno_de_prueba();
        turno.imagenes = vec![Image {
            mime: "image/png".to_owned(),
            data: PNG_MINIMO.to_owned(),
            preview: String::new(),
        }];
        let _preparado = agente.preparar("mira", turno).expect("entra");

        let vista = agente.history_vista();
        assert_eq!(
            vista[0].images.as_ref().expect("la imagen viaja")[0].data,
            PNG_MINIMO,
            "sin miniatura el archivo completo sí viaja a la vista"
        );
    }

    #[test]
    fn los_pasos_en_vivo_se_recortan_como_al_reconstruir() {
        let (agente, _dir) = agente_de_prueba();
        let uso = Arc::new(Mutex::new((0i64, 0i64)));
        let emitir = agente.emisor(agente.generation(), Arc::clone(&uso));

        emitir(Event::ToolResult {
            tool: "leer".to_owned(),
            text: "x".repeat(5000),
        });

        let pasos = agente.steps();
        assert_eq!(pasos.len(), 1);
        assert!(
            pasos[0].text.chars().count() <= 2001,
            "el paso en vivo viaja completo en cada sondeo: {}",
            pasos[0].text.len()
        );
        assert!(pasos[0].text.ends_with('…'));
    }
}
