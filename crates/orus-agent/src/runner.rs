//! El bucle del agente: pregunta al modelo, acumula uso y emite los pasos.
//!
//! Las herramientas viven
//! en `runner_tools` (T2.4) y la compactación en `compact` (T2.5).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use orus_core::{Message, ToolCall};
use serde_json::json;
use thiserror::Error;
use tokio::sync::Notify;

use crate::error::{ProviderError, SIN_RESPUESTA, respuesta_vacia};
use crate::policy::{Mode, Risk};
use crate::types::{ChatReply, ChatRequest, OnDelta, Provider, Tool, ToolError, ToolSpec};

/// Pasos por defecto cuando `max_steps` no se declara.
pub const PASOS_POR_DEFECTO: usize = 12;

/// Lo que el bucle espera entre reintentos de un turno transitorio.
pub const PAUSA_REINTENTO_TURNO: Duration = Duration::from_millis(500);

/// Cuántas veces se vuelve a pedir una respuesta cuando el proveedor
/// contesta en blanco justo después de los resultados de una herramienta.
const REINTENTOS_SIN_RESPUESTA: usize = 2;

/// Aviso efímero para ese reintento; no se guarda en el historial.
const AVISO_CONTINUAR: &str = "(nota del programa: tu respuesta anterior llegó vacía. Usa el resultado de la herramienta: si el trabajo ya está hecho y verificado, respóndele al usuario con un resumen; si falta algo, pide la siguiente herramienta.)";

/// La herramienta con la que el modelo propone su plan.
pub const HERRAMIENTA_PROPOSE_PLAN: &str = "propose_plan";

const DESCRIPCION_PROPOSE_PLAN: &str = "Propón el plan de la tarea ANTES de usar otras herramientas: qué harás, paso a paso. Al aprobarlo, las lecturas y las escrituras corren sin preguntar durante el resto de esta ejecución; en modo autónomo la ejecución de programas también corre sin preguntar.";

/// Qué clase de paso es. Los strings son el contrato con el arnés.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepKind {
    ToolCall,
    ToolResult,
    Denied,
    Answer,
    /// T2.4 lo usa al resolver `propose_plan`.
    Plan,
}

impl StepKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ToolCall => "tool_call",
            Self::ToolResult => "tool_result",
            Self::Denied => "denied",
            Self::Answer => "answer",
            Self::Plan => "plan",
        }
    }
}

/// Un paso del bucle, para que la interfaz lo muestre.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub kind: StepKind,
    pub tool: String,
    pub text: String,
}

/// Lo que el proveedor cobró por la conversación, en tokens.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub prompt: i64,
    pub completion: i64,
}

impl Usage {
    pub fn total(self) -> i64 {
        self.prompt.saturating_add(self.completion)
    }
}

/// Callback de emisión de pasos. `Arc` permite compartirlo con las herramientas
/// en paralelo de T2.4.
pub type EmitFn = Arc<dyn Fn(Step) + Send + Sync>;

/// Callback de streaming de la respuesta.
pub type OnTokenFn = Arc<dyn Fn(&str) + Send + Sync>;

/// Aprobación de una herramienta. No lleva contexto porque el futuro del
/// run ya se cancela al dejar caer el proveedor, y T2.4 decide cómo esperar.
pub type ApproveFn = Arc<dyn Fn(&ToolCall) -> Result<bool, ToolError> + Send + Sync>;

/// Aprobación del plan propuesto.
pub type PlanApproveFn = Arc<dyn Fn(&str) -> Result<bool, ToolError> + Send + Sync>;

/// Regla recordada para una llamada que la política dejaría en `AskUser`.
pub type AllowRuleFn = Arc<dyn Fn(&ToolCall, Risk) -> bool + Send + Sync>;

/// Error del bucle. Son variantes para que el
/// arnés pueda distinguirlas sin comparar textos.
#[derive(Debug, Error)]
pub enum RunnerError {
    #[error("el agente llegó al tope de pasos sin dar respuesta")]
    TopeDePasos,

    #[error(
        "la ventana del modelo se llenó ({usados} de {ventana} tokens de entrada); no puedo seguir en esta conversación"
    )]
    VentanaLlena { usados: i64, ventana: i64 },

    #[error(
        "el presupuesto de tokens de la conversación ({max}) se agotó ({usados} usados); no hago el siguiente pedido"
    )]
    PresupuestoAgotado { max: i64, usados: i64 },

    /// `context.Canceled`. El bucle abandona en la espera del proveedor
    /// y en la pausa del reintento.
    #[error("la ejecución se canceló")]
    Cancelado,

    /// El turno no se completó. La respuesta parcial, si la hay, viaja en
    /// [`ProviderError::partial`].
    #[error(transparent)]
    Proveedor(#[from] ProviderError),
}

/// Cancelación de una corrida. `tokio-util`
/// no está en el grafo de dependencias (`cargo tree -p orus-agent`), así que
/// este token mínimo cumple el mismo papel: un `AtomicBool` para consultar y
/// un `Notify` para despertar a quien espera.
#[derive(Clone, Default)]
pub struct CancelToken {
    cancelado: Arc<AtomicBool>,
    aviso: Arc<Notify>,
}

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    /// Marca la corrida como cancelada y despierta a quien esté esperando. Es
    /// idempotente.
    pub fn cancelar(&self) {
        if !self.cancelado.swap(true, Ordering::SeqCst) {
            self.aviso.notify_waiters();
        }
    }

    pub fn esta_cancelado(&self) -> bool {
        self.cancelado.load(Ordering::SeqCst)
    }

    /// Espera hasta que la corrida se cancele. Cada esperador se registra con
    /// `enable` antes de mirar la marca, así que `notify_waiters` no se pierde
    /// y despierta a todos, no solo a uno.
    pub async fn esperar(&self) {
        loop {
            let aviso = self.aviso.notified();
            tokio::pin!(aviso);
            aviso.as_mut().enable();
            if self.esta_cancelado() {
                return;
            }
            aviso.await;
        }
    }
}

/// Corre el bucle: pregunta al modelo, ejecuta sus herramientas y repite hasta
/// tener respuesta.
///
/// Los campos son públicos porque el arnés arma el runner una sola vez y las
/// pruebas sustituyen el proveedor y los callbacks
pub struct Runner {
    pub provider: Arc<dyn Provider>,
    pub model: String,
    pub system: String,
    pub tools: Vec<Arc<dyn Tool>>,
    /// `None` es el valor cero: un modo desconocido que la política niega.
    pub mode: Option<Mode>,
    pub max_steps: i64,
    /// Presupuesto de la conversación en caracteres; `0` usa el de siempre.
    pub max_chars: i64,
    pub approve: Option<ApproveFn>,
    pub emit: Option<EmitFn>,
    pub allow_rule: Option<AllowRuleFn>,
    pub on_token: Option<OnTokenFn>,
    pub plan_approve: Option<PlanApproveFn>,
    pub max_tokens: i64,
    pub chat_retries: i64,
    pub window_tokens: i64,

    /// Se acumula durante [`Runner::run`].
    pub usage: Usage,

    pub(crate) uso_compartido: Option<Arc<Mutex<Usage>>>,

    /// Tokens de entrada del último pedido al modelo principal, no del resumen.
    /// `0` tras compactar o antes de medir.
    pub ultimo_prompt: i64,

    /// Vive lo que vive la corrida y se reinicia al empezar otra. T2.4 lo lee
    /// al decidir una llamada; nunca amplía un `Deny` ni abre `exec`. El `Arc`
    /// permite compartirlo con las herramientas en paralelo.
    pub(crate) plan_aprobado: Arc<AtomicBool>,

    /// Serializa las emisiones, como el mutex del contrato. Lo
    /// comparten el runner y el despachador de herramientas.
    pub(crate) emitir_mu: Arc<Mutex<()>>,
}

impl Runner {
    /// Arma un runner con los valores cero. El proveedor y el modelo son
    /// lo único obligatorio.
    pub fn new(provider: Arc<dyn Provider>, model: impl Into<String>) -> Self {
        Self {
            provider,
            model: model.into(),
            system: String::new(),
            tools: Vec::new(),
            mode: None,
            max_steps: 0,
            max_chars: 0,
            approve: None,
            emit: None,
            allow_rule: None,
            on_token: None,
            plan_approve: None,
            max_tokens: 0,
            chat_retries: 0,
            window_tokens: 0,
            usage: Usage::default(),
            uso_compartido: None,
            ultimo_prompt: 0,
            plan_aprobado: Arc::new(AtomicBool::new(false)),
            emitir_mu: Arc::new(Mutex::new(())),
        }
    }

    /// Sigue la conversación y devuelve el historial completo. No modifica el
    /// arreglo que recibe. `Run`.
    pub async fn run(
        &mut self,
        history: &[Message],
        cancel: &CancelToken,
    ) -> (Vec<Message>, Result<(), RunnerError>) {
        let pasos = if self.max_steps <= 0 {
            PASOS_POR_DEFECTO
        } else {
            usize::try_from(self.max_steps).unwrap_or(usize::MAX)
        };
        // La aprobación del plan vale para esta ejecución, no para la
        // siguiente: el Runner puede reutilizarse.
        self.plan_aprobado.store(false, Ordering::SeqCst);
        // Copia: no escribimos en el arreglo del llamador.
        let mut msgs = history.to_vec();

        for _ in 0..pasos {
            if cancel.esta_cancelado() {
                return (msgs, Err(RunnerError::Cancelado));
            }
            msgs = self.compactar(msgs, cancel).await;
            if self.window_tokens > 0
                && (self.ultimo_prompt as f64) >= 0.95 * (self.window_tokens as f64)
            {
                return (
                    msgs,
                    Err(RunnerError::VentanaLlena {
                        usados: self.ultimo_prompt,
                        ventana: self.window_tokens,
                    }),
                );
            }

            // Tras los resultados de una herramienta, un proveedor que no
            // contesta nada suele recuperarse si se le pide seguir: se
            // reintenta con un aviso que no entra al historial. Sin
            // herramientas previas, el vacío sigue siendo un error.
            let mut vacias = 0usize;
            let resultado_chat = loop {
                let resultado = if vacias == 0 {
                    self.chat_con_reintento(&msgs, cancel).await
                } else {
                    let mut con_aviso = msgs.clone();
                    con_aviso.push(Message {
                        role: "user".to_owned(),
                        content: AVISO_CONTINUAR.to_owned(),
                        ..Message::default()
                    });
                    self.chat_con_reintento(&con_aviso, cancel).await
                };
                let vacia = match &resultado {
                    Ok(respuesta) => respuesta_vacia(&respuesta.message),
                    Err(RunnerError::Proveedor(error)) => error.es_sin_respuesta(),
                    Err(_) => false,
                };
                let tras_herramienta = msgs
                    .iter()
                    .rev()
                    .find(|mensaje| mensaje.role != "user")
                    .is_some_and(|mensaje| mensaje.role == "tool");
                if vacia && tras_herramienta && vacias < REINTENTOS_SIN_RESPUESTA {
                    vacias += 1;
                    continue;
                }
                break resultado;
            };
            let respuesta = match resultado_chat {
                Ok(respuesta) => respuesta,
                // un error del proveedor corta el run antes
                // de acumular uso o emitir `answer`. La parcial que la persona
                // ya vio entra al historial para no perderla (issue #93): solo
                // el texto, porque las llamadas no se ejecutaron y dejarían
                // tool_use huérfano. El error conserva la parcial para quien
                // llama.
                Err(error) => {
                    if let RunnerError::Proveedor(error_proveedor) = &error
                        && let Some(parcial) = &error_proveedor.partial
                        && !parcial.message.content.is_empty()
                    {
                        msgs.push(Message {
                            role: "assistant".to_owned(),
                            content: parcial.message.content.clone(),
                            ..Message::default()
                        });
                    }
                    return (msgs, Err(error));
                }
            };

            let prompt = respuesta.prompt_tokens.max(0);
            self.contabilizar(prompt, respuesta.completion_tokens.max(0));
            self.ultimo_prompt = prompt;
            let mut mensaje = respuesta.message;
            crate::runner_tools::sanear_llamadas(&mut mensaje.tool_calls);
            // Un turno sin texto y sin llamadas no puede cerrarse como éxito:
            // el arnés guardaría la sesión en blanco (issue #126). El guard de
            // 200 vacío de #93 solo cubría OpenAI y Anthropic sin streaming.
            if respuesta_vacia(&mensaje) {
                return (msgs, Err(ProviderError::local(SIN_RESPUESTA).into()));
            }
            msgs.push(mensaje.clone());

            let llamadas = mensaje.tool_calls.clone().unwrap_or_default();
            if llamadas.is_empty() {
                self.emitir(Step {
                    kind: StepKind::Answer,
                    tool: String::new(),
                    text: mensaje.content,
                });
                return (msgs, Ok(()));
            }

            // El tope se aplica antes del SIGUIENTE pedido: la respuesta que ya
            // llegó se conserva y solo se evita otra llamada al proveedor.
            if self.max_tokens > 0 && self.usage.total() >= self.max_tokens {
                cerrar_llamadas_sin_ejecutar(&mut msgs, &llamadas);
                return (
                    msgs,
                    Err(RunnerError::PresupuestoAgotado {
                        max: self.max_tokens,
                        usados: self.usage.total(),
                    }),
                );
            }

            let despachador = crate::runner_tools::Despachador::desde(self);
            let (salidas, consumo_herramientas) = despachador.turno(&llamadas, cancel).await;
            // El subagente gasta tokens del mismo proveedor: sin esto el
            // presupuesto y el coste de la sesión se quedan cortos (#126).
            self.contabilizar(consumo_herramientas.prompt, consumo_herramientas.completion);
            for (llamada, salida) in llamadas.iter().zip(salidas) {
                msgs.push(Message {
                    role: "tool".to_owned(),
                    content: salida,
                    tool_call_id: llamada.id.clone(),
                    ..Message::default()
                });
            }
        }

        (msgs, Err(RunnerError::TopeDePasos))
    }

    /// Manda el turno al proveedor. Si el intento falla con un error transitorio
    /// y no se emitió ningún token, lo reintenta hasta `chat_retries` veces,
    /// esperando [`PAUSA_REINTENTO_TURNO`] entre intentos. Reintentar con texto
    /// ya mostrado duplicaría la respuesta.
    async fn chat_con_reintento(
        &self,
        msgs: &[Message],
        cancel: &CancelToken,
    ) -> Result<ChatReply, RunnerError> {
        let mut reintentos = self.chat_retries;
        loop {
            // La compactación pudo devolver el control con la corrida ya
            // cancelada: no se inicia otra petición.
            if cancel.esta_cancelado() {
                return Err(RunnerError::Cancelado);
            }
            let emitido = Arc::new(AtomicBool::new(false));
            let on_delta = self.on_token.as_ref().map(|on_token| {
                let on_token = Arc::clone(on_token);
                let emitido = Arc::clone(&emitido);
                Box::new(move |texto: &str| {
                    emitido.store(true, Ordering::SeqCst);
                    on_token(texto);
                }) as OnDelta
            });
            let req = ChatRequest {
                model: self.model.clone(),
                messages: self.con_sistema(msgs),
                tools: self.especificaciones(),
                on_delta,
            };

            let respuesta = tokio::select! {
                resultado = self.provider.chat(req) => resultado,
                () = cancel.esperar() => return Err(RunnerError::Cancelado),
            };

            match respuesta {
                Ok(respuesta) => return Ok(respuesta),
                Err(error) => {
                    if reintentos <= 0 || emitido.load(Ordering::SeqCst) || !error.is_transient() {
                        return Err(RunnerError::Proveedor(error));
                    }
                    reintentos -= 1;
                    tokio::select! {
                        () = tokio::time::sleep(PAUSA_REINTENTO_TURNO) => {}
                        () = cancel.esperar() => return Err(RunnerError::Cancelado),
                    }
                }
            }
        }
    }

    /// Compacta la conversación cuando ya no cabe: al 80 % de la ventana
    /// declarada o al presupuesto de caracteres. `compactar`
    /// vive en [`crate::compact`]; acá solo se acumula el consumo del resumen y
    /// se reinicia la medición del último prompt.
    async fn compactar(&mut self, msgs: Vec<Message>, cancel: &CancelToken) -> Vec<Message> {
        let resultado = crate::compact::compactar(
            crate::compact::Peticion {
                provider: self.provider.as_ref(),
                model: &self.model,
                window_tokens: self.window_tokens,
                max_chars: self.max_chars,
                ultimo_prompt: self.ultimo_prompt,
                cancel,
            },
            msgs,
        )
        .await;
        self.contabilizar(resultado.prompt_tokens, resultado.completion_tokens);
        if resultado.compacto {
            // El historial cambió; el próximo pedido mide de nuevo.
            self.ultimo_prompt = 0;
        }
        resultado.msgs
    }

    fn contabilizar(&mut self, prompt: i64, completion: i64) {
        self.usage.prompt = self.usage.prompt.saturating_add(prompt);
        self.usage.completion = self.usage.completion.saturating_add(completion);
        if let Some(compartido) = &self.uso_compartido {
            let mut uso = compartido
                .lock()
                .unwrap_or_else(|envenenado| envenenado.into_inner());
            uso.prompt = uso.prompt.saturating_add(prompt);
            uso.completion = uso.completion.saturating_add(completion);
        }
    }

    /// Ofrece el plan solo cuando hay quien pueda responderlo y cuando cambia
    /// algo: en `Ask` convierte varias aprobaciones en una; en autónomo no hay
    /// nada que aprobar y en solo lectura ninguna escritura puede abrirse.
    /// `especificaciones`.
    fn especificaciones(&self) -> Vec<ToolSpec> {
        let mut out: Vec<ToolSpec> = self.tools.iter().map(|tool| tool.spec()).collect();
        if self.mode == Some(Mode::Ask) && (self.plan_approve.is_some() || self.approve.is_some()) {
            out.push(especificacion_propose_plan());
        }
        out
    }

    /// Pone el mensaje de sistema delante, solo para esta llamada.
    fn con_sistema(&self, msgs: &[Message]) -> Vec<Message> {
        if self.system.is_empty() {
            return msgs.to_vec();
        }
        let mut out = Vec::with_capacity(msgs.len() + 1);
        out.push(Message {
            role: "system".to_owned(),
            content: self.system.clone(),
            ..Message::default()
        });
        out.extend_from_slice(msgs);
        out
    }

    /// Emite un paso si hay a quién. El mutex serializa las emisiones, igual
    /// que `emitir`; el despachador de herramientas usa el
    /// mismo mutex para que el orden se conserve entre tareas.
    fn emitir(&self, paso: Step) {
        crate::runner_tools::emitir(&self.emit, &self.emitir_mu, paso);
    }
}

/// Deja un resultado por cada llamada que no llegó a correr. Sin esto, el
/// historial guardado termina en una respuesta assistant con `tool_calls` y sin
/// mensajes tool; OpenAI rechaza ese turno en la ejecución siguiente.
fn cerrar_llamadas_sin_ejecutar(msgs: &mut Vec<Message>, llamadas: &[ToolCall]) {
    for llamada in llamadas {
        msgs.push(Message {
            role: "tool".to_owned(),
            content: "no se ejecutó: se agotó el presupuesto de tokens de la conversación"
                .to_owned(),
            tool_call_id: llamada.id.clone(),
            ..Message::default()
        });
    }
}

/// La especificación de `propose_plan`.
fn especificacion_propose_plan() -> ToolSpec {
    ToolSpec {
        name: HERRAMIENTA_PROPOSE_PLAN.to_owned(),
        description: DESCRIPCION_PROPOSE_PLAN.to_owned(),
        schema: json!({
            "type": "object",
            "properties": {
                "plan": {
                    "type": "string",
                    "description": "el plan, paso a paso"
                }
            },
            "required": ["plan"]
        }),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use async_trait::async_trait;
    use orus_core::{Message, ToolCall};
    use serde_json::json;

    use super::*;

    #[test]
    fn la_descripcion_de_propose_plan_no_promete_preguntar_al_ejecutar() {
        let spec = especificacion_propose_plan();
        assert!(
            spec.description
                .contains("en modo autónomo la ejecución de programas también corre sin preguntar"),
            "{}",
            spec.description
        );
        assert!(!spec.description.contains("pregunta siempre"));
    }

    /// Un paso del guion del proveedor falso.
    pub(crate) enum Paso {
        Responde(ChatReply),
        Falla(ProviderError),
        EmiteYFalla { texto: String, error: ProviderError },
        EsperaSiempre,
    }

    /// Lo que se guarda de cada petición para que los tests la inspeccionen.
    #[derive(Clone)]
    pub(crate) struct PedidoVisto {
        pub(crate) messages: Vec<Message>,
        pub(crate) tools: Vec<ToolSpec>,
        pub(crate) con_delta: bool,
    }

    /// Responde con un guion y guarda cada petición que recibe.
    pub(crate) struct ProveedorFalso {
        guion: Mutex<VecDeque<Paso>>,
        pedidos: Mutex<Vec<PedidoVisto>>,
        llamadas: AtomicUsize,
    }

    impl ProveedorFalso {
        pub(crate) fn nuevo(guion: Vec<Paso>) -> Self {
            Self {
                guion: Mutex::new(guion.into()),
                pedidos: Mutex::new(Vec::new()),
                llamadas: AtomicUsize::new(0),
            }
        }

        pub(crate) fn llamadas(&self) -> usize {
            self.llamadas.load(Ordering::SeqCst)
        }

        pub(crate) fn pedidos(&self) -> Vec<PedidoVisto> {
            self.pedidos
                .lock()
                .expect("los pedidos del proveedor falso")
                .clone()
        }
    }

    #[async_trait]
    impl Provider for ProveedorFalso {
        async fn chat(&self, req: ChatRequest) -> Result<ChatReply, ProviderError> {
            self.llamadas.fetch_add(1, Ordering::SeqCst);
            self.pedidos
                .lock()
                .expect("los pedidos del proveedor falso")
                .push(PedidoVisto {
                    messages: req.messages.clone(),
                    tools: req.tools.clone(),
                    con_delta: req.on_delta.is_some(),
                });

            let paso = self
                .guion
                .lock()
                .expect("el guion del proveedor falso")
                .pop_front()
                .unwrap_or_else(|| {
                    Paso::Falla(ProviderError::local(
                        "el proveedor falso se quedó sin respuestas",
                    ))
                });

            match paso {
                Paso::Responde(respuesta) => Ok(respuesta),
                Paso::Falla(error) => Err(error),
                Paso::EmiteYFalla { texto, error } => {
                    if let Some(on_delta) = req.on_delta.as_deref() {
                        on_delta(&texto);
                    }
                    Err(error)
                }
                Paso::EsperaSiempre => {
                    std::future::pending::<Result<ChatReply, ProviderError>>().await
                }
            }
        }
    }

    pub(crate) fn como_proveedor(proveedor: &Arc<ProveedorFalso>) -> Arc<dyn Provider> {
        proveedor.clone()
    }

    pub(crate) fn respuesta_texto(texto: &str) -> ChatReply {
        ChatReply {
            message: Message {
                role: "assistant".to_owned(),
                content: texto.to_owned(),
                ..Message::default()
            },
            prompt_tokens: 0,
            completion_tokens: 0,
        }
    }

    fn respuesta_con_uso(texto: &str, prompt: i64, completion: i64) -> ChatReply {
        ChatReply {
            prompt_tokens: prompt,
            completion_tokens: completion,
            ..respuesta_texto(texto)
        }
    }

    pub(crate) fn respuesta_herramienta(id: &str, nombre: &str) -> ChatReply {
        ChatReply {
            message: Message {
                role: "assistant".to_owned(),
                tool_calls: Some(vec![ToolCall {
                    id: id.to_owned(),
                    name: nombre.to_owned(),
                    args: json!({}),
                    invalid_args: false,
                }]),
                ..Message::default()
            },
            prompt_tokens: 0,
            completion_tokens: 0,
        }
    }

    fn respuesta_herramienta_con_uso(
        id: &str,
        nombre: &str,
        prompt: i64,
        completion: i64,
    ) -> ChatReply {
        ChatReply {
            prompt_tokens: prompt,
            completion_tokens: completion,
            ..respuesta_herramienta(id, nombre)
        }
    }

    fn pedido_usuario(texto: &str) -> Vec<Message> {
        vec![Message {
            role: "user".to_owned(),
            content: texto.to_owned(),
            ..Message::default()
        }]
    }

    fn roles(msgs: &[Message]) -> String {
        msgs.iter()
            .map(|mensaje| mensaje.role.as_str())
            .collect::<Vec<_>>()
            .join(",")
    }

    fn colector() -> (Arc<Mutex<Vec<StepKind>>>, EmitFn) {
        let pasos = Arc::new(Mutex::new(Vec::new()));
        let copia = Arc::clone(&pasos);
        let emit: EmitFn = Arc::new(move |paso| {
            copia.lock().expect("los pasos emitidos").push(paso.kind);
        });
        (pasos, emit)
    }

    #[tokio::test]
    async fn respuesta_directa() {
        let proveedor = Arc::new(ProveedorFalso::nuevo(vec![Paso::Responde(
            respuesta_texto("hola"),
        )]));
        let (pasos, emit) = colector();
        let mut runner = Runner::new(como_proveedor(&proveedor), "m");
        runner.system = "sé breve".to_owned();
        runner.mode = Some(Mode::ReadOnly);
        runner.emit = Some(emit);

        let historial = pedido_usuario("hi");
        let (hist, resultado) = runner.run(&historial, &CancelToken::new()).await;

        assert!(resultado.is_ok(), "Run falló: {resultado:?}");
        assert_eq!(roles(&hist), "user,assistant");
        assert_eq!(hist[1].content, "hola");
        assert_eq!(
            historial.len(),
            1,
            "run no debe mutar el historial del llamador"
        );
        let pedidos = proveedor.pedidos();
        assert_eq!(pedidos[0].messages[0].role, "system");
        assert_eq!(pedidos[0].messages[0].content, "sé breve");
        assert!(pedidos[0].tools.is_empty());
        assert_eq!(*pasos.lock().expect("los pasos"), vec![StepKind::Answer]);
    }

    #[tokio::test]
    async fn tope_de_pasos() {
        let proveedor = Arc::new(ProveedorFalso::nuevo(vec![
            Paso::Responde(respuesta_herramienta("c1", "nada")),
            Paso::Responde(respuesta_herramienta("c2", "nada")),
            Paso::Responde(respuesta_herramienta("c3", "nada")),
        ]));
        let mut runner = Runner::new(como_proveedor(&proveedor), "m");
        runner.mode = Some(Mode::ReadOnly);
        runner.max_steps = 2;

        let (hist, resultado) = runner
            .run(&pedido_usuario("sigue"), &CancelToken::new())
            .await;

        assert!(matches!(resultado, Err(RunnerError::TopeDePasos)));
        assert_eq!(proveedor.llamadas(), 2);
        assert_eq!(roles(&hist), "user,assistant,tool,assistant,tool");
    }

    #[tokio::test]
    async fn presupuesto_de_tokens_detiene_el_bucle() {
        let proveedor = Arc::new(ProveedorFalso::nuevo(vec![
            Paso::Responde(respuesta_herramienta_con_uso("1", "nada", 8, 4)),
            Paso::Responde(respuesta_texto("inalcanzable")),
        ]));
        let mut runner = Runner::new(como_proveedor(&proveedor), "m");
        runner.mode = Some(Mode::ReadOnly);
        runner.max_tokens = 10;

        let (hist, resultado) = runner.run(&pedido_usuario("hi"), &CancelToken::new()).await;

        let Err(RunnerError::PresupuestoAgotado { max, usados }) = resultado else {
            panic!("el presupuesto debía detener el bucle");
        };
        assert_eq!(max, 10);
        assert_eq!(usados, 12);
        assert_eq!(proveedor.llamadas(), 1);
        let ultimo = hist.last().expect("el historial no puede quedar vacío");
        assert_eq!(ultimo.role, "tool");
        assert_eq!(ultimo.tool_call_id, "1");
        assert_eq!(
            ultimo.content,
            "no se ejecutó: se agotó el presupuesto de tokens de la conversación"
        );
        assert_eq!(runner.usage.prompt, 8);
        assert_eq!(runner.usage.completion, 4);
    }

    #[tokio::test]
    async fn presupuesto_conserva_la_respuesta_final() {
        let proveedor = Arc::new(ProveedorFalso::nuevo(vec![Paso::Responde(
            respuesta_con_uso("listo", 8, 4),
        )]));
        let mut runner = Runner::new(como_proveedor(&proveedor), "m");
        runner.mode = Some(Mode::ReadOnly);
        runner.max_tokens = 10;

        let (hist, resultado) = runner.run(&pedido_usuario("hi"), &CancelToken::new()).await;

        assert!(resultado.is_ok(), "Run falló: {resultado:?}");
        assert_eq!(hist.last().expect("la respuesta final").content, "listo");
        assert_eq!(proveedor.llamadas(), 1);
        assert_eq!(runner.usage.prompt, 8);
        assert_eq!(runner.usage.completion, 4);
    }

    /// La parcial que la persona ya vio entra al historial aunque el turno
    /// falle: el texto no se pierde (issue #93). No suma uso ni emite answer.
    #[tokio::test]
    async fn error_con_parcial_conserva_el_texto_visto() {
        let proveedor = Arc::new(ProveedorFalso::nuevo(vec![Paso::Falla(
            ProviderError::local("la respuesta se cortó")
                .con_parcial(respuesta_con_uso("a medias", 8, 4)),
        )]));
        let (pasos, emit) = colector();
        let mut runner = Runner::new(como_proveedor(&proveedor), "m");
        runner.mode = Some(Mode::ReadOnly);
        runner.emit = Some(emit);

        let (hist, resultado) = runner.run(&pedido_usuario("hi"), &CancelToken::new()).await;

        match resultado {
            Err(RunnerError::Proveedor(error)) => {
                let parcial = error
                    .partial
                    .as_deref()
                    .expect("el error debe conservar la respuesta parcial");
                assert_eq!(parcial.message.content, "a medias");
            }
            otro => panic!("el corte debía devolver el error del proveedor: {otro:?}"),
        }
        assert_eq!(runner.usage.total(), 0, "la parcial no debe sumar uso");
        assert_eq!(
            roles(&hist),
            "user,assistant",
            "el texto que la persona vio entra al historial"
        );
        assert_eq!(hist[1].content, "a medias");
        assert!(
            pasos.lock().expect("los pasos").is_empty(),
            "la parcial no debe emitir un answer"
        );
    }

    /// Solo el texto visto entra: una parcial vacía no agrega mensajes y las
    /// llamadas que nunca se ejecutaron no dejan tool_use huérfano (issue #93).
    #[tokio::test]
    async fn la_parcial_sin_texto_no_ensucia_el_historial() {
        for parcial in [
            respuesta_con_uso("", 8, 4),
            ChatReply {
                message: Message {
                    role: "assistant".to_owned(),
                    tool_calls: Some(vec![ToolCall {
                        id: "c1".to_owned(),
                        name: "leer".to_owned(),
                        args: json!({"ruta": "a"}),
                        invalid_args: false,
                    }]),
                    ..Message::default()
                },
                prompt_tokens: 8,
                completion_tokens: 4,
            },
        ] {
            let proveedor = Arc::new(ProveedorFalso::nuevo(vec![Paso::Falla(
                ProviderError::local("la respuesta se cortó").con_parcial(parcial),
            )]));
            let mut runner = Runner::new(como_proveedor(&proveedor), "m");
            runner.mode = Some(Mode::ReadOnly);
            let (hist, resultado) = runner.run(&pedido_usuario("hi"), &CancelToken::new()).await;
            assert!(matches!(resultado, Err(RunnerError::Proveedor(_))));
            assert_eq!(roles(&hist), "user", "sin texto visto no entra nada");
        }

        // Con texto y llamadas, entra solo el texto: las llamadas no se ejecutaron.
        let parcial = ChatReply {
            message: Message {
                role: "assistant".to_owned(),
                content: "a medias".to_owned(),
                tool_calls: Some(vec![ToolCall {
                    id: "c1".to_owned(),
                    name: "leer".to_owned(),
                    args: json!({}),
                    invalid_args: false,
                }]),
                ..Message::default()
            },
            prompt_tokens: 8,
            completion_tokens: 4,
        };
        let proveedor = Arc::new(ProveedorFalso::nuevo(vec![Paso::Falla(
            ProviderError::local("la respuesta se cortó").con_parcial(parcial),
        )]));
        let mut runner = Runner::new(como_proveedor(&proveedor), "m");
        runner.mode = Some(Mode::ReadOnly);
        let (hist, resultado) = runner.run(&pedido_usuario("hi"), &CancelToken::new()).await;
        assert!(matches!(resultado, Err(RunnerError::Proveedor(_))));
        assert_eq!(roles(&hist), "user,assistant");
        assert_eq!(hist[1].content, "a medias");
        assert_eq!(
            hist[1].tool_calls, None,
            "las llamadas que no se ejecutaron no entran al historial"
        );
    }

    #[tokio::test]
    async fn reintenta_transitorio_sin_deltas() {
        let proveedor = Arc::new(ProveedorFalso::nuevo(vec![
            Paso::Falla(ProviderError::http(429, "espera".to_owned())),
            Paso::Responde(respuesta_texto("ok")),
        ]));
        let mut runner = Runner::new(como_proveedor(&proveedor), "m");
        runner.mode = Some(Mode::ReadOnly);
        runner.chat_retries = 1;

        let (_, resultado) = runner.run(&pedido_usuario("hi"), &CancelToken::new()).await;

        assert!(resultado.is_ok(), "Run falló: {resultado:?}");
        assert_eq!(proveedor.llamadas(), 2);
    }

    #[tokio::test]
    async fn no_reintenta_si_ya_hubo_deltas() {
        let proveedor = Arc::new(ProveedorFalso::nuevo(vec![Paso::EmiteYFalla {
            texto: "hola".to_owned(),
            error: ProviderError::http(500, String::new()),
        }]));
        let deltas = Arc::new(Mutex::new(Vec::new()));
        let deltas_emitidos = Arc::clone(&deltas);
        let mut runner = Runner::new(como_proveedor(&proveedor), "m");
        runner.mode = Some(Mode::ReadOnly);
        runner.chat_retries = 3;
        runner.on_token = Some(Arc::new(move |texto: &str| {
            deltas_emitidos
                .lock()
                .expect("los deltas")
                .push(texto.to_owned());
        }));

        let (_, resultado) = runner.run(&pedido_usuario("hi"), &CancelToken::new()).await;

        assert!(matches!(resultado, Err(RunnerError::Proveedor(_))));
        assert_eq!(proveedor.llamadas(), 1);
        assert_eq!(*deltas.lock().expect("los deltas"), vec!["hola".to_owned()]);
        assert!(proveedor.pedidos()[0].con_delta);
    }

    #[tokio::test]
    async fn reintento_espera_al_proveedor() {
        let proveedor = Arc::new(ProveedorFalso::nuevo(vec![
            Paso::Falla(ProviderError::http(429, String::new())),
            Paso::Responde(respuesta_texto("ok")),
        ]));
        let mut runner = Runner::new(como_proveedor(&proveedor), "m");
        runner.mode = Some(Mode::ReadOnly);
        runner.chat_retries = 2;

        let principio = std::time::Instant::now();
        let (_, resultado) = runner.run(&pedido_usuario("hi"), &CancelToken::new()).await;

        assert!(resultado.is_ok(), "Run falló: {resultado:?}");
        assert_eq!(proveedor.llamadas(), 2);
        assert!(
            principio.elapsed() >= PAUSA_REINTENTO_TURNO,
            "el reintento no esperó al proveedor: {:?}",
            principio.elapsed()
        );
    }

    #[tokio::test]
    async fn reintento_abandona_si_se_cancela() {
        let proveedor = Arc::new(ProveedorFalso::nuevo(vec![
            Paso::Falla(ProviderError::http(429, String::new())),
            Paso::Falla(ProviderError::http(429, String::new())),
            Paso::Falla(ProviderError::http(429, String::new())),
            Paso::Falla(ProviderError::http(429, String::new())),
        ]));
        let mut runner = Runner::new(como_proveedor(&proveedor), "m");
        runner.mode = Some(Mode::ReadOnly);
        runner.chat_retries = 3;

        let token = CancelToken::new();
        let token_hijo = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            token_hijo.cancelar();
        });

        let (_, resultado) = runner.run(&pedido_usuario("hi"), &token).await;

        assert!(matches!(resultado, Err(RunnerError::Cancelado)));
        assert_eq!(proveedor.llamadas(), 1);
    }

    #[tokio::test]
    async fn cancelar_despierta_a_todos_los_esperadores() {
        let token = CancelToken::new();
        let esperadores: Vec<_> = (0..3)
            .map(|_| {
                let token = token.clone();
                tokio::spawn(async move { token.esperar().await })
            })
            .collect();
        tokio::time::sleep(Duration::from_millis(50)).await;
        token.cancelar();
        for esperador in esperadores {
            tokio::time::timeout(Duration::from_secs(2), esperador)
                .await
                .expect("todos los esperadores despiertan")
                .expect("la tarea no falla");
        }
    }

    #[tokio::test]
    async fn abandona_durante_la_espera_del_proveedor() {
        let proveedor = Arc::new(ProveedorFalso::nuevo(vec![Paso::EsperaSiempre]));
        let mut runner = Runner::new(como_proveedor(&proveedor), "m");
        runner.mode = Some(Mode::ReadOnly);

        let token = CancelToken::new();
        let token_hijo = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            token_hijo.cancelar();
        });

        let (_, resultado) = runner.run(&pedido_usuario("hi"), &token).await;

        assert!(matches!(resultado, Err(RunnerError::Cancelado)));
        assert_eq!(proveedor.llamadas(), 1);
    }

    #[tokio::test]
    async fn ventana_llena_detiene_con_mensaje_claro() {
        let proveedor = Arc::new(ProveedorFalso::nuevo(vec![
            Paso::Responde(respuesta_herramienta_con_uso("1", "nada", 96, 4)),
            Paso::Responde(respuesta_texto("inalcanzable")),
        ]));
        let mut runner = Runner::new(como_proveedor(&proveedor), "m");
        runner.mode = Some(Mode::ReadOnly);
        runner.window_tokens = 100;

        let (_, resultado) = runner.run(&pedido_usuario("hi"), &CancelToken::new()).await;

        match resultado {
            Err(RunnerError::VentanaLlena { usados, ventana }) => {
                assert_eq!(usados, 96);
                assert_eq!(ventana, 100);
            }
            otro => panic!("la ventana llena debía detener el run: {otro:?}"),
        }
        assert_eq!(proveedor.llamadas(), 1);
        assert_eq!(
            RunnerError::VentanaLlena {
                usados: 96,
                ventana: 100
            }
            .to_string(),
            "la ventana del modelo se llenó (96 de 100 tokens de entrada); no puedo seguir en esta conversación"
        );
    }

    #[tokio::test]
    async fn ventana_dispara_la_compactacion() {
        let mut inicial = vec![Message {
            role: "user".to_owned(),
            content: "x".repeat(400),
            ..Message::default()
        }];
        for _ in 0..12 {
            inicial.push(Message {
                role: "assistant".to_owned(),
                content: "y".repeat(50),
                ..Message::default()
            });
            inicial.push(Message {
                role: "user".to_owned(),
                content: "z".repeat(50),
                ..Message::default()
            });
        }

        let proveedor = Arc::new(ProveedorFalso::nuevo(vec![
            Paso::Responde(respuesta_herramienta_con_uso("1", "nada", 80, 2)),
            Paso::Responde(respuesta_texto("resumen compacto")),
            Paso::Responde(respuesta_texto("final")),
        ]));
        let mut runner = Runner::new(como_proveedor(&proveedor), "m");
        runner.mode = Some(Mode::ReadOnly);
        runner.window_tokens = 100;

        let (hist, resultado) = runner.run(&inicial, &CancelToken::new()).await;

        assert!(resultado.is_ok(), "Run falló: {resultado:?}");
        assert_eq!(proveedor.llamadas(), 3);
        let pedidos = proveedor.pedidos();
        assert!(
            pedidos[2].messages.len() < pedidos[0].messages.len(),
            "la tercera petición debía ir compactada ({} vs {})",
            pedidos[2].messages.len(),
            pedidos[0].messages.len()
        );
        assert!(
            pedidos[2].messages[0].content.contains("Resumen"),
            "el primer mensaje tras compactar debe traer el resumen: {:?}",
            pedidos[2].messages[0].content
        );
        assert_eq!(hist.last().expect("la respuesta final").content, "final");
    }

    #[tokio::test]
    async fn omision_vieja_no_aborta_y_los_ids_duplicados_se_correlacionan() {
        fn dos_llamadas(id: &str) -> ChatReply {
            let llamada = || ToolCall {
                id: id.to_owned(),
                name: "nada".to_owned(),
                args: json!({}),
                invalid_args: false,
            };
            ChatReply {
                message: Message {
                    tool_calls: Some(vec![llamada(), llamada()]),
                    ..Message::default()
                },
                prompt_tokens: 96,
                completion_tokens: 4,
            }
        }

        let proveedor = Arc::new(ProveedorFalso::nuevo(vec![
            Paso::Falla(ProviderError::local("el resumen falló")),
            Paso::Responde(dos_llamadas("c1")),
            Paso::Falla(ProviderError::local("el resumen falló")),
            Paso::Responde(respuesta_texto("final")),
        ]));
        let mut runner = Runner::new(como_proveedor(&proveedor), "m");
        runner.mode = Some(Mode::ReadOnly);
        runner.window_tokens = 100;
        runner.ultimo_prompt = 80;
        let historial: Vec<Message> = (0..14)
            .map(|indice| Message {
                role: if indice % 2 == 0 { "user" } else { "assistant" }.to_owned(),
                content: "x".repeat(50),
                ..Message::default()
            })
            .collect();

        let (hist, resultado) = runner.run(&historial, &CancelToken::new()).await;

        assert!(resultado.is_ok(), "la medición vieja abortó: {resultado:?}");
        assert_eq!(hist.last().expect("la respuesta final").content, "final");
        let ids: Vec<&str> = hist
            .iter()
            .filter(|mensaje| mensaje.role == "tool")
            .map(|mensaje| mensaje.tool_call_id.as_str())
            .collect();
        assert_eq!(ids.len(), 2);
        assert_eq!(ids[0], "c1");
        assert_ne!(ids[1], ids[0], "dos resultados con el mismo tool_call_id");
    }

    /// Un turno sin texto y sin llamadas no se cierra en blanco: no emite
    /// `answer` y no entra al historial que se guarda (issue #126).
    #[tokio::test]
    async fn respuesta_vacia_no_cierra_el_turno() {
        for contenido in ["", "   ", "\n\t"] {
            let proveedor = Arc::new(ProveedorFalso::nuevo(vec![Paso::Responde(
                respuesta_texto(contenido),
            )]));
            let (pasos, emit) = colector();
            let mut runner = Runner::new(como_proveedor(&proveedor), "m");
            runner.mode = Some(Mode::ReadOnly);
            runner.emit = Some(emit);

            let (hist, resultado) = runner.run(&pedido_usuario("hi"), &CancelToken::new()).await;

            match resultado {
                Err(RunnerError::Proveedor(error)) => assert_eq!(
                    error.to_string(),
                    "el proveedor no devolvió ninguna respuesta"
                ),
                otro => panic!("un turno vacío debe fallar: {otro:?}"),
            }
            assert_eq!(roles(&hist), "user", "el turno vacío no entra al historial");
            assert!(
                pasos.lock().expect("los pasos").is_empty(),
                "un turno vacío no emite answer"
            );
        }
    }

    /// Tras una herramienta, un proveedor en blanco se reintenta con un aviso
    /// que no queda en el historial; si insiste, el error sigue siendo visible.
    #[tokio::test]
    async fn respuesta_vacia_tras_herramienta_se_reintenta_con_aviso() {
        let vacia = || {
            Paso::Falla(ProviderError::local(
                "el proveedor no devolvió ninguna respuesta",
            ))
        };
        let proveedor = Arc::new(ProveedorFalso::nuevo(vec![
            Paso::Responde(respuesta_herramienta("c1", "nada")),
            vacia(),
            Paso::Responde(respuesta_texto("listo")),
        ]));
        let mut runner = Runner::new(como_proveedor(&proveedor), "m");
        runner.mode = Some(Mode::ReadOnly);

        let (hist, resultado) = runner.run(&pedido_usuario("hi"), &CancelToken::new()).await;

        assert!(resultado.is_ok(), "no se recuperó del vacío: {resultado:?}");
        assert_eq!(roles(&hist), "user,assistant,tool,assistant");
        assert_eq!(proveedor.llamadas(), 3);
        let ultimo = proveedor.pedidos().pop().expect("tercer pedido");
        assert_eq!(
            ultimo.messages.last().expect("el aviso").role,
            "user",
            "el reintento debe llevar el aviso"
        );

        let insistente = Arc::new(ProveedorFalso::nuevo(vec![
            Paso::Responde(respuesta_herramienta("c1", "nada")),
            vacia(),
            vacia(),
            vacia(),
        ]));
        let mut runner = Runner::new(como_proveedor(&insistente), "m");
        runner.mode = Some(Mode::ReadOnly);
        let (hist, resultado) = runner.run(&pedido_usuario("hi"), &CancelToken::new()).await;
        match resultado {
            Err(RunnerError::Proveedor(error)) => assert!(error.es_sin_respuesta()),
            otro => panic!("el vacío persistente debe fallar: {otro:?}"),
        }
        assert_eq!(insistente.llamadas(), 4);
        assert_eq!(roles(&hist), "user,assistant,tool");
    }

    /// «continua» tras un fallo deja un `user` final sobre el `tool`: el vacío
    /// debe reintentarse igual que cuando el `tool` es el último mensaje.
    #[tokio::test]
    async fn respuesta_vacia_se_reintenta_aunque_continua_siga_a_la_herramienta() {
        let proveedor = Arc::new(ProveedorFalso::nuevo(vec![
            Paso::Falla(ProviderError::local(
                "el proveedor no devolvió ninguna respuesta",
            )),
            Paso::Responde(respuesta_texto("seguimos")),
        ]));
        let mut runner = Runner::new(como_proveedor(&proveedor), "m");
        runner.mode = Some(Mode::ReadOnly);
        let mut historial = pedido_usuario("crea el archivo");
        historial.push(respuesta_herramienta("c1", "bash").message);
        historial.push(Message {
            role: "tool".to_owned(),
            content: "ok".to_owned(),
            tool_call_id: "c1".to_owned(),
            ..Message::default()
        });
        historial.extend(pedido_usuario("continua"));

        let (_, resultado) = runner.run(&historial, &CancelToken::new()).await;

        assert!(resultado.is_ok(), "no se recuperó del vacío: {resultado:?}");
        assert_eq!(proveedor.llamadas(), 2);
    }

    /// Cancelar mientras se pide el resumen no descarta los turnos viejos:
    /// el arnés guarda la sesión aunque el turno falle (issue #126).
    #[tokio::test]
    async fn cancelar_durante_la_compactacion_conserva_el_historial() {
        let mut historial = pedido_usuario("pregunta");
        for indice in 0..13 {
            let role = if indice % 2 == 0 { "assistant" } else { "user" };
            historial.push(Message {
                role: role.to_owned(),
                content: format!("turno {indice}"),
                ..Message::default()
            });
        }
        assert_eq!(historial.len(), 14);

        let proveedor = Arc::new(ProveedorFalso::nuevo(vec![Paso::EsperaSiempre]));
        let mut runner = Runner::new(como_proveedor(&proveedor), "m");
        runner.mode = Some(Mode::ReadOnly);
        runner.max_chars = 20;

        let token = CancelToken::new();
        let token_hijo = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            token_hijo.cancelar();
        });

        let (hist, resultado) = runner.run(&historial, &token).await;

        assert!(matches!(resultado, Err(RunnerError::Cancelado)));
        assert_eq!(hist, historial, "la cancelación no debe recortar turnos");
        assert!(
            !hist.iter().any(|m| m.content.contains("se omitieron")),
            "no debe quedar la marca de omisión"
        );
    }
}
