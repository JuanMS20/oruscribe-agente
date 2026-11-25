//! Rutas del agente: vista, diario, reglas, aprobaciones, conversaciones y
//! deshacer. El diario y `settings.json` viven en un `ORUSCRIBE_DATA_DIR`
//! temporal dentro de un proceso hijo: así ningún test toca el perfil real.

mod soporte;

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use orus_app::{
    Agent, App, ApprovalQueue, CancelToken, Conversations, Emisor, ErrorTurno, Session, TurnRunner,
    Turno,
};
use orus_config::Settings;
use serde_json::{Value, json};

use soporte::{
    AppDePrueba, RunnerOk, SinRed, SinSesion, afirmar_cabeceras, conversaciones_en, cuerpo_json,
    levantar,
};

const MARCA_HIJO: &str = "ORUSCRIBE_AGENT_TEST_HIJO";
const MARCA_HECHO: &str = "ORUSCRIBE_AGENT_TEST_HECHO";

struct RunnerBloqueado {
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
        self.liberar.notified().await;
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
    settings.agent.wake_enabled = true;
    settings.agent.voice_stt_url = "http://127.0.0.1:9000/v1".to_owned();
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
        .con_settings(settings)
        .con_clave(clave)
        .con_conversaciones(conversaciones)
        .con_runner(runner)
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

fn guardar_sesion(dir: &Path, id: &str, titulo: &str, actualizada: &str, mensajes: Value) {
    let sesion = json!({
        "version": 1,
        "id": id,
        "title": titulo,
        "created": actualizada,
        "updated": actualizada,
        "messages": mensajes,
        "tokens_in": 7,
        "tokens_out": 3,
        "cost_usd": 0.5,
    });
    std::fs::write(
        dir.join(format!("c-{id}.json")),
        serde_json::to_vec(&sesion).expect("serializar la sesión"),
    )
    .expect("escribir la sesión");
}

/// Ejecuta `cuerpo` en un proceso hijo con su propio `ORUSCRIBE_DATA_DIR`.
/// `std::env::set_var` es `unsafe` en edition 2024 y el crate prohíbe `unsafe`.
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

#[tokio::test]
async fn agente_view_expone_configuracion_y_listas_vacias() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let app = app_de_prueba(
        ajustes(Some(dir.path())),
        Some("sk-12345678"),
        conversaciones_en(&dir.path().join("conversaciones")),
        Arc::new(RunnerOk),
    );
    let (base, server) = levantar(app).await;

    let response = reqwest::Client::new()
        .get(format!("{base}/api/agent"))
        .send()
        .await
        .expect("pedir la vista");
    assert_eq!(response.status(), 200);
    afirmar_cabeceras(&response);
    let body = cuerpo_json(response).await;

    assert_eq!(body["configured"], true);
    assert_eq!(body["kind"], "openai");
    assert_eq!(body["base_url"], "https://api.openai.com/v1");
    assert_eq!(body["model"], "gpt-prueba");
    assert_eq!(body["mode"], "ask");
    assert_eq!(body["key_hint"], "5678");
    assert_eq!(body["running"], false);
    assert_eq!(body["folders"][0], dir.path().to_string_lossy().as_ref());
    assert_eq!(body["programs"][0], "git");
    assert_eq!(body["wake_enabled"], true);
    assert_eq!(body["wake_per_minute"], 3);
    assert_eq!(body["wake_per_day"], 20);
    assert_eq!(body["wake_tokens_per_day"], 20000);
    assert_eq!(body["voice_stt_url"], "http://127.0.0.1:9000/v1");
    for clave in [
        "messages",
        "steps",
        "approvals",
        "folders",
        "programs",
        "wake_log",
    ] {
        assert!(
            body[clave].is_array(),
            "{clave} debe ser una lista, no null: {body}"
        );
    }
    assert!(body.get("voice_stt_model").is_none(), "voz vacía omitida");
    assert!(body.get("cost_usd").is_none(), "costo cero omitido");
    assert!(body.get("last_error").is_none(), "error vacío omitido");

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn agente_view_org_sin_clave_esta_configurado() {
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

    let response = reqwest::Client::new()
        .get(format!("{base}/api/agent"))
        .send()
        .await
        .expect("pedir la vista");
    assert_eq!(response.status(), 200);
    let body = cuerpo_json(response).await;
    assert_eq!(body["configured"], true, "org no necesita clave");
    assert_eq!(body["key_hint"], "");

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn agente_view_key_corta_no_deja_pista() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let app = app_de_prueba(
        ajustes(None),
        Some("12345678"),
        conversaciones_en(&dir.path().join("conversaciones")),
        Arc::new(RunnerOk),
    );
    let (base, server) = levantar(app).await;

    let response = reqwest::Client::new()
        .get(format!("{base}/api/agent"))
        .send()
        .await
        .expect("pedir la vista");
    assert_eq!(response.status(), 200);
    let body = cuerpo_json(response).await;
    assert_eq!(body["key_hint"], "", "ocho o menos no muestra pista");
    assert_eq!(body["configured"], true, "hay clave y modelo");

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn proveedores_es_el_catalogo_ordenado() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let app = app_de_prueba(
        ajustes(None),
        None,
        conversaciones_en(&dir.path().join("conversaciones")),
        Arc::new(RunnerOk),
    );
    let (base, server) = levantar(app).await;

    let response = reqwest::Client::new()
        .get(format!("{base}/api/agent/providers"))
        .send()
        .await
        .expect("pedir proveedores");
    assert_eq!(response.status(), 200);
    afirmar_cabeceras(&response);
    let body = cuerpo_json(response).await;
    let proveedores = body.as_array().expect("lista de proveedores");
    assert!(!proveedores.is_empty(), "el catálogo no puede estar vacío");
    let nombres: Vec<&str> = proveedores
        .iter()
        .map(|proveedor| proveedor["name"].as_str().unwrap_or_default())
        .collect();
    let mut ordenados = nombres.clone();
    ordenados.sort_unstable();
    assert_eq!(nombres, ordenados, "el catálogo va ordenado por nombre");
    let openai = proveedores
        .iter()
        .find(|proveedor| proveedor["id"] == "openai")
        .expect("openai en el catálogo");
    assert_eq!(openai["supported"], true);
    assert_eq!(openai["base_url"], "https://api.openai.com/v1");
    assert!(
        openai.get("auth_bearer").is_none(),
        "auth_bearer falso se omite"
    );
    let google = proveedores
        .iter()
        .find(|proveedor| proveedor["id"] == "google")
        .expect("google en el catálogo");
    assert_eq!(google["supported"], false);
    assert!(
        google["reason"]
            .as_str()
            .is_some_and(|texto| !texto.is_empty()),
        "un proveedor sin adaptador explica por qué"
    );

    server.abort();
    let _ = server.await;
}

#[test]
fn diario_sin_archivo_responde_entries_vacio() {
    en_datos_aislados("diario_sin_archivo_responde_entries_vacio", || {
        con_runtime(async {
            let dir = tempfile::tempdir().expect("carpeta temporal");
            let app = app_de_prueba(
                Settings::default(),
                None,
                conversaciones_en(&dir.path().join("conversaciones")),
                Arc::new(RunnerOk),
            );
            let (base, server) = levantar(app).await;
            let response = reqwest::Client::new()
                .get(format!("{base}/api/agent/journal"))
                .send()
                .await
                .expect("pedir el diario");
            assert_eq!(response.status(), 200);
            let body = cuerpo_json(response).await;
            let entradas = body["entries"].as_array().expect("entries es lista");
            assert!(entradas.is_empty(), "sin diario la lista es vacía");
            server.abort();
            let _ = server.await;
        });
    });
}

#[test]
fn reglas_y_revocar_usan_el_settings_temporal() {
    en_datos_aislados("reglas_y_revocar_usan_el_settings_temporal", || {
        let datos = orus_config::data_dir();
        std::fs::create_dir_all(&datos).expect("carpeta de datos");
        std::fs::write(
            datos.join("settings.json"),
            serde_json::to_vec(&json!({
                "agent": {
                    "kind": "openai",
                    "model": "gpt-prueba",
                    "allow_rules": [{
                        "id": "abc",
                        "tool": "local_run_command",
                        "pattern": "program:git",
                        "created_at": "2026-01-01T00:00:00Z"
                    }]
                }
            }))
            .expect("serializar ajustes"),
        )
        .expect("escribir ajustes");

        con_runtime(async {
            let dir = tempfile::tempdir().expect("carpeta temporal");
            let app = app_de_prueba(
                Settings::default(),
                None,
                conversaciones_en(&dir.path().join("conversaciones")),
                Arc::new(RunnerOk),
            );
            let (base, server) = levantar(app).await;
            let client = reqwest::Client::new();

            let response = client
                .get(format!("{base}/api/agent/rules"))
                .send()
                .await
                .expect("pedir reglas");
            assert_eq!(response.status(), 200);
            let body = cuerpo_json(response).await;
            assert_eq!(body["rules"][0]["id"], "abc");
            assert_eq!(body["rules"][0]["tool"], "local_run_command");
            assert_eq!(body["rules"][0]["pattern"], "program:git");
            assert!(body["rules"][0].get("note").is_none(), "note vacío omitido");
            assert!(body["rules"][0]["created_at"].is_string());

            let response = client
                .post(format!("{base}/api/agent/rules/revoke"))
                .json(&json!({"id": "abc"}))
                .send()
                .await
                .expect("revocar");
            assert_eq!(response.status(), 200);
            assert_eq!(cuerpo_json(response).await, json!({"ok": true}));

            let response = client
                .get(format!("{base}/api/agent/rules"))
                .send()
                .await
                .expect("pedir reglas vacías");
            assert_eq!(response.status(), 200);
            let body = cuerpo_json(response).await;
            assert!(body["rules"].is_null(), "sin reglas el contrato es null");

            let response = client
                .post(format!("{base}/api/agent/rules/revoke"))
                .json(&json!({"id": "no-existe"}))
                .send()
                .await
                .expect("revocar inexistente");
            assert_eq!(response.status(), 404);
            let body = cuerpo_json(response).await;
            assert_eq!(body["error"], "no existe la regla");

            let response = client
                .post(format!("{base}/api/agent/rules/revoke"))
                .json(&json!({"id": "   "}))
                .send()
                .await
                .expect("revocar sin id");
            assert_eq!(response.status(), 400);
            let body = cuerpo_json(response).await;
            assert_eq!(body["error"], "falta el id de la regla");

            server.abort();
            let _ = server.await;
        });
    });
}

#[tokio::test]
async fn cancelar_responde_ok() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let app = app_de_prueba(
        ajustes(None),
        None,
        conversaciones_en(&dir.path().join("conversaciones")),
        Arc::new(RunnerOk),
    );
    let (base, server) = levantar(app).await;

    let response = reqwest::Client::new()
        .post(format!("{base}/api/agent/cancel"))
        .send()
        .await
        .expect("cancelar");
    assert_eq!(response.status(), 200);
    assert_eq!(cuerpo_json(response).await, json!({"ok": true}));

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn aprobaciones_sin_pendientes_es_lista_vacia() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let app = app_de_prueba(
        ajustes(None),
        None,
        conversaciones_en(&dir.path().join("conversaciones")),
        Arc::new(RunnerOk),
    );
    let (base, server) = levantar(app).await;

    let response = reqwest::Client::new()
        .get(format!("{base}/api/agent/approvals"))
        .send()
        .await
        .expect("pedir aprobaciones");
    assert_eq!(response.status(), 200);
    let body = cuerpo_json(response).await;
    assert!(body.as_array().is_some_and(Vec::is_empty));

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn aprobar_sin_id_responde_400_e_id_desconocido_no_hace_nada() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let app = app_de_prueba(
        ajustes(None),
        None,
        conversaciones_en(&dir.path().join("conversaciones")),
        Arc::new(RunnerOk),
    );
    let (base, server) = levantar(app).await;
    let client = reqwest::Client::new();

    let response = client
        .post(format!("{base}/api/agent/approve"))
        .json(&json!({"id": ""}))
        .send()
        .await
        .expect("aprobar sin id");
    assert_eq!(response.status(), 400);
    let body = cuerpo_json(response).await;
    assert_eq!(body["error"], "falta el id de la aprobación");

    let response = client
        .post(format!("{base}/api/agent/approve"))
        .json(&json!({"id": "ap-404", "approve": true, "remember": true}))
        .send()
        .await
        .expect("aprobar desconocida");
    assert_eq!(response.status(), 200);
    let body = cuerpo_json(response).await;
    assert!(body.as_array().is_some_and(Vec::is_empty));

    // En Go `approve` es *bool: null se lee como false, no como error.
    let response = client
        .post(format!("{base}/api/agent/approve"))
        .json(&json!({"id": "ap-null", "approve": null}))
        .send()
        .await
        .expect("aprobar con null");
    assert_eq!(response.status(), 200);
    assert!(
        cuerpo_json(response)
            .await
            .as_array()
            .is_some_and(Vec::is_empty),
        "una aprobación desconocida no deja pendientes"
    );

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn reset_responde_ok() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let app = app_de_prueba(
        ajustes(None),
        None,
        conversaciones_en(&dir.path().join("conversaciones")),
        Arc::new(RunnerOk),
    );
    let (base, server) = levantar(app).await;

    let response = reqwest::Client::new()
        .post(format!("{base}/api/agent/reset"))
        .send()
        .await
        .expect("reiniciar");
    assert_eq!(response.status(), 200);
    assert_eq!(cuerpo_json(response).await, json!({"ok": true}));

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn conversaciones_lista_de_la_mas_reciente_a_la_mas_vieja() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let conversaciones_dir = dir.path().join("conversaciones");
    std::fs::create_dir_all(&conversaciones_dir).expect("carpeta de conversaciones");
    let mensaje = json!([{
        "Role": "user",
        "Content": "hola",
        "Images": null,
        "ToolCalls": null,
        "ToolCallID": ""
    }]);
    guardar_sesion(
        &conversaciones_dir,
        "1",
        "vieja",
        "2026-01-01T00:00:00Z",
        mensaje.clone(),
    );
    guardar_sesion(
        &conversaciones_dir,
        "2",
        "nueva",
        "2026-02-01T00:00:00Z",
        mensaje,
    );
    let app = app_de_prueba(
        Settings::default(),
        None,
        conversaciones_en(&conversaciones_dir),
        Arc::new(RunnerOk),
    );
    let (base, server) = levantar(app).await;

    let response = reqwest::Client::new()
        .get(format!("{base}/api/agent/conversations"))
        .send()
        .await
        .expect("listar conversaciones");
    assert_eq!(response.status(), 200);
    let body = cuerpo_json(response).await;
    assert_eq!(body[0]["id"], "2", "la más reciente va primero");
    assert_eq!(body[0]["title"], "nueva");
    assert_eq!(body[0]["messages"], 1);
    assert!(body[0]["at"].is_string());

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn conversaciones_open_carga_la_sesion_y_body_vacio_es_500() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let conversaciones_dir = dir.path().join("conversaciones");
    std::fs::create_dir_all(&conversaciones_dir).expect("carpeta de conversaciones");
    guardar_sesion(
        &conversaciones_dir,
        "123",
        "hola",
        "2026-01-01T00:00:00Z",
        json!([{
            "Role": "user",
            "Content": "hola",
            "Images": null,
            "ToolCalls": null,
            "ToolCallID": ""
        }]),
    );
    let app = app_de_prueba(
        Settings::default(),
        None,
        conversaciones_en(&conversaciones_dir),
        Arc::new(RunnerOk),
    );
    let (base, server) = levantar(app).await;
    let client = reqwest::Client::new();

    let response = client
        .post(format!("{base}/api/agent/conversations/open"))
        .json(&json!({"id": "123"}))
        .send()
        .await
        .expect("abrir conversación");
    assert_eq!(response.status(), 200);
    let body = cuerpo_json(response).await;
    assert_eq!(body["conv_id"], "123");
    assert_eq!(body["messages"][0]["role"], "user");
    assert_eq!(body["messages"][0]["content"], "hola");
    assert_eq!(body["tokens_in"], 7);
    assert_eq!(body["tokens_out"], 3);
    assert_eq!(body["cost_usd"], 0.5);

    let response = client
        .post(format!("{base}/api/agent/conversations/open"))
        .send()
        .await
        .expect("abrir sin cuerpo");
    assert_eq!(response.status(), 500, "body vacío es id vacío, no 400");
    let body = cuerpo_json(response).await;
    assert_eq!(body["error"], "la sesión no tiene un id válido");

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn conversaciones_delete_valida_y_borra() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let conversaciones_dir = dir.path().join("conversaciones");
    std::fs::create_dir_all(&conversaciones_dir).expect("carpeta de conversaciones");
    guardar_sesion(
        &conversaciones_dir,
        "123",
        "hola",
        "2026-01-01T00:00:00Z",
        json!([]),
    );
    let app = app_de_prueba(
        Settings::default(),
        None,
        conversaciones_en(&conversaciones_dir),
        Arc::new(RunnerOk),
    );
    let (base, server) = levantar(app).await;
    let client = reqwest::Client::new();

    let response = client
        .post(format!("{base}/api/agent/conversations/delete"))
        .json(&json!({"id": "", "all": false}))
        .send()
        .await
        .expect("borrar sin id");
    assert_eq!(response.status(), 400);
    let body = cuerpo_json(response).await;
    assert_eq!(body["error"], "falta la conversación o la marca all");

    let response = client
        .post(format!("{base}/api/agent/conversations/delete"))
        .json(&json!({"id": "123"}))
        .send()
        .await
        .expect("borrar conversación");
    assert_eq!(response.status(), 200);
    assert_eq!(cuerpo_json(response).await["conv_id"], "");

    let response = client
        .get(format!("{base}/api/agent/conversations"))
        .send()
        .await
        .expect("listar después de borrar");
    assert_eq!(response.status(), 200);
    assert!(
        cuerpo_json(response)
            .await
            .as_array()
            .is_some_and(Vec::is_empty),
        "la conversación borrada ya no aparece"
    );

    server.abort();
    let _ = server.await;
}

#[test]
fn undo_valida_el_numero_y_las_carpetas() {
    en_datos_aislados("undo_valida_el_numero_y_las_carpetas", || {
        con_runtime(async {
            let dir = tempfile::tempdir().expect("carpeta temporal");
            let app = app_de_prueba(
                Settings::default(),
                None,
                conversaciones_en(&dir.path().join("conversaciones")),
                Arc::new(RunnerOk),
            );
            let (base, server) = levantar(app).await;
            let client = reqwest::Client::new();

            let response = client
                .post(format!("{base}/api/agent/undo"))
                .json(&json!({"n": 0}))
                .send()
                .await
                .expect("deshacer cero");
            assert_eq!(response.status(), 400);
            assert_eq!(
                cuerpo_json(response).await["error"],
                "falta el número de cambios a deshacer"
            );

            let response = client
                .post(format!("{base}/api/agent/undo"))
                .json(&json!({"n": 1}))
                .send()
                .await
                .expect("deshacer sin carpetas");
            assert_eq!(response.status(), 400);
            assert_eq!(
                cuerpo_json(response).await["error"],
                "no hay carpetas permitidas configuradas que existan; no puedo deshacer con seguridad"
            );

            server.abort();
            let _ = server.await;
        });
    });
}

#[test]
fn undo_sin_entradas_responde_400() {
    en_datos_aislados("undo_sin_entradas_responde_400", || {
        con_runtime(async {
            let dir = tempfile::tempdir().expect("carpeta temporal");
            let permitida = dir.path().join("permitida");
            std::fs::create_dir_all(&permitida).expect("carpeta permitida");
            let app = app_de_prueba(
                ajustes(Some(&permitida)),
                None,
                conversaciones_en(&dir.path().join("conversaciones")),
                Arc::new(RunnerOk),
            );
            let (base, server) = levantar(app).await;
            let response = reqwest::Client::new()
                .post(format!("{base}/api/agent/undo"))
                .json(&json!({"n": 1}))
                .send()
                .await
                .expect("deshacer sin entradas");
            assert_eq!(response.status(), 400);
            assert_eq!(
                cuerpo_json(response).await["error"],
                "no hay nada que deshacer"
            );
            server.abort();
            let _ = server.await;
        });
    });
}

#[test]
fn undo_deshace_una_entrada_del_diario_temporal() {
    en_datos_aislados("undo_deshace_una_entrada_del_diario_temporal", || {
        let datos = orus_config::data_dir();
        let permitida = datos.join("permitida");
        std::fs::create_dir_all(&permitida).expect("carpeta permitida");
        let archivo = permitida.join("archivo.txt");
        std::fs::write(&archivo, b"hola").expect("archivo a deshacer");
        let diario = datos.join("journal");
        std::fs::create_dir_all(&diario).expect("carpeta del diario");
        let entrada = json!({
            "id": "1",
            "path": archivo.to_string_lossy(),
            "existed": false,
            "backup": "",
            "sha_after": "",
            "size": 4,
            "when": "2026-01-01T00:00:00Z"
        });
        std::fs::write(
            diario.join("journal.jsonl"),
            format!(
                "{}\n",
                serde_json::to_string(&entrada).expect("serializar entrada")
            ),
        )
        .expect("escribir diario");

        con_runtime(async {
            let dir = tempfile::tempdir().expect("carpeta temporal");
            let app = app_de_prueba(
                ajustes(Some(&permitida)),
                None,
                conversaciones_en(&dir.path().join("conversaciones")),
                Arc::new(RunnerOk),
            );
            let (base, server) = levantar(app).await;
            let response = reqwest::Client::new()
                .post(format!("{base}/api/agent/undo"))
                .json(&json!({"n": 1, "force": true}))
                .send()
                .await
                .expect("deshacer");
            assert_eq!(response.status(), 200);
            assert_eq!(cuerpo_json(response).await, json!({"undone": 1}));
            assert!(!archivo.exists(), "deshacer borra el archivo nuevo");
            server.abort();
            let _ = server.await;
        });
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ocupado_responde_409_en_reset_undo_y_conversaciones() {
    let dir = tempfile::tempdir().expect("carpeta temporal");
    let conversaciones_dir = dir.path().join("conversaciones");
    std::fs::create_dir_all(&conversaciones_dir).expect("carpeta de conversaciones");
    guardar_sesion(
        &conversaciones_dir,
        "123",
        "hola",
        "2026-01-01T00:00:00Z",
        json!([]),
    );
    let liberar = Arc::new(tokio::sync::Notify::new());
    let conversaciones = conversaciones_en(&conversaciones_dir);
    let agente = Arc::new(Agent::new(
        conversaciones.clone(),
        Arc::new(ApprovalQueue::new()),
        Arc::new(RunnerBloqueado {
            liberar: Arc::clone(&liberar),
        }),
    ));
    let app = App::new(
        Box::new(SinSesion),
        ajustes(Some(dir.path())),
        Arc::new(SinRed),
        Arc::new(SinRed),
    )
    .with_conversations(Arc::new(conversaciones))
    .with_agent(Arc::clone(&agente));
    let (base, server) = levantar(app).await;

    let turno = tokio::spawn({
        let agente = Arc::clone(&agente);
        async move { agente.send("hola", turno_de_prueba()).await }
    });
    while !agente.busy() {
        tokio::task::yield_now().await;
    }

    let client = reqwest::Client::new();
    let casos = [
        ("/api/agent/reset", json!({})),
        ("/api/agent/undo", json!({"n": 1})),
        ("/api/agent/conversations/open", json!({"id": "123"})),
        ("/api/agent/conversations/delete", json!({"id": "123"})),
    ];
    for (ruta, cuerpo) in casos {
        let response = client
            .post(format!("{base}{ruta}"))
            .json(&cuerpo)
            .send()
            .await
            .expect("pedir ruta ocupada");
        assert_eq!(response.status(), 409, "{ruta}");
        assert_eq!(
            cuerpo_json(response).await["error"],
            "ya hay una ejecución del agente en curso",
            "{ruta}"
        );
    }

    liberar.notify_one();
    assert!(
        turno.await.expect("unir el turno").is_ok(),
        "el turno de prueba termina"
    );

    server.abort();
    let _ = server.await;
}
