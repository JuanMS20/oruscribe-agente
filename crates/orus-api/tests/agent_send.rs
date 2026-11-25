//! `POST /api/agent/send`: validaciones, turno armado y respuesta temprana.
//! Un `TurnRunner` falso frena el turno con un `Notify` para observar lo que
//! recibió antes de que termine, como la ventana real.

mod soporte;

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use orus_app::{
    Agent, AgentImage, App, ApprovalQueue, Backend, CancelToken, Connector, Conversations, Emisor,
    ErrorTurno, Session, SessionError, TurnRunner, Turno,
};
use orus_config::Settings;
use orus_org::doorbell::Config as DoorbellConfig;
use orus_org::{JsonObject, Me, MessagesPage, Room, Task, ToolSpec};
use serde_json::{Value, json};
use tokio::sync::Notify;

use soporte::{
    AppDePrueba, ConSesion, ConectorPrueba, RunnerOk, Secretos, SinDespertar, SinRed, SinSesion,
    afirmar_cabeceras, conversaciones_en, cuerpo_json, levantar,
};

struct BackendLento;

#[async_trait]
impl Backend for BackendLento {
    async fn me(&self) -> Result<Me, SessionError> {
        Ok(Me::default())
    }

    async fn tasks(&self) -> Result<Vec<Task>, SessionError> {
        Ok(Vec::new())
    }

    async fn rooms(&self) -> Result<Vec<Room>, SessionError> {
        Ok(Vec::new())
    }

    async fn messages(&self, _: &str, _: i64, _: i64) -> Result<MessagesPage, SessionError> {
        Ok(MessagesPage::default())
    }

    async fn send(&self, _: &str, _: &str, _: &str) -> Result<(), SessionError> {
        Ok(())
    }

    async fn tool_specs(&self) -> Result<Vec<ToolSpec>, SessionError> {
        tokio::time::sleep(Duration::from_secs(3)).await;
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
}

struct BackendSinSpecs;

#[async_trait]
impl Backend for BackendSinSpecs {
    async fn me(&self) -> Result<Me, SessionError> {
        Ok(Me::default())
    }

    async fn tasks(&self) -> Result<Vec<Task>, SessionError> {
        Ok(Vec::new())
    }

    async fn rooms(&self) -> Result<Vec<Room>, SessionError> {
        Ok(Vec::new())
    }

    async fn messages(&self, _: &str, _: i64, _: i64) -> Result<MessagesPage, SessionError> {
        Ok(MessagesPage::default())
    }

    async fn send(&self, _: &str, _: &str, _: &str) -> Result<(), SessionError> {
        Ok(())
    }

    async fn tool_specs(&self) -> Result<Vec<ToolSpec>, SessionError> {
        Err(SessionError::Tool("MCP caído".to_owned()))
    }

    async fn call_tool(&self, _: &str, _: JsonObject) -> Result<String, SessionError> {
        Ok(String::new())
    }

    async fn doorbell(&self) -> Result<DoorbellConfig, SessionError> {
        Err(SessionError::NoDoorbell)
    }
}

struct ConectorSinSpecs;

#[async_trait]
impl Connector for ConectorSinSpecs {
    async fn open(
        &self,
        _: &str,
        _: &str,
        _: &str,
    ) -> Result<Arc<dyn orus_app::Backend>, SessionError> {
        Ok(Arc::new(BackendSinSpecs))
    }
}

struct ConectorLento;

#[async_trait]
impl Connector for ConectorLento {
    async fn open(
        &self,
        _: &str,
        _: &str,
        _: &str,
    ) -> Result<Arc<dyn orus_app::Backend>, SessionError> {
        Ok(Arc::new(BackendLento))
    }
}

fn app_con_org_lenta(
    settings: Settings,
    conversaciones: Conversations,
    runner: Arc<dyn TurnRunner>,
) -> App {
    AppDePrueba::nuevo()
        .con_store(Box::new(ConSesion))
        .con_connector(Arc::new(ConectorLento))
        .con_settings(settings)
        .con_clave(Some("sk-12345678"))
        .con_conversaciones(conversaciones)
        .con_runner(runner)
        .construir()
}

/// Lo que la ruta armó, sin los tipos que no son `Debug`.
#[derive(Clone)]
struct TurnoVisto {
    base_url: String,
    modelo: String,
    modo: String,
    cwd: String,
    folders: Vec<String>,
    mensaje: String,
    imagenes: Vec<AgentImage>,
    clave: String,
    proveedor: bool,
    programas: Vec<String>,
    no_agents_md: bool,
    no_skills: bool,
    yes: bool,
    allow_exec: bool,
    es_tty: bool,
    memoria: bool,
    org_tools: Vec<String>,
    org_note: String,
    cost_input_per_mtok: f64,
    cost_output_per_mtok: f64,
}

#[derive(Default)]
struct Senal {
    empezado: Notify,
    liberar: Notify,
    terminado: Notify,
    visto: Mutex<Option<TurnoVisto>>,
}

struct RunnerSenal {
    senal: Arc<Senal>,
}

#[async_trait]
impl TurnRunner for RunnerSenal {
    async fn run(
        &self,
        turno: Turno,
        _: &mut Session,
        _: CancelToken,
        _: Emisor,
    ) -> Result<(), ErrorTurno> {
        let visto = TurnoVisto {
            base_url: turno.base_url,
            modelo: turno.modelo,
            modo: turno.modo,
            cwd: turno.cwd,
            folders: turno.folders,
            mensaje: turno.mensaje,
            imagenes: turno
                .imagenes
                .into_iter()
                .map(|imagen| AgentImage {
                    mime: imagen.mime,
                    data: imagen.data,
                    preview: imagen.preview,
                })
                .collect(),
            clave: turno.clave,
            proveedor: turno.proveedor.is_some(),
            programas: turno.programas,
            no_agents_md: turno.no_agents_md,
            no_skills: turno.no_skills,
            yes: turno.yes,
            allow_exec: turno.allow_exec,
            es_tty: turno.es_tty,
            memoria: turno.memoria.is_some(),
            org_tools: turno
                .org_tools
                .iter()
                .map(|herramienta| herramienta.spec().name)
                .collect(),
            org_note: turno.org_note,
            cost_input_per_mtok: turno.cost_input_per_mtok,
            cost_output_per_mtok: turno.cost_output_per_mtok,
        };
        {
            let mut guardado = self.senal.visto.lock().expect("guardar el turno");
            *guardado = Some(visto);
        }
        self.senal.empezado.notify_one();
        self.senal.liberar.notified().await;
        self.senal.terminado.notify_one();
        Ok(())
    }
}

fn ajustes(carpeta: Option<&Path>) -> Settings {
    let mut settings = Settings::default();
    settings.agent.kind = "openai".to_owned();
    settings.agent.base_url = "https://api.openai.com/v1".to_owned();
    settings.agent.model = "gpt-prueba".to_owned();
    settings.agent.mode = "ask".to_owned();
    settings.agent.programs = vec!["git".to_owned()];
    settings.agent.cost_input_per_mtok = 3.0;
    settings.agent.cost_output_per_mtok = 15.0;
    if let Some(carpeta) = carpeta {
        settings.agent.folders = vec![carpeta.to_string_lossy().into_owned()];
    }
    settings
}

fn app_de_prueba(
    settings: Settings,
    clave: Option<&str>,
    conversaciones: Conversations,
    runner: Arc<dyn TurnRunner>,
) -> App {
    AppDePrueba::nuevo()
        .con_store(Box::new(ConSesion))
        .con_connector(Arc::new(ConectorPrueba))
        .con_settings(settings)
        .con_clave(clave)
        .con_conversaciones(conversaciones)
        .con_runner(runner)
        .construir()
}

fn app_sin_sesion(
    settings: Settings,
    clave: Option<&str>,
    conversaciones: Conversations,
    runner: Arc<dyn TurnRunner>,
) -> App {
    AppDePrueba::nuevo()
        .con_settings(settings)
        .con_clave(clave)
        .con_conversaciones(conversaciones)
        .con_runner(runner)
        .construir()
}

/// Con sesión guardada y sin conector org: `client()` falla con `Connect`, no
/// con `LoggedOut`, y la ruta lo pinta 400 (`agent_send.rs:430`).
fn app_con_sesion_sin_org(
    settings: Settings,
    clave: Option<&str>,
    conversaciones: Conversations,
    runner: Arc<dyn TurnRunner>,
) -> App {
    let mut app = AppDePrueba::nuevo()
        .con_store(Box::new(ConSesion))
        .con_connector(Arc::new(SinRed))
        .con_settings(settings)
        .con_clave(clave)
        .con_conversaciones(conversaciones)
        .con_runner(runner)
        .construir();
    // `connect` reintenta tres veces con su backoff; sin red no hay nada que
    // esperar y el test no debe pagar esos segundos.
    app.set_connect_backoff(Duration::ZERO);
    app
}

async fn enviar(base: &str, cuerpo: Value) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{base}/api/agent/send"))
        .json(&cuerpo)
        .send()
        .await
        .expect("enviar el mensaje")
}

/// Arranca un turno válido y espera a que el runner avise que empezó: desde
/// ahí el agente queda ocupado hasta `senal.liberar`.
async fn ocupar(base: &str, senal: &Senal) {
    let primero = enviar(base, json!({"text": "primero"})).await;
    assert_eq!(primero.status(), 200);
    tokio::time::timeout(std::time::Duration::from_secs(5), senal.empezado.notified())
        .await
        .expect("el turno no arrancó");
}

/// Suelta el turno y espera a que el runner termine.
async fn liberar(senal: &Senal) {
    senal.liberar.notify_one();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        senal.terminado.notified(),
    )
    .await
    .expect("el turno no terminó");
}

#[tokio::test]
async fn envio_valido_responde_antes_y_el_runner_ve_el_turno() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let senal = Arc::new(Senal::default());
    let app = app_de_prueba(
        ajustes(Some(dir.path())),
        Some("sk-12345678"),
        conversaciones_en(&dir.path().join("conversaciones")),
        Arc::new(RunnerSenal {
            senal: Arc::clone(&senal),
        }),
    );
    let (base, server) = levantar(app).await;

    let response = enviar(&base, json!({"text": "  hola mundo  "})).await;
    assert_eq!(response.status(), 200, "el turno se acepta enseguida");
    afirmar_cabeceras(&response);
    assert_eq!(cuerpo_json(response).await, json!({"ok": true}));

    // El 200 ya llegó con el turno frenado: el runner avisa que empezó y espera
    // la señal para terminar.
    tokio::time::timeout(std::time::Duration::from_secs(5), senal.empezado.notified())
        .await
        .expect("el turno no arrancó");
    let visto = {
        let guardado = senal.visto.lock().expect("leer el turno");
        guardado.clone()
    }
    .expect("el runner recibió el turno");
    assert_eq!(visto.mensaje, "hola mundo", "el texto se recorta");
    assert_eq!(visto.base_url, "https://api.openai.com/v1");
    assert_eq!(visto.modelo, "gpt-prueba");
    assert_eq!(visto.modo, "ask");
    assert_eq!(visto.cwd, dir.path().to_string_lossy());
    assert_eq!(
        visto.folders,
        vec![dir.path().to_string_lossy().into_owned()],
        "todas las carpetas de los ajustes viajan al turno"
    );
    assert_eq!(visto.clave, "sk-12345678");
    assert!(
        !visto.proveedor,
        "sin cerebro org el turno no lleva proveedor"
    );
    assert_eq!(visto.programas, ["git"]);
    assert!(!visto.no_agents_md);
    assert!(!visto.no_skills);
    assert!(!visto.yes);
    assert!(!visto.allow_exec);
    assert!(!visto.es_tty);
    assert!(!visto.memoria, "la memoria queda para otra tarjeta");
    assert_eq!(
        visto.org_tools,
        ["get_my_tasks"],
        "las herramientas MCP de la organización viajan al turno"
    );
    assert_eq!(visto.org_note, "");
    assert_eq!(visto.cost_input_per_mtok, 3.0);
    assert_eq!(visto.cost_output_per_mtok, 15.0);

    senal.liberar.notify_one();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        senal.terminado.notified(),
    )
    .await
    .expect("el turno no terminó");
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn texto_vacio_devuelve_400() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let app = app_de_prueba(
        ajustes(Some(dir.path())),
        Some("sk-12345678"),
        conversaciones_en(&dir.path().join("conversaciones")),
        Arc::new(RunnerOk),
    );
    let (base, server) = levantar(app).await;

    let response = enviar(&base, json!({"text": "   "})).await;
    assert_eq!(response.status(), 400);
    let body = cuerpo_json(response).await;
    assert_eq!(body["error"], "escribe un mensaje o adjunta una imagen");

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn texto_de_mas_de_80000_devuelve_400() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let app = app_de_prueba(
        ajustes(Some(dir.path())),
        Some("sk-12345678"),
        conversaciones_en(&dir.path().join("conversaciones")),
        Arc::new(RunnerOk),
    );
    let (base, server) = levantar(app).await;

    let response = enviar(&base, json!({"text": "a".repeat(80_001)})).await;
    assert_eq!(response.status(), 400);
    let body = cuerpo_json(response).await;
    assert_eq!(
        body["error"],
        "el mensaje no puede tener más de 80000 caracteres"
    );

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn agente_ocupado_devuelve_409() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let senal = Arc::new(Senal::default());
    let app = app_sin_sesion(
        ajustes(Some(dir.path())),
        Some("sk-12345678"),
        conversaciones_en(&dir.path().join("conversaciones")),
        Arc::new(RunnerSenal {
            senal: Arc::clone(&senal),
        }),
    );
    let (base, server) = levantar(app).await;

    ocupar(&base, &senal).await;

    let segundo = enviar(&base, json!({"text": "segundo"})).await;
    assert_eq!(segundo.status(), 409);
    let body = cuerpo_json(segundo).await;
    assert_eq!(body["error"], "ya hay una ejecución del agente en curso");

    liberar(&senal).await;
    server.abort();
    let _ = server.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dos_envios_simultaneos_uno_recibe_200_y_el_otro_409() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let senal = Arc::new(Senal::default());
    let conversaciones = conversaciones_en(&dir.path().join("conversaciones"));
    let agente = Arc::new(Agent::new(
        conversaciones.clone(),
        Arc::new(ApprovalQueue::new()),
        Arc::new(RunnerSenal {
            senal: Arc::clone(&senal),
        }),
    ));
    let app = App::new(
        Box::new(SinSesion),
        ajustes(Some(dir.path())),
        Arc::new(ConectorPrueba),
        Arc::new(SinRed),
    )
    .with_conversations(Arc::new(conversaciones))
    .with_agent(agente)
    .with_secrets(Arc::new(Secretos(Some("sk-12345678".to_owned()))))
    .with_wake_log(Arc::new(SinDespertar));
    let (base, server) = levantar(app).await;

    let (uno, dos) = tokio::join!(
        enviar(&base, json!({"text": "uno"})),
        enviar(&base, json!({"text": "dos"})),
    );
    let mut estados = [uno.status().as_u16(), dos.status().as_u16()];
    estados.sort_unstable();
    assert_eq!(
        estados,
        [200, 409],
        "solo un envío reserva el agente antes de responder"
    );
    let perdedor = if uno.status() == 409 { uno } else { dos };
    let body = cuerpo_json(perdedor).await;
    assert_eq!(body["error"], "ya hay una ejecución del agente en curso");

    liberar(&senal).await;
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn ocupado_gana_a_texto_vacio() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let senal = Arc::new(Senal::default());
    let app = app_sin_sesion(
        ajustes(Some(dir.path())),
        Some("sk-12345678"),
        conversaciones_en(&dir.path().join("conversaciones")),
        Arc::new(RunnerSenal {
            senal: Arc::clone(&senal),
        }),
    );
    let (base, server) = levantar(app).await;

    ocupar(&base, &senal).await;

    let response = enviar(&base, json!({"text": "   "})).await;
    assert_eq!(response.status(), 409, "ocupado gana al texto vacío");
    let body = cuerpo_json(response).await;
    assert_eq!(body["error"], "ya hay una ejecución del agente en curso");

    liberar(&senal).await;
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn ocupado_gana_a_imagenes() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let senal = Arc::new(Senal::default());
    let app = app_sin_sesion(
        ajustes(Some(dir.path())),
        Some("sk-12345678"),
        conversaciones_en(&dir.path().join("conversaciones")),
        Arc::new(RunnerSenal {
            senal: Arc::clone(&senal),
        }),
    );
    let (base, server) = levantar(app).await;

    ocupar(&base, &senal).await;

    let response = enviar(
        &base,
        json!({
            "text": "mira esto",
            "images": [{"mime": "image/png", "data": "aG9sYQ=="}],
        }),
    )
    .await;
    assert_eq!(response.status(), 409, "ocupado gana a las imágenes");
    let body = cuerpo_json(response).await;
    assert_eq!(body["error"], "ya hay una ejecución del agente en curso");

    liberar(&senal).await;
    server.abort();
    let _ = server.await;
}

/// PNG de 1x1 transparente, el archivo real más pequeño que se puede mandar
/// (`agent_images_test.go:13`).
const PNG_MINIMO: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAFgwJ/lU7c5wAAAABJRU5ErkJggg==";

#[tokio::test]
async fn imagen_valida_llega_al_turno_y_responde_200() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let senal = Arc::new(Senal::default());
    let app = app_de_prueba(
        ajustes(Some(dir.path())),
        Some("sk-12345678"),
        conversaciones_en(&dir.path().join("conversaciones")),
        Arc::new(RunnerSenal {
            senal: Arc::clone(&senal),
        }),
    );
    let (base, server) = levantar(app).await;

    let previa = format!("data:image/png;base64,{PNG_MINIMO}");
    let response = enviar(
        &base,
        json!({
            "text": "  ",
            "images": [{"mime": "image/png", "data": PNG_MINIMO, "preview": previa.clone()}],
        }),
    )
    .await;
    assert_eq!(
        response.status(),
        200,
        "solo-imagen sin texto es un envío válido"
    );

    tokio::time::timeout(std::time::Duration::from_secs(5), senal.empezado.notified())
        .await
        .expect("el turno no arrancó");
    let visto = {
        let guardado = senal.visto.lock().expect("leer el turno");
        guardado.clone()
    }
    .expect("el runner recibió el turno");
    assert_eq!(visto.mensaje, "", "sin texto, el mensaje va vacío");
    assert_eq!(visto.imagenes.len(), 1, "la imagen llega al turno");
    assert_eq!(visto.imagenes[0].mime, "image/png");
    assert_eq!(
        visto.imagenes[0].data, PNG_MINIMO,
        "el data llega completo y re-codificado en base64"
    );
    assert_eq!(
        visto.imagenes[0].preview, previa,
        "la miniatura se conserva"
    );

    liberar(&senal).await;
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn imagen_invalida_devuelve_400() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let app = app_de_prueba(
        ajustes(Some(dir.path())),
        Some("sk-12345678"),
        conversaciones_en(&dir.path().join("conversaciones")),
        Arc::new(RunnerOk),
    );
    let (base, server) = levantar(app).await;

    let response = enviar(
        &base,
        json!({
            "text": "mira esto",
            "images": [{"mime": "image/png", "data": "aG9sYQ=="}],
        }),
    )
    .await;
    assert_eq!(response.status(), 400);
    let body = cuerpo_json(response).await;
    assert_eq!(body["error"], "el archivo no es la imagen que dice ser");

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn org_con_imagen_devuelve_400() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let mut settings = ajustes(None);
    settings.agent.kind = "org".to_owned();
    let app = app_de_prueba(
        settings,
        None,
        conversaciones_en(&dir.path().join("conversaciones")),
        Arc::new(RunnerOk),
    );
    let (base, server) = levantar(app).await;

    let response = enviar(
        &base,
        json!({
            "text": "mira",
            "images": [{"mime": "image/png", "data": PNG_MINIMO}],
        }),
    )
    .await;
    assert_eq!(response.status(), 400);
    let body = cuerpo_json(response).await;
    assert_eq!(
        body["error"],
        "el cerebro de la organización todavía no recibe imágenes: cambia a tu propia clave o quita la imagen"
    );

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn openai_sin_modelo_devuelve_400() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let mut settings = ajustes(Some(dir.path()));
    settings.agent.model.clear();
    let app = app_de_prueba(
        settings,
        Some("sk-12345678"),
        conversaciones_en(&dir.path().join("conversaciones")),
        Arc::new(RunnerOk),
    );
    let (base, server) = levantar(app).await;

    let response = enviar(&base, json!({"text": "hola"})).await;
    assert_eq!(response.status(), 400);
    let body = cuerpo_json(response).await;
    assert_eq!(body["error"], "configura el agente antes de usarlo");

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn org_sin_conexion_devuelve_400() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let mut settings = ajustes(None);
    settings.agent.kind = "org".to_owned();
    // Con sesión guardada y sin conector org: hay a quién preguntar y la
    // respuesta es «todavía no hay conexión», no «no has iniciado sesión».
    let app = app_con_sesion_sin_org(
        settings,
        None,
        conversaciones_en(&dir.path().join("conversaciones")),
        Arc::new(RunnerOk),
    );
    let (base, server) = levantar(app).await;

    let response = enviar(&base, json!({"text": "hola"})).await;
    assert_eq!(response.status(), 400);
    let body = cuerpo_json(response).await;
    assert_eq!(
        body["error"],
        "aún no hay conexión con Oruscribe: espera a estar en línea y vuelve a intentar"
    );

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn sin_sesion_con_mi_clave_arranca_el_agente_local() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let app = app_sin_sesion(
        ajustes(Some(dir.path())),
        Some("sk-12345678"),
        conversaciones_en(&dir.path().join("conversaciones")),
        Arc::new(RunnerOk),
    );
    let (base, server) = levantar(app).await;

    let response = enviar(&base, json!({"text": "hola"})).await;
    assert_eq!(
        response.status(),
        200,
        "Mi clave no puede exigir la sesión de Oruscribe"
    );

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn cuerpo_de_mas_de_24_mib_devuelve_413() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let app = app_de_prueba(
        ajustes(Some(dir.path())),
        Some("sk-12345678"),
        conversaciones_en(&dir.path().join("conversaciones")),
        Arc::new(RunnerOk),
    );
    let (base, server) = levantar(app).await;

    let response = reqwest::Client::new()
        .post(format!("{base}/api/agent/send"))
        .body(vec![b'a'; (24 << 20) + 1])
        .send()
        .await
        .expect("enviar el cuerpo grande");
    assert_eq!(response.status(), 413);
    let body = cuerpo_json(response).await;
    assert_eq!(body["code"], "cuerpo_grande");

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn send_no_espera_a_la_organizacion_lenta() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let senal = Arc::new(Senal::default());
    let app = app_con_org_lenta(
        ajustes(Some(dir.path())),
        conversaciones_en(&dir.path().join("conversaciones")),
        Arc::new(RunnerSenal {
            senal: Arc::clone(&senal),
        }),
    );
    let (base, server) = levantar(app).await;

    let inicio = std::time::Instant::now();
    let response = tokio::time::timeout(
        Duration::from_secs(2),
        enviar(&base, json!({"text": "hola"})),
    )
    .await
    .expect("el POST no debe esperar a tool_specs de la organización");
    assert_eq!(response.status(), 200);
    assert!(
        inicio.elapsed() < Duration::from_secs(2),
        "la respuesta tiene que llegar dentro de la ventana del frente"
    );

    tokio::time::timeout(Duration::from_secs(5), senal.empezado.notified())
        .await
        .expect("el turno no arrancó");
    let visto = {
        let guardado = senal.visto.lock().expect("leer el turno");
        guardado.clone()
    }
    .expect("el runner recibió el turno");
    assert_eq!(visto.org_tools, ["get_my_tasks"]);
    assert_eq!(visto.org_note, "");

    liberar(&senal).await;
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn send_durante_preparacion_lenta_devuelve_409() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let senal = Arc::new(Senal::default());
    let app = app_con_org_lenta(
        ajustes(Some(dir.path())),
        conversaciones_en(&dir.path().join("conversaciones")),
        Arc::new(RunnerSenal {
            senal: Arc::clone(&senal),
        }),
    );
    let (base, server) = levantar(app).await;

    let primero = tokio::time::timeout(
        Duration::from_secs(2),
        enviar(&base, json!({"text": "primero"})),
    )
    .await
    .expect("el primer POST no debe esperar a la organización");
    assert_eq!(primero.status(), 200);

    let segundo = tokio::time::timeout(
        Duration::from_secs(2),
        enviar(&base, json!({"text": "segundo"})),
    )
    .await
    .expect("el segundo POST no debe esperar a la organización");
    assert_eq!(segundo.status(), 409);
    let body = cuerpo_json(segundo).await;
    assert_eq!(body["error"], "ya hay una ejecución del agente en curso");

    tokio::time::timeout(Duration::from_secs(5), senal.empezado.notified())
        .await
        .expect("el turno no arrancó");
    liberar(&senal).await;
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn tool_specs_fallido_no_tumba_el_post_y_deja_nota_en_el_turno() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let senal = Arc::new(Senal::default());
    let app = AppDePrueba::nuevo()
        .con_store(Box::new(ConSesion))
        .con_connector(Arc::new(ConectorSinSpecs))
        .con_settings(ajustes(Some(dir.path())))
        .con_clave(Some("sk-12345678"))
        .con_conversaciones(conversaciones_en(&dir.path().join("conversaciones")))
        .con_runner(Arc::new(RunnerSenal {
            senal: Arc::clone(&senal),
        }))
        .construir();
    let (base, server) = levantar(app).await;

    let response = enviar(&base, json!({"text": "hola"})).await;
    assert_eq!(response.status(), 200);

    tokio::time::timeout(Duration::from_secs(5), senal.empezado.notified())
        .await
        .expect("el turno no arrancó");
    let visto = {
        let guardado = senal.visto.lock().expect("leer el turno");
        guardado.clone()
    }
    .expect("el runner recibió el turno");
    assert!(visto.org_tools.is_empty());
    assert!(
        visto
            .org_note
            .contains("no pude leer las herramientas de Oruscribe"),
        "la nota explica la falla: {}",
        visto.org_note
    );

    liberar(&senal).await;
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn sin_sesion_en_la_preparacion_deja_nota_y_no_tumba_el_post() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let senal = Arc::new(Senal::default());
    let app = app_sin_sesion(
        ajustes(Some(dir.path())),
        Some("sk-12345678"),
        conversaciones_en(&dir.path().join("conversaciones")),
        Arc::new(RunnerSenal {
            senal: Arc::clone(&senal),
        }),
    );
    let (base, server) = levantar(app).await;

    let response = enviar(&base, json!({"text": "hola"})).await;
    assert_eq!(response.status(), 200);

    tokio::time::timeout(Duration::from_secs(5), senal.empezado.notified())
        .await
        .expect("el turno no arrancó");
    let visto = {
        let guardado = senal.visto.lock().expect("leer el turno");
        guardado.clone()
    }
    .expect("el runner recibió el turno");
    assert!(visto.org_tools.is_empty());
    assert!(
        visto.org_note.contains("sin sesión de Oruscribe"),
        "la nota explica la falla: {}",
        visto.org_note
    );

    liberar(&senal).await;
    server.abort();
    let _ = server.await;
}

struct BackendTimbre {
    base: String,
    retardo: Duration,
    fallar: bool,
}

#[async_trait]
impl Backend for BackendTimbre {
    async fn me(&self) -> Result<Me, SessionError> {
        Ok(Me::default())
    }

    async fn tasks(&self) -> Result<Vec<Task>, SessionError> {
        Ok(Vec::new())
    }

    async fn rooms(&self) -> Result<Vec<Room>, SessionError> {
        Ok(Vec::new())
    }

    async fn messages(&self, _: &str, _: i64, _: i64) -> Result<MessagesPage, SessionError> {
        Ok(MessagesPage::default())
    }

    async fn send(&self, _: &str, _: &str, _: &str) -> Result<(), SessionError> {
        Ok(())
    }

    async fn tool_specs(&self) -> Result<Vec<ToolSpec>, SessionError> {
        Ok(Vec::new())
    }

    async fn call_tool(&self, _: &str, _: JsonObject) -> Result<String, SessionError> {
        Ok(String::new())
    }

    async fn doorbell(&self) -> Result<DoorbellConfig, SessionError> {
        tokio::time::sleep(self.retardo).await;
        if self.fallar {
            return Err(SessionError::NoDoorbell);
        }
        Ok(DoorbellConfig {
            supabase_url: self.base.clone(),
            ..DoorbellConfig::default()
        })
    }
}

struct ConectorTimbre {
    base: String,
    retardo: Duration,
    fallar: bool,
}

#[async_trait]
impl Connector for ConectorTimbre {
    async fn open(&self, _: &str, _: &str, _: &str) -> Result<Arc<dyn Backend>, SessionError> {
        Ok(Arc::new(BackendTimbre {
            base: self.base.clone(),
            retardo: self.retardo,
            fallar: self.fallar,
        }))
    }
}

fn ajustes_org(carpeta: Option<&Path>) -> Settings {
    let mut settings = ajustes(carpeta);
    settings.agent.kind = "org".to_owned();
    settings
}

fn app_con_timbre(
    settings: Settings,
    conversaciones: Conversations,
    runner: Arc<dyn TurnRunner>,
    base: &str,
    retardo: Duration,
    fallar: bool,
) -> App {
    AppDePrueba::nuevo()
        .con_store(Box::new(ConSesion))
        .con_connector(Arc::new(ConectorTimbre {
            base: base.to_owned(),
            retardo,
            fallar,
        }))
        .con_settings(settings)
        .con_clave(Some("sk-12345678"))
        .con_conversaciones(conversaciones)
        .con_runner(runner)
        .construir()
}

#[tokio::test]
async fn send_org_no_espera_al_timbre_lento() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let senal = Arc::new(Senal::default());
    let base_timbre = "https://org.example";
    let app = app_con_timbre(
        ajustes_org(Some(dir.path())),
        conversaciones_en(&dir.path().join("conversaciones")),
        Arc::new(RunnerSenal {
            senal: Arc::clone(&senal),
        }),
        base_timbre,
        Duration::from_secs(3),
        false,
    );
    let (base, server) = levantar(app).await;

    let inicio = std::time::Instant::now();
    let response = tokio::time::timeout(
        Duration::from_secs(2),
        enviar(&base, json!({"text": "hola"})),
    )
    .await
    .expect("el POST no debe esperar al timbre de la organización");
    assert_eq!(response.status(), 200);
    assert!(
        inicio.elapsed() < Duration::from_secs(2),
        "la respuesta tiene que llegar dentro de la ventana del frente"
    );

    tokio::time::timeout(Duration::from_secs(10), senal.empezado.notified())
        .await
        .expect("el turno no arrancó tras resolver el timbre");
    let visto = {
        let guardado = senal.visto.lock().expect("leer el turno");
        guardado.clone()
    }
    .expect("el runner recibió el turno");
    assert_eq!(visto.base_url, format!("{base_timbre}/functions/v1"));
    assert!(visto.proveedor, "el cerebro resuelto lleva su proveedor");
    assert_eq!(visto.org_note, "");

    liberar(&senal).await;
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn send_org_durante_timbre_lento_devuelve_409() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let senal = Arc::new(Senal::default());
    let app = app_con_timbre(
        ajustes_org(Some(dir.path())),
        conversaciones_en(&dir.path().join("conversaciones")),
        Arc::new(RunnerSenal {
            senal: Arc::clone(&senal),
        }),
        "https://org.example",
        Duration::from_secs(3),
        false,
    );
    let (base, server) = levantar(app).await;

    let primero = tokio::time::timeout(
        Duration::from_secs(2),
        enviar(&base, json!({"text": "primero"})),
    )
    .await
    .expect("el primer POST no debe esperar al timbre");
    assert_eq!(primero.status(), 200);

    let segundo = tokio::time::timeout(
        Duration::from_secs(2),
        enviar(&base, json!({"text": "segundo"})),
    )
    .await
    .expect("el segundo POST no debe esperar al timbre");
    assert_eq!(segundo.status(), 409);
    let body = cuerpo_json(segundo).await;
    assert_eq!(body["error"], "ya hay una ejecución del agente en curso");

    tokio::time::timeout(Duration::from_secs(10), senal.empezado.notified())
        .await
        .expect("el turno no arrancó tras resolver el timbre");
    liberar(&senal).await;
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn send_org_timbre_vacio_deja_el_error_visible_y_no_arranca_el_turno() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let senal = Arc::new(Senal::default());
    let app = app_con_timbre(
        ajustes_org(Some(dir.path())),
        conversaciones_en(&dir.path().join("conversaciones")),
        Arc::new(RunnerSenal {
            senal: Arc::clone(&senal),
        }),
        "",
        Duration::ZERO,
        false,
    );
    let agente = app.agent().expect("la app lleva el agente");
    let (base, server) = levantar(app).await;

    let response = tokio::time::timeout(
        Duration::from_secs(2),
        enviar(&base, json!({"text": "hola"})),
    )
    .await
    .expect("un timbre vacío no puede tumbar el POST");
    assert_eq!(response.status(), 200);

    for _ in 0..50 {
        if !agente.ocupado() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        !agente.ocupado(),
        "la preparación fallida dejó el agente ocupado"
    );
    assert!(
        agente.error().contains("aún no hay conexión con Oruscribe"),
        "el error explica la falla: {}",
        agente.error()
    );
    assert!(
        senal.visto.lock().expect("leer el turno").is_none(),
        "sin proveedor el turno no debe arrancar"
    );

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn send_org_timbre_fallido_deja_el_error_visible_y_no_arranca_el_turno() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let senal = Arc::new(Senal::default());
    let app = app_con_timbre(
        ajustes_org(Some(dir.path())),
        conversaciones_en(&dir.path().join("conversaciones")),
        Arc::new(RunnerSenal {
            senal: Arc::clone(&senal),
        }),
        "https://org.example",
        Duration::ZERO,
        true,
    );
    let agente = app.agent().expect("la app lleva el agente");
    let (base, server) = levantar(app).await;

    let response = tokio::time::timeout(
        Duration::from_secs(2),
        enviar(&base, json!({"text": "hola"})),
    )
    .await
    .expect("un timbre caído no puede tumbar el POST");
    assert_eq!(response.status(), 200);

    for _ in 0..50 {
        if !agente.ocupado() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        !agente.ocupado(),
        "la preparación fallida dejó el agente ocupado"
    );
    assert!(
        agente.error().contains("aún no hay conexión con Oruscribe"),
        "el error explica la falla: {}",
        agente.error()
    );
    assert!(
        senal.visto.lock().expect("leer el turno").is_none(),
        "sin proveedor el turno no debe arrancar"
    );

    server.abort();
    let _ = server.await;
}
