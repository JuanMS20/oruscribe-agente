//! Contrato de abrir y borrar conversaciones desde el agente
//! (`internal/app/conversations.go:63-118`).

use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use orus_agent::{CancelToken, Risk, RunnerError};
use orus_app::{
    Agent, AgentError, ApprovalError, ApprovalQueue, Conversations, Emisor, TurnRunner, Turno,
};
use orus_core::{Event, Message, ToolCall};
use orus_harness::ErrorTurno;
use orus_harness::Toolset;
use orus_harness::sessions::{Session, Store, VERSION};
use tokio::sync::Notify;

enum Guion {
    Responder,
    FallaConPaso,
    EsperarCancelacion,
    Panico,
}

struct RunnerFalso {
    guion: Guion,
    respuesta: String,
    uso: (u64, u64),
    empezado: Arc<Notify>,
    terminado: Arc<Notify>,
}

impl RunnerFalso {
    fn nuevo(guion: Guion, uso: (u64, u64)) -> Arc<Self> {
        Arc::new(Self {
            guion,
            respuesta: "respuesta".to_owned(),
            uso,
            empezado: Arc::new(Notify::new()),
            terminado: Arc::new(Notify::new()),
        })
    }
}

#[async_trait]
impl TurnRunner for RunnerFalso {
    async fn run(
        &self,
        _turno: Turno,
        session: &mut Session,
        cancel: CancelToken,
        emitir: Emisor,
    ) -> Result<(), ErrorTurno> {
        self.empezado.notify_one();
        let resultado = match self.guion {
            Guion::Responder => {
                session.messages.push(mensaje("assistant", &self.respuesta));
                emitir(Event::Answer {
                    text: self.respuesta.clone(),
                });
                emitir(Event::Usage {
                    session: session.id.clone(),
                    prompt_tokens: (self.uso.0 > 0).then_some(self.uso.0),
                    completion_tokens: (self.uso.1 > 0).then_some(self.uso.1),
                    cost_usd: None,
                });
                Ok(())
            }
            Guion::FallaConPaso => {
                session.messages.push(mensaje("assistant", "parcial"));
                emitir(Event::ToolCall {
                    tool: "local_read_file".to_owned(),
                    args: "{}".to_owned(),
                });
                Err(ErrorTurno::Turno(RunnerError::TopeDePasos))
            }
            Guion::EsperarCancelacion => {
                cancel.esperar().await;
                Err(ErrorTurno::Turno(RunnerError::Cancelado))
            }
            Guion::Panico => panic!("fallo simulado del runner"),
        };
        self.terminado.notify_one();
        resultado
    }
}

fn mensaje(role: &str, content: &str) -> Message {
    Message {
        role: role.to_owned(),
        content: content.to_owned(),
        ..Message::default()
    }
}

fn conversaciones(dir: &Path) -> Conversations {
    Conversations {
        store: Store {
            dir: dir.to_path_buf(),
            prefix: "c-".to_owned(),
        },
    }
}

fn armar(dir: &Path, runner: Arc<RunnerFalso>) -> Arc<Agent> {
    Arc::new(Agent::new(
        conversaciones(dir),
        Arc::new(ApprovalQueue::new()),
        runner,
    ))
}

fn armar_con_gap<F>(dir: &Path, runner: Arc<RunnerFalso>, gap: F) -> Arc<Agent>
where
    F: Fn() + Send + Sync + 'static,
{
    Arc::new(
        Agent::new(conversaciones(dir), Arc::new(ApprovalQueue::new()), runner)
            .with_delete_gap(gap),
    )
}

fn turno() -> Turno {
    Turno {
        base_url: "http://127.0.0.1:1".to_owned(),
        modelo: "modelo".to_owned(),
        modo: "ask".to_owned(),
        cwd: ".".to_owned(),
        folders: vec![".".to_owned()],
        cerebro: false,
        mensaje: String::new(),
        imagenes: Vec::new(),
        clave: "clave".to_owned(),
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

fn guardar(
    conversaciones: &Conversations,
    id: &str,
    mensajes: Vec<Message>,
    tokens: (i64, i64),
    costo: f64,
    creada: DateTime<Utc>,
) {
    let mut sesion = Session {
        version: VERSION,
        id: id.to_owned(),
        title: String::new(),
        cwd: String::new(),
        model: "modelo".to_owned(),
        created: creada,
        updated: creada,
        messages: mensajes,
        tokens_in: tokens.0,
        tokens_out: tokens.1,
        cost_usd: costo,
    };
    conversaciones.save(&mut sesion).unwrap();
}

#[tokio::test]
async fn reset_clears_state() {
    let base = tempfile::tempdir().unwrap();
    let agent = armar(base.path(), RunnerFalso::nuevo(Guion::Responder, (100, 50)));
    let mut con_tasas = turno();
    con_tasas.cost_input_per_mtok = 1.0;
    con_tasas.cost_output_per_mtok = 2.0;
    agent.send("hola", con_tasas).await.unwrap();
    let generacion = agent.generation();

    agent.reset().unwrap();

    assert_eq!(agent.generation(), generacion);
    assert!(agent.history().is_empty());
    assert!(agent.steps().is_empty());
    assert_eq!(agent.tokens(), (0, 0));
    assert!((agent.cost_usd() - 0.0).abs() < 1e-12);
    assert_eq!(agent.error(), "");
    assert_eq!(agent.draft(), "");
    assert_eq!(agent.conversation_id(), None);
    assert_eq!(agent.created(), None);
}

#[tokio::test]
async fn open_loads_history() {
    let base = tempfile::tempdir().unwrap();
    let store = conversaciones(base.path());
    let creada = Utc::now() - ChronoDuration::seconds(120);
    let mensajes = vec![mensaje("user", "vieja"), mensaje("assistant", "guardada")];
    guardar(&store, "101", mensajes.clone(), (321, 45), 0.25, creada);

    // Una corrida fallida deja pasos y error: abrir otra los debe limpiar.
    let agent = armar(base.path(), RunnerFalso::nuevo(Guion::FallaConPaso, (0, 0)));
    assert!(agent.send("nueva", turno()).await.is_err());
    assert!(!agent.steps().is_empty());
    assert!(!agent.error().is_empty());

    agent.open_conversation("101").unwrap();

    assert_eq!(agent.conversation_id().as_deref(), Some("101"));
    assert_eq!(agent.history(), mensajes);
    assert_eq!(agent.tokens(), (321, 45));
    assert!((agent.cost_usd() - 0.25).abs() < 1e-12);
    assert_eq!(agent.created(), Some(creada));
    assert!(agent.steps().is_empty());
    assert_eq!(agent.error(), "");
    assert_eq!(agent.draft(), "");
}

#[tokio::test]
async fn open_rebuilds_tool_steps_from_history() {
    let base = tempfile::tempdir().unwrap();
    let store = conversaciones(base.path());
    let creada = Utc::now() - ChronoDuration::seconds(60);
    let mensajes = vec![
        mensaje("user", "lee README"),
        Message {
            role: "assistant".to_owned(),
            content: String::new(),
            tool_calls: Some(vec![ToolCall {
                id: "call_1".to_owned(),
                name: "read".to_owned(),
                args: serde_json::json!({ "path": "C:/repo/README.md" }),
                invalid_args: false,
            }]),
            ..Message::default()
        },
        Message {
            role: "tool".to_owned(),
            content: "# Oruscribe Agente".to_owned(),
            tool_call_id: "call_1".to_owned(),
            ..Message::default()
        },
        mensaje("assistant", "Listo, leí el README."),
    ];
    guardar(&store, "102", mensajes, (50, 20), 0.01, creada);

    let agent = armar(base.path(), RunnerFalso::nuevo(Guion::Responder, (0, 0)));
    agent.open_conversation("102").unwrap();

    let pasos = agent.steps();
    assert_eq!(pasos.len(), 2);
    assert_eq!(pasos[0].kind, "tool_call");
    assert_eq!(pasos[0].tool, "read");
    assert!(pasos[0].text.contains("README.md"));
    assert_eq!(pasos[1].kind, "tool_result");
    assert_eq!(pasos[1].tool, "read");
    assert_eq!(pasos[1].text, "# Oruscribe Agente");
}

#[tokio::test]
async fn delete_current_resets() {
    let base = tempfile::tempdir().unwrap();
    let store = conversaciones(base.path());
    guardar(
        &store,
        "202",
        vec![mensaje("user", "hola")],
        (10, 5),
        0.5,
        Utc::now(),
    );
    let agent = armar(base.path(), RunnerFalso::nuevo(Guion::Responder, (0, 0)));

    agent.open_conversation("202").unwrap();
    agent.delete_conversation("202").unwrap();

    assert_eq!(agent.conversation_id(), None);
    assert!(agent.history().is_empty());
    assert_eq!(agent.tokens(), (0, 0));
    assert!((agent.cost_usd() - 0.0).abs() < 1e-12);
    assert_eq!(agent.created(), None);
    assert!(store.list().unwrap().is_empty());
}

#[tokio::test]
async fn delete_all_resets() {
    let base = tempfile::tempdir().unwrap();
    let store = conversaciones(base.path());
    guardar(
        &store,
        "301",
        vec![mensaje("user", "una")],
        (1, 2),
        0.1,
        Utc::now(),
    );
    guardar(
        &store,
        "302",
        vec![mensaje("user", "otra")],
        (3, 4),
        0.2,
        Utc::now(),
    );
    let agent = armar(base.path(), RunnerFalso::nuevo(Guion::Responder, (0, 0)));
    agent.open_conversation("301").unwrap();

    agent.delete_conversation("").unwrap();

    assert_eq!(agent.conversation_id(), None);
    assert!(agent.history().is_empty());
    assert_eq!(agent.tokens(), (0, 0));
    assert_eq!(agent.created(), None);
    assert!(store.list().unwrap().is_empty());
}

#[tokio::test]
async fn open_missing_conversation_errors() {
    let base = tempfile::tempdir().unwrap();
    let agent = armar(base.path(), RunnerFalso::nuevo(Guion::Responder, (0, 0)));

    let error = agent.open_conversation("987654321").unwrap_err();

    let texto = match error {
        AgentError::Conversaciones(texto) => texto,
        otro => panic!("se esperaba Conversaciones, llegó {otro:?}"),
    };
    assert!(
        texto.contains("987654321"),
        "el texto crudo del store debe nombrar el archivo: {texto}"
    );
    assert_eq!(agent.conversation_id(), None);
    assert!(agent.history().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delete_busy_rejects() {
    let base = tempfile::tempdir().unwrap();
    let runner = RunnerFalso::nuevo(Guion::EsperarCancelacion, (0, 0));
    let agent = armar(base.path(), Arc::clone(&runner));
    let tarea = tokio::spawn({
        let agent = Arc::clone(&agent);
        async move { agent.send("uno", turno()).await }
    });
    runner.empezado.notified().await;
    let id = agent.conversation_id().expect("id de la conversación");

    assert!(matches!(
        agent.delete_conversation(&id),
        Err(AgentError::Busy)
    ));
    assert!(matches!(
        agent.delete_conversation(""),
        Err(AgentError::Busy)
    ));

    agent.cancel();
    assert!(matches!(tarea.await.unwrap(), Err(AgentError::Cancelled)));
    assert_eq!(conversaciones(base.path()).list().unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn borrar_no_pisa_el_turno_del_timbre() {
    let base = tempfile::tempdir().unwrap();
    let store = conversaciones(base.path());
    guardar(
        &store,
        "401",
        vec![mensaje("user", "hola")],
        (1, 1),
        0.1,
        Utc::now(),
    );

    let runner = RunnerFalso::nuevo(Guion::EsperarCancelacion, (0, 0));
    let celda: Arc<OnceLock<Arc<Agent>>> = Arc::new(OnceLock::new());
    let agente = armar_con_gap(base.path(), Arc::clone(&runner), {
        let celda = Arc::clone(&celda);
        move || {
            let agente = celda.get().expect("el agente existe al borrar");
            agente
                .empezar("aviso del timbre", turno())
                .expect("el turno del timbre arranca");
        }
    });
    assert!(celda.set(Arc::clone(&agente)).is_ok());
    agente.open_conversation("401").unwrap();

    agente.delete_conversation("401").unwrap();

    assert!(agente.busy(), "el turno del timbre sigue en curso");
    assert_eq!(
        agente.conversation_id().as_deref(),
        Some("401"),
        "borrar en el hueco no le quita el id al turno que recién arrancó"
    );

    agente.cancel();
    tokio::time::timeout(Duration::from_secs(2), runner.terminado.notified())
        .await
        .expect("el turno termina: cancelar sigue funcionando");
    tokio::time::timeout(Duration::from_secs(2), async {
        while agente.busy() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("el turno suelta el agente");
    assert_eq!(
        agente.conversation_id(),
        None,
        "la conversación borrada no puede quedar abierta al terminar el turno"
    );
    assert!(
        !base.path().join("c-401.json").exists(),
        "la conversación borrada no debe volver al disco al terminar el turno"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn un_fallo_de_guardado_se_ve_aunque_el_turno_ya_tenga_error() {
    let base = tempfile::tempdir().unwrap();
    let store = conversaciones(base.path());
    guardar(
        &store,
        "403",
        vec![mensaje("user", "hola")],
        (1, 1),
        0.1,
        Utc::now(),
    );

    let runner = RunnerFalso::nuevo(Guion::FallaConPaso, (0, 0));
    let destino = base.path().join("c-403.json");
    let agente = Arc::new(
        Agent::new(
            conversaciones(base.path()),
            Arc::new(ApprovalQueue::new()),
            runner.clone() as Arc<dyn TurnRunner>,
        )
        .with_save_gap(move || {
            std::fs::remove_file(&destino).expect("había una conversación guardada");
            std::fs::create_dir(&destino).expect("el guardado final falla");
        }),
    );
    agente.open_conversation("403").unwrap();

    agente.empezar("hola", turno()).expect("el turno arranca");
    tokio::time::timeout(Duration::from_secs(2), async {
        while agente.busy() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("el turno termina");

    let error = agente.error();
    assert!(
        error.contains("no se pudo guardar"),
        "el fallo de disco se ocultó detrás del error del turno: {error:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn un_panico_en_el_turno_no_deja_el_agente_ocupado() {
    let base = tempfile::tempdir().unwrap();
    let runner = RunnerFalso::nuevo(Guion::Panico, (0, 0));
    let agente = armar(base.path(), Arc::clone(&runner));

    agente.empezar("hola", turno()).expect("el turno arranca");
    tokio::time::timeout(Duration::from_secs(2), async {
        while agente.busy() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("un pánico no puede dejar el agente ocupado para siempre");
}

/// #95: el pánico del runner no se traga: queda un error visible y lo que
/// haya en el historial llega al disco.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn un_panico_en_el_turno_deja_error_visible_y_guarda() {
    let base = tempfile::tempdir().unwrap();
    let runner = RunnerFalso::nuevo(Guion::Panico, (0, 0));
    let agente = armar(base.path(), Arc::clone(&runner));

    agente.empezar("hola", turno()).expect("el turno arranca");
    tokio::time::timeout(Duration::from_secs(2), async {
        while agente.busy() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("el turno suelta el agente");

    let error = agente.error();
    assert!(
        error.contains("pánico"),
        "el pánico se tragó sin error visible: {error:?}"
    );
    let id = agente
        .conversation_id()
        .expect("la conversación sigue abierta");
    let guardada = conversaciones(base.path())
        .load(&id)
        .expect("el mensaje del usuario tiene que llegar al disco");
    assert!(
        guardada
            .messages
            .iter()
            .any(|mensaje| mensaje.role == "user" && mensaje.content == "hola"),
        "el guardado perdió el mensaje del usuario: {:?}",
        guardada.messages
    );
}

#[tokio::test]
async fn un_fallo_del_guardado_inicial_no_arranca_el_turno() {
    let base = tempfile::tempdir().unwrap();
    let ruta = base.path().join("no-es-una-carpeta");
    std::fs::write(&ruta, "ocupado").unwrap();
    let agente = Arc::new(Agent::new(
        conversaciones(&ruta),
        Arc::new(ApprovalQueue::new()),
        RunnerFalso::nuevo(Guion::Responder, (0, 0)),
    ));

    let error = agente
        .empezar("hola", turno())
        .expect_err("sin disco no puede contestar 200");

    assert!(matches!(error, AgentError::Save(_)), "error = {error:?}");
    assert!(!agente.busy(), "fallar el guardado dejó el agente ocupado");
    assert_eq!(agente.conversation_id(), None);
    assert!(agente.history().is_empty());
}

#[tokio::test]
async fn abrir_despues_del_logout_no_revive_el_historial() {
    let base = tempfile::tempdir().unwrap();
    let store = conversaciones(base.path());
    guardar(
        &store,
        "501",
        vec![mensaje("user", "vieja")],
        (1, 1),
        0.1,
        Utc::now(),
    );
    let celda: Arc<Mutex<Option<Arc<Agent>>>> = Arc::new(Mutex::new(None));
    let agente = Arc::new(
        Agent::new(
            conversaciones(base.path()),
            Arc::new(ApprovalQueue::new()),
            RunnerFalso::nuevo(Guion::Responder, (0, 0)),
        )
        .with_open_gap({
            let celda = Arc::clone(&celda);
            move || {
                let agente = celda.lock().unwrap().clone().expect("el agente existe");
                agente.clear();
            }
        }),
    );
    *celda.lock().unwrap() = Some(Arc::clone(&agente));

    let error = agente
        .open_conversation("501")
        .expect_err("el logout ocurrió después de leer el archivo");
    assert!(matches!(error, AgentError::Invalid(_)), "error = {error:?}");
    assert_eq!(agente.conversation_id(), None);
    assert!(agente.history().is_empty());
}

/// #76/#95: cerrar la app a mitad del turno no puede perder el mensaje de la
/// persona: se guarda al arrancar, no solo al final.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cerrar_a_mitad_del_turno_conserva_el_mensaje() {
    let base = tempfile::tempdir().unwrap();
    let runner = RunnerFalso::nuevo(Guion::EsperarCancelacion, (0, 0));
    let agente = armar(base.path(), Arc::clone(&runner));

    agente
        .empezar("mensaje que no se puede perder", turno())
        .expect("el turno arranca");
    runner.empezado.notified().await;

    // El proceso podría morir en este punto: el mensaje ya tiene que estar
    // en disco aunque el turno no haya terminado.
    let id = agente.conversation_id().expect("id de la conversación");
    let guardada = conversaciones(base.path())
        .load(&id)
        .expect("el mensaje del usuario se guarda al arrancar el turno");
    assert!(
        guardada.messages.iter().any(|mensaje| {
            mensaje.role == "user" && mensaje.content == "mensaje que no se puede perder"
        }),
        "el mensaje del usuario no llegó al disco a mitad del turno: {:?}",
        guardada.messages
    );

    agente.cancel();
    tokio::time::timeout(Duration::from_secs(2), async {
        while agente.busy() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("el turno cancelado termina");
}

/// Un runner que pide una aprobación a la cola, como hace el arnés real.
struct RunnerAprobacion {
    approvals: Arc<ApprovalQueue>,
}

#[async_trait]
impl TurnRunner for RunnerAprobacion {
    async fn run(
        &self,
        _turno: Turno,
        _session: &mut Session,
        cancel: CancelToken,
        _emitir: Emisor,
    ) -> Result<(), ErrorTurno> {
        let llamada = ToolCall {
            name: "local_write_file".to_owned(),
            args: serde_json::json!({"path": "archivo.txt"}),
            ..ToolCall::default()
        };
        match self.approvals.request(&llamada, Risk::Write, &cancel) {
            Ok(true) => Ok(()),
            Ok(false) => Err(ErrorTurno::Turno(RunnerError::Cancelado)),
            Err(ApprovalError::Vencida) => {
                cancel.cancelar();
                Err(ErrorTurno::Turno(RunnerError::Cancelado))
            }
            Err(_) => Err(ErrorTurno::Turno(RunnerError::Cancelado)),
        }
    }
}

/// #95: la tarjeta vence sin respuesta y el error nombra la aprobación; no
/// dice que el agente tardó demasiado ni que se canceló.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn aprobacion_vencida_no_culpa_al_agente() {
    let base = tempfile::tempdir().unwrap();
    let cola = Arc::new(ApprovalQueue::with_wait(Duration::from_millis(50)));
    let runner = Arc::new(RunnerAprobacion {
        approvals: Arc::clone(&cola),
    });
    let agente = Arc::new(Agent::new(
        conversaciones(base.path()),
        Arc::clone(&cola),
        runner,
    ));

    let error = agente.send("hola", turno()).await.unwrap_err();
    let texto = error.to_string();
    assert!(
        texto.contains("aprobación venció"),
        "el error no nombra la aprobación vencida: {texto}"
    );
    assert!(!agente.busy(), "el turno suelta el agente");
    assert!(
        agente.error().contains("aprobación venció"),
        "la ventana tiene que ver el error: {:?}",
        agente.error()
    );
    let id = agente.conversation_id().expect("id de la conversación");
    let guardada = conversaciones(base.path())
        .load(&id)
        .expect("el mensaje del usuario queda guardado");
    assert!(
        guardada
            .messages
            .iter()
            .any(|mensaje| mensaje.role == "user" && mensaje.content == "hola")
    );
}

/// #95: si el plazo del turno vence mientras una tarjeta sigue pendiente, el
/// error también nombra la aprobación, no al agente.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn timeout_con_aprobacion_pendiente_nombra_la_aprobacion() {
    let base = tempfile::tempdir().unwrap();
    let cola = Arc::new(ApprovalQueue::with_wait(Duration::from_secs(30)));
    let runner = Arc::new(RunnerAprobacion {
        approvals: Arc::clone(&cola),
    });
    let agente = Arc::new(
        Agent::new(conversaciones(base.path()), Arc::clone(&cola), runner)
            .with_turn_timeout(Duration::from_millis(100)),
    );

    let error = agente.send("hola", turno()).await.unwrap_err();
    let texto = error.to_string();
    assert!(
        texto.contains("aprobación venció"),
        "el turno venció esperando la aprobación y el error culpa al agente: {texto}"
    );
    assert!(!agente.busy(), "el turno suelta el agente");
}
