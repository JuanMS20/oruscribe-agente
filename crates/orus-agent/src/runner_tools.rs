//! Ejecución de herramientas, aprobación y paralelo.
//!
//! Port de `internal/agent/loop.go:208-368`: `paralelizable`,
//! `ejecutarParalelo`, `ejecutar`, `permitida`, `proponerPlan`,
//! `preguntarPlan`, `resultado` y `recortar`.
//!
//! Go le pasa a cada herramienta un `context.Context` con el timeout de 60 s y
//! la cancelación. El trait [`Tool`] de Rust no recibe contexto, así que acá el
//! futuro se corta con `tokio::select!`: un timeout de [`TIEMPO_HERRAMIENTA`] y
//! la espera de [`CancelToken`]. `task` corre un subagente de varios pasos y
//! lleva un plazo más largo ([`FACTOR_TAREA`]). El texto que ve el modelo imita
//! al de una herramienta de Go que mira su contexto: `error: context deadline
//! exceeded` y `error: context canceled`.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use orus_core::ToolCall;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::policy::{Decision, Mode, Risk, decide};
use crate::runner::{
    AllowRuleFn, ApproveFn, CancelToken, EmitFn, HERRAMIENTA_PROPOSE_PLAN, PlanApproveFn, Runner,
    Step, StepKind, Usage,
};
use crate::types::Tool;

/// Lo que una herramienta puede tardar. `tiempoHerramienta` (`loop.go:37`).
const TIEMPO_HERRAMIENTA: Duration = Duration::from_secs(60);

/// Cuánto se multiplica el plazo base para `task`. El subagente corre hasta 8
/// pasos (`DEFAULT_STEPS`) y cada paso es un pedido al modelo, así que un
/// minuto no alcanza: con el plazo base de 60 s son 300 s. Se deriva del plazo
/// base para que una prueba pueda bajarlo sin esperar cinco minutos.
const FACTOR_TAREA: u32 = 5;

/// El nombre de la herramienta que corre un subagente (`subagent.rs`). Se
/// reconoce por nombre porque [`Tool`] no declara su plazo.
const HERRAMIENTA_TAREA: &str = "task";

/// Bytes de la salida de una herramienta que ve el modelo.
/// `maxResultado` (`loop.go:38`).
const MAX_RESULTADO: usize = 16 << 10;

/// Marca del recorte. `marcaRecorte` (`loop.go:39`).
const MARCA_RECORTE: &str = "\n[recortado]";

/// Cuántas herramientas corren a la vez en el camino paralelo. `loop.go:237`.
const MAX_PARALELO: usize = 4;

/// Lo que recibe una llamada que no alcanzó a entrar al paralelo porque la
/// corrida se canceló. `ejecutarParalelo` (`loop.go:248`).
const RESULTADO_CANCELADO: &str = "cancelado por el contexto";

const PLAN_VACIO: &str = "el plan llegó vacío o roto: quiero el objeto {\"plan\":\"...\"}; vuelve a llamar propose_plan con el plan en texto";
const PLAN_RECHAZADO: &str =
    "plan no aprobado: durante el resto de esta ejecución cada escritura pregunta como siempre";
const PLAN_APROBADO: &str = "plan aprobado: lecturas y escrituras corren sin preguntar durante el resto de esta ejecución; ejecutar programas pregunta siempre";
const ARGUMENTOS_INVALIDOS: &str = "argumentos inválidos: no son JSON válido";
const PERMISO_DENEGADO: &str = "permiso denegado";
const TAREA_SIN_TERMINAR: &str = "error: la herramienta no terminó";

/// Lo que necesita una llamada para resolverse sin volver a mirar el `Runner`.
///
/// Copia los `Arc` del runner para que el camino paralelo pueda mover las
/// herramientas a sus tareas. El `Runner` arma uno por turno; los callbacks y
/// el flag del plan son los mismos.
#[derive(Clone)]
pub(crate) struct Despachador {
    herramientas: Vec<Arc<dyn Tool>>,
    modo: Option<Mode>,
    aprobar: Option<ApproveFn>,
    aprobar_plan: Option<PlanApproveFn>,
    regla_permitida: Option<AllowRuleFn>,
    emitir: Option<EmitFn>,
    emitir_mu: Arc<Mutex<()>>,
    plan_aprobado: Arc<AtomicBool>,
    /// Plazo base de una herramienta; `task` usa [`FACTOR_TAREA`] veces este
    /// valor. Es un campo y no una constante para que una prueba lo baje sin
    /// esperar un minuto.
    plazo_herramienta: Duration,
}

impl Despachador {
    /// Toma lo que el runner ya tiene. `Runner::run` lo llama una vez por
    /// turno, igual que Go consulta los campos en cada llamada.
    pub(crate) fn desde(runner: &Runner) -> Self {
        Self {
            herramientas: runner.tools.clone(),
            modo: runner.mode,
            aprobar: runner.approve.clone(),
            aprobar_plan: runner.plan_approve.clone(),
            regla_permitida: runner.allow_rule.clone(),
            emitir: runner.emit.clone(),
            emitir_mu: Arc::clone(&runner.emitir_mu),
            plan_aprobado: Arc::clone(&runner.plan_aprobado),
            plazo_herramienta: TIEMPO_HERRAMIENTA,
        }
    }

    /// Resuelve todas las llamadas de un turno y devuelve un resultado por
    /// llamada, en el orden en que llegaron, junto con el consumo que
    /// reportaron las herramientas (el subagente). `Run` (`loop.go:148-155`).
    pub(crate) async fn turno(
        &self,
        llamadas: &[ToolCall],
        cancel: &CancelToken,
    ) -> (Vec<String>, Usage) {
        let salidas = if self.paralelizable(llamadas) {
            self.ejecutar_paralelo(llamadas, cancel).await
        } else {
            let mut salidas = Vec::with_capacity(llamadas.len());
            for llamada in llamadas {
                salidas.push(self.ejecutar(llamada, cancel).await);
            }
            salidas
        };
        let mut consumo = Usage::default();
        for herramienta in &self.herramientas {
            let propio = herramienta.consumo();
            consumo.prompt += propio.prompt;
            consumo.completion += propio.completion;
        }
        (salidas, consumo)
    }

    /// Dice si TODAS las llamadas del turno corren sin preguntar: al menos
    /// dos, herramientas conocidas de riesgo `read` y con `Decide == Allow`.
    /// Una desconocida no fuerza el camino secuencial porque `ejecutar`
    /// responde "herramienta desconocida" sin pedir permiso.
    /// `paralelizable` (`loop.go:208-229`).
    fn paralelizable(&self, llamadas: &[ToolCall]) -> bool {
        if llamadas.len() < 2 {
            return false;
        }
        for llamada in llamadas {
            let Some(herramienta) = self.buscar(&llamada.name) else {
                continue;
            };
            // Solo lecturas: en autónomo las escrituras también son Allow, y
            // dos escrituras a la misma ruta dejarían de ser deterministas.
            let riesgo = herramienta.risk();
            if riesgo != Some(Risk::Read) || decide(self.modo, riesgo) != Decision::Allow {
                return false;
            }
        }
        true
    }

    /// Corre las llamadas en tareas separadas, con un tope de
    /// [`MAX_PARALELO`], y concatena los resultados en el orden de las
    /// llamadas. `ejecutarParalelo` (`loop.go:231-259`).
    async fn ejecutar_paralelo(&self, llamadas: &[ToolCall], cancel: &CancelToken) -> Vec<String> {
        let semaforo = Arc::new(Semaphore::new(MAX_PARALELO));
        let mut tareas = JoinSet::new();
        for (indice, llamada) in llamadas.iter().enumerate() {
            let despachador = self.clone();
            let llamada = llamada.clone();
            let cancel = cancel.clone();
            let semaforo = Arc::clone(&semaforo);
            tareas.spawn(async move {
                let permiso = tokio::select! {
                    permiso = semaforo.acquire_owned() => permiso,
                    () = cancel.esperar() => {
                        return (indice, RESULTADO_CANCELADO.to_owned());
                    }
                };
                if permiso.is_err() {
                    return (indice, RESULTADO_CANCELADO.to_owned());
                }
                let salida = despachador.ejecutar(&llamada, &cancel).await;
                (indice, salida)
            });
        }

        let mut salidas: Vec<Option<String>> = vec![None; llamadas.len()];
        while let Some(resultado) = tareas.join_next().await {
            // Una herramienta que entra en pánico no puede dejar el turno sin
            // resultado: la llamada que no volvió recibe un aviso al final.
            if let Ok((indice, salida)) = resultado {
                salidas[indice] = Some(salida);
            }
        }
        salidas
            .into_iter()
            .map(|salida| salida.unwrap_or_else(|| TAREA_SIN_TERMINAR.to_owned()))
            .collect()
    }

    /// Corre una llamada y devuelve el texto que verá el modelo. `ejecutar`
    /// (`loop.go:313-337`).
    async fn ejecutar(&self, llamada: &ToolCall, cancel: &CancelToken) -> String {
        if llamada.name == HERRAMIENTA_PROPOSE_PLAN {
            return self.proponer_plan(llamada);
        }
        let Some(herramienta) = self.buscar(&llamada.name) else {
            return self.resultado(
                &llamada.name,
                format!("herramienta desconocida: {}", llamada.name),
            );
        };
        if llamada.invalid_args {
            return self.resultado(&llamada.name, ARGUMENTOS_INVALIDOS.to_owned());
        }
        self.emitir(Step {
            kind: StepKind::ToolCall,
            tool: llamada.name.clone(),
            text: llamada.args.to_string(),
        });
        if !self.permitida(herramienta, llamada) {
            self.emitir(Step {
                kind: StepKind::Denied,
                tool: llamada.name.clone(),
                text: String::new(),
            });
            return PERMISO_DENEGADO.to_owned();
        }

        let plazo = herramienta
            .plazo(&llamada.args)
            .unwrap_or_else(|| self.plazo(&llamada.name));
        let salida = tokio::select! {
            biased;
            () = cancel.esperar() => "error: context canceled".to_owned(),
            resultado = tokio::time::timeout(
                plazo,
                herramienta.run(llamada.args.clone()),
            ) => match resultado {
                Ok(Ok(salida)) => salida,
                Ok(Err(error)) => format!("error: {error}"),
                Err(_vencido) => "error: context deadline exceeded".to_owned(),
            },
        };
        self.resultado(&llamada.name, salida)
    }

    /// Resuelve `propose_plan`: valida el plan, muestra la tarjeta y, con el
    /// sí, abre lecturas y escrituras para el resto del run. Nunca deja pasar
    /// `exec` ni amplía una `Deny`. `proponerPlan` (`loop.go:261-278`).
    fn proponer_plan(&self, llamada: &ToolCall) -> String {
        let plan = match serde_json::from_value::<ArgumentosPlan>(llamada.args.clone()) {
            Ok(args) if !args.plan.trim().is_empty() => args.plan.trim().to_owned(),
            _ => return self.resultado(HERRAMIENTA_PROPOSE_PLAN, PLAN_VACIO.to_owned()),
        };
        self.emitir(Step {
            kind: StepKind::Plan,
            tool: HERRAMIENTA_PROPOSE_PLAN.to_owned(),
            text: recortar(&plan),
        });
        if !self.preguntar_plan(llamada, &plan) {
            return self.resultado(HERRAMIENTA_PROPOSE_PLAN, PLAN_RECHAZADO.to_owned());
        }
        self.plan_aprobado.store(true, Ordering::SeqCst);
        self.resultado(HERRAMIENTA_PROPOSE_PLAN, PLAN_APROBADO.to_owned())
    }

    /// Decide el sí del plan. `PlanApprove` manda; si no está, el mismo
    /// callback `Approve` recibe el plan crudo. Un fallo al preguntar cuenta
    /// como no. `preguntarPlan` (`loop.go:280-294`).
    fn preguntar_plan(&self, llamada: &ToolCall, plan: &str) -> bool {
        if let Some(aprobar) = &self.aprobar_plan {
            return matches!(aprobar(plan), Ok(true));
        }
        let Some(aprobar) = &self.aprobar else {
            return false;
        };
        // Go pasa `json.RawMessage(plan)`: el plan crudo, no el JSON del
        // modelo. En Rust un `Value` no puede guardar texto que no sea JSON,
        // así que el equivalente es un `Value::String`.
        let crudo = ToolCall {
            id: llamada.id.clone(),
            name: HERRAMIENTA_PROPOSE_PLAN.to_owned(),
            args: Value::String(plan.to_owned()),
            invalid_args: false,
        };
        matches!(aprobar(&crudo), Ok(true))
    }

    /// Aplica la política. Un fallo al preguntar cuenta como no.
    /// `permitida` (`loop.go:339-361`).
    fn permitida(&self, herramienta: &Arc<dyn Tool>, llamada: &ToolCall) -> bool {
        let riesgo = herramienta.risk();
        match decide(self.modo, riesgo) {
            Decision::Allow => true,
            Decision::AskUser => {
                // `decide` solo pide confirmación para un riesgo conocido; si
                // llegara `None`, no se abre nada (fail-closed).
                let Some(riesgo) = riesgo else {
                    return false;
                };
                if let Some(regla) = &self.regla_permitida
                    && regla(llamada, riesgo)
                {
                    return true;
                }
                // El plan aprobado abre lecturas y escrituras para lo que
                // queda del run. `exec` nunca se abre: es la única
                // herramienta que puede hacer cualquier cosa. Una `Deny` no
                // llega hasta acá.
                if self.plan_aprobado.load(Ordering::SeqCst) && riesgo != Risk::Exec {
                    return true;
                }
                let Some(aprobar) = &self.aprobar else {
                    return false;
                };
                matches!(aprobar(llamada), Ok(true))
            }
            Decision::Deny => false,
        }
    }

    /// Recorta el texto, lo emite y lo devuelve para el modelo.
    /// `resultado` (`loop.go:363-368`).
    fn resultado(&self, herramienta: &str, texto: String) -> String {
        let texto = recortar(&texto);
        self.emitir(Step {
            kind: StepKind::ToolResult,
            tool: herramienta.to_owned(),
            text: texto.clone(),
        });
        texto
    }

    /// Cuánto puede tardar esta llamada. `task` lleva [`FACTOR_TAREA`] veces el
    /// plazo base porque adentro corre un subagente de varios pasos; las demás
    /// conservan el plazo base.
    fn plazo(&self, nombre: &str) -> Duration {
        if nombre == HERRAMIENTA_TAREA {
            self.plazo_herramienta.saturating_mul(FACTOR_TAREA)
        } else {
            self.plazo_herramienta
        }
    }

    fn buscar(&self, nombre: &str) -> Option<&Arc<dyn Tool>> {
        self.herramientas
            .iter()
            .find(|herramienta| herramienta.spec().name == nombre)
    }

    fn emitir(&self, paso: Step) {
        emitir(&self.emitir, &self.emitir_mu, paso);
    }
}

/// El argumento de `propose_plan`. `loop.go:265-267`.
#[derive(Deserialize)]
struct ArgumentosPlan {
    plan: String,
}

/// Bytes del inicio que se conservan al recortar; el resto del tope es cola.
const CABEZA_RECORTE: usize = 4 << 10;

/// Conserva el inicio y el final, que es donde los comandos dejan el error, y
/// marca el corte en frontera de runa.
fn recortar(texto: &str) -> String {
    if texto.len() <= MAX_RESULTADO {
        return texto.to_owned();
    }
    let mut fin_cabeza = CABEZA_RECORTE;
    while !texto.is_char_boundary(fin_cabeza) {
        fin_cabeza -= 1;
    }
    let mut ini_cola = texto.len() - (MAX_RESULTADO - CABEZA_RECORTE);
    while !texto.is_char_boundary(ini_cola) {
        ini_cola += 1;
    }
    let mut salida = String::with_capacity(MAX_RESULTADO + MARCA_RECORTE.len() + 1);
    salida.push_str(&texto[..fin_cabeza]);
    salida.push_str(MARCA_RECORTE);
    salida.push('\n');
    salida.push_str(&texto[ini_cola..]);
    salida
}

/// Deja el historial serializable: argumentos vacíos pasan a `{}` y el JSON
/// roto se marca para no ejecutar la herramienta con datos inventados. El
/// parseo de la respuesta ya deja `invalid_args` en `true`
/// (`openai_stream.rs:209-214`); acá se conserva la marca y se normaliza el
/// argumento nulo. `sanearLlamadas` (`loop.go:296-311`).
pub(crate) const MAX_LLAMADAS: usize = 64;

pub(crate) fn sanear_llamadas(llamadas: &mut Option<Vec<ToolCall>>) {
    let Some(llamadas) = llamadas else {
        return;
    };
    llamadas.truncate(MAX_LLAMADAS);
    let mut vistos = HashSet::with_capacity(llamadas.len());
    for (indice, llamada) in llamadas.iter_mut().enumerate() {
        if llamada.invalid_args || llamada.args.is_null() {
            llamada.args = json!({});
        }
        if !vistos.insert(llamada.id.clone()) {
            let base = format!("{}_{}", llamada.id, indice);
            let mut candidato = base.clone();
            let mut sufijo = 1;
            while vistos.contains(&candidato) {
                candidato = format!("{base}_{sufijo}");
                sufijo += 1;
            }
            llamada.id = candidato;
            vistos.insert(llamada.id.clone());
        }
    }
}

/// Emite un paso con la misma serialización que usa el runner.
/// `emitir` (`loop.go:475-481`).
pub(crate) fn emitir(emitir: &Option<EmitFn>, mu: &Mutex<()>, paso: Step) {
    let Some(callback) = emitir else {
        return;
    };
    let _guard = mu
        .lock()
        .unwrap_or_else(|envenenado| envenenado.into_inner());
    callback(paso);
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::AtomicUsize;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use orus_core::{Message, ToolCall};
    use serde_json::{Value, json};

    use super::*;
    use crate::error::ProviderError;
    use crate::runner::RunnerError;
    use crate::types::{ChatReply, ChatRequest, Provider, ToolError, ToolSpec};

    /// Responde con un guion y guarda las herramientas ofrecidas en cada
    /// pedido.
    struct ProveedorFalso {
        respuestas: Mutex<VecDeque<ChatReply>>,
        ofertas: Mutex<Vec<Vec<ToolSpec>>>,
        llamadas: AtomicUsize,
    }

    impl ProveedorFalso {
        fn nuevo(respuestas: Vec<ChatReply>) -> Self {
            Self {
                respuestas: Mutex::new(respuestas.into()),
                ofertas: Mutex::new(Vec::new()),
                llamadas: AtomicUsize::new(0),
            }
        }

        fn llamadas(&self) -> usize {
            self.llamadas.load(Ordering::SeqCst)
        }

        fn ofertas(&self) -> Vec<Vec<ToolSpec>> {
            self.ofertas
                .lock()
                .expect("las ofertas del proveedor")
                .clone()
        }
    }

    #[async_trait]
    impl Provider for ProveedorFalso {
        async fn chat(&self, req: ChatRequest) -> Result<ChatReply, ProviderError> {
            self.llamadas.fetch_add(1, Ordering::SeqCst);
            self.ofertas
                .lock()
                .expect("las ofertas del proveedor")
                .push(req.tools);
            self.respuestas
                .lock()
                .expect("el guion del proveedor")
                .pop_front()
                .ok_or_else(|| ProviderError::local("el proveedor falso se quedó sin respuestas"))
        }
    }

    fn como_proveedor(proveedor: &Arc<ProveedorFalso>) -> Arc<dyn Provider> {
        proveedor.clone()
    }

    fn como_tool(herramienta: &Arc<HerramientaFalsa>) -> Arc<dyn Tool> {
        herramienta.clone()
    }

    fn respuesta_texto(texto: &str) -> ChatReply {
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

    fn respuesta_llamada_con(id: &str, nombre: &str, args: Value, invalid_args: bool) -> ChatReply {
        ChatReply {
            message: Message {
                role: "assistant".to_owned(),
                tool_calls: Some(vec![ToolCall {
                    id: id.to_owned(),
                    name: nombre.to_owned(),
                    args,
                    invalid_args,
                }]),
                ..Message::default()
            },
            prompt_tokens: 0,
            completion_tokens: 0,
        }
    }

    fn respuesta_llamada(id: &str, nombre: &str, args: Value) -> ChatReply {
        respuesta_llamada_con(id, nombre, args, false)
    }

    fn respuesta_llamadas(llamadas: Vec<(&str, &str, Value)>) -> ChatReply {
        ChatReply {
            message: Message {
                role: "assistant".to_owned(),
                tool_calls: Some(
                    llamadas
                        .into_iter()
                        .map(|(id, nombre, args)| ToolCall {
                            id: id.to_owned(),
                            name: nombre.to_owned(),
                            args,
                            invalid_args: false,
                        })
                        .collect(),
                ),
                ..Message::default()
            },
            prompt_tokens: 0,
            completion_tokens: 0,
        }
    }

    fn pedido() -> Vec<Message> {
        vec![Message {
            role: "user".to_owned(),
            content: "hola".to_owned(),
            ..Message::default()
        }]
    }

    fn roles(msgs: &[Message]) -> String {
        msgs.iter()
            .map(|mensaje| mensaje.role.as_str())
            .collect::<Vec<_>>()
            .join(",")
    }

    struct Concurrencia {
        activos: Arc<AtomicUsize>,
        maximo: Arc<AtomicUsize>,
        espera: Duration,
    }

    /// Herramienta de prueba con salida fija y contador de ejecuciones.
    struct HerramientaFalsa {
        nombre: String,
        riesgo: Option<Risk>,
        salida: String,
        ejecuciones: AtomicUsize,
        concurrencia: Option<Concurrencia>,
        al_correr: Option<Box<dyn Fn() + Send + Sync>>,
        plazo_propio: Option<Duration>,
    }

    impl HerramientaFalsa {
        fn nueva(nombre: &str, riesgo: Risk, salida: &str) -> Self {
            Self {
                nombre: nombre.to_owned(),
                riesgo: Some(riesgo),
                salida: salida.to_owned(),
                ejecuciones: AtomicUsize::new(0),
                concurrencia: None,
                al_correr: None,
                plazo_propio: None,
            }
        }

        /// Riesgo desconocido: el equivalente Rust del string fuera de la
        /// lista de Go (`policy_test.go:22`).
        fn sin_riesgo(mut self) -> Self {
            self.riesgo = None;
            self
        }

        fn lenta(
            mut self,
            espera: Duration,
            activos: Arc<AtomicUsize>,
            maximo: Arc<AtomicUsize>,
        ) -> Self {
            self.concurrencia = Some(Concurrencia {
                activos,
                maximo,
                espera,
            });
            self
        }

        fn al_correr(mut self, al_correr: impl Fn() + Send + Sync + 'static) -> Self {
            self.al_correr = Some(Box::new(al_correr));
            self
        }

        fn con_plazo(mut self, plazo: Duration) -> Self {
            self.plazo_propio = Some(plazo);
            self
        }

        fn ejecuciones(&self) -> usize {
            self.ejecuciones.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl Tool for HerramientaFalsa {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: self.nombre.clone(),
                description: "prueba".to_owned(),
                schema: json!({"type": "object", "properties": {}}),
            }
        }

        fn risk(&self) -> Option<Risk> {
            self.riesgo
        }

        fn plazo(&self, _args: &Value) -> Option<Duration> {
            self.plazo_propio
        }

        async fn run(&self, _args: Value) -> Result<String, ToolError> {
            self.ejecuciones.fetch_add(1, Ordering::SeqCst);
            if let Some(concurrencia) = &self.concurrencia {
                let activos = concurrencia.activos.fetch_add(1, Ordering::SeqCst) + 1;
                concurrencia.maximo.fetch_max(activos, Ordering::SeqCst);
            }
            if let Some(al_correr) = &self.al_correr {
                al_correr();
            }
            if let Some(concurrencia) = &self.concurrencia {
                tokio::time::sleep(concurrencia.espera).await;
                concurrencia.activos.fetch_sub(1, Ordering::SeqCst);
            }
            Ok(self.salida.clone())
        }
    }

    fn preparar(
        proveedor: &Arc<ProveedorFalso>,
        modo: Mode,
        herramientas: Vec<Arc<dyn Tool>>,
    ) -> Runner {
        let mut runner = Runner::new(como_proveedor(proveedor), "m");
        runner.mode = Some(modo);
        runner.tools = herramientas;
        runner
    }

    type Pasos = Arc<Mutex<Vec<Step>>>;

    fn colector() -> (Pasos, EmitFn) {
        let pasos: Pasos = Arc::new(Mutex::new(Vec::new()));
        let copia = Arc::clone(&pasos);
        let emit: EmitFn = Arc::new(move |paso| copia.lock().expect("los pasos").push(paso));
        (pasos, emit)
    }

    fn tipos(pasos: &Pasos) -> String {
        pasos
            .lock()
            .expect("los pasos")
            .iter()
            .map(|paso| paso.kind.as_str())
            .collect::<Vec<_>>()
            .join(",")
    }

    fn ofrece_plan(specs: &[ToolSpec]) -> bool {
        specs
            .iter()
            .any(|spec| spec.name == HERRAMIENTA_PROPOSE_PLAN)
    }

    const PLAN_TEXTO: &str = "1) leo el archivo; 2) lo reescribo con el cambio";

    #[tokio::test]
    async fn llama_herramienta_y_luego_responde() {
        let leer = Arc::new(HerramientaFalsa::nueva("leer", Risk::Read, "contenido"));
        let proveedor = Arc::new(ProveedorFalso::nuevo(vec![
            respuesta_llamada("c1", "leer", json!({"ruta": "a"})),
            respuesta_texto("listo"),
        ]));
        let (pasos, emit) = colector();
        let mut runner = preparar(&proveedor, Mode::ReadOnly, vec![como_tool(&leer)]);
        runner.emit = Some(emit);

        let (hist, resultado) = runner.run(&pedido(), &CancelToken::new()).await;

        assert!(resultado.is_ok(), "Run falló: {resultado:?}");
        assert_eq!(roles(&hist), "user,assistant,tool,assistant");
        assert_eq!(hist[2].tool_call_id, "c1");
        assert_eq!(hist[2].content, "contenido");
        assert_eq!(hist[3].content, "listo");
        assert_eq!(leer.ejecuciones(), 1);
        assert_eq!(tipos(&pasos), "tool_call,tool_result,answer");
    }

    #[tokio::test]
    async fn herramienta_desconocida() {
        let proveedor = Arc::new(ProveedorFalso::nuevo(vec![
            respuesta_llamada("c1", "nope", json!({})),
            respuesta_texto("ok"),
        ]));
        let mut runner = preparar(&proveedor, Mode::Autonomous, Vec::new());

        let (hist, resultado) = runner.run(&pedido(), &CancelToken::new()).await;

        assert!(
            resultado.is_ok(),
            "un nombre malo no corta el bucle: {resultado:?}"
        );
        assert_eq!(hist[2].role, "tool");
        assert_eq!(hist[2].content, "herramienta desconocida: nope");
        assert_eq!(hist[3].content, "ok");
    }

    #[tokio::test]
    async fn argumentos_invalidos() {
        let leer = Arc::new(HerramientaFalsa::nueva("leer", Risk::Read, "x"));
        let proveedor = Arc::new(ProveedorFalso::nuevo(vec![
            respuesta_llamada_con("c1", "leer", json!({}), true),
            respuesta_texto("ok"),
        ]));
        let mut runner = preparar(&proveedor, Mode::Autonomous, vec![como_tool(&leer)]);

        let (hist, resultado) = runner.run(&pedido(), &CancelToken::new()).await;

        assert!(resultado.is_ok(), "Run falló: {resultado:?}");
        assert_eq!(leer.ejecuciones(), 0, "corrió con argumentos inválidos");
        assert_eq!(hist[2].content, "argumentos inválidos: no son JSON válido");
        assert!(
            serde_json::to_value(&hist).is_ok(),
            "el historial debe poder guardarse"
        );
    }

    #[tokio::test]
    async fn denegada_por_solo_lectura() {
        let escribir = Arc::new(HerramientaFalsa::nueva("escribir", Risk::Write, "escrito"));
        let proveedor = Arc::new(ProveedorFalso::nuevo(vec![
            respuesta_llamada("c1", "escribir", json!({})),
            respuesta_texto("ok"),
        ]));
        let (pasos, emit) = colector();
        let mut runner = preparar(&proveedor, Mode::ReadOnly, vec![como_tool(&escribir)]);
        runner.emit = Some(emit);

        let (hist, resultado) = runner.run(&pedido(), &CancelToken::new()).await;

        assert!(resultado.is_ok(), "Run falló: {resultado:?}");
        assert_eq!(escribir.ejecuciones(), 0);
        assert_eq!(hist[2].content, "permiso denegado");
        assert_eq!(tipos(&pasos), "tool_call,denied,answer");
    }

    #[tokio::test]
    async fn pide_aprobacion_al_escribir() {
        let casos: Vec<(&str, Option<ApproveFn>, bool)> = vec![
            (
                "el usuario dice no",
                Some(Arc::new(|_: &ToolCall| -> Result<bool, ToolError> {
                    Ok(false)
                })),
                false,
            ),
            (
                "el usuario dice sí",
                Some(Arc::new(|_: &ToolCall| -> Result<bool, ToolError> {
                    Ok(true)
                })),
                true,
            ),
            ("sin quien apruebe se niega", None, false),
            (
                "si falla al preguntar, se niega",
                Some(Arc::new(|_: &ToolCall| -> Result<bool, ToolError> {
                    Err(ToolError::Message("la ventana se cerró".to_owned()))
                })),
                false,
            ),
        ];
        for (nombre, aprobar, ejecuta) in casos {
            let escribir = Arc::new(HerramientaFalsa::nueva("escribir", Risk::Write, "escrito"));
            let proveedor = Arc::new(ProveedorFalso::nuevo(vec![
                respuesta_llamada("c1", "escribir", json!({})),
                respuesta_texto("ok"),
            ]));
            let mut runner = preparar(&proveedor, Mode::Ask, vec![como_tool(&escribir)]);
            runner.approve = aprobar;

            let (hist, resultado) = runner.run(&pedido(), &CancelToken::new()).await;

            assert!(resultado.is_ok(), "{nombre}: Run falló: {resultado:?}");
            assert_eq!(
                escribir.ejecuciones(),
                usize::from(ejecuta),
                "{nombre}: ejecuciones"
            );
            let querido = if ejecuta {
                "escrito"
            } else {
                "permiso denegado"
            };
            assert_eq!(hist[2].content, querido, "{nombre}: resultado");
        }
    }

    #[tokio::test]
    async fn recorta_resultado() {
        let casos = [
            (
                "texto largo",
                format!("{}ERROR-FINAL", "a".repeat(20 << 10)),
            ),
            (
                "letras de dos bytes",
                format!("x{}ERROR-FINAL", "ñ".repeat(10 << 10)),
            ),
        ];
        for (nombre, salida) in casos {
            let leer = Arc::new(HerramientaFalsa::nueva("leer", Risk::Read, &salida));
            let proveedor = Arc::new(ProveedorFalso::nuevo(vec![
                respuesta_llamada("c1", "leer", json!({})),
                respuesta_texto("ok"),
            ]));
            let mut runner = preparar(&proveedor, Mode::ReadOnly, vec![como_tool(&leer)]);

            let (hist, resultado) = runner.run(&pedido(), &CancelToken::new()).await;

            assert!(resultado.is_ok(), "{nombre}: Run falló: {resultado:?}");
            let contenido = &hist[2].content;
            assert!(
                contenido.contains(MARCA_RECORTE),
                "{nombre}: falta la marca de recorte"
            );
            assert!(
                contenido.ends_with("ERROR-FINAL"),
                "{nombre}: el recorte perdió el final, donde está el error"
            );
            assert!(
                contenido.len() <= MAX_RESULTADO + MARCA_RECORTE.len() + 1,
                "{nombre}: el resultado mide {} bytes",
                contenido.len()
            );
        }
    }

    #[tokio::test]
    async fn respeta_cancelacion() {
        let cancel = CancelToken::new();
        let token = cancel.clone();
        let leer = Arc::new(
            HerramientaFalsa::nueva("leer", Risk::Read, "x").al_correr(move || token.cancelar()),
        );
        let proveedor = Arc::new(ProveedorFalso::nuevo(vec![
            respuesta_llamada("c1", "leer", json!({})),
            respuesta_texto("no debía llegar"),
        ]));
        let mut runner = preparar(&proveedor, Mode::ReadOnly, vec![como_tool(&leer)]);

        let (_, resultado) = runner.run(&pedido(), &cancel).await;

        assert!(matches!(resultado, Err(RunnerError::Cancelado)));
        assert_eq!(proveedor.llamadas(), 1);
    }

    #[tokio::test]
    async fn tools_en_paralelo_conservan_el_orden() {
        let activos = Arc::new(AtomicUsize::new(0));
        let maximo = Arc::new(AtomicUsize::new(0));
        let a = Arc::new(HerramientaFalsa::nueva("leer_a", Risk::Read, "uno").lenta(
            Duration::from_millis(30),
            Arc::clone(&activos),
            Arc::clone(&maximo),
        ));
        let b = Arc::new(HerramientaFalsa::nueva("leer_b", Risk::Read, "dos").lenta(
            Duration::from_millis(30),
            Arc::clone(&activos),
            Arc::clone(&maximo),
        ));
        let proveedor = Arc::new(ProveedorFalso::nuevo(vec![
            respuesta_llamadas(vec![("1", "leer_a", json!({})), ("2", "leer_b", json!({}))]),
            respuesta_texto("listo"),
        ]));
        let mut runner = preparar(
            &proveedor,
            Mode::ReadOnly,
            vec![como_tool(&a), como_tool(&b)],
        );

        let (hist, resultado) = runner.run(&pedido(), &CancelToken::new()).await;

        assert!(resultado.is_ok(), "Run falló: {resultado:?}");
        assert!(
            maximo.load(Ordering::SeqCst) >= 2,
            "no corrió en paralelo (máximo visto: {})",
            maximo.load(Ordering::SeqCst)
        );
        let salidas: Vec<&str> = hist
            .iter()
            .filter(|mensaje| mensaje.role == "tool")
            .map(|mensaje| mensaje.content.as_str())
            .collect();
        assert_eq!(salidas, ["uno", "dos"]);
    }

    #[tokio::test]
    async fn en_autonomo_las_escrituras_no_corren_en_paralelo() {
        let activos = Arc::new(AtomicUsize::new(0));
        let maximo = Arc::new(AtomicUsize::new(0));
        let a = Arc::new(
            HerramientaFalsa::nueva("escribir_a", Risk::Write, "uno").lenta(
                Duration::from_millis(30),
                Arc::clone(&activos),
                Arc::clone(&maximo),
            ),
        );
        let b = Arc::new(
            HerramientaFalsa::nueva("escribir_b", Risk::Write, "dos").lenta(
                Duration::from_millis(30),
                Arc::clone(&activos),
                Arc::clone(&maximo),
            ),
        );
        let proveedor = Arc::new(ProveedorFalso::nuevo(vec![
            respuesta_llamadas(vec![
                ("1", "escribir_a", json!({})),
                ("2", "escribir_b", json!({})),
            ]),
            respuesta_texto("listo"),
        ]));
        let mut runner = preparar(
            &proveedor,
            Mode::Autonomous,
            vec![como_tool(&a), como_tool(&b)],
        );

        let (_, resultado) = runner.run(&pedido(), &CancelToken::new()).await;

        assert!(resultado.is_ok(), "Run falló: {resultado:?}");
        assert!(
            maximo.load(Ordering::SeqCst) <= 1,
            "dos escrituras corrieron a la vez (máximo visto: {})",
            maximo.load(Ordering::SeqCst)
        );
    }

    #[tokio::test]
    async fn una_herramienta_que_pide_aprobacion_vuelve_secuencial() {
        let activos = Arc::new(AtomicUsize::new(0));
        let maximo = Arc::new(AtomicUsize::new(0));
        let lenta = Arc::new(
            HerramientaFalsa::nueva("leer_lenta", Risk::Read, "lenta").lenta(
                Duration::from_millis(30),
                Arc::clone(&activos),
                Arc::clone(&maximo),
            ),
        );
        let escribe = Arc::new(
            HerramientaFalsa::nueva("escribir", Risk::Write, "escrita").lenta(
                Duration::from_millis(30),
                Arc::clone(&activos),
                Arc::clone(&maximo),
            ),
        );
        let proveedor = Arc::new(ProveedorFalso::nuevo(vec![
            respuesta_llamadas(vec![
                ("1", "leer_lenta", json!({})),
                ("2", "escribir", json!({})),
            ]),
            respuesta_texto("listo"),
        ]));
        let mut runner = preparar(
            &proveedor,
            Mode::Ask,
            vec![como_tool(&lenta), como_tool(&escribe)],
        );
        runner.approve = Some(Arc::new(|_: &ToolCall| -> Result<bool, ToolError> {
            Ok(true)
        }));

        let (_, resultado) = runner.run(&pedido(), &CancelToken::new()).await;

        assert!(resultado.is_ok(), "Run falló: {resultado:?}");
        assert!(
            maximo.load(Ordering::SeqCst) <= 1,
            "con una herramienta que pide aprobación no se paraleliza (máximo visto: {})",
            maximo.load(Ordering::SeqCst)
        );
    }

    #[tokio::test]
    async fn plan_aprobado_abre_escrituras() {
        let escribir = Arc::new(HerramientaFalsa::nueva("escribir", Risk::Write, "escrito"));
        let proveedor = Arc::new(ProveedorFalso::nuevo(vec![
            respuesta_llamada("p1", HERRAMIENTA_PROPOSE_PLAN, json!({"plan": PLAN_TEXTO})),
            respuesta_llamada("c1", "escribir", json!({})),
            respuesta_texto("listo"),
        ]));
        let (pasos, emit) = colector();
        let aprobaciones = Arc::new(AtomicUsize::new(0));
        let contador = Arc::clone(&aprobaciones);
        let plan_visto = Arc::new(Mutex::new(String::new()));
        let visto = Arc::clone(&plan_visto);
        let mut runner = preparar(&proveedor, Mode::Ask, vec![como_tool(&escribir)]);
        runner.approve = Some(Arc::new(move |_: &ToolCall| -> Result<bool, ToolError> {
            contador.fetch_add(1, Ordering::SeqCst);
            Ok(true)
        }));
        runner.plan_approve = Some(Arc::new(move |plan: &str| -> Result<bool, ToolError> {
            *visto.lock().expect("el plan visto") = plan.to_owned();
            Ok(true)
        }));
        runner.emit = Some(emit);

        let (_, resultado) = runner.run(&pedido(), &CancelToken::new()).await;

        assert!(resultado.is_ok(), "Run falló: {resultado:?}");
        assert_eq!(escribir.ejecuciones(), 1);
        assert_eq!(aprobaciones.load(Ordering::SeqCst), 0);
        assert!(
            plan_visto
                .lock()
                .expect("el plan visto")
                .contains("reescribo"),
            "PlanApprove no vio el plan completo"
        );
        assert_eq!(
            tipos(&pasos),
            "plan,tool_result,tool_call,tool_result,answer"
        );
    }

    #[tokio::test]
    async fn plan_rechazado_mantiene_preguntas() {
        let escribir = Arc::new(HerramientaFalsa::nueva("escribir", Risk::Write, "escrito"));
        let proveedor = Arc::new(ProveedorFalso::nuevo(vec![
            respuesta_llamada("p1", HERRAMIENTA_PROPOSE_PLAN, json!({"plan": PLAN_TEXTO})),
            respuesta_llamada("c1", "escribir", json!({})),
            respuesta_texto("listo"),
        ]));
        let preguntas = Arc::new(AtomicUsize::new(0));
        let contador = Arc::clone(&preguntas);
        let mut runner = preparar(&proveedor, Mode::Ask, vec![como_tool(&escribir)]);
        runner.approve = Some(Arc::new(move |_: &ToolCall| -> Result<bool, ToolError> {
            contador.fetch_add(1, Ordering::SeqCst);
            Ok(false)
        }));
        runner.plan_approve = Some(Arc::new(|_: &str| -> Result<bool, ToolError> { Ok(false) }));

        let (_, resultado) = runner.run(&pedido(), &CancelToken::new()).await;

        assert!(resultado.is_ok(), "Run falló: {resultado:?}");
        assert_eq!(escribir.ejecuciones(), 0);
        assert_eq!(preguntas.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn plan_no_abre_exec() {
        let ejecutora = Arc::new(HerramientaFalsa::nueva("ejecutar", Risk::Exec, "corrida"));
        let proveedor = Arc::new(ProveedorFalso::nuevo(vec![
            respuesta_llamada("p1", HERRAMIENTA_PROPOSE_PLAN, json!({"plan": PLAN_TEXTO})),
            respuesta_llamada("c1", "ejecutar", json!({})),
            respuesta_texto("listo"),
        ]));
        let preguntas = Arc::new(AtomicUsize::new(0));
        let contador = Arc::clone(&preguntas);
        let mut runner = preparar(&proveedor, Mode::Ask, vec![como_tool(&ejecutora)]);
        runner.approve = Some(Arc::new(
            move |llamada: &ToolCall| -> Result<bool, ToolError> {
                if llamada.name == "ejecutar" {
                    contador.fetch_add(1, Ordering::SeqCst);
                }
                Ok(true)
            },
        ));
        runner.plan_approve = Some(Arc::new(|_: &str| -> Result<bool, ToolError> { Ok(true) }));

        let (_, resultado) = runner.run(&pedido(), &CancelToken::new()).await;

        assert!(resultado.is_ok(), "Run falló: {resultado:?}");
        assert_eq!(preguntas.load(Ordering::SeqCst), 1, "exec pregunta siempre");
        assert_eq!(ejecutora.ejecuciones(), 1);
    }

    #[tokio::test]
    async fn plan_no_rompe_solo_lectura() {
        let escribir = Arc::new(HerramientaFalsa::nueva("escribir", Risk::Write, "escrito"));
        let proveedor = Arc::new(ProveedorFalso::nuevo(vec![
            respuesta_llamada("p1", HERRAMIENTA_PROPOSE_PLAN, json!({"plan": PLAN_TEXTO})),
            respuesta_llamada("c1", "escribir", json!({})),
            respuesta_texto("listo"),
        ]));
        let preguntas = Arc::new(AtomicUsize::new(0));
        let contador = Arc::clone(&preguntas);
        let mut runner = preparar(&proveedor, Mode::ReadOnly, vec![como_tool(&escribir)]);
        runner.approve = Some(Arc::new(move |_: &ToolCall| -> Result<bool, ToolError> {
            contador.fetch_add(1, Ordering::SeqCst);
            Ok(true)
        }));
        runner.plan_approve = Some(Arc::new(|_: &str| -> Result<bool, ToolError> { Ok(true) }));

        let (_, resultado) = runner.run(&pedido(), &CancelToken::new()).await;

        assert!(resultado.is_ok(), "Run falló: {resultado:?}");
        assert_eq!(escribir.ejecuciones(), 0);
        assert_eq!(preguntas.load(Ordering::SeqCst), 0, "una Deny no pregunta");
    }

    #[tokio::test]
    async fn plan_vacio_no_abre_nada() {
        let escribir = Arc::new(HerramientaFalsa::nueva("escribir", Risk::Write, "escrito"));
        let proveedor = Arc::new(ProveedorFalso::nuevo(vec![
            respuesta_llamada("p1", HERRAMIENTA_PROPOSE_PLAN, json!({"plan": "  "})),
            respuesta_llamada("c1", "escribir", json!({})),
            respuesta_texto("listo"),
        ]));
        let preguntas = Arc::new(AtomicUsize::new(0));
        let contador = Arc::clone(&preguntas);
        let plan_preguntado = Arc::new(AtomicUsize::new(0));
        let plan_contador = Arc::clone(&plan_preguntado);
        let mut runner = preparar(&proveedor, Mode::Ask, vec![como_tool(&escribir)]);
        runner.approve = Some(Arc::new(move |_: &ToolCall| -> Result<bool, ToolError> {
            contador.fetch_add(1, Ordering::SeqCst);
            Ok(true)
        }));
        runner.plan_approve = Some(Arc::new(move |_: &str| -> Result<bool, ToolError> {
            plan_contador.fetch_add(1, Ordering::SeqCst);
            Ok(true)
        }));

        let (_, resultado) = runner.run(&pedido(), &CancelToken::new()).await;

        assert!(resultado.is_ok(), "Run falló: {resultado:?}");
        assert_eq!(
            plan_preguntado.load(Ordering::SeqCst),
            0,
            "un plan vacío no se pregunta"
        );
        assert_eq!(preguntas.load(Ordering::SeqCst), 1);
        assert_eq!(escribir.ejecuciones(), 1);
    }

    #[tokio::test]
    async fn sin_quien_apruebe_plan_se_niega_todo() {
        let escribir = Arc::new(HerramientaFalsa::nueva("escribir", Risk::Write, "escrito"));
        let proveedor = Arc::new(ProveedorFalso::nuevo(vec![
            respuesta_llamada("p1", HERRAMIENTA_PROPOSE_PLAN, json!({"plan": PLAN_TEXTO})),
            respuesta_llamada("c1", "escribir", json!({})),
            respuesta_texto("listo"),
        ]));
        let mut runner = preparar(&proveedor, Mode::Ask, vec![como_tool(&escribir)]);

        let (_, resultado) = runner.run(&pedido(), &CancelToken::new()).await;

        assert!(resultado.is_ok(), "Run falló: {resultado:?}");
        assert_eq!(escribir.ejecuciones(), 0);
    }

    #[tokio::test]
    async fn plan_que_falla_al_preguntar_cuenta_como_no() {
        let escribir = Arc::new(HerramientaFalsa::nueva("escribir", Risk::Write, "escrito"));
        let proveedor = Arc::new(ProveedorFalso::nuevo(vec![
            respuesta_llamada("p1", HERRAMIENTA_PROPOSE_PLAN, json!({"plan": PLAN_TEXTO})),
            respuesta_texto("listo"),
        ]));
        let mut runner = preparar(&proveedor, Mode::Ask, vec![como_tool(&escribir)]);
        runner.plan_approve = Some(Arc::new(|_: &str| -> Result<bool, ToolError> {
            Err(ToolError::Message("la ventana se cerró".to_owned()))
        }));

        let (_, resultado) = runner.run(&pedido(), &CancelToken::new()).await;

        assert!(resultado.is_ok(), "Run falló: {resultado:?}");
        assert!(
            !runner.plan_aprobado.load(Ordering::SeqCst),
            "el run quedó marcado como aprobado"
        );
    }

    #[tokio::test]
    async fn plan_llega_crudo_no_json() {
        let plan = Arc::new(Mutex::new(None::<ToolCall>));
        let visto = Arc::clone(&plan);
        let proveedor = Arc::new(ProveedorFalso::nuevo(vec![
            respuesta_llamada(
                "p1",
                HERRAMIENTA_PROPOSE_PLAN,
                json!({"plan": "Paso 1\nPaso 2 <b>html</b>"}),
            ),
            respuesta_texto("listo"),
        ]));
        let mut runner = preparar(&proveedor, Mode::Ask, Vec::new());
        runner.approve = Some(Arc::new(
            move |llamada: &ToolCall| -> Result<bool, ToolError> {
                *visto.lock().expect("la llamada vista") = Some(llamada.clone());
                Ok(true)
            },
        ));

        let (_, resultado) = runner.run(&pedido(), &CancelToken::new()).await;

        assert!(resultado.is_ok(), "Run falló: {resultado:?}");
        let visto = plan.lock().expect("la llamada vista").clone();
        let visto = visto.expect("Approve debía recibir el plan");
        assert_eq!(visto.name, HERRAMIENTA_PROPOSE_PLAN);
        assert_eq!(visto.args, json!("Paso 1\nPaso 2 <b>html</b>"));
    }

    fn runner_con_plan(
        proveedor: &Arc<ProveedorFalso>,
        herramienta: &Arc<HerramientaFalsa>,
        plan_si: bool,
    ) -> Runner {
        let mut runner = preparar(proveedor, Mode::Ask, vec![como_tool(herramienta)]);
        runner.approve = Some(Arc::new(|_: &ToolCall| -> Result<bool, ToolError> {
            Ok(false)
        }));
        runner.plan_approve = Some(Arc::new(move |_: &str| -> Result<bool, ToolError> {
            Ok(plan_si)
        }));
        runner
    }

    #[tokio::test]
    async fn plan_grant_no_se_filtra_a_otro_runner() {
        let escribir = Arc::new(HerramientaFalsa::nueva("escribir", Risk::Write, "escrito"));
        let guion = || {
            vec![
                respuesta_llamada("p1", HERRAMIENTA_PROPOSE_PLAN, json!({"plan": PLAN_TEXTO})),
                respuesta_llamada("c1", "escribir", json!({})),
                respuesta_texto("listo"),
            ]
        };
        let proveedor_uno = Arc::new(ProveedorFalso::nuevo(guion()));
        let mut uno = runner_con_plan(&proveedor_uno, &escribir, true);

        let (_, resultado) = uno.run(&pedido(), &CancelToken::new()).await;
        assert!(resultado.is_ok(), "run 1 falló: {resultado:?}");
        assert_eq!(escribir.ejecuciones(), 1, "run 1 debía escribir");

        let proveedor_dos = Arc::new(ProveedorFalso::nuevo(guion()));
        let mut dos = runner_con_plan(&proveedor_dos, &escribir, false);

        let (_, resultado) = dos.run(&pedido(), &CancelToken::new()).await;
        assert!(resultado.is_ok(), "run 2 falló: {resultado:?}");
        assert_eq!(escribir.ejecuciones(), 1, "run 2 heredó el grant del run 1");
    }

    #[tokio::test]
    async fn plan_no_abre_riesgo_desconocido() {
        // `Tool::risk()` devuelve `None` para el riesgo desconocido; `Decide`
        // lo niega antes de que el plan aprobado pueda abrir nada
        // (`loop.go:340-341`, `policy.rs:83-87`).
        let misterio =
            Arc::new(HerramientaFalsa::nueva("misterio", Risk::Read, "corrió").sin_riesgo());
        let proveedor = Arc::new(ProveedorFalso::nuevo(vec![
            respuesta_llamada("p1", HERRAMIENTA_PROPOSE_PLAN, json!({"plan": PLAN_TEXTO})),
            respuesta_llamada("m1", "misterio", json!({})),
            respuesta_texto("listo"),
        ]));
        let mut runner = preparar(&proveedor, Mode::Ask, vec![como_tool(&misterio)]);
        runner.approve = Some(Arc::new(|_: &ToolCall| -> Result<bool, ToolError> {
            Ok(true)
        }));
        runner.plan_approve = Some(Arc::new(|_: &str| -> Result<bool, ToolError> { Ok(true) }));

        let (hist, resultado) = runner.run(&pedido(), &CancelToken::new()).await;

        assert!(resultado.is_ok(), "Run falló: {resultado:?}");
        assert!(
            runner.plan_aprobado.load(Ordering::SeqCst),
            "el plan debía quedar aprobado para probar que no abre el riesgo desconocido"
        );
        assert_eq!(
            misterio.ejecuciones(),
            0,
            "el riesgo desconocido corrió con el plan aprobado"
        );
        assert_eq!(hist[4].content, "permiso denegado");
    }

    #[tokio::test]
    async fn ofrece_plan_con_quien_preguntar() {
        let proveedor = Arc::new(ProveedorFalso::nuevo(vec![respuesta_texto("ok")]));
        let mut runner = preparar(&proveedor, Mode::Ask, Vec::new());
        runner.approve = Some(Arc::new(|_: &ToolCall| -> Result<bool, ToolError> {
            Ok(true)
        }));

        let (_, resultado) = runner.run(&pedido(), &CancelToken::new()).await;

        assert!(resultado.is_ok(), "Run falló: {resultado:?}");
        let ofertas = proveedor.ofertas();
        assert!(
            ofrece_plan(&ofertas[0]),
            "propose_plan debía ofrecerse cuando Approve existe"
        );
    }

    #[tokio::test]
    async fn no_ofrece_plan_sin_quien_conteste() {
        let proveedor = Arc::new(ProveedorFalso::nuevo(vec![respuesta_texto("ok")]));
        let mut runner = preparar(&proveedor, Mode::Ask, Vec::new());

        let (_, resultado) = runner.run(&pedido(), &CancelToken::new()).await;

        assert!(resultado.is_ok(), "Run falló: {resultado:?}");
        let ofertas = proveedor.ofertas();
        assert!(
            !ofrece_plan(&ofertas[0]),
            "propose_plan no debía ofrecerse sin Approve ni PlanApprove"
        );
    }

    #[tokio::test]
    async fn plan_sin_campo_plan_se_rechaza() {
        let proveedor = Arc::new(ProveedorFalso::nuevo(vec![
            respuesta_llamada(
                "p1",
                HERRAMIENTA_PROPOSE_PLAN,
                json!({"objetivo": "arreglar"}),
            ),
            respuesta_texto("listo"),
        ]));
        let preguntas = Arc::new(AtomicUsize::new(0));
        let contador = Arc::clone(&preguntas);
        let mut runner = preparar(&proveedor, Mode::Ask, Vec::new());
        runner.plan_approve = Some(Arc::new(move |_: &str| -> Result<bool, ToolError> {
            contador.fetch_add(1, Ordering::SeqCst);
            Ok(true)
        }));

        let (_, resultado) = runner.run(&pedido(), &CancelToken::new()).await;

        assert!(resultado.is_ok(), "Run falló: {resultado:?}");
        assert_eq!(
            preguntas.load(Ordering::SeqCst),
            0,
            "el plan sin el campo plan no se pregunta"
        );
    }

    #[tokio::test]
    async fn plan_grant_no_sobrevive_a_otro_run() {
        let escribir = Arc::new(HerramientaFalsa::nueva("escribir", Risk::Write, "escrito"));
        let proveedor = Arc::new(ProveedorFalso::nuevo(vec![
            respuesta_llamada("p1", HERRAMIENTA_PROPOSE_PLAN, json!({"plan": PLAN_TEXTO})),
            respuesta_llamada("c1", "escribir", json!({})),
            respuesta_texto("listo"),
            respuesta_llamada("c2", "escribir", json!({})),
            respuesta_texto("listo"),
        ]));
        let preguntas = Arc::new(AtomicUsize::new(0));
        let contador = Arc::clone(&preguntas);
        let mut runner = preparar(&proveedor, Mode::Ask, vec![como_tool(&escribir)]);
        runner.approve = Some(Arc::new(move |_: &ToolCall| -> Result<bool, ToolError> {
            contador.fetch_add(1, Ordering::SeqCst);
            Ok(false)
        }));
        runner.plan_approve = Some(Arc::new(|_: &str| -> Result<bool, ToolError> { Ok(true) }));

        let (_, resultado) = runner.run(&pedido(), &CancelToken::new()).await;
        assert!(resultado.is_ok(), "run 1 falló: {resultado:?}");
        assert_eq!(escribir.ejecuciones(), 1, "run 1 debía escribir");

        let (_, resultado) = runner.run(&pedido(), &CancelToken::new()).await;
        assert!(resultado.is_ok(), "run 2 falló: {resultado:?}");
        assert_eq!(
            preguntas.load(Ordering::SeqCst),
            1,
            "la escritura del run 2 no preguntó: heredó el permiso"
        );
        assert_eq!(escribir.ejecuciones(), 1, "la escritura del run 2 corrió");
    }

    #[tokio::test]
    async fn no_ofrece_plan_donde_no_cambia_nada() {
        for modo in [Mode::Autonomous, Mode::ReadOnly] {
            let proveedor = Arc::new(ProveedorFalso::nuevo(vec![respuesta_texto("ok")]));
            let mut runner = preparar(&proveedor, modo, Vec::new());
            runner.approve = Some(Arc::new(|_: &ToolCall| -> Result<bool, ToolError> {
                Ok(true)
            }));

            let (_, resultado) = runner.run(&pedido(), &CancelToken::new()).await;

            assert!(resultado.is_ok(), "{modo:?}: Run falló: {resultado:?}");
            let ofertas = proveedor.ofertas();
            assert!(
                !ofrece_plan(&ofertas[0]),
                "{modo:?}: propose_plan no debía ofrecerse"
            );
        }
    }

    fn llamada(id: &str, nombre: &str) -> ToolCall {
        ToolCall {
            id: id.to_owned(),
            name: nombre.to_owned(),
            args: json!({}),
            invalid_args: false,
        }
    }

    /// `task` tarda más que el plazo base y aun así termina: su plazo es
    /// [`FACTOR_TAREA`] veces el de las demás. Antes del arreglo, las dos
    /// llamadas se cortaban a los 100 ms.
    #[tokio::test]
    async fn task_no_se_corta_con_el_plazo_de_las_demas() {
        let (tarea, leer) = (
            Arc::new(
                HerramientaFalsa::nueva(HERRAMIENTA_TAREA, Risk::Read, "resumen").lenta(
                    Duration::from_millis(300),
                    Arc::new(AtomicUsize::new(0)),
                    Arc::new(AtomicUsize::new(0)),
                ),
            ),
            Arc::new(
                HerramientaFalsa::nueva("leer", Risk::Read, "contenido").lenta(
                    Duration::from_millis(300),
                    Arc::new(AtomicUsize::new(0)),
                    Arc::new(AtomicUsize::new(0)),
                ),
            ),
        );
        let proveedor = Arc::new(ProveedorFalso::nuevo(Vec::new()));
        let runner = preparar(
            &proveedor,
            Mode::ReadOnly,
            vec![como_tool(&tarea), como_tool(&leer)],
        );
        let mut despachador = Despachador::desde(&runner);
        despachador.plazo_herramienta = Duration::from_millis(100);

        let (salidas, _) = despachador
            .turno(
                &[llamada("1", HERRAMIENTA_TAREA), llamada("2", "leer")],
                &CancelToken::new(),
            )
            .await;

        assert_eq!(
            salidas[0], "resumen",
            "task se cortó con el plazo de las demás herramientas"
        );
        assert_eq!(
            salidas[1], "error: context deadline exceeded",
            "una herramienta normal conserva el plazo base"
        );
    }

    /// El plazo que pide el modelo en los argumentos gana al plazo base del
    /// runner: un `timeout: 120` no puede cortarse a los 60 s.
    #[tokio::test]
    async fn el_plazo_declarado_por_la_herramienta_gana_al_base() {
        let lenta = Arc::new(
            HerramientaFalsa::nueva("bash", Risk::Exec, "terminó")
                .con_plazo(Duration::from_millis(400))
                .lenta(
                    Duration::from_millis(200),
                    Arc::new(AtomicUsize::new(0)),
                    Arc::new(AtomicUsize::new(0)),
                ),
        );
        let proveedor = Arc::new(ProveedorFalso::nuevo(Vec::new()));
        let runner = preparar(&proveedor, Mode::Autonomous, vec![como_tool(&lenta)]);
        let mut despachador = Despachador::desde(&runner);
        despachador.plazo_herramienta = Duration::from_millis(50);

        let (salidas, _) = despachador
            .turno(&[llamada("1", "bash")], &CancelToken::new())
            .await;

        assert_eq!(salidas[0], "terminó");
    }

    /// Con margen, el timeout interno de la herramienta termina y devuelve su
    /// texto antes de que el reloj externo reporte `context deadline exceeded`.
    #[tokio::test]
    async fn el_margen_del_plazo_deja_reportar_el_timeout_interno() {
        const PARCIAL: &str = "el comando se pasó de 2m0s y se cortó. Salida: ORUS_PARCIAL";
        let interna = Arc::new(
            HerramientaFalsa::nueva("bash", Risk::Exec, PARCIAL)
                .con_plazo(Duration::from_millis(400))
                .lenta(
                    Duration::from_millis(200),
                    Arc::new(AtomicUsize::new(0)),
                    Arc::new(AtomicUsize::new(0)),
                ),
        );
        let proveedor = Arc::new(ProveedorFalso::nuevo(Vec::new()));
        let runner = preparar(&proveedor, Mode::Autonomous, vec![como_tool(&interna)]);
        let mut despachador = Despachador::desde(&runner);
        despachador.plazo_herramienta = Duration::from_millis(50);

        let (salidas, _) = despachador
            .turno(&[llamada("1", "bash")], &CancelToken::new())
            .await;

        assert_eq!(salidas[0], PARCIAL);
    }

    /// El plazo largo no es infinito: `task` sigue cortándose, solo que más
    /// tarde (500 ms con el plazo base de 100 ms).
    #[tokio::test]
    async fn task_sigue_teniendo_plazo() {
        let tarea = Arc::new(
            HerramientaFalsa::nueva(HERRAMIENTA_TAREA, Risk::Read, "tarde").lenta(
                Duration::from_millis(700),
                Arc::new(AtomicUsize::new(0)),
                Arc::new(AtomicUsize::new(0)),
            ),
        );
        let proveedor = Arc::new(ProveedorFalso::nuevo(Vec::new()));
        let runner = preparar(&proveedor, Mode::ReadOnly, vec![como_tool(&tarea)]);
        let mut despachador = Despachador::desde(&runner);
        despachador.plazo_herramienta = Duration::from_millis(100);

        let (salidas, _) = despachador
            .turno(&[llamada("1", HERRAMIENTA_TAREA)], &CancelToken::new())
            .await;

        assert_eq!(salidas[0], "error: context deadline exceeded");
    }
}
