//! Contrato de la cola de aprobaciones de la ventana (`agent.go:712-828`).

use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use chrono::Utc;
use orus_agent::{CancelToken, Risk};
use orus_app::{AgentApproval, ApprovalError, ApprovalQueue, nombre_accion};
use orus_core::ToolCall;
use orus_tools::permissions;
use serde_json::{Value, json};

const MARCA_HIJO: &str = "ORUSCRIBE_APPROVALS_TEST_HIJO";
const MARCA_HECHO: &str = "ORUSCRIBE_APPROVALS_TEST_HECHO";

/// Corre `cuerpo` en un proceso hijo con su propio `ORUSCRIBE_DATA_DIR`, igual
/// que los tests de `permissions`: `set_var` es `unsafe` en edition 2024 y los
/// tests de un mismo binario comparten entorno.
fn en_datos_aislados(nombre: &str, cuerpo: fn()) {
    if std::env::var_os(MARCA_HIJO).is_some() {
        cuerpo();
        if let Some(ruta) = std::env::var_os(MARCA_HECHO) {
            std::fs::write(ruta, b"ok").expect("marcar el test ejecutado");
        }
        return;
    }
    let base = tempfile::tempdir().expect("carpeta temporal");
    let datos = base.path().join("datos");
    let hecho = base.path().join("hecho");
    let exe = std::env::current_exe().expect("ruta del binario de tests");
    let estado = Command::new(exe)
        .args(["--exact", nombre, "--test-threads=1", "--nocapture"])
        .env(MARCA_HIJO, "1")
        .env(MARCA_HECHO, &hecho)
        .env("ORUSCRIBE_DATA_DIR", &datos)
        .status()
        .expect("correr el test aislado en su proceso");
    assert!(
        estado.success(),
        "el test aislado {nombre} falló en su proceso"
    );
    assert!(hecho.exists(), "el hijo no ejecutó {nombre}");
}

fn llamada(nombre: &str, args: Value) -> ToolCall {
    ToolCall {
        name: nombre.to_owned(),
        args,
        ..ToolCall::default()
    }
}

fn esperar_pendientes(cola: &ApprovalQueue, cuantas: usize) {
    let limite = Instant::now() + Duration::from_secs(2);
    while cola.pending().len() != cuantas {
        assert!(Instant::now() < limite, "la aprobación no llegó a la cola");
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn answer_removes_and_returns() {
    let cola = ApprovalQueue::with_wait(Duration::from_secs(5));
    let cancel = CancelToken::new();
    let llamada = llamada("create_task", json!({"title": "x"}));

    thread::scope(|alcance| {
        let hilo = alcance.spawn(|| cola.request(&llamada, Risk::Write, &cancel));
        esperar_pendientes(&cola, 1);
        let info = cola.pending().into_iter().next().expect("una pendiente");
        assert_eq!(info.id, "ap-1");
        assert_eq!(info.tool, "create_task");
        assert_eq!(info.action, "Crear una tarea");
        assert_eq!(info.text, r#"{"title":"x"}"#);
        assert_eq!(info.rule_pattern, "tool");

        cola.answer(&info.id, true, false).expect("contestar");
        assert!(cola.pending().is_empty(), "answer saca la pendiente");
        assert!(hilo.join().expect("el hilo responde").expect("aprobada"));
    });
}

#[test]
fn unknown_id_is_noop() {
    let cola = ApprovalQueue::with_wait(Duration::from_secs(5));
    cola.answer("ap-99", true, false)
        .expect("un id desconocido no falla");
    assert!(cola.pending().is_empty());

    let cancel = CancelToken::new();
    let llamada = llamada("create_task", json!({}));
    thread::scope(|alcance| {
        let hilo = alcance.spawn(|| cola.request(&llamada, Risk::Write, &cancel));
        esperar_pendientes(&cola, 1);
        cola.answer("ap-99", true, false)
            .expect("otro id desconocido");
        assert_eq!(cola.pending().len(), 1, "no contesta la aprobación real");

        let id = cola.pending()[0].id.clone();
        cola.answer(&id, false, false).expect("contestar la real");
        assert!(!hilo.join().expect("el hilo responde").expect("negada"));
    });
}

#[test]
fn timeout_denies_and_removes() {
    let cola = ApprovalQueue::with_wait(Duration::from_millis(50));
    let cancel = CancelToken::new();
    let llamada = llamada("create_task", json!({}));

    assert!(
        matches!(
            cola.request(&llamada, Risk::Write, &cancel),
            Err(ApprovalError::Vencida)
        ),
        "el vencimiento tiene que distinguirse de la negación (#95)"
    );
    assert!(cola.pending().is_empty(), "vence y saca la pendiente");
}

#[test]
fn cancel_denies_and_removes() {
    let cola = ApprovalQueue::with_wait(Duration::from_secs(30));
    let llamada = llamada("create_task", json!({}));

    let cancelado = CancelToken::new();
    cancelado.cancelar();
    assert!(
        matches!(cola.request(&llamada, Risk::Write, &cancelado), Ok(false)),
        "un token ya cancelado no encola"
    );
    assert!(cola.pending().is_empty());

    let cancel = CancelToken::new();
    thread::scope(|alcance| {
        let hilo = alcance.spawn(|| cola.request(&llamada, Risk::Write, &cancel));
        esperar_pendientes(&cola, 1);
        cancel.cancelar();
        assert!(
            matches!(hilo.join().expect("el hilo responde"), Ok(false)),
            "la cancelación es no"
        );
        assert!(cola.pending().is_empty(), "cancelar saca la pendiente");
    });
}

#[test]
fn remember_saves_rule() {
    en_datos_aislados("remember_saves_rule", || {
        let cola = ApprovalQueue::with_wait(Duration::from_secs(5));
        let cancel = CancelToken::new();
        let llamada = llamada("create_task", json!({"title": "x"}));
        thread::scope(|alcance| {
            let hilo = alcance.spawn(|| cola.request(&llamada, Risk::Write, &cancel));
            esperar_pendientes(&cola, 1);
            let id = cola.pending()[0].id.clone();
            cola.answer(&id, true, true).expect("aprobar y recordar");
            assert!(hilo.join().expect("el hilo responde").expect("aprobada"));
        });
        let reglas = permissions::list();
        assert!(
            reglas
                .iter()
                .any(|regla| regla.tool == "create_task" && regla.pattern == "tool"),
            "la regla quedó guardada"
        );
    });
}

#[test]
fn remember_failure_still_approves() {
    en_datos_aislados("remember_failure_still_approves", || {
        let datos = std::env::var_os("ORUSCRIBE_DATA_DIR").expect("datos aislados");
        std::fs::write(&datos, b"bloqueo").expect("un archivo donde va la carpeta");

        let cola = ApprovalQueue::with_wait(Duration::from_secs(5));
        let cancel = CancelToken::new();
        let llamada = llamada("create_task", json!({}));
        thread::scope(|alcance| {
            let hilo = alcance.spawn(|| cola.request(&llamada, Risk::Write, &cancel));
            esperar_pendientes(&cola, 1);
            let id = cola.pending()[0].id.clone();
            let error = cola
                .answer(&id, true, true)
                .expect_err("recordar tiene que fallar");
            assert!(
                error
                    .to_string()
                    .starts_with("la acción se aprobó, pero no se pudo recordar:"),
                "texto del error: {error}"
            );
            assert!(
                hilo.join().expect("el hilo responde").expect("aprobada"),
                "la aprobación ya se dio"
            );
        });
    });
}

#[test]
fn unknown_tool_uses_technical_name() {
    assert_eq!(nombre_accion("create_task"), "Crear una tarea");
    assert_eq!(nombre_accion("propose_plan"), "Ejecutar este plan");
    assert_eq!(nombre_accion("herramienta_nueva"), "herramienta_nueva");
}

/// #95: la herramienta de edición existe y la tarjeta tiene que mostrar una
/// acción legible, no el nombre técnico.
#[test]
fn local_edit_file_tiene_nombre_en_la_tarjeta() {
    assert_eq!(nombre_accion("local_edit_file"), "Editar un archivo");
}

#[test]
fn rule_pattern_is_omitted_when_empty() {
    let aprobacion = AgentApproval {
        id: "ap-1".to_owned(),
        tool: "local_read_file".to_owned(),
        action: nombre_accion("local_read_file"),
        text: "{}".to_owned(),
        at: Utc::now(),
        rule_pattern: String::new(),
    };
    let valor = serde_json::to_value(&aprobacion).expect("serializar");
    assert!(
        valor.get("rule_pattern").is_none(),
        "un patrón vacío no viaja"
    );
}
