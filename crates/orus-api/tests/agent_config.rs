//! `POST /api/agent/config`: valida, guarda y devuelve la vista.
//!
//! Los tests que guardan corren en un proceso hijo con `ORUSCRIBE_DATA_DIR`
//! temporal: `std::env::set_var` es `unsafe` en edition 2024 y este crate
//! prohíbe `unsafe`. Las claves son de mentira y nunca tocan el llavero real.

mod soporte;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use orus_app::{
    Agent, App, ApprovalQueue, CancelToken, Conversations, Emisor, ErrorTurno, Secrets, Session,
    TurnRunner, Turno,
};
use orus_config::Settings;
use serde_json::{Value, json};

use soporte::{
    AppDePrueba, SinRed, SinSesion, afirmar_cabeceras, conversaciones_en, cuerpo_json, levantar,
};

const MARCA_HIJO: &str = "ORUSCRIBE_AGENT_CONFIG_TEST_HIJO";
const MARCA_HECHO: &str = "ORUSCRIBE_AGENT_CONFIG_TEST_HECHO";

/// Clave de mentira con memoria: `get` devuelve lo último puesto y `puestos`
/// deja ver si la ruta llamó a `set`.
struct Secretos {
    valor: Mutex<Option<String>>,
    puestos: Mutex<Vec<String>>,
}

impl Secretos {
    fn nuevo(valor: Option<&str>) -> Self {
        Self {
            valor: Mutex::new(valor.map(str::to_owned)),
            puestos: Mutex::new(Vec::new()),
        }
    }

    fn valor(&self) -> Option<String> {
        self.valor.lock().expect("clave de prueba").clone()
    }

    fn puestos(&self) -> Vec<String> {
        self.puestos.lock().expect("claves puestas").clone()
    }
}

impl Secrets for Secretos {
    fn get(&self) -> Result<Option<String>, String> {
        Ok(self.valor())
    }

    fn set(&self, value: &str) -> Result<(), String> {
        self.puestos
            .lock()
            .expect("claves puestas")
            .push(value.to_owned());
        *self.valor.lock().expect("clave de prueba") = Some(value.to_owned());
        Ok(())
    }

    fn delete(&self) -> Result<(), String> {
        *self.valor.lock().expect("clave de prueba") = None;
        Ok(())
    }
}

struct RunnerBloqueado {
    iniciado: Arc<tokio::sync::Notify>,
    liberar: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl TurnRunner for RunnerBloqueado {
    async fn run(
        &self,
        _: Turno,
        _: &mut Session,
        _: CancelToken,
        _: Emisor,
    ) -> Result<(), ErrorTurno> {
        self.iniciado.notify_one();
        self.liberar.notified().await;
        Ok(())
    }
}

fn app_de_prueba(
    settings: Settings,
    secretos: Arc<dyn Secrets>,
    conversaciones: Conversations,
) -> App {
    AppDePrueba::nuevo()
        .con_settings(settings)
        .con_secretos(secretos)
        .con_conversaciones(conversaciones)
        .construir()
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
        toolset: orus_app::Toolset::Full,
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

async fn configurar(base: &str, cuerpo: Value) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{base}/api/agent/config"))
        .json(&cuerpo)
        .send()
        .await
        .expect("pedir configuración")
}

async fn error_de(response: reqwest::Response) -> (u16, String) {
    let status = response.status().as_u16();
    let body = cuerpo_json(response).await;
    (
        status,
        body["error"].as_str().unwrap_or_default().to_owned(),
    )
}

/// Ejecuta `cuerpo` en un proceso hijo con su propio `ORUSCRIBE_DATA_DIR`.
fn en_datos_aislados(nombre: &str, cuerpo: impl FnOnce()) {
    if std::env::var_os(MARCA_HIJO).is_some() {
        cuerpo();
        if let Some(ruta) = std::env::var_os(MARCA_HECHO) {
            std::fs::write(ruta, b"ok").expect("marcar el test ejecutado");
        }
        return;
    }
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let hecho = dir.path().join("hecho");
    let salida = std::process::Command::new(std::env::current_exe().expect("binario de test"))
        .arg("--exact")
        .arg(nombre)
        .arg("--nocapture")
        .env("ORUSCRIBE_DATA_DIR", dir.path())
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

fn con_runtime<F: std::future::Future<Output = ()>>(futuro: F) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime de prueba")
        .block_on(futuro);
}

fn guardar_settings(valores: Value) {
    let datos = orus_config::data_dir();
    std::fs::create_dir_all(&datos).expect("carpeta de datos");
    std::fs::write(
        datos.join("settings.json"),
        serde_json::to_vec(&valores).expect("serializar ajustes"),
    )
    .expect("escribir ajustes");
}

fn ajustes_openai(base: &str, modelo: &str) -> Settings {
    let mut settings = Settings::default();
    settings.agent.kind = "openai".to_owned();
    settings.agent.base_url = base.to_owned();
    settings.agent.model = modelo.to_owned();
    settings.agent.mode = "ask".to_owned();
    settings
}

#[test]
fn config_valido_guarda_y_devuelve_la_vista() {
    en_datos_aislados("config_valido_guarda_y_devuelve_la_vista", || {
        con_runtime(async {
            let carpeta = tempfile::tempdir().expect("carpeta permitida");
            let conversaciones = tempfile::tempdir().expect("conversaciones");
            let secretos = Arc::new(Secretos::nuevo(None));
            let app = app_de_prueba(
                Settings::default(),
                secretos.clone(),
                conversaciones_en(conversaciones.path()),
            );
            let (base, server) = levantar(app).await;

            let response = configurar(
                &base,
                json!({
                    "kind": " openai ",
                    "base_url": " https://api.openai.com/v1 ",
                    "model": " gpt-nuevo ",
                    "mode": "ask",
                    "api_key": "sk-nueva-12345678",
                    "folders": [carpeta.path().to_string_lossy(), "   "],
                    "programs": ["git"],
                    "wake_enabled": true,
                    "wake_per_minute": 5,
                    "wake_per_day": 7,
                    "wake_tokens_per_day": 900,
                    "voice_stt_url": " http://127.0.0.1:9000/v1/ ",
                    "voice_tts_voice": "  voz  "
                }),
            )
            .await;
            assert_eq!(response.status(), 200);
            afirmar_cabeceras(&response);
            let vista = cuerpo_json(response).await;
            assert_eq!(vista["kind"], "openai");
            assert_eq!(vista["base_url"], "https://api.openai.com/v1");
            assert_eq!(vista["model"], "gpt-nuevo");
            assert_eq!(vista["mode"], "ask");
            assert_eq!(
                vista["folders"][0],
                carpeta.path().to_string_lossy().as_ref()
            );
            assert_eq!(
                vista["folders"].as_array().map(Vec::len),
                Some(1),
                "la carpeta en blanco se omite"
            );
            assert_eq!(vista["programs"][0], "git");
            assert_eq!(vista["wake_per_minute"], 5);
            assert_eq!(vista["key_hint"], "5678");
            assert_eq!(vista["voice_stt_url"], "http://127.0.0.1:9000/v1");
            assert_eq!(vista["voice_tts_voice"], "voz");
            assert_eq!(
                secretos.valor().as_deref(),
                Some("api.openai.com:443\nsk-nueva-12345678"),
                "la clave nueva queda en el llavero ligada a su host"
            );

            let disco: Value = serde_json::from_slice(
                &std::fs::read(orus_config::data_dir().join("settings.json"))
                    .expect("leer settings.json"),
            )
            .expect("settings.json válido");
            assert_eq!(disco["agent"]["model"], "gpt-nuevo");
            assert_eq!(
                disco["agent"]["folders"][0],
                carpeta.path().to_string_lossy().as_ref()
            );

            server.abort();
            let _ = server.await;
        });
    });
}

#[tokio::test]
async fn cuerpo_vacio_y_modelo_null_valen_cero() {
    let dir = tempfile::tempdir().expect("conversaciones");
    let app = app_de_prueba(
        Settings::default(),
        Arc::new(Secretos::nuevo(None)),
        conversaciones_en(dir.path()),
    );
    let (base, server) = levantar(app).await;

    let response = configurar(&base, json!({})).await;
    let (status, error) = error_de(response).await;
    assert_eq!(status, 400);
    assert_eq!(error, "la dirección del proveedor no es válida");

    let response = configurar(
        &base,
        json!({"base_url": "https://api.openai.com/v1", "model": null}),
    )
    .await;
    let (status, error) = error_de(response).await;
    assert_eq!(status, 400);
    assert_eq!(error, "escribe el modelo que quieres usar");

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn modelo_en_blanco_da_400() {
    let dir = tempfile::tempdir().expect("conversaciones");
    let app = app_de_prueba(
        Settings::default(),
        Arc::new(Secretos::nuevo(None)),
        conversaciones_en(dir.path()),
    );
    let (base, server) = levantar(app).await;

    let response = configurar(
        &base,
        json!({
            "kind": "openai",
            "base_url": "https://api.openai.com/v1",
            "model": "   ",
            "mode": "ask"
        }),
    )
    .await;
    let (status, error) = error_de(response).await;
    assert_eq!(status, 400);
    assert_eq!(error, "escribe el modelo que quieres usar");

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn modo_con_espacios_da_400() {
    let dir = tempfile::tempdir().expect("conversaciones");
    let app = app_de_prueba(
        Settings::default(),
        Arc::new(Secretos::nuevo(None)),
        conversaciones_en(dir.path()),
    );
    let (base, server) = levantar(app).await;

    let response = configurar(
        &base,
        json!({
            "kind": "openai",
            "base_url": "https://api.openai.com/v1",
            "model": "gpt-prueba",
            "mode": " ask "
        }),
    )
    .await;
    let (status, error) = error_de(response).await;
    assert_eq!(status, 400);
    assert_eq!(error, "el modo debe ser autonomous, ask o readonly");

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn kind_invalido_da_400() {
    let dir = tempfile::tempdir().expect("conversaciones");
    let app = app_de_prueba(
        Settings::default(),
        Arc::new(Secretos::nuevo(None)),
        conversaciones_en(dir.path()),
    );
    let (base, server) = levantar(app).await;

    let response = configurar(&base, json!({"kind": "otro"})).await;
    let (status, error) = error_de(response).await;
    assert_eq!(status, 400);
    assert_eq!(error, "el cerebro debe ser org u openai");

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn carpeta_relativa_da_400() {
    let dir = tempfile::tempdir().expect("conversaciones");
    let app = app_de_prueba(
        Settings::default(),
        Arc::new(Secretos::nuevo(None)),
        conversaciones_en(dir.path()),
    );
    let (base, server) = levantar(app).await;

    let response = configurar(
        &base,
        json!({
            "kind": "openai",
            "base_url": "https://api.openai.com/v1",
            "model": "gpt-prueba",
            "mode": "ask",
            "folders": ["carpeta-relativa"]
        }),
    )
    .await;
    let (status, error) = error_de(response).await;
    assert_eq!(status, 400);
    assert_eq!(
        error,
        "cada carpeta permitida tiene que ser una ruta absoluta"
    );

    server.abort();
    let _ = server.await;
}

#[test]
fn carpetas_vacias_se_aceptan() {
    en_datos_aislados("carpetas_vacias_se_aceptan", || {
        con_runtime(async {
            let conversaciones = tempfile::tempdir().expect("conversaciones");
            let app = app_de_prueba(
                Settings::default(),
                Arc::new(Secretos::nuevo(None)),
                conversaciones_en(conversaciones.path()),
            );
            let (base, server) = levantar(app).await;

            let response = configurar(
                &base,
                json!({
                    "kind": "openai",
                    "base_url": "https://api.openai.com/v1",
                    "model": "gpt-prueba",
                    "mode": "ask",
                    "folders": []
                }),
            )
            .await;
            assert_eq!(response.status(), 200);
            let vista = cuerpo_json(response).await;
            assert_eq!(vista["folders"].as_array().map(Vec::len), Some(0));

            server.abort();
            let _ = server.await;
        });
    });
}

#[tokio::test]
async fn programa_relativo_da_400() {
    let dir = tempfile::tempdir().expect("conversaciones");
    let app = app_de_prueba(
        Settings::default(),
        Arc::new(Secretos::nuevo(None)),
        conversaciones_en(dir.path()),
    );
    let (base, server) = levantar(app).await;

    let response = configurar(
        &base,
        json!({
            "kind": "openai",
            "base_url": "https://api.openai.com/v1",
            "model": "gpt-prueba",
            "mode": "ask",
            "programs": ["herramientas/git.exe"]
        }),
    )
    .await;
    let (status, error) = error_de(response).await;
    assert_eq!(status, 400);
    assert_eq!(
        error,
        "el programa \"herramientas/git.exe\" tiene que ser una ruta absoluta"
    );

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn voz_invalida_da_400() {
    let dir = tempfile::tempdir().expect("conversaciones");
    let app = app_de_prueba(
        Settings::default(),
        Arc::new(Secretos::nuevo(None)),
        conversaciones_en(dir.path()),
    );
    let (base, server) = levantar(app).await;

    let response = configurar(
        &base,
        json!({
            "kind": "openai",
            "base_url": "https://api.openai.com/v1",
            "model": "gpt-prueba",
            "mode": "ask",
            "voice_stt_url": "http://ejemplo.com/v1"
        }),
    )
    .await;
    let (status, error) = error_de(response).await;
    assert_eq!(status, 400);
    assert_eq!(
        error,
        "la dirección de voz no es válida: la dirección del proveedor debe usar https; http solo se permite en localhost"
    );

    let response = configurar(
        &base,
        json!({
            "kind": "openai",
            "base_url": "https://api.openai.com/v1",
            "model": "gpt-prueba",
            "mode": "ask",
            "voice_tts_voice": "x".repeat(121)
        }),
    )
    .await;
    let (status, error) = error_de(response).await;
    assert_eq!(status, 400);
    assert_eq!(error, "la voz no puede tener más de 120 caracteres");

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn wake_fuera_de_rango_da_400() {
    let dir = tempfile::tempdir().expect("conversaciones");
    let app = app_de_prueba(
        Settings::default(),
        Arc::new(Secretos::nuevo(None)),
        conversaciones_en(dir.path()),
    );
    let (base, server) = levantar(app).await;

    let casos = [
        (
            json!({"wake_per_minute": 31}),
            "las ejecuciones por minuto tienen que estar entre 0 y 30 (0 = valor por defecto)",
        ),
        (
            json!({"wake_per_day": 501}),
            "las ejecuciones por día tienen que estar entre 0 y 500 (0 = valor por defecto)",
        ),
        (
            json!({"wake_tokens_per_day": 200_001}),
            "el presupuesto de tokens por día tiene que estar entre 0 y 200000 (0 = valor por defecto)",
        ),
    ];
    for (extra, esperado) in casos {
        let mut cuerpo = json!({
            "kind": "openai",
            "base_url": "https://api.openai.com/v1",
            "model": "gpt-prueba",
            "mode": "ask"
        });
        if let (Some(destino), Some(fuente)) = (cuerpo.as_object_mut(), extra.as_object()) {
            for (clave, valor) in fuente {
                destino.insert(clave.clone(), valor.clone());
            }
        }
        let response = configurar(&base, cuerpo).await;
        let (status, error) = error_de(response).await;
        assert_eq!(status, 400, "{esperado}");
        assert_eq!(error, esperado);
    }

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn cambio_de_host_con_clave_da_400() {
    let dir = tempfile::tempdir().expect("conversaciones");
    let secretos = Arc::new(Secretos::nuevo(Some("sk-vieja-12345678")));
    let app = app_de_prueba(
        ajustes_openai("https://api.openai.com/v1", "gpt-prueba"),
        secretos.clone(),
        conversaciones_en(dir.path()),
    );
    let (base, server) = levantar(app).await;

    let response = configurar(
        &base,
        json!({
            "kind": "openai",
            "base_url": "https://otro-proveedor.example/v1",
            "model": "gpt-prueba",
            "mode": "ask",
            "api_key": ""
        }),
    )
    .await;
    let (status, error) = error_de(response).await;
    assert_eq!(status, 400);
    assert_eq!(
        error,
        "cambiaste de proveedor: escribe también la clave de ese proveedor; la anterior no se manda a otro sitio"
    );
    assert!(secretos.puestos().is_empty(), "sin clave nueva no se toca");

    server.abort();
    let _ = server.await;
}

#[test]
fn mismo_host_con_otro_path_no_da_400() {
    en_datos_aislados("mismo_host_con_otro_path_no_da_400", || {
        con_runtime(async {
            let conversaciones = tempfile::tempdir().expect("conversaciones");
            let secretos = Arc::new(Secretos::nuevo(Some("sk-vieja-12345678")));
            let app = app_de_prueba(
                ajustes_openai("https://api.openai.com/v1", "gpt-prueba"),
                secretos.clone(),
                conversaciones_en(conversaciones.path()),
            );
            let (base, server) = levantar(app).await;

            let response = configurar(
                &base,
                json!({
                    "kind": "openai",
                    "base_url": "https://api.openai.com/v2",
                    "model": "gpt-prueba",
                    "mode": "ask",
                    "api_key": ""
                }),
            )
            .await;
            assert_eq!(response.status(), 200);
            let vista = cuerpo_json(response).await;
            assert_eq!(vista["base_url"], "https://api.openai.com/v2");
            assert_eq!(
                secretos.valor().as_deref(),
                Some("sk-vieja-12345678"),
                "la clave anterior se conserva"
            );
            assert!(secretos.puestos().is_empty());

            server.abort();
            let _ = server.await;
        });
    });
}

#[test]
fn org_conserva_los_ajustes_del_arnes() {
    en_datos_aislados("org_conserva_los_ajustes_del_arnes", || {
        con_runtime(async {
            let carpeta = tempfile::tempdir().expect("carpeta permitida");
            guardar_settings(json!({
                "agent": {
                    "kind": "openai",
                    "base_url": "https://api.ejemplo/v1",
                    "model": "modelo-viejo",
                    "mode": "readonly",
                    "folders": [carpeta.path().to_string_lossy()],
                    "programs": ["git"],
                    "wake_enabled": true,
                    "wake_per_minute": 7,
                    "wake_per_day": 11,
                    "wake_tokens_per_day": 1234
                }
            }));

            let conversaciones = tempfile::tempdir().expect("conversaciones");
            let secretos = Arc::new(Secretos::nuevo(None));
            let app = app_de_prueba(
                Settings::default(),
                secretos.clone(),
                conversaciones_en(conversaciones.path()),
            );
            let (base, server) = levantar(app).await;

            let response = configurar(
                &base,
                json!({
                    "kind": "org",
                    "voice_tts_voice": "  voz-nueva  ",
                    "wake_enabled": true,
                    "wake_per_minute": 7,
                    "wake_per_day": 11,
                    "wake_tokens_per_day": 1234
                }),
            )
            .await;
            assert_eq!(response.status(), 200);
            let vista = cuerpo_json(response).await;
            assert_eq!(vista["kind"], "org");
            assert_eq!(vista["base_url"], "https://api.ejemplo/v1");
            assert_eq!(vista["model"], "modelo-viejo");
            assert_eq!(vista["mode"], "readonly");
            assert_eq!(
                vista["folders"][0],
                carpeta.path().to_string_lossy().as_ref()
            );
            assert_eq!(vista["programs"][0], "git");
            assert_eq!(vista["wake_enabled"], true);
            assert_eq!(vista["wake_per_minute"], 7);
            assert_eq!(vista["wake_per_day"], 11);
            assert_eq!(vista["wake_tokens_per_day"], 1234);
            assert_eq!(vista["voice_tts_voice"], "voz-nueva");
            assert!(secretos.puestos().is_empty(), "org no guarda clave");

            server.abort();
            let _ = server.await;
        });
    });
}

#[test]
fn org_aplica_valores_explicitos_de_despertar() {
    en_datos_aislados("org_aplica_valores_explicitos_de_despertar", || {
        con_runtime(async {
            guardar_settings(json!({
                "agent": {
                    "kind": "openai",
                    "wake_enabled": true,
                    "wake_per_minute": 7,
                    "wake_per_day": 11,
                    "wake_tokens_per_day": 1234
                }
            }));

            let conversaciones = tempfile::tempdir().expect("conversaciones");
            let secretos = Arc::new(Secretos::nuevo(None));
            let app = app_de_prueba(
                Settings::default(),
                secretos.clone(),
                conversaciones_en(conversaciones.path()),
            );
            let (base, server) = levantar(app).await;

            let response = configurar(
                &base,
                json!({
                    "kind": "org",
                    "wake_enabled": false,
                    "wake_per_minute": 0,
                    "wake_per_day": 0,
                    "wake_tokens_per_day": 0
                }),
            )
            .await;
            assert_eq!(response.status(), 200);
            let vista = cuerpo_json(response).await;
            assert_eq!(vista["kind"], "org");
            assert_eq!(vista["wake_enabled"], false);
            // 0 significa "valor por defecto": la vista devuelve 3/20/20000.
            assert_eq!(vista["wake_per_minute"], 3);
            assert_eq!(vista["wake_per_day"], 20);
            assert_eq!(vista["wake_tokens_per_day"], 20000);

            server.abort();
            let _ = server.await;
        });
    });
}

#[tokio::test]
async fn configuracion_ocupada_da_409() {
    let iniciado = Arc::new(tokio::sync::Notify::new());
    let liberar = Arc::new(tokio::sync::Notify::new());
    let runner = Arc::new(RunnerBloqueado {
        iniciado: Arc::clone(&iniciado),
        liberar: Arc::clone(&liberar),
    });
    let dir = tempfile::tempdir().expect("conversaciones");
    let conversaciones = conversaciones_en(dir.path());
    let agente = Arc::new(Agent::new(
        conversaciones.clone(),
        Arc::new(ApprovalQueue::new()),
        runner,
    ));
    let app = App::new(
        Box::new(SinSesion),
        Settings::default(),
        Arc::new(SinRed),
        Arc::new(SinRed),
    )
    .with_conversations(Arc::new(conversaciones))
    .with_agent(Arc::clone(&agente))
    .with_secrets(Arc::new(Secretos::nuevo(None)));
    let (base, server) = levantar(app).await;

    let tarea = tokio::spawn(async move { agente.send("hola", turno_de_prueba()).await });
    iniciado.notified().await;

    let response = configurar(&base, json!({})).await;
    assert_eq!(response.status(), 409);
    let body = cuerpo_json(response).await;
    assert_eq!(body["error"], "ya hay una ejecución del agente en curso");

    liberar.notify_one();
    let _ = tarea.await;
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn cuerpo_de_mas_de_64_kib_da_413() {
    let dir = tempfile::tempdir().expect("conversaciones");
    let app = app_de_prueba(
        Settings::default(),
        Arc::new(Secretos::nuevo(None)),
        conversaciones_en(dir.path()),
    );
    let (base, server) = levantar(app).await;

    let relleno = "a".repeat((64 << 10) + 1);
    let response = reqwest::Client::new()
        .post(format!("{base}/api/agent/config"))
        .header("content-type", "application/json")
        .body(format!(r#"{{"model":"{relleno}"}}"#))
        .send()
        .await
        .expect("cuerpo grande");
    assert_eq!(response.status(), 413);
    let body = cuerpo_json(response).await;
    assert_eq!(body["code"], "cuerpo_grande");
    assert_eq!(
        body["error"],
        "el cuerpo de la petición pasa del máximo de la ruta"
    );

    server.abort();
    let _ = server.await;
}

#[test]
fn fallo_de_guardado_restaura_la_clave_anterior() {
    en_datos_aislados("fallo_de_guardado_restaura_la_clave_anterior", || {
        con_runtime(async {
            // Un settings.json que es carpeta hace fallar el rename final.
            let datos = orus_config::data_dir();
            std::fs::create_dir_all(&datos).expect("carpeta de datos");
            std::fs::create_dir(datos.join("settings.json"))
                .expect("bloquear el settings.json con una carpeta");

            let conversaciones = tempfile::tempdir().expect("conversaciones");
            let secretos = Arc::new(Secretos::nuevo(Some("sk-vieja-12345678")));
            let app = app_de_prueba(
                ajustes_openai("https://api.viejo/v1", "modelo-viejo"),
                secretos.clone(),
                conversaciones_en(conversaciones.path()),
            );
            let (base, server) = levantar(app).await;

            let response = configurar(
                &base,
                json!({
                    "kind": "openai",
                    "base_url": "https://api.nuevo/v1",
                    "model": "modelo-nuevo",
                    "mode": "ask",
                    "api_key": "sk-nueva-12345678",
                    "folders": []
                }),
            )
            .await;
            assert_eq!(response.status(), 500);
            assert_eq!(
                secretos.valor().as_deref(),
                Some("sk-vieja-12345678"),
                "si falla el settings, la clave nueva no queda en el llavero"
            );

            let response = reqwest::Client::new()
                .get(format!("{base}/api/agent"))
                .send()
                .await
                .expect("pedir la vista");
            let vista = cuerpo_json(response).await;
            assert_eq!(vista["model"], "modelo-viejo");
            assert_eq!(vista["base_url"], "https://api.viejo/v1");

            server.abort();
            let _ = server.await;
        });
    });
}
