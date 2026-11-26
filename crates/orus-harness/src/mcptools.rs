//! Herramientas de un servidor MCP como herramientas del agente.
//!
//! La ventana y la terminal comparten
//! las listas de prohibidas y de solo lectura para que no se separen.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use orus_agent::{Risk, Tool, ToolError, ToolSpec as SpecAgente};
use orus_org::{Client, JsonObject, ToolSpec};
use rmcp::service::ServiceError;
use serde_json::Value;

/// Devuelven un secreto o cambian el contexto de toda la cuenta. El modelo no
/// las ve y nunca llegan al servidor.
fn prohibida(nombre: &str) -> bool {
    matches!(
        nombre,
        "create_agent"
            | "get_my_doorbell"
            | "invite_member"
            | "switch_org"
            | "create_organization"
            | "move_task_to_org"
            | "move_goal_to_org"
    )
}

/// La lista explícita de lectura. Una herramienta nueva
/// cuenta como escritura hasta que alguien la revisa y la agrega acá: fallar
/// cerrado es el default seguro.
fn solo_lectura(nombre: &str) -> bool {
    matches!(
        nombre,
        "get_me"
            | "get_my_tasks"
            | "search_tasks"
            | "get_worker_tasks"
            | "get_team_status"
            | "get_my_mentions"
            | "list_my_chat_rooms"
            | "get_chat_messages"
            | "get_my_goals"
            | "list_goals"
            | "search_decisions"
            | "get_activity"
            | "get_task_files"
            | "get_my_alerts"
            | "get_goal"
            | "list_periods"
            | "list_task_requests"
            | "list_points_pending"
    )
}

/// La parte del cliente MCP que estas herramientas necesitan. En el contrato la
/// satisfacen `*mcp.Client` y `*oruscribe.Client`; acá
/// alcanza con [`Client`] y los tests pasan su propio doble.
#[async_trait]
pub trait Backend: Send + Sync {
    async fn tool_specs(&self) -> orus_org::Result<Vec<ToolSpec>>;

    async fn call(&self, nombre: &str, args: JsonObject) -> orus_org::Result<String>;
}

#[async_trait]
impl Backend for Client {
    async fn tool_specs(&self) -> orus_org::Result<Vec<ToolSpec>> {
        Client::tool_specs(self).await
    }

    async fn call(&self, nombre: &str, args: JsonObject) -> orus_org::Result<String> {
        Client::call(self, nombre, args).await
    }
}

/// El regulamiento del sondeo que quemó la cuota del plan gratuito (incidente
/// del 2026-11-15: salas y mensajes cada 10 s durante horas). El tope corta ese
/// patrón —18 llamadas en la ventana— y deja pasar una ráfaga legítima. Las
/// escrituras no se topan: ya pasan por aprobación.
const TOPE_LECTURAS: u32 = 15;
const VENTANA_LECTURAS: Duration = Duration::from_secs(180);

/// Ventana fija por herramienta de solo lectura. El error que devuelve es la
/// regla del arnés: lo en vivo llega por el timbre, no por sondeo.
struct Regulador {
    inicio: Instant,
    hechas: u32,
}

impl Regulador {
    fn nuevo() -> Self {
        Self {
            inicio: Instant::now(),
            hechas: 0,
        }
    }

    /// Comprueba si cabe otra lectura y la reserva antes de soltar el cerrojo.
    /// Reservar aquí es lo que corta la ráfaga: cuatro llamadas que vean 14 no
    /// pueden convertirse en 18. Devuelve el inicio de la ventana reservada
    /// para que un fallo del backend devuelva el cupo solo a esa ventana.
    fn permitir(&mut self, ahora: Instant, nombre: &str) -> Result<Instant, ToolError> {
        self.reiniciar(ahora);
        if self.hechas >= TOPE_LECTURAS {
            return Err(ToolError::Message(format!(
                "{}: {} llamadas en {} minutos: parece sondeo, y cada llamada le cuesta \
                 cuota a la cuenta. Los avisos en vivo llegan solos por el timbre \
                 (doorbell): espera el despertar o pregunta a la persona.",
                nombre,
                self.hechas,
                VENTANA_LECTURAS.as_secs() / 60
            )));
        }
        self.hechas += 1;
        Ok(self.inicio)
    }

    /// Devuelve la reserva de una llamada que el backend no respondió. Un error
    /// de conexión no gasta la ventana: al volver la red, el agente recupera sus
    /// lecturas. Si la ventana ya cambió, no se le quita cupo a la nueva.
    fn liberar(&mut self, ventana: Instant) {
        if self.inicio == ventana {
            self.hechas = self.hechas.saturating_sub(1);
        }
    }

    fn reiniciar(&mut self, ahora: Instant) {
        if ahora.duration_since(self.inicio) >= VENTANA_LECTURAS {
            self.inicio = ahora;
            self.hechas = 0;
        }
    }
}

/// Solo un error que no llegó al servidor devuelve el cupo: la llamada no
/// consumió cuota. Un error que el servidor SÍ respondió (`Tool`, `Call` con
/// `McpError`, `InvalidJson`) ya gastó la ventana; reembolsarlo dejaría que un
/// bucle de lecturas fallidas esquivara el tope anti-sondeo (#94).
fn sin_respuesta_del_servidor(error: &orus_org::OrgError) -> bool {
    match error {
        orus_org::OrgError::NotConnected
        | orus_org::OrgError::Connect(_)
        | orus_org::OrgError::HttpClient(_) => true,
        // `Call` envuelve el `ServiceError` de rmcp: solo transporte, envío
        // fallido o timeout cortan la lectura a mitad (#131). Por variante,
        // nunca por texto.
        orus_org::OrgError::Call { source, .. } => matches!(
            source.as_ref(),
            ServiceError::TransportSend(_)
                | ServiceError::TransportClosed
                | ServiceError::Timeout { .. }
        ),
        _ => false,
    }
}

/// El tope vive en el dueño de la sesión, no en cada herramienta: si se
/// reconstruyera por turno, un mensaje nuevo reiniciaría la ventana y el
/// sondeo entre turnos no se cortaría.
#[derive(Default)]
pub struct RegistroLecturas {
    por_herramienta: Mutex<HashMap<String, Arc<Mutex<Regulador>>>>,
}

impl RegistroLecturas {
    pub fn new() -> Self {
        Self::default()
    }

    fn de(&self, nombre: &str) -> Arc<Mutex<Regulador>> {
        let mut mapa = self
            .por_herramienta
            .lock()
            .unwrap_or_else(|envenenado| envenenado.into_inner());
        mapa.entry(nombre.to_owned())
            .or_insert_with(|| Arc::new(Mutex::new(Regulador::nuevo())))
            .clone()
    }
}

/// Una herramienta del servidor expuesta al bucle del agente.
pub struct McpTool {
    backend: Option<Arc<dyn Backend>>,
    info: ToolSpec,
    regulador: Arc<Mutex<Regulador>>,
}

impl McpTool {
    /// Envuelve una herramienta. Un backend ausente deja la herramienta
    /// visible para el modelo pero falla al ejecutarse.
    pub fn new(
        backend: Option<Arc<dyn Backend>>,
        info: ToolSpec,
        registro: &RegistroLecturas,
    ) -> Self {
        let regulador = registro.de(&info.name);
        Self {
            backend,
            info,
            regulador,
        }
    }
}

#[async_trait]
impl Tool for McpTool {
    fn spec(&self) -> SpecAgente {
        SpecAgente {
            name: self.info.name.clone(),
            description: self.info.description.clone(),
            schema: self.info.schema.clone(),
        }
    }

    /// Solo cuenta como lectura lo que está en la lista explícita
    fn risk(&self) -> Option<Risk> {
        Some(if solo_lectura(&self.info.name) {
            Risk::Read
        } else {
            Risk::Write
        })
    }

    async fn run(&self, args: Value) -> Result<String, ToolError> {
        if prohibida(&self.info.name) {
            return Err(ToolError::Message(format!(
                "{}: herramienta prohibida para el agente",
                self.info.name
            )));
        }
        let Some(backend) = &self.backend else {
            return Err(ToolError::Message(format!(
                "{}: no hay conexión con Oruscribe",
                self.info.name
            )));
        };
        let args: JsonObject = if args.is_null() {
            // Un `null` equivale a sin argumentos: mapa vacío.
            JsonObject::new()
        } else {
            serde_json::from_value(args).map_err(|error| ToolError::Message(error.to_string()))?
        };
        // El cupo se reserva bajo el cerrojo, antes de soltarlo para llamar al
        // backend. Reservar después dejaba pasar una ráfaga en paralelo y la
        // cuenta del plan pagaba de más. Un error de conexión devuelve el cupo.
        let lectura = solo_lectura(&self.info.name);
        let ventana = if lectura {
            let mut regulador = self
                .regulador
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            Some(regulador.permitir(Instant::now(), &self.info.name)?)
        } else {
            None
        };
        let respuesta = match backend.call(&self.info.name, args).await {
            Ok(respuesta) => respuesta,
            Err(error) => {
                // Un error de conexión devuelve el cupo: la llamada no llegó
                // al servidor. Un error que el servidor SÍ respondió
                // (Tool, Call con McpError, InvalidJson) ya consumió la
                // ventana; reembolsarlo dejaría que un bucle de lecturas
                // fallidas esquivara el tope anti-sondeo (#94, #131).
                if let Some(ventana) = ventana
                    && sin_respuesta_del_servidor(&error)
                {
                    self.regulador
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .liberar(ventana);
                }
                return Err(ToolError::Message(error.to_string()));
            }
        };
        Ok(respuesta)
    }
}

/// Devuelve las herramientas que el agente puede ver, en el orden en que las
/// mandó el servidor y sin las prohibidas.
pub fn build(
    backend: Option<Arc<dyn Backend>>,
    specs: Vec<ToolSpec>,
    registro: &RegistroLecturas,
) -> Vec<Arc<dyn Tool>> {
    let mut herramientas: Vec<Arc<dyn Tool>> = Vec::with_capacity(specs.len());
    for spec in specs {
        if prohibida(&spec.name) {
            continue;
        }
        herramientas.push(Arc::new(McpTool::new(backend.clone(), spec, registro)));
    }
    herramientas
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU32, Ordering};

    use serde_json::json;

    use super::*;

    fn spec(nombre: &str) -> ToolSpec {
        ToolSpec {
            name: nombre.to_owned(),
            description: format!("descripción de {nombre}"),
            schema: json!({"type": "object"}),
        }
    }

    #[test]
    fn build_filters_prohibited_and_classifies() {
        let tools = build(
            None,
            vec![
                spec("get_my_tasks"),
                spec("send_chat_message"),
                spec("create_agent"),
                spec("get_algo_nuevo"),
            ],
            &RegistroLecturas::new(),
        );
        let nombres: Vec<String> = tools.iter().map(|tool| tool.spec().name).collect();
        assert_eq!(
            nombres,
            ["get_my_tasks", "send_chat_message", "get_algo_nuevo"],
            "se descartan las prohibidas y se conserva el orden"
        );

        let riesgo = |nombre: &str| {
            tools
                .iter()
                .find(|tool| tool.spec().name == nombre)
                .and_then(|tool| tool.risk())
        };
        assert_eq!(riesgo("get_my_tasks"), Some(Risk::Read));
        assert_eq!(riesgo("send_chat_message"), Some(Risk::Write));
        assert_eq!(
            riesgo("get_algo_nuevo"),
            Some(Risk::Write),
            "una herramienta nueva cuenta como escritura"
        );

        assert_eq!(tools[0].spec().description, "descripción de get_my_tasks");
        assert_eq!(tools[0].spec().schema, json!({"type": "object"}));
    }

    #[tokio::test]
    async fn tool_without_backend_errors() {
        let tool = McpTool::new(None, spec("get_my_tasks"), &RegistroLecturas::new());
        let error = tool.run(json!({})).await.expect_err("sin backend falla");
        assert_eq!(
            error.to_string(),
            "get_my_tasks: no hay conexión con Oruscribe"
        );
    }

    #[tokio::test]
    async fn tool_prohibited_errors() {
        let tool = McpTool::new(None, spec("create_agent"), &RegistroLecturas::new());
        let error = tool.run(json!({})).await.expect_err("prohibida falla");
        assert_eq!(
            error.to_string(),
            "create_agent: herramienta prohibida para el agente"
        );
    }

    struct BackendFalso {
        llamada: Mutex<Option<(String, JsonObject)>>,
    }

    #[async_trait]
    impl Backend for BackendFalso {
        async fn tool_specs(&self) -> orus_org::Result<Vec<ToolSpec>> {
            Ok(Vec::new())
        }

        async fn call(&self, nombre: &str, args: JsonObject) -> orus_org::Result<String> {
            *self.llamada.lock().unwrap() = Some((nombre.to_owned(), args));
            Ok("resultado del servidor".to_owned())
        }
    }

    fn backend_falso() -> Arc<BackendFalso> {
        Arc::new(BackendFalso {
            llamada: Mutex::new(None),
        })
    }

    #[tokio::test]
    async fn tool_delegates_args_to_backend() {
        let backend = backend_falso();
        let tool = McpTool::new(
            Some(backend.clone()),
            spec("get_my_tasks"),
            &RegistroLecturas::new(),
        );
        let respuesta = tool.run(json!({"status": "open"})).await.unwrap();
        assert_eq!(respuesta, "resultado del servidor");

        let (nombre, args) = backend
            .llamada
            .lock()
            .unwrap()
            .clone()
            .expect("la herramienta llamó al backend");
        assert_eq!(nombre, "get_my_tasks");
        assert_eq!(args["status"], json!("open"));
    }

    #[tokio::test]
    async fn tool_accepts_null_args_as_empty() {
        let backend = backend_falso();
        let tool = McpTool::new(
            Some(backend.clone()),
            spec("get_my_tasks"),
            &RegistroLecturas::new(),
        );
        let respuesta = tool.run(Value::Null).await.expect("null vale sin args");
        assert_eq!(respuesta, "resultado del servidor");
        let (_, args) = backend
            .llamada
            .lock()
            .unwrap()
            .clone()
            .expect("la herramienta llamó al backend");
        assert!(args.is_empty());
    }

    #[tokio::test]
    async fn tool_rejects_non_object_args() {
        let backend = backend_falso();
        let tool = McpTool::new(
            Some(backend.clone()),
            spec("get_my_tasks"),
            &RegistroLecturas::new(),
        );
        let error = tool
            .run(json!(["no", "es", "objeto"]))
            .await
            .expect_err("los argumentos no son objeto");
        assert!(
            error.to_string().contains("invalid type"),
            "el error es el de serde: {error}"
        );
        assert!(
            backend.llamada.lock().unwrap().is_none(),
            "no se llama al backend con argumentos inválidos"
        );
    }

    struct BackendContador {
        total: AtomicU32,
    }

    #[async_trait]
    impl Backend for BackendContador {
        async fn tool_specs(&self) -> orus_org::Result<Vec<ToolSpec>> {
            Ok(Vec::new())
        }

        async fn call(&self, _nombre: &str, _args: JsonObject) -> orus_org::Result<String> {
            self.total.fetch_add(1, Ordering::SeqCst);
            Ok("resultado del servidor".to_owned())
        }
    }

    /// El incidente del 2026-11-15: sondear cada 10 s quemó la cuota del plan
    /// gratuito. El tope son 15 lecturas por herramienta en 3 minutos: cae el
    /// patrón del sondeo (18 en la ventana) y pasa una ráfaga legítima.
    #[tokio::test]
    async fn lectura_sondeada_corta_y_ensena_el_timbre() {
        let backend = Arc::new(BackendContador {
            total: AtomicU32::new(0),
        });
        let tool = McpTool::new(
            Some(backend.clone()),
            spec("list_my_chat_rooms"),
            &RegistroLecturas::new(),
        );
        for _ in 0..15 {
            tool.run(json!({}))
                .await
                .expect("dentro del tope la lectura pasa");
        }
        let error = tool.run(json!({})).await.expect_err("la 16 cae");
        let mensaje = error.to_string();
        assert!(
            mensaje.contains("parece sondeo"),
            "el error nombra el fallo: {mensaje}"
        );
        assert!(
            mensaje.contains("timbre"),
            "el error enseña la regla del timbre: {mensaje}"
        );
        assert_eq!(
            backend.total.load(Ordering::SeqCst),
            15,
            "la llamada bloqueada no llega al backend"
        );
    }

    /// Un mensaje nuevo construye las herramientas otra vez. El tope no puede
    /// empezar de cero en cada turno o el sondeo entre mensajes no se corta.
    #[tokio::test]
    async fn el_tope_sobrevive_a_un_turno_nuevo() {
        let backend = Arc::new(BackendContador {
            total: AtomicU32::new(0),
        });
        let registro = RegistroLecturas::new();
        for _turno in 0..2 {
            let herramientas = build(
                Some(backend.clone()),
                vec![spec("list_my_chat_rooms")],
                &registro,
            );
            for _llamada in 0..7 {
                herramientas[0]
                    .run(json!({}))
                    .await
                    .expect("catorce lecturas entre dos turnos pasan");
            }
        }
        let herramientas = build(
            Some(backend.clone()),
            vec![spec("list_my_chat_rooms")],
            &registro,
        );
        herramientas[0]
            .run(json!({}))
            .await
            .expect("la quince pasa en el turno nuevo");
        herramientas[0]
            .run(json!({}))
            .await
            .expect_err("la dieciséis cae aunque el turno sea nuevo");
        assert_eq!(backend.total.load(Ordering::SeqCst), 15);
    }

    #[tokio::test]
    async fn escritura_no_tiene_tope() {
        let backend = Arc::new(BackendContador {
            total: AtomicU32::new(0),
        });
        let tool = McpTool::new(
            Some(backend.clone()),
            spec("send_chat_message"),
            &RegistroLecturas::new(),
        );
        for _ in 0..16 {
            tool.run(json!({}))
                .await
                .expect("las escrituras no se topan");
        }
        assert_eq!(backend.total.load(Ordering::SeqCst), 16);
    }

    #[test]
    fn el_regulador_reinicia_la_ventana() {
        let base = Instant::now();
        let mut regulador = Regulador {
            inicio: base,
            hechas: 0,
        };
        for paso in 0..TOPE_LECTURAS {
            let ahora = base + Duration::from_secs(paso as u64);
            regulador
                .permitir(ahora, "get_chat_messages")
                .expect("dentro del tope pasa");
        }
        assert!(
            regulador
                .permitir(
                    base + Duration::from_secs(TOPE_LECTURAS as u64),
                    "get_chat_messages"
                )
                .is_err(),
            "la de más cae dentro de la ventana"
        );
        assert!(
            regulador
                .permitir(base + VENTANA_LECTURAS, "get_chat_messages")
                .is_ok(),
            "la ventana nueva vuelve a permitir"
        );
    }

    struct BackendQueFalla {
        fallos: AtomicU32,
        total: AtomicU32,
    }

    #[async_trait]
    impl Backend for BackendQueFalla {
        async fn tool_specs(&self) -> orus_org::Result<Vec<ToolSpec>> {
            Ok(Vec::new())
        }

        async fn call(&self, _nombre: &str, _args: JsonObject) -> orus_org::Result<String> {
            if self.fallos.load(Ordering::SeqCst) > 0 {
                self.fallos.fetch_sub(1, Ordering::SeqCst);
                return Err(orus_org::OrgError::NotConnected);
            }
            self.total.fetch_add(1, Ordering::SeqCst);
            Ok("resultado del servidor".to_owned())
        }
    }

    /// El hallazgo: la ventana se gastaba antes de llamar, así que una racha
    /// de errores de conexión dejaba al agente sin lecturas al volver la red.
    #[tokio::test]
    async fn los_errores_de_conexion_no_gastan_la_ventana() {
        let backend = Arc::new(BackendQueFalla {
            fallos: AtomicU32::new(20),
            total: AtomicU32::new(0),
        });
        let tool = McpTool::new(
            Some(backend.clone()),
            spec("get_my_tasks"),
            &RegistroLecturas::new(),
        );
        for _ in 0..20 {
            tool.run(json!({}))
                .await
                .expect_err("sin conexión la lectura falla");
        }
        for _ in 0..15 {
            tool.run(json!({}))
                .await
                .expect("la conexión volvió y la ventana seguía entera");
        }
        tool.run(json!({}))
            .await
            .expect_err("la dieciséis con conexión cae");
        assert_eq!(
            backend.total.load(Ordering::SeqCst),
            15,
            "solo las lecturas respondidas gastan la ventana"
        );
    }

    struct BackendErrorServidor {
        total: AtomicU32,
    }

    #[async_trait]
    impl Backend for BackendErrorServidor {
        async fn tool_specs(&self) -> orus_org::Result<Vec<ToolSpec>> {
            Ok(Vec::new())
        }

        async fn call(&self, nombre: &str, _args: JsonObject) -> orus_org::Result<String> {
            self.total.fetch_add(1, Ordering::SeqCst);
            Err(orus_org::OrgError::Tool {
                tool: nombre.to_owned(),
                message: "sala inexistente".to_owned(),
            })
        }
    }

    /// Issue #94: un error que el servidor SÍ respondió gasta la ventana.
    /// Antes cualquier Err reembolsaba el cupo y un bucle de lecturas que
    /// fallan en el servidor esquivaba el tope anti-sondeo, quemando la
    /// cuota del plan como el 2026-11-15.
    #[tokio::test]
    async fn los_errores_del_servidor_gastan_la_ventana() {
        let backend = Arc::new(BackendErrorServidor {
            total: AtomicU32::new(0),
        });
        let tool = McpTool::new(
            Some(backend.clone()),
            spec("get_my_tasks"),
            &RegistroLecturas::new(),
        );
        for _ in 0..TOPE_LECTURAS {
            let error = tool
                .run(json!({}))
                .await
                .expect_err("el servidor responde error");
            assert!(error.to_string().contains("sala inexistente"), "{error}");
        }
        let error = tool
            .run(json!({}))
            .await
            .expect_err("la dieciséis se corta como sondeo");
        assert!(
            error.to_string().contains("parece sondeo"),
            "un bucle de errores del servidor esquivó el tope: {error}"
        );
        assert_eq!(
            backend.total.load(Ordering::SeqCst),
            TOPE_LECTURAS,
            "la llamada bloqueada no llega al backend"
        );
    }

    struct BackendRafaga {
        total: AtomicU32,
    }

    #[async_trait]
    impl Backend for BackendRafaga {
        async fn tool_specs(&self) -> orus_org::Result<Vec<ToolSpec>> {
            Ok(Vec::new())
        }

        async fn call(&self, _nombre: &str, _args: JsonObject) -> orus_org::Result<String> {
            self.total.fetch_add(1, Ordering::SeqCst);
            tokio::task::yield_now().await;
            Ok("resultado del servidor".to_owned())
        }
    }

    /// El despachador lanza hasta cuatro lecturas a la vez. Si el cupo se
    /// comprueba y se suelta antes de llamar al backend, las cuatro ven 14 y
    /// pasan: la cuenta del plan paga 18 llamadas en lugar de 15.
    #[tokio::test]
    async fn la_rafaga_en_paralelo_no_pasa_el_tope() {
        let backend = Arc::new(BackendRafaga {
            total: AtomicU32::new(0),
        });
        let herramienta = Arc::new(McpTool::new(
            Some(backend.clone()),
            spec("get_my_tasks"),
            &RegistroLecturas::new(),
        ));
        for _ in 0..TOPE_LECTURAS - 1 {
            herramienta
                .run(json!({}))
                .await
                .expect("las catorce lecturas previas pasan");
        }
        let mut tareas = Vec::new();
        for _ in 0..4 {
            let herramienta = herramienta.clone();
            tareas.push(tokio::spawn(
                async move { herramienta.run(json!({})).await },
            ));
        }
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        let mut exitos = 0;
        let mut rechazos = 0;
        for tarea in tareas {
            match tarea.await.expect("la tarea termina") {
                Ok(_) => exitos += 1,
                Err(_) => rechazos += 1,
            }
        }
        assert!(
            backend.total.load(Ordering::SeqCst) <= TOPE_LECTURAS,
            "cuatro lecturas en paralelo con catorce hechas no pueden rebasar el tope: \
             {} llamadas llegaron al backend",
            backend.total.load(Ordering::SeqCst)
        );
        assert_eq!(exitos, 1, "solo una de las cuatro cabe en la ventana");
        assert_eq!(rechazos, 3, "las otras tres se cortan como sondeo");
    }

    struct BackendCallTransporte {
        fallos: AtomicU32,
        total: AtomicU32,
    }

    #[async_trait]
    impl Backend for BackendCallTransporte {
        async fn tool_specs(&self) -> orus_org::Result<Vec<ToolSpec>> {
            Ok(Vec::new())
        }

        async fn call(&self, _nombre: &str, _args: JsonObject) -> orus_org::Result<String> {
            if self.fallos.load(Ordering::SeqCst) > 0 {
                self.fallos.fetch_sub(1, Ordering::SeqCst);
                return Err(orus_org::OrgError::Call {
                    tool: "get_my_tasks".to_owned(),
                    source: Box::new(ServiceError::TransportClosed),
                });
            }
            self.total.fetch_add(1, Ordering::SeqCst);
            Ok("resultado del servidor".to_owned())
        }
    }

    /// #131: un `Call` de transporte (conexión cerrada a mitad de lectura) no
    /// llegó al servidor y no puede gastar el cupo. La red vuelve y la ventana
    /// sigue entera; solo la lectura dieciséis cae como sondeo.
    #[tokio::test]
    async fn call_de_transporte_no_gasta_la_ventana() {
        let backend = Arc::new(BackendCallTransporte {
            fallos: AtomicU32::new(20),
            total: AtomicU32::new(0),
        });
        let tool = McpTool::new(
            Some(backend.clone()),
            spec("get_my_tasks"),
            &RegistroLecturas::new(),
        );
        for _ in 0..20 {
            tool.run(json!({}))
                .await
                .expect_err("el transporte falla durante la llamada");
        }
        for _ in 0..15 {
            tool.run(json!({}))
                .await
                .expect("la ventana sigue entera cuando vuelve la red");
        }
        let error = tool
            .run(json!({}))
            .await
            .expect_err("la dieciséis con red cae como sondeo");
        assert!(
            error.to_string().contains("parece sondeo"),
            "el cupo se agotó antes de tiempo: {error}"
        );
        assert_eq!(
            backend.total.load(Ordering::SeqCst),
            15,
            "solo las lecturas respondidas gastan la ventana"
        );
    }

    struct BackendCallRespondido {
        total: AtomicU32,
    }

    #[async_trait]
    impl Backend for BackendCallRespondido {
        async fn tool_specs(&self) -> orus_org::Result<Vec<ToolSpec>> {
            Ok(Vec::new())
        }

        async fn call(&self, nombre: &str, _args: JsonObject) -> orus_org::Result<String> {
            self.total.fetch_add(1, Ordering::SeqCst);
            Err(orus_org::OrgError::Call {
                tool: nombre.to_owned(),
                source: Box::new(ServiceError::McpError(rmcp::model::ErrorData::new(
                    rmcp::model::ErrorCode::INTERNAL_ERROR,
                    "fallo del servidor",
                    None,
                ))),
            })
        }
    }

    /// #94: un `Call` que SÍ obtuvo respuesta del servidor (`McpError`) gasta
    /// la ventana. Si se reembolsara, un bucle de errores de herramienta
    /// esquivaría el tope anti-sondeo y quemaría la cuota del plan.
    #[tokio::test]
    async fn call_respondido_por_el_servidor_gasta_la_ventana() {
        let backend = Arc::new(BackendCallRespondido {
            total: AtomicU32::new(0),
        });
        let tool = McpTool::new(
            Some(backend.clone()),
            spec("get_my_tasks"),
            &RegistroLecturas::new(),
        );
        for _ in 0..TOPE_LECTURAS {
            tool.run(json!({}))
                .await
                .expect_err("el servidor responde error");
        }
        let error = tool
            .run(json!({}))
            .await
            .expect_err("la dieciséis se corta como sondeo");
        assert!(
            error.to_string().contains("parece sondeo"),
            "un bucle de Call respondidos esquivó el tope: {error}"
        );
        assert_eq!(
            backend.total.load(Ordering::SeqCst),
            TOPE_LECTURAS,
            "la llamada bloqueada no llega al backend"
        );
    }
}
