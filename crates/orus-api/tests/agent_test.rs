//! `POST /api/agent/test`: prueba el proveedor sin guardar nada.
//!
//! Los tests corren en un proceso hijo con `ORUSCRIBE_DATA_DIR` temporal, como
//! `agent_config.rs`: `std::env::set_var` es `unsafe` en edition 2024 y este
//! crate prohíbe `unsafe`. La costura del proveedor evita salir a la red.

mod soporte;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use orus_app::{
    Agent, App, ApprovalQueue, CancelToken, ChatMessage, ChatReply, ChatRequest, Conversations,
    Emisor, ErrorTurno, ProveedorFn, Provider, ProviderError, Secrets, Session, TurnRunner, Turno,
};
use orus_config::Settings;
use orus_org::doorbell::Config as DoorbellConfig;
use serde_json::{Value, json};

use soporte::{AppDePrueba, SinRed, SinSesion, conversaciones_en, cuerpo_json, levantar};

const MARCA_HIJO: &str = "ORUSCRIBE_AGENT_TEST_HIJO";
const MARCA_HECHO: &str = "ORUSCRIBE_AGENT_TEST_HECHO";

/// Clave de mentira con memoria: `get` devuelve lo último puesto y `puestos`
/// deja ver si la sonda la escribió.
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

    fn puestos(&self) -> Vec<String> {
        self.puestos.lock().expect("claves puestas").clone()
    }
}

impl Secrets for Secretos {
    fn get(&self) -> Result<Option<String>, String> {
        Ok(self.valor.lock().expect("clave de prueba").clone())
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

/// Lo que el proveedor falso vio: la costura guarda base y clave, y el `chat`
/// guarda el modelo y el primer mensaje.
#[derive(Default)]
struct Registro {
    credenciales: Vec<(String, String)>,
    modelo: String,
    mensaje: String,
}

type RegistroCompartido = Arc<Mutex<Registro>>;

#[derive(Clone, Copy)]
enum Respuesta {
    Ok(&'static str),
    Falla(&'static str),
}

struct ProveedorFalso {
    registro: RegistroCompartido,
    respuesta: Respuesta,
}

#[async_trait]
impl Provider for ProveedorFalso {
    async fn chat(&self, peticion: ChatRequest) -> Result<ChatReply, ProviderError> {
        {
            let mut registro = self.registro.lock().expect("registro");
            registro.modelo = peticion.model;
            registro.mensaje = peticion
                .messages
                .first()
                .map(|mensaje| mensaje.content.clone())
                .unwrap_or_default();
        }
        match self.respuesta {
            Respuesta::Ok(texto) => Ok(ChatReply {
                message: ChatMessage {
                    role: "assistant".to_owned(),
                    content: texto.to_owned(),
                    ..ChatMessage::default()
                },
                prompt_tokens: 0,
                completion_tokens: 0,
            }),
            Respuesta::Falla(error) => Err(ProviderError::local(error.to_owned())),
        }
    }
}

fn costura(registro: &RegistroCompartido, respuesta: Respuesta) -> ProveedorFn {
    let registro = Arc::clone(registro);
    Arc::new(move |base, clave| {
        registro
            .lock()
            .expect("registro")
            .credenciales
            .push((base.to_owned(), clave.to_owned()));
        Ok(Arc::new(ProveedorFalso {
            registro: Arc::clone(&registro),
            respuesta,
        }) as Arc<dyn Provider>)
    })
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

async fn probar(base: &str, cuerpo: Value) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{base}/api/agent/test"))
        .json(&cuerpo)
        .send()
        .await
        .expect("probar el agente")
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

fn ajustes_openai(base: &str, modelo: &str) -> Settings {
    let mut settings = Settings::default();
    settings.agent.kind = "openai".to_owned();
    settings.agent.base_url = base.to_owned();
    settings.agent.model = modelo.to_owned();
    settings.agent.mode = "ask".to_owned();
    settings
}

#[test]
fn base_vacia_da_400() {
    en_datos_aislados("base_vacia_da_400", || {
        con_runtime(async {
            let dir = tempfile::tempdir().expect("conversaciones");
            let mut settings = Settings::default();
            settings.agent.base_url.clear();
            let app = app_de_prueba(
                settings,
                Arc::new(Secretos::nuevo(Some("sk-guardada"))),
                conversaciones_en(dir.path()),
            );
            let (base, server) = levantar(app).await;

            let response = probar(&base, json!({"kind": "openai", "model": "gpt"})).await;
            let (status, error) = error_de(response).await;
            assert_eq!(status, 400);
            assert_eq!(error, "escribe la dirección del proveedor");

            server.abort();
            let _ = server.await;
        });
    });
}

#[test]
fn base_invalida_da_400() {
    en_datos_aislados("base_invalida_da_400", || {
        con_runtime(async {
            let dir = tempfile::tempdir().expect("conversaciones");
            let app = app_de_prueba(
                Settings::default(),
                Arc::new(Secretos::nuevo(None)),
                conversaciones_en(dir.path()),
            );
            let (base, server) = levantar(app).await;

            let response = probar(
                &base,
                json!({
                    "kind": "openai",
                    "base_url": "no-es-url",
                    "model": "gpt",
                    "api_key": "sk-prueba"
                }),
            )
            .await;
            let (status, error) = error_de(response).await;
            assert_eq!(status, 400);
            assert_eq!(error, "la dirección del proveedor no es válida");

            server.abort();
            let _ = server.await;
        });
    });
}

#[test]
fn sin_clave_guardada_da_400() {
    en_datos_aislados("sin_clave_guardada_da_400", || {
        con_runtime(async {
            let dir = tempfile::tempdir().expect("conversaciones");
            let app = app_de_prueba(
                ajustes_openai("https://api.viejo/v1", "modelo-viejo"),
                Arc::new(Secretos::nuevo(None)),
                conversaciones_en(dir.path()),
            );
            let (base, server) = levantar(app).await;

            let response = probar(
                &base,
                json!({
                    "kind": "openai",
                    "base_url": "https://api.viejo/v1",
                    "model": "modelo-viejo"
                }),
            )
            .await;
            let (status, error) = error_de(response).await;
            assert_eq!(status, 400);
            assert_eq!(error, "configura el agente antes de usarlo");

            server.abort();
            let _ = server.await;
        });
    });
}

#[test]
fn guardado_org_con_kind_vacio_prueba_openai_con_la_base_guardada() {
    en_datos_aislados(
        "guardado_org_con_kind_vacio_prueba_openai_con_la_base_guardada",
        || {
            con_runtime(async {
                let registro = Arc::new(Mutex::new(Registro::default()));
                let mut settings = Settings::default();
                settings.agent.kind = "org".to_owned();
                settings.agent.base_url = "https://api.guardado/v1".to_owned();
                settings.agent.model = "modelo-guardado".to_owned();
                let dir = tempfile::tempdir().expect("conversaciones");
                let app = app_de_prueba(
                    settings,
                    Arc::new(Secretos::nuevo(Some("sk-guardada"))),
                    conversaciones_en(dir.path()),
                )
                .with_proveedor(costura(&registro, Respuesta::Ok("ok")));
                let (base, server) = levantar(app).await;

                let response = probar(&base, json!({"kind": ""})).await;
                assert_eq!(response.status(), 200);
                let sonda = cuerpo_json(response).await;
                assert_eq!(sonda["ok"], true);
                assert_eq!(sonda["reply"], "ok");
                {
                    let registro = registro.lock().expect("registro");
                    assert_eq!(
                        registro.credenciales,
                        vec![(
                            "https://api.guardado/v1".to_owned(),
                            "sk-guardada".to_owned()
                        )],
                        "kind vacío usa openai con la base y la clave guardadas"
                    );
                    assert_eq!(registro.modelo, "modelo-guardado");
                }

                server.abort();
                let _ = server.await;
            });
        },
    );
}

#[test]
fn busy_con_kind_invalido_da_400_antes_de_mirar_ocupado() {
    en_datos_aislados(
        "busy_con_kind_invalido_da_400_antes_de_mirar_ocupado",
        || {
            con_runtime(async {
                let iniciado = Arc::new(tokio::sync::Notify::new());
                let liberar = Arc::new(tokio::sync::Notify::new());
                let dir = tempfile::tempdir().expect("conversaciones");
                let conversaciones = conversaciones_en(dir.path());
                let agente = Arc::new(Agent::new(
                    conversaciones.clone(),
                    Arc::new(ApprovalQueue::new()),
                    Arc::new(RunnerBloqueado {
                        iniciado: Arc::clone(&iniciado),
                        liberar: Arc::clone(&liberar),
                    }),
                ));
                let app = App::new(
                    Box::new(SinSesion),
                    Settings::default(),
                    Arc::new(SinRed),
                    Arc::new(SinRed),
                )
                .with_conversations(Arc::new(conversaciones))
                .with_agent(Arc::clone(&agente))
                .with_secrets(Arc::new(Secretos::nuevo(Some("sk"))));
                let (base, server) = levantar(app).await;

                let tarea =
                    tokio::spawn(async move { agente.send("hola", turno_de_prueba()).await });
                iniciado.notified().await;

                let response = probar(&base, json!({"kind": "otro"})).await;
                let (status, error) = error_de(response).await;
                assert_eq!(status, 400, "el kind inválido gana a estar ocupado");
                assert_eq!(error, "el cerebro debe ser org u openai");

                liberar.notify_one();
                let _ = tarea.await;
                server.abort();
                let _ = server.await;
            });
        },
    );
}

#[test]
fn busy_valido_da_200_con_el_error() {
    en_datos_aislados("busy_valido_da_200_con_el_error", || {
        con_runtime(async {
            let iniciado = Arc::new(tokio::sync::Notify::new());
            let liberar = Arc::new(tokio::sync::Notify::new());
            let dir = tempfile::tempdir().expect("conversaciones");
            let conversaciones = conversaciones_en(dir.path());
            let agente = Arc::new(Agent::new(
                conversaciones.clone(),
                Arc::new(ApprovalQueue::new()),
                Arc::new(RunnerBloqueado {
                    iniciado: Arc::clone(&iniciado),
                    liberar: Arc::clone(&liberar),
                }),
            ));
            let app = App::new(
                Box::new(SinSesion),
                Settings::default(),
                Arc::new(SinRed),
                Arc::new(SinRed),
            )
            .with_conversations(Arc::new(conversaciones))
            .with_agent(Arc::clone(&agente))
            .with_secrets(Arc::new(Secretos::nuevo(Some("sk"))));
            let (base, server) = levantar(app).await;

            let tarea = tokio::spawn(async move { agente.send("hola", turno_de_prueba()).await });
            iniciado.notified().await;

            let response = probar(&base, json!({"kind": "openai"})).await;
            assert_eq!(response.status(), 200, "ocupado no es 409 en esta ruta");
            let sonda = cuerpo_json(response).await;
            assert_eq!(sonda["ok"], false);
            assert_eq!(sonda["ms"], 0);
            assert_eq!(
                sonda["error"],
                "espera a que termine la ejecución en curso y vuelve a probar"
            );
            assert!(sonda.get("reply").is_none(), "sin reply: {sonda}");

            liberar.notify_one();
            let _ = tarea.await;
            server.abort();
            let _ = server.await;
        });
    });
}

#[test]
fn org_sin_timbre_da_200_con_el_texto() {
    en_datos_aislados("org_sin_timbre_da_200_con_el_texto", || {
        con_runtime(async {
            let dir = tempfile::tempdir().expect("conversaciones");
            let app = app_de_prueba(
                Settings::default(),
                Arc::new(Secretos::nuevo(None)),
                conversaciones_en(dir.path()),
            );
            let (base, server) = levantar(app).await;

            let response = probar(&base, json!({"kind": "org"})).await;
            assert_eq!(response.status(), 200);
            let sonda = cuerpo_json(response).await;
            assert_eq!(sonda["ok"], false);
            assert_eq!(
                sonda["error"],
                "aún no hay conexión con Oruscribe: espera a estar en línea"
            );

            server.abort();
            let _ = server.await;
        });
    });
}

#[test]
fn org_con_base_y_sin_token_da_200_con_el_error_del_cerebro() {
    en_datos_aislados(
        "org_con_base_y_sin_token_da_200_con_el_error_del_cerebro",
        || {
            con_runtime(async {
                let dir = tempfile::tempdir().expect("conversaciones");
                let app = app_de_prueba(
                    Settings::default(),
                    Arc::new(Secretos::nuevo(None)),
                    conversaciones_en(dir.path()),
                );
                app.set_bell_config(DoorbellConfig {
                    supabase_url: "https://proyecto.supabase.co/".to_owned(),
                    ..DoorbellConfig::default()
                });
                let (base, server) = levantar(app).await;

                let response = probar(&base, json!({"kind": "org"})).await;
                assert_eq!(response.status(), 200);
                let sonda = cuerpo_json(response).await;
                assert_eq!(sonda["ok"], false);
                assert_eq!(
                    sonda["error"],
                    "el cerebro de la organización necesita una sesión activa: inicia sesión y vuelve a intentar"
                );

                server.abort();
                let _ = server.await;
            });
        },
    );
}

#[test]
fn proveedor_falso_responde_ok() {
    en_datos_aislados("proveedor_falso_responde_ok", || {
        con_runtime(async {
            let registro = Arc::new(Mutex::new(Registro::default()));
            let secretos = Arc::new(Secretos::nuevo(Some("sk-guardada")));
            let dir = tempfile::tempdir().expect("conversaciones");
            let app = app_de_prueba(
                ajustes_openai("https://api.viejo/v1", "modelo-viejo"),
                secretos.clone(),
                conversaciones_en(dir.path()),
            )
            .with_proveedor(costura(&registro, Respuesta::Ok("  ok  ")));
            let (base, server) = levantar(app).await;

            let response = probar(
                &base,
                json!({
                    "kind": " openai ",
                    "base_url": " https://api.ejemplo/v1 ",
                    "model": " gpt-prueba ",
                    "api_key": " sk-prueba "
                }),
            )
            .await;
            assert_eq!(response.status(), 200);
            let sonda = cuerpo_json(response).await;
            assert_eq!(sonda["ok"], true);
            assert_eq!(sonda["reply"], "ok", "la respuesta se recorta");
            assert!(sonda.get("error").is_none(), "sin error: {sonda}");
            assert!(
                sonda.get("ms").and_then(Value::as_i64).is_some(),
                "ms viaja siempre: {sonda}"
            );
            {
                let registro = registro.lock().expect("registro");
                assert_eq!(
                    registro.credenciales,
                    vec![("https://api.ejemplo/v1".to_owned(), "sk-prueba".to_owned())]
                );
                assert_eq!(registro.modelo, "gpt-prueba", "el modelo se recorta");
                assert_eq!(registro.mensaje, "Responde solo: ok");
            }
            assert!(secretos.puestos().is_empty(), "probar no guarda la clave");

            server.abort();
            let _ = server.await;
        });
    });
}

#[test]
fn proveedor_que_falla_da_200_con_el_error() {
    en_datos_aislados("proveedor_que_falla_da_200_con_el_error", || {
        con_runtime(async {
            let registro = Arc::new(Mutex::new(Registro::default()));
            let dir = tempfile::tempdir().expect("conversaciones");
            let app = app_de_prueba(
                Settings::default(),
                Arc::new(Secretos::nuevo(None)),
                conversaciones_en(dir.path()),
            )
            .with_proveedor(costura(&registro, Respuesta::Falla("el proveedor falló")));
            let (base, server) = levantar(app).await;

            let response = probar(
                &base,
                json!({
                    "kind": "openai",
                    "base_url": "https://api.ejemplo/v1",
                    "model": "gpt",
                    "api_key": "sk-prueba"
                }),
            )
            .await;
            assert_eq!(response.status(), 200, "el fallo del proveedor no es 500");
            let sonda = cuerpo_json(response).await;
            assert_eq!(sonda["ok"], false);
            assert_eq!(sonda["error"], "el proveedor falló");
            assert!(sonda.get("reply").is_none(), "sin reply: {sonda}");

            server.abort();
            let _ = server.await;
        });
    });
}

#[test]
fn cuerpo_de_mas_de_64_kib_da_413() {
    en_datos_aislados("cuerpo_de_mas_de_64_kib_da_413", || {
        con_runtime(async {
            let dir = tempfile::tempdir().expect("conversaciones");
            let app = app_de_prueba(
                Settings::default(),
                Arc::new(Secretos::nuevo(None)),
                conversaciones_en(dir.path()),
            );
            let (base, server) = levantar(app).await;

            let relleno = "a".repeat((64 << 10) + 1);
            let response = reqwest::Client::new()
                .post(format!("{base}/api/agent/test"))
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
        });
    });
}

#[test]
fn get_agent_antes_y_despues_no_cambia() {
    en_datos_aislados("get_agent_antes_y_despues_no_cambia", || {
        con_runtime(async {
            let registro = Arc::new(Mutex::new(Registro::default()));
            let dir = tempfile::tempdir().expect("conversaciones");
            let app = app_de_prueba(
                ajustes_openai("https://api.viejo/v1", "modelo-viejo"),
                Arc::new(Secretos::nuevo(Some("sk-guardada"))),
                conversaciones_en(dir.path()),
            )
            .with_proveedor(costura(&registro, Respuesta::Ok("ok")));
            let (base, server) = levantar(app).await;

            let antes = cuerpo_json(
                reqwest::get(format!("{base}/api/agent"))
                    .await
                    .expect("vista antes"),
            )
            .await;

            let response = probar(
                &base,
                json!({
                    "kind": "openai",
                    "base_url": "https://api.nuevo/v1",
                    "model": "modelo-nuevo",
                    "api_key": "sk-nueva"
                }),
            )
            .await;
            assert_eq!(response.status(), 200);

            let despues = cuerpo_json(
                reqwest::get(format!("{base}/api/agent"))
                    .await
                    .expect("vista después"),
            )
            .await;
            assert_eq!(
                antes, despues,
                "probar no puede cambiar los ajustes guardados"
            );

            server.abort();
            let _ = server.await;
        });
    });
}

#[test]
fn host_distinto_con_clave_guardada_da_400() {
    en_datos_aislados("host_distinto_con_clave_guardada_da_400", || {
        con_runtime(async {
            let dir = tempfile::tempdir().expect("conversaciones");
            let app = app_de_prueba(
                ajustes_openai("https://api.viejo/v1", "modelo-viejo"),
                Arc::new(Secretos::nuevo(Some("sk-guardada"))),
                conversaciones_en(dir.path()),
            );
            let (base, server) = levantar(app).await;

            let response = probar(
                &base,
                json!({
                    "kind": "openai",
                    "base_url": "https://api.nuevo/v1",
                    "model": "modelo-viejo"
                }),
            )
            .await;
            let (status, error) = error_de(response).await;
            assert_eq!(status, 400);
            assert_eq!(
                error,
                "cambiaste de proveedor: escribe también la clave de ese proveedor"
            );

            server.abort();
            let _ = server.await;
        });
    });
}

#[test]
fn modelo_de_mas_de_200_da_400() {
    en_datos_aislados("modelo_de_mas_de_200_da_400", || {
        con_runtime(async {
            let dir = tempfile::tempdir().expect("conversaciones");
            let app = app_de_prueba(
                Settings::default(),
                Arc::new(Secretos::nuevo(None)),
                conversaciones_en(dir.path()),
            );
            let (base, server) = levantar(app).await;

            let modelo = "m".repeat(201);
            let response = probar(
                &base,
                json!({
                    "kind": "openai",
                    "base_url": "https://api.ejemplo/v1",
                    "model": modelo,
                    "api_key": "sk-prueba"
                }),
            )
            .await;
            let (status, error) = error_de(response).await;
            assert_eq!(status, 400);
            assert_eq!(error, "el modelo no puede tener más de 200 caracteres");

            server.abort();
            let _ = server.await;
        });
    });
}
