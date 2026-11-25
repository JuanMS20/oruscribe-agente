//! Fixture compartido de los tests HTTP de `orus-api`.
//!
//! Un `Store` sin sesión, costuras que no salen a la red, un `Backend` falso
//! con memoria y el `App` armado igual que en los tests de cada ruta. Los
//! ficheros de test importan solo lo que usan; por eso el módulo permite
//! elementos sin usar.

#![allow(dead_code)]

use std::future::Future;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use orus_api::{Api, BellStatus, GitLog};
use orus_app::{
    Agent, App, ApprovalQueue, Backend, CancelToken, Connector, Conversations, Emisor, ErrorTurno,
    Refresh, Secrets, Session, SessionError, TaskStatus, TurnRunner, Turno, WakeEntry, WakeLog,
};
use orus_auth::{Error as AuthError, Store, Token};
use orus_config::Settings;
use orus_org::doorbell::Config as DoorbellConfig;
use orus_org::{
    ActivityEvent, Alert, Decision, Digest, DigestCandidate, DigestSource, Goal, GoalDetail,
    GoalInput, JsonObject, Me, Mention, Message, MessagesPage, Org, OtherOrg, Period, PeriodInput,
    Progress, Room, Status, Task, TaskFiles, TaskInput, TeamOverview, ToolSpec,
};
use serde_json::{Value, json};

/// `Store` que siempre falla al cargar: la app arranca en `logged_out`.
pub struct SinSesion;

impl Store for SinSesion {
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

/// `Store` con un token sin vencimiento para las rutas que piden sesión.
pub struct ConSesion;

impl Store for ConSesion {
    fn save(&self, _: Token) -> Result<(), AuthError> {
        Ok(())
    }

    fn load(&self) -> Result<Token, AuthError> {
        Ok(token_de_prueba())
    }

    fn clear(&self) -> Result<(), AuthError> {
        Ok(())
    }
}

/// `Store` que arranca sin sesión y recuerda el token que se guarda.
#[derive(Default)]
pub struct StoreMemoria(Mutex<Option<Token>>);

impl Store for StoreMemoria {
    fn save(&self, token: Token) -> Result<(), AuthError> {
        *self.0.lock().expect("token de prueba") = Some(token);
        Ok(())
    }

    fn load(&self) -> Result<Token, AuthError> {
        self.0
            .lock()
            .expect("token de prueba")
            .clone()
            .ok_or(AuthError::NoSession)
    }

    fn clear(&self) -> Result<(), AuthError> {
        *self.0.lock().expect("token de prueba") = None;
        Ok(())
    }
}

/// Organización adicional que `get_me` ofrece en el backend falso (#117).
pub fn otra_org() -> OtherOrg {
    OtherOrg {
        slug: "otra-org".to_owned(),
        name: "TalenAI".to_owned(),
        role: "worker".to_owned(),
    }
}

/// Un token sin vencimiento (el cero de Go) para que `client()` no renueve.
pub fn token_de_prueba() -> Token {
    serde_json::from_value(json!({"access_token": "eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJ0ZXN0In0.firma-prueba"})).expect("token de prueba")
}

/// Costuras que solo fallan: las rutas no deben llamarlas.
pub struct SinRed;

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

/// Igual que `SinRed`, con el texto de error que pide el caso.
pub struct SinRedConError(pub String);

#[async_trait]
impl Connector for SinRedConError {
    async fn open(&self, _: &str, _: &str, _: &str) -> Result<Arc<dyn Backend>, SessionError> {
        Err(SessionError::Connect(self.0.clone()))
    }
}

#[async_trait]
impl Refresh for SinRedConError {
    async fn refresh(&self, _: &Token) -> Result<Token, SessionError> {
        Err(SessionError::RefreshRejected)
    }
}

/// Clave de mentira con memoria.
pub struct Secretos(pub Option<String>);

impl Secrets for Secretos {
    fn get(&self) -> Result<Option<String>, String> {
        Ok(self.0.clone())
    }

    fn set(&self, _: &str) -> Result<(), String> {
        Ok(())
    }

    fn delete(&self) -> Result<(), String> {
        Ok(())
    }
}

/// Llavero que siempre falla al leer; el resto de operaciones no se usan.
pub struct SecretosQueFalla;

impl Secrets for SecretosQueFalla {
    fn get(&self) -> Result<Option<String>, String> {
        Err("llavero roto".to_owned())
    }

    fn set(&self, _: &str) -> Result<(), String> {
        Ok(())
    }

    fn delete(&self) -> Result<(), String> {
        Ok(())
    }
}

/// Estado del timbre que leen los ajustes y el diagnóstico.
pub struct Timbre {
    pub encendido: bool,
    pub error: String,
}

impl BellStatus for Timbre {
    fn doorbell(&self) -> bool {
        self.encendido
    }

    fn error(&self) -> String {
        self.error.clone()
    }
}

/// Sin registro de despertares: la vista lo devuelve vacío.
pub struct SinDespertar;

impl WakeLog for SinDespertar {
    fn entries(&self) -> Vec<WakeEntry> {
        Vec::new()
    }
}

/// Un turno que termina enseguida; las rutas no esperan su final.
pub struct RunnerOk;

#[async_trait]
impl TurnRunner for RunnerOk {
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

/// Conversaciones en una carpeta temporal del test.
pub fn conversaciones_en(dir: &Path) -> Conversations {
    let mut conversaciones = Conversations::new();
    conversaciones.store.dir = dir.to_path_buf();
    conversaciones.store.prefix = "c-".to_owned();
    conversaciones
}

/// Arma un `App` de prueba por partes. El orden de las llamadas no importa:
/// cada una reemplaza la pieza anterior.
pub struct AppDePrueba {
    store: Box<dyn Store + Send + Sync>,
    connector: Arc<dyn Connector>,
    settings: Settings,
    secretos: Arc<dyn Secrets>,
    conversaciones: Conversations,
    runner: Arc<dyn TurnRunner>,
}

impl AppDePrueba {
    pub fn nuevo() -> Self {
        Self {
            store: Box::new(SinSesion),
            connector: Arc::new(SinRed),
            settings: Settings::default(),
            secretos: Arc::new(Secretos(None)),
            conversaciones: Conversations::new(),
            runner: Arc::new(RunnerOk),
        }
    }

    pub fn con_store(mut self, store: Box<dyn Store + Send + Sync>) -> Self {
        self.store = store;
        self
    }

    pub fn con_connector(mut self, connector: Arc<dyn Connector>) -> Self {
        self.connector = connector;
        self
    }

    pub fn con_settings(mut self, settings: Settings) -> Self {
        self.settings = settings;
        self
    }

    pub fn con_secretos(mut self, secretos: Arc<dyn Secrets>) -> Self {
        self.secretos = secretos;
        self
    }

    /// Atajo de los tests que solo necesitan una clave guardada.
    pub fn con_clave(self, clave: Option<&str>) -> Self {
        self.con_secretos(Arc::new(Secretos(clave.map(str::to_owned))))
    }

    pub fn con_conversaciones(mut self, conversaciones: Conversations) -> Self {
        self.conversaciones = conversaciones;
        self
    }

    pub fn con_runner(mut self, runner: Arc<dyn TurnRunner>) -> Self {
        self.runner = runner;
        self
    }

    pub fn construir(self) -> App {
        let agente = Arc::new(Agent::new(
            self.conversaciones.clone(),
            Arc::new(ApprovalQueue::new()),
            self.runner,
        ));
        App::new(self.store, self.settings, self.connector, Arc::new(SinRed))
            .with_conversations(Arc::new(self.conversaciones))
            .with_agent(agente)
            .with_secrets(self.secretos)
            .with_wake_log(Arc::new(SinDespertar))
    }
}

/// Marca que el test corre en el proceso hijo aislado.
pub const MARCA_HIJO: &str = "ORUSCRIBE_API_SOPORTE_HIJO";
/// Archivo que el hijo toca al terminar; el padre lo usa para saber que llegó.
pub const MARCA_HECHO: &str = "ORUSCRIBE_API_SOPORTE_HECHO";

/// Ejecuta `cuerpo` en un proceso hijo con su propio `ORUSCRIBE_DATA_DIR`.
/// `nombre` es el nombre exacto del test que llama.
pub fn en_datos_aislados(nombre: &str, cuerpo: impl FnOnce()) {
    if std::env::var_os(MARCA_HIJO).is_some() {
        cuerpo();
        if let Some(ruta) = std::env::var_os(MARCA_HECHO) {
            std::fs::write(ruta, b"ok").expect("marcar el test ejecutado");
        }
        return;
    }
    let dir = tempfile::tempdir().expect("carpeta temporal");
    // `tempfile` crea su carpeta con el umask (0755 en Linux), no con 0700
    // como el `t.TempDir()` de Go. El hijo recibe una subcarpeta que aún no
    // existe: la crea la propia app con el 0700 del contrato.
    let datos = dir.path().join("datos");
    let hecho = dir.path().join("hecho");
    let salida = std::process::Command::new(std::env::current_exe().expect("binario de test"))
        .arg("--exact")
        .arg(nombre)
        .arg("--nocapture")
        .env("ORUSCRIBE_DATA_DIR", &datos)
        .env(MARCA_HIJO, "1")
        .env(MARCA_HECHO, &hecho)
        .output()
        .expect("correr el test hijo");
    assert!(
        salida.status.success(),
        "el hijo falló:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&salida.stdout),
        String::from_utf8_lossy(&salida.stderr)
    );
    assert!(
        hecho.exists(),
        "el test hijo no llegó al final (stdout: {})",
        String::from_utf8_lossy(&salida.stdout)
    );
}

/// Runtime de un solo hilo para los tests que no usan `#[tokio::test]`.
pub fn con_runtime<F: Future<Output = ()>>(futuro: F) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime de prueba")
        .block_on(futuro);
}

type TresTextos = (String, String, String);
type Subida = (String, String, String, Vec<u8>, String);
type Aviso = (String, String, String, String);
type CambioEstado = (String, TaskStatus, String);

/// Qué datos devuelve cada método del backend falso.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Datos {
    /// El valor conocido que usan las suites que no piden un caso especial.
    #[default]
    Habitual,
    /// Listas y archivos vacíos, para los casos de `[]` y `{}`.
    Vacio,
    /// Los valores de las rutas GET del espacio de trabajo.
    Lectura,
    /// Los identificadores y el progreso de las rutas POST de tareas.
    Escritura,
    /// Digest, avisos, reacciones y contexto con sus fallos de validación.
    Captura,
}

/// El backend mínimo que las rutas usan sin salir a la red; guarda lo que
/// recibe para que cada caso afirme que el dato llegó.
#[derive(Default)]
pub struct BackendDePrueba {
    datos: Datos,
    sin_espacio: bool,

    // Proyecciones que ya afirma `contrato.rs`; los recorders completos de
    // abajo conservan lo que necesitan las suites migradas.
    pub creado_titulo: Mutex<String>,
    pub estado_calls: AtomicUsize,
    pub ultimo_estado: Mutex<Option<Status>>,
    pub ultima_nota: Mutex<String>,
    pub comentario_calls: AtomicUsize,
    pub ultimo_comentario: Mutex<String>,
    pub directos: AtomicUsize,
    pub sala_leida: Mutex<String>,
    pub alertas_leidas: AtomicUsize,
    pub menciones_leidas: AtomicUsize,
    pub meta_titulo: Mutex<String>,
    pub periodo_titulo: Mutex<String>,
    pub aplicado: Mutex<String>,
    pub aviso_tipo: Mutex<String>,
    pub aviso_texto: Mutex<String>,
    pub reaccion: Mutex<String>,
    pub foco: Mutex<String>,
    pub animo: Mutex<String>,
    pub subidas: AtomicUsize,
    pub subida_tarea: Mutex<String>,
    pub subidas_chat: AtomicUsize,
    pub subida_sala: Mutex<String>,
    pub digest_desde: Mutex<String>,

    // Recorders completos.
    pub tarea_creada: Mutex<Option<TaskInput>>,
    pub estado_cambiado: Mutex<Option<CambioEstado>>,
    pub comentario_creado: Mutex<Option<TresTextos>>,
    pub mensaje_sala: Mutex<Option<TresTextos>>,
    pub mensaje_directo: Mutex<Option<TresTextos>>,
    pub sala_marcada: Mutex<Option<String>>,
    pub alertas_leidas_ids: Mutex<Option<Vec<String>>>,
    pub menciones_leidas_ids: Mutex<Option<Vec<String>>>,
    pub meta_creada: Mutex<Option<GoalInput>>,
    pub periodo_creado: Mutex<Option<PeriodInput>>,
    pub digest_propuesto: Mutex<Option<(String, Vec<DigestSource>)>>,
    pub digest_aplicado: Mutex<Option<(String, Vec<String>)>>,
    pub aviso_enviado: Mutex<Option<Aviso>>,
    pub reaccion_enviada: Mutex<Option<(String, String)>>,
    pub contexto_actualizado: Mutex<Option<TresTextos>>,
    pub subida_avance: Mutex<Option<Subida>>,
    pub subida_chat: Mutex<Option<Subida>>,
    pub consulta_status: Mutex<Option<String>>,
    pub consulta_offset: Mutex<Option<i64>>,
    pub consulta_since: Mutex<Option<DateTime<Utc>>>,
}

impl BackendDePrueba {
    /// Listas vacías para los casos de `[]` y `{}`.
    pub fn vacio() -> Self {
        Self {
            datos: Datos::Vacio,
            ..Self::default()
        }
    }

    /// Los valores de las rutas GET del espacio de trabajo.
    pub fn de_lectura() -> Self {
        Self {
            datos: Datos::Lectura,
            ..Self::default()
        }
    }

    /// Los identificadores y el progreso de las rutas POST de tareas.
    pub fn de_escritura() -> Self {
        Self {
            datos: Datos::Escritura,
            ..Self::default()
        }
    }

    /// Digest, avisos, reacciones y contexto con sus fallos de validación.
    pub fn de_captura() -> Self {
        Self {
            datos: Datos::Captura,
            ..Self::default()
        }
    }

    /// Las subidas fallan con `SinEspacio`.
    pub fn sin_espacio() -> Self {
        Self {
            sin_espacio: true,
            ..Self::default()
        }
    }

    pub fn texto(&self, campo: &Mutex<String>) -> String {
        campo.lock().expect("campo de prueba").clone()
    }

    /// `status` que recibió la última búsqueda de tareas.
    pub fn status(&self) -> Option<String> {
        self.consulta_status.lock().expect("status").clone()
    }

    /// `offset` que recibió la última página de mensajes.
    pub fn offset(&self) -> Option<i64> {
        *self.consulta_offset.lock().expect("offset")
    }

    /// `since` que recibió la última consulta de actividad.
    pub fn since(&self) -> Option<DateTime<Utc>> {
        *self.consulta_since.lock().expect("since")
    }
}

#[async_trait]
impl Backend for BackendDePrueba {
    async fn me(&self) -> Result<Me, SessionError> {
        Ok(Me {
            user_id: "u1".to_owned(),
            name: "Ana Núñez".to_owned(),
            role: "worker".to_owned(),
            organization: Org {
                id: "o1".to_owned(),
                slug: "syncra".to_owned(),
                name: "Oruscribe".to_owned(),
            },
            other_organizations: Some(vec![otra_org()]),
            ..Me::default()
        })
    }

    async fn tasks(&self) -> Result<Vec<Task>, SessionError> {
        if self.datos == Datos::Vacio {
            return Ok(Vec::new());
        }
        if self.datos == Datos::Lectura {
            return Ok(vec![Task {
                id: "t1".to_owned(),
                title: "facturas".to_owned(),
                status: Status::Pending,
                ..Task::default()
            }]);
        }
        Ok(vec![Task {
            id: "t1".to_owned(),
            title: "Ordenar facturas".to_owned(),
            status: Status::Pending,
            ..Task::default()
        }])
    }

    async fn rooms(&self) -> Result<Vec<Room>, SessionError> {
        if self.datos == Datos::Vacio {
            return Ok(Vec::new());
        }
        if self.datos == Datos::Lectura {
            return Ok(vec![Room {
                room_id: "r1".to_owned(),
                kind: "team".to_owned(),
                name: "general".to_owned(),
                ..Room::default()
            }]);
        }
        Ok(vec![Room {
            room_id: "r1".to_owned(),
            kind: "team".to_owned(),
            name: "Arnés".to_owned(),
            ..Room::default()
        }])
    }

    async fn messages(&self, _: &str, _: i64, offset: i64) -> Result<MessagesPage, SessionError> {
        *self.consulta_offset.lock().expect("offset") = Some(offset);
        if self.datos == Datos::Lectura {
            return Ok(MessagesPage {
                messages: vec![Message {
                    id: "m1".to_owned(),
                    body: "hola".to_owned(),
                    author_id: "u1".to_owned(),
                    ..Message::default()
                }],
                has_more: true,
                next_offset: 50,
            });
        }
        Ok(MessagesPage {
            messages: vec![],
            has_more: false,
            next_offset: 0,
        })
    }

    async fn send(&self, room: &str, body: &str, reply: &str) -> Result<(), SessionError> {
        *self.mensaje_sala.lock().expect("mensaje de sala") =
            Some((room.to_owned(), body.to_owned(), reply.to_owned()));
        Ok(())
    }

    async fn tool_specs(&self) -> Result<Vec<ToolSpec>, SessionError> {
        Ok(vec![ToolSpec {
            name: "get_my_tasks".to_owned(),
            description: "las tareas de la persona".to_owned(),
            schema: json!({"type": "object"}),
        }])
    }

    async fn call_tool(&self, _: &str, _: JsonObject) -> Result<String, SessionError> {
        Ok(String::new())
    }

    async fn doorbell(&self) -> Result<DoorbellConfig, SessionError> {
        Err(SessionError::NoDoorbell)
    }

    async fn search_tasks(&self, q: &str, status: &str, _: i64) -> Result<Vec<Task>, SessionError> {
        *self.consulta_status.lock().expect("status") = Some(status.to_owned());
        if self.datos == Datos::Vacio {
            return Ok(Vec::new());
        }
        if self.datos == Datos::Lectura {
            return Ok(vec![Task {
                id: "t2".to_owned(),
                title: "encontrada".to_owned(),
                status: Status::Pending,
                ..Task::default()
            }]);
        }
        Ok(vec![Task {
            id: "t1".to_owned(),
            title: format!("Ordenar facturas {q}"),
            status: Status::Pending,
            ..Task::default()
        }])
    }

    async fn task_files(&self, _: &str) -> Result<TaskFiles, SessionError> {
        if self.datos == Datos::Lectura {
            return Ok(serde_json::from_value::<TaskFiles>(json!({
                "files": [{
                    "id": "f1",
                    "file_name": "informe.pdf",
                    "mime_type": "application/pdf",
                    "size_bytes": 12
                }],
                "update_comments": {"u1": [{"id": "c1", "body": "bien"}]}
            }))
            .expect("archivos de tarea"));
        }
        Ok(TaskFiles::default())
    }

    async fn alerts(&self) -> Result<Vec<Alert>, SessionError> {
        match self.datos {
            Datos::Vacio => Ok(Vec::new()),
            Datos::Lectura => Ok(serde_json::from_value::<Vec<Alert>>(json!([
                {"id": "a1", "type": "info", "message": "leída", "read": true},
                {"id": "a2", "type": "blocker", "message": "pendiente", "read": false}
            ]))
            .expect("alertas")),
            _ => Ok(vec![Alert {
                id: "a1".to_owned(),
                kind: "blocker".to_owned(),
                message: "aviso".to_owned(),
                ..Alert::default()
            }]),
        }
    }

    async fn mentions(&self) -> Result<Vec<Mention>, SessionError> {
        match self.datos {
            Datos::Vacio => Ok(Vec::new()),
            Datos::Lectura => Ok(serde_json::from_value::<Vec<Mention>>(json!([
                {"id": "n1", "body": "te nombran", "read": false},
                {"id": "n2", "body": "vieja", "read": true}
            ]))
            .expect("menciones")),
            _ => Ok(vec![Mention {
                id: "m1".to_owned(),
                body: "revisa el arnés".to_owned(),
                ..Mention::default()
            }]),
        }
    }

    async fn goals(&self) -> Result<Vec<Goal>, SessionError> {
        match self.datos {
            Datos::Vacio => Ok(Vec::new()),
            Datos::Lectura => Ok(vec![Goal {
                id: "g1".to_owned(),
                title: "meta anual".to_owned(),
                progress_pct: 28.6,
                ..Goal::default()
            }]),
            _ => Ok(vec![Goal {
                id: "g1".to_owned(),
                title: "Lanzar el arnés".to_owned(),
                progress_pct: 28.6,
                cells_done: 2,
                cells_total: 7,
                ..Goal::default()
            }]),
        }
    }

    async fn goal_detail(&self, id: &str) -> Result<GoalDetail, SessionError> {
        if self.datos == Datos::Lectura {
            return Ok(GoalDetail {
                goal: Goal {
                    id: "g1".to_owned(),
                    title: "meta anual".to_owned(),
                    ..Goal::default()
                },
                linked_tasks: None,
                progress_pct: 50.0,
                ..GoalDetail::default()
            });
        }
        Ok(GoalDetail {
            goal: Goal {
                id: id.to_owned(),
                title: "Lanzar el arnés".to_owned(),
                ..Goal::default()
            },
            linked_tasks: Some(vec![]),
            ..GoalDetail::default()
        })
    }

    async fn periods(&self) -> Result<Vec<Period>, SessionError> {
        match self.datos {
            Datos::Vacio => Ok(Vec::new()),
            Datos::Lectura => Ok(vec![Period {
                id: "p1".to_owned(),
                name: "noviembre".to_owned(),
                kind: "month".to_owned(),
                ..Period::default()
            }]),
            _ => Ok(vec![Period {
                id: "p1".to_owned(),
                name: "Noviembre 2026".to_owned(),
                kind: "month".to_owned(),
                start_date: "2026-11-01".to_owned(),
                end_date: "2026-11-30".to_owned(),
                is_current: true,
                is_active: true,
                ..Period::default()
            }]),
        }
    }

    async fn activity(
        &self,
        since: Option<DateTime<Utc>>,
        _: i64,
    ) -> Result<Vec<ActivityEvent>, SessionError> {
        *self.consulta_since.lock().expect("since") = since;
        match self.datos {
            Datos::Vacio => Ok(Vec::new()),
            Datos::Lectura => Ok(vec![
                serde_json::from_value::<ActivityEvent>(json!({
                    "type": "task_status",
                    "title": "avance",
                    "timestamp": "2026-11-01T00:00:00Z"
                }))
                .expect("actividad"),
            ]),
            _ => Ok(vec![ActivityEvent {
                event_type: "goal_created".to_owned(),
                actor_name: "Ana Núñez".to_owned(),
                title: "Lanzar el arnés".to_owned(),
                ..ActivityEvent::default()
            }]),
        }
    }

    async fn decisions(&self, _: &str, _: i64) -> Result<Vec<Decision>, SessionError> {
        match self.datos {
            Datos::Vacio => Ok(Vec::new()),
            Datos::Lectura => Ok(vec![Decision {
                id: "d1".to_owned(),
                title: "decisión".to_owned(),
                ..Decision::default()
            }]),
            _ => Ok(vec![Decision {
                id: "d1".to_owned(),
                title: "Una sola identidad por token".to_owned(),
                decision: "El agente usa su propio token.".to_owned(),
                ..Decision::default()
            }]),
        }
    }

    async fn team_status(&self) -> Result<Vec<TeamOverview>, SessionError> {
        match self.datos {
            Datos::Vacio => Ok(Vec::new()),
            Datos::Lectura => Ok(vec![TeamOverview {
                id: "tm1".to_owned(),
                name: "equipo".to_owned(),
                ..TeamOverview::default()
            }]),
            _ => Ok(vec![TeamOverview {
                id: "tm1".to_owned(),
                name: "Arnés".to_owned(),
                ..TeamOverview::default()
            }]),
        }
    }

    async fn create_task(&self, input: &TaskInput) -> Result<Task, SessionError> {
        *self.creado_titulo.lock().expect("título creado") = input.title.clone();
        *self.tarea_creada.lock().expect("tarea creada") = Some(input.clone());
        let id = if self.datos == Datos::Escritura {
            "t-creada"
        } else {
            "t-new"
        };
        Ok(Task {
            id: id.to_owned(),
            title: input.title.clone(),
            status: Status::Pending,
            ..Task::default()
        })
    }

    async fn set_task_status(
        &self,
        id: &str,
        status: TaskStatus,
        note: &str,
    ) -> Result<Progress, SessionError> {
        self.estado_calls.fetch_add(1, Ordering::Relaxed);
        *self.ultimo_estado.lock().expect("estado") = Some(status.clone());
        *self.ultima_nota.lock().expect("nota") = note.to_owned();
        *self.estado_cambiado.lock().expect("estado cambiado") =
            Some((id.to_owned(), status, note.to_owned()));
        if self.datos == Datos::Escritura {
            return Ok(Progress {
                reminder: "quedan dos".to_owned(),
                goal_pct: 42.5,
            });
        }
        Ok(Progress {
            reminder: "cuenta qué cambió".to_owned(),
            ..Progress::default()
        })
    }

    async fn comment_update(
        &self,
        task: &str,
        update: &str,
        body: &str,
    ) -> Result<(), SessionError> {
        self.comentario_calls.fetch_add(1, Ordering::Relaxed);
        *self.ultimo_comentario.lock().expect("comentario") = body.to_owned();
        *self.comentario_creado.lock().expect("comentario creado") =
            Some((task.to_owned(), update.to_owned(), body.to_owned()));
        Ok(())
    }

    async fn send_direct(&self, to: &str, body: &str, reply: &str) -> Result<(), SessionError> {
        self.directos.fetch_add(1, Ordering::Relaxed);
        *self.mensaje_directo.lock().expect("mensaje directo") =
            Some((to.to_owned(), body.to_owned(), reply.to_owned()));
        Ok(())
    }

    async fn mark_room_read(&self, room_id: &str) -> Result<(), SessionError> {
        *self.sala_leida.lock().expect("sala leída") = room_id.to_owned();
        *self.sala_marcada.lock().expect("sala marcada") = Some(room_id.to_owned());
        Ok(())
    }

    async fn mark_alerts_read(&self, ids: &[String]) -> Result<(), SessionError> {
        self.alertas_leidas.fetch_add(ids.len(), Ordering::Relaxed);
        *self.alertas_leidas_ids.lock().expect("alertas leídas") = Some(ids.to_vec());
        Ok(())
    }

    async fn mark_mentions_read(&self, ids: &[String]) -> Result<(), SessionError> {
        self.menciones_leidas
            .fetch_add(ids.len(), Ordering::Relaxed);
        *self.menciones_leidas_ids.lock().expect("menciones leídas") = Some(ids.to_vec());
        Ok(())
    }

    async fn create_goal(&self, input: GoalInput) -> Result<Goal, SessionError> {
        *self.meta_titulo.lock().expect("meta") = input.title.clone();
        *self.meta_creada.lock().expect("meta creada") = Some(input.clone());
        if self.datos == Datos::Captura {
            return Ok(Goal {
                id: "g-nueva".to_owned(),
                title: input.title,
                percent: 12.5,
                ..Goal::default()
            });
        }
        Ok(Goal {
            id: "g-new".to_owned(),
            title: input.title,
            ..Goal::default()
        })
    }

    async fn create_period(&self, input: &PeriodInput) -> Result<Period, SessionError> {
        *self.periodo_titulo.lock().expect("periodo") = input.name.clone();
        *self.periodo_creado.lock().expect("periodo creado") = Some(input.clone());
        if self.datos == Datos::Captura {
            return Ok(Period {
                id: "p-nuevo".to_owned(),
                name: input.name.clone(),
                start_date: input.start_date.clone(),
                end_date: input.end_date.clone(),
                kind: input.kind.clone(),
                ..Period::default()
            });
        }
        Ok(Period {
            id: "p-new".to_owned(),
            name: input.name.clone(),
            start_date: input.start_date.clone(),
            end_date: input.end_date.clone(),
            kind: input.kind.clone(),
            ..Period::default()
        })
    }

    async fn propose_digest(
        &self,
        since: &str,
        sources: &[DigestSource],
    ) -> Result<Digest, SessionError> {
        *self.digest_desde.lock().expect("desde") = since.to_owned();
        *self.digest_propuesto.lock().expect("propuesta") =
            Some((since.to_owned(), sources.to_vec()));
        if self.datos == Datos::Captura {
            return Ok(serde_json::from_value::<Digest>(json!({
                "digest_id": "d-1",
                "expires_at": "2026-11-14T00:00:00Z",
                "next_step": "elige los candidatos"
            }))
            .expect("digest de captura"));
        }
        let titulo = sources
            .first()
            .and_then(|fuente| fuente.items.as_ref())
            .and_then(|items| items.first())
            .map(|item| item.title.clone())
            .unwrap_or_default();
        Ok(Digest {
            id: "dig-1".to_owned(),
            candidates: Some(vec![DigestCandidate {
                id: "c1".to_owned(),
                title: titulo,
                ..DigestCandidate::default()
            }]),
            ..Digest::default()
        })
    }

    async fn apply_digest(
        &self,
        digest_id: &str,
        accept: &[String],
    ) -> Result<String, SessionError> {
        if self.datos == Datos::Captura && (digest_id.is_empty() || accept.is_empty()) {
            return Err(SessionError::Tool("no hay nada seleccionado".to_owned()));
        }
        *self.aplicado.lock().expect("aplicado") = format!("{digest_id}:{}", accept.join(","));
        *self.digest_aplicado.lock().expect("digest aplicado") =
            Some((digest_id.to_owned(), accept.to_vec()));
        if self.datos == Datos::Captura {
            return Ok("Listo: 2 candidatos".to_owned());
        }
        Ok("aplicado".to_owned())
    }

    async fn send_alert(
        &self,
        kind: &str,
        message: &str,
        team: &str,
        user: &str,
    ) -> Result<(), SessionError> {
        if self.datos == Datos::Captura && message.trim().is_empty() {
            return Err(SessionError::Tool("el aviso está vacío".to_owned()));
        }
        *self.aviso_tipo.lock().expect("tipo de aviso") = kind.to_owned();
        *self.aviso_texto.lock().expect("texto de aviso") = message.to_owned();
        *self.aviso_enviado.lock().expect("aviso enviado") = Some((
            kind.to_owned(),
            message.to_owned(),
            team.to_owned(),
            user.to_owned(),
        ));
        Ok(())
    }

    async fn react(&self, message_id: &str, emoji: &str) -> Result<(), SessionError> {
        if self.datos == Datos::Captura && (message_id.is_empty() || emoji.is_empty()) {
            return Err(SessionError::Tool("reacción incompleta".to_owned()));
        }
        *self.reaccion.lock().expect("reacción") = format!("{message_id}:{emoji}");
        *self.reaccion_enviada.lock().expect("reacción enviada") =
            Some((message_id.to_owned(), emoji.to_owned()));
        Ok(())
    }

    async fn update_context(
        &self,
        focus: &str,
        mood: &str,
        notes: &str,
    ) -> Result<(), SessionError> {
        if self.datos == Datos::Captura && focus.is_empty() && mood.is_empty() && notes.is_empty() {
            return Err(SessionError::Tool("no hay nada que actualizar".to_owned()));
        }
        *self.foco.lock().expect("foco") = focus.to_owned();
        *self.animo.lock().expect("ánimo") = mood.to_owned();
        *self.contexto_actualizado.lock().expect("contexto") =
            Some((focus.to_owned(), mood.to_owned(), notes.to_owned()));
        Ok(())
    }

    async fn upload_advance(
        &self,
        task_id: &str,
        name: &str,
        mime: &str,
        data: &[u8],
        note: &str,
    ) -> Result<String, SessionError> {
        if self.sin_espacio {
            return Err(SessionError::SinEspacio("cuota llena".to_owned()));
        }
        self.subidas.fetch_add(1, Ordering::Relaxed);
        *self.subida_tarea.lock().expect("tarea subida") = task_id.to_owned();
        *self.subida_avance.lock().expect("avance subido") = Some((
            task_id.to_owned(),
            name.to_owned(),
            mime.to_owned(),
            data.to_vec(),
            note.to_owned(),
        ));
        Ok("u-1".to_owned())
    }

    async fn upload_chat_file(
        &self,
        room_id: &str,
        name: &str,
        mime: &str,
        data: &[u8],
        body: &str,
    ) -> Result<(), SessionError> {
        if self.sin_espacio {
            return Err(SessionError::SinEspacio("cuota llena".to_owned()));
        }
        self.subidas_chat.fetch_add(1, Ordering::Relaxed);
        *self.subida_sala.lock().expect("sala subida") = room_id.to_owned();
        *self.subida_chat.lock().expect("chat subido") = Some((
            room_id.to_owned(),
            name.to_owned(),
            mime.to_owned(),
            data.to_vec(),
            body.to_owned(),
        ));
        Ok(())
    }
}

/// Conector que entrega un `BackendDePrueba` nuevo en cada apertura.
pub struct ConectorPrueba;

#[async_trait]
impl Connector for ConectorPrueba {
    async fn open(&self, _: &str, _: &str, _: &str) -> Result<Arc<dyn Backend>, SessionError> {
        Ok(Arc::new(BackendDePrueba::default()))
    }
}

/// Conector que siempre entrega el mismo backend, para leer sus recorders.
pub struct ConectorCompartido(pub Arc<dyn Backend>);

#[async_trait]
impl Connector for ConectorCompartido {
    async fn open(&self, _: &str, _: &str, _: &str) -> Result<Arc<dyn Backend>, SessionError> {
        Ok(Arc::clone(&self.0))
    }
}

/// Lectura de `git log` con memoria y un fallo configurable.
#[derive(Default)]
pub struct GitLogDePrueba {
    visto: Mutex<Option<(String, Option<DateTime<Utc>>)>>,
    falla: bool,
    commit: bool,
}

impl GitLogDePrueba {
    /// Falla como una carpeta que no existe.
    pub fn que_falla() -> Self {
        Self {
            falla: true,
            ..Self::default()
        }
    }

    /// Lo último que recibió `log`.
    pub fn visto(&self) -> Option<(String, Option<DateTime<Utc>>)> {
        self.visto.lock().expect("git log").clone()
    }
}

impl GitLog for GitLogDePrueba {
    fn log(&self, dir: &str, since: Option<DateTime<Utc>>) -> Result<DigestSource, SessionError> {
        *self.visto.lock().expect("git log") = Some((dir.to_owned(), since));
        if self.falla {
            return Err(SessionError::Tool(format!("esa carpeta no existe: {dir}")));
        }
        if self.commit {
            return Ok(serde_json::from_value::<DigestSource>(json!({
                "name": dir,
                "items": [{"kind": "commit", "title": "Commit de prueba inmune"}]
            }))
            .expect("origen con commit"));
        }
        Ok(serde_json::from_value::<DigestSource>(json!({
            "name": "repo",
            "items": []
        }))
        .expect("origen vacío"))
    }
}

/// Levanta el router en un puerto efímero y devuelve su base.
pub async fn levantar(app: App) -> (String, tokio::task::JoinHandle<std::io::Result<()>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("escuchar");
    let addr = listener.local_addr().expect("dirección local");
    let server = tokio::spawn(orus_api::serve(listener, Arc::new(app)));
    (format!("http://{addr}"), server)
}

/// Levanta un `Api` ya armado (hub, git log, versión o timbre inyectados).
pub async fn levantar_api(api: Api) -> (String, tokio::task::JoinHandle<std::io::Result<()>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("escuchar");
    let addr = listener.local_addr().expect("dirección local");
    (format!("http://{addr}"), tokio::spawn(api.serve(listener)))
}

pub fn afirmar_cabeceras(response: &reqwest::Response) {
    assert_eq!(
        response.headers()["content-type"],
        "application/json; charset=utf-8"
    );
    assert_eq!(response.headers()["cache-control"], "no-store");
}

pub async fn cuerpo_json(response: reqwest::Response) -> Value {
    let texto = response.text().await.expect("leer el cuerpo");
    assert!(
        texto.ends_with('\n'),
        "el cuerpo no termina en \\n: {texto:?}"
    );
    serde_json::from_str(&texto).expect("JSON válido")
}
