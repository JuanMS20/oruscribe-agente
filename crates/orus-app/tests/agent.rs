//! Contrato del núcleo del agente (`internal/app/agent.go:539-1113`).

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use orus_agent::{CancelToken, RunnerError};
use orus_app::{Agent, AgentError, ApprovalQueue, Conversations, Emisor, TurnRunner, Turno};
use orus_core::{Event, Message};
use orus_harness::ErrorTurno;
use orus_harness::Toolset;
use orus_harness::sessions::{Session, Store};
use tokio::sync::Notify;

enum Guion {
    Responder,
    EsperarCancelacion,
    EsperarCancelacionConParcial,
    Bloquear { seguir: Arc<Notify> },
}

struct RunnerFalso {
    guion: Guion,
    respuesta: String,
    uso: (u64, u64),
    empezado: Arc<Notify>,
    turnos: Mutex<Vec<String>>,
}

impl RunnerFalso {
    fn nuevo(guion: Guion, uso: (u64, u64)) -> Arc<Self> {
        Arc::new(Self {
            guion,
            respuesta: "respuesta".to_owned(),
            uso,
            empezado: Arc::new(Notify::new()),
            turnos: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl TurnRunner for RunnerFalso {
    async fn run(
        &self,
        turno: Turno,
        session: &mut Session,
        cancel: CancelToken,
        emitir: Emisor,
    ) -> Result<(), ErrorTurno> {
        self.turnos.lock().unwrap().push(turno.mensaje.clone());
        self.empezado.notify_one();
        match &self.guion {
            Guion::Responder => {
                session.messages.push(Message {
                    role: "assistant".to_owned(),
                    content: self.respuesta.clone(),
                    ..Message::default()
                });
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
            Guion::EsperarCancelacion => {
                cancel.esperar().await;
                Err(ErrorTurno::Turno(RunnerError::Cancelado))
            }
            Guion::EsperarCancelacionConParcial => {
                session.messages.push(Message {
                    role: "assistant".to_owned(),
                    content: "parcial".to_owned(),
                    ..Message::default()
                });
                cancel.esperar().await;
                Err(ErrorTurno::Turno(RunnerError::Cancelado))
            }
            Guion::Bloquear { seguir } => {
                seguir.notified().await;
                session.messages.push(Message {
                    role: "assistant".to_owned(),
                    content: "viejo".to_owned(),
                    ..Message::default()
                });
                Err(ErrorTurno::Turno(RunnerError::Cancelado))
            }
        }
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

#[tokio::test]
async fn send_runs_turn_and_saves() {
    let base = tempfile::tempdir().unwrap();
    let runner = RunnerFalso::nuevo(Guion::Responder, (0, 0));
    let agent = armar(base.path(), Arc::clone(&runner));

    agent.send("  hola  ", turno()).await.unwrap();

    assert_eq!(runner.turnos.lock().unwrap().as_slice(), ["hola"]);
    assert!(agent.created().is_some());
    assert_eq!(agent.history().len(), 2);
    assert_eq!(agent.history()[0].content, "hola");
    assert_eq!(agent.history()[1].content, "respuesta");
    assert_eq!(
        agent.steps().last().map(|paso| paso.kind.as_str()),
        Some("answer")
    );
    assert!(!agent.busy());

    let id = agent.conversation_id().expect("id de la conversación");
    let guardada = conversaciones(base.path()).load(&id).unwrap();
    assert_eq!(guardada.messages, agent.history());
    assert_eq!(guardada.title, "hola");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn busy_rejects_second_send() {
    let base = tempfile::tempdir().unwrap();
    let runner = RunnerFalso::nuevo(Guion::EsperarCancelacion, (0, 0));
    let agent = armar(base.path(), Arc::clone(&runner));
    let primero = tokio::spawn({
        let agent = Arc::clone(&agent);
        async move { agent.send("uno", turno()).await }
    });
    runner.empezado.notified().await;

    assert!(matches!(
        agent.send("dos", turno()).await,
        Err(AgentError::Busy)
    ));

    agent.cancel();
    assert!(matches!(primero.await.unwrap(), Err(AgentError::Cancelled)));
    assert!(!agent.busy());
}

#[tokio::test]
async fn cancel_cancels_running_turn() {
    let base = tempfile::tempdir().unwrap();
    let runner = RunnerFalso::nuevo(Guion::EsperarCancelacion, (0, 0));
    let agent = armar(base.path(), Arc::clone(&runner));
    let tarea = tokio::spawn({
        let agent = Arc::clone(&agent);
        async move { agent.send("uno", turno()).await }
    });
    runner.empezado.notified().await;

    agent.cancel();

    assert!(matches!(tarea.await.unwrap(), Err(AgentError::Cancelled)));
    assert_eq!(agent.error(), "Se canceló la respuesta del agente.");
    assert!(!agent.busy());
}

#[tokio::test]
async fn tokens_and_cost_accumulate() {
    let base = tempfile::tempdir().unwrap();
    let runner = RunnerFalso::nuevo(Guion::Responder, (100, 50));
    let agent = armar(base.path(), Arc::clone(&runner));

    let mut primero = turno();
    primero.cost_input_per_mtok = 1.0;
    primero.cost_output_per_mtok = 2.0;
    agent.send("uno", primero).await.unwrap();

    let mut segundo = turno();
    segundo.cost_input_per_mtok = 1.0;
    segundo.cost_output_per_mtok = 2.0;
    agent.send("dos", segundo).await.unwrap();

    assert_eq!(agent.tokens(), (200, 100));
    assert!((agent.cost_usd() - 0.0004).abs() < 1e-12);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_generation_does_not_write() {
    let base = tempfile::tempdir().unwrap();
    let seguir = Arc::new(Notify::new());
    let runner = RunnerFalso::nuevo(
        Guion::Bloquear {
            seguir: Arc::clone(&seguir),
        },
        (0, 0),
    );
    let agent = armar(base.path(), Arc::clone(&runner));
    let tarea = tokio::spawn({
        let agent = Arc::clone(&agent);
        async move { agent.send("uno", turno()).await }
    });
    runner.empezado.notified().await;

    let antes = agent.generation();
    agent.clear();
    assert_eq!(agent.generation(), antes + 1);
    seguir.notify_one();

    assert!(matches!(tarea.await.unwrap(), Err(AgentError::Cancelled)));
    assert!(agent.history().is_empty());
    assert!(agent.conversation_id().is_none());
    // #76: el mensaje del usuario se guarda al arrancar el turno, cuando la
    // generación todavía era vigente. Lo que una generación vieja no puede
    // escribir es su cierre: el assistant «viejo» no llega al disco.
    let lista = conversaciones(base.path()).list().unwrap();
    assert_eq!(lista.len(), 1, "solo el guardado de arranque puede quedar");
    let guardada = conversaciones(base.path()).load(&lista[0].id).unwrap();
    assert!(
        guardada
            .messages
            .iter()
            .any(|m| m.role == "user" && m.content == "uno"),
        "el guardado de arranque perdió el mensaje: {:?}",
        guardada.messages
    );
    assert!(
        guardada.messages.iter().all(|m| m.role != "assistant"),
        "la generación vieja escribió su cierre: {:?}",
        guardada.messages
    );
    assert!(!agent.busy());

    let otro = tempfile::tempdir().unwrap();
    let store = conversaciones(otro.path());
    let celda: Arc<Mutex<Option<Arc<Agent>>>> = Arc::new(Mutex::new(None));
    let agente = Arc::new(
        Agent::new(
            store.clone(),
            Arc::new(ApprovalQueue::new()),
            RunnerFalso::nuevo(Guion::Responder, (0, 0)),
        )
        .with_save_gap({
            let celda = Arc::clone(&celda);
            let store = store.clone();
            move || {
                let agente = celda.lock().unwrap().clone().expect("el agente existe");
                agente.clear();
                store.clear().expect("el logout borra las conversaciones");
            }
        }),
    );
    *celda.lock().unwrap() = Some(Arc::clone(&agente));
    let tarea = tokio::spawn({
        let agente = Arc::clone(&agente);
        async move { agente.send("dos", turno()).await }
    });
    let _ = tarea.await.unwrap();
    assert!(
        store.list().unwrap().is_empty(),
        "un guardado posterior al logout resucitó la conversación borrada"
    );
}

#[tokio::test]
async fn save_skips_without_id_or_history() {
    let base = tempfile::tempdir().unwrap();
    let agent = armar(base.path(), RunnerFalso::nuevo(Guion::Responder, (0, 0)));

    agent.save().unwrap();

    assert!(agent.conversation_id().is_none());
    assert!(conversaciones(base.path()).list().unwrap().is_empty());
}

#[tokio::test]
async fn timeout_saves_partial_history() {
    let base = tempfile::tempdir().unwrap();
    let agent = Agent::new(
        conversaciones(base.path()),
        Arc::new(ApprovalQueue::new()),
        RunnerFalso::nuevo(Guion::EsperarCancelacionConParcial, (0, 0)),
    )
    .with_turn_timeout(Duration::from_millis(20));

    let resultado = agent.send("hola", turno()).await;

    assert!(matches!(resultado, Err(AgentError::TimedOut)));
    let historial = agent.history();
    assert!(
        historial.iter().any(|mensaje| mensaje.content == "parcial"),
        "el historial en memoria perdió lo parcial: {historial:?}"
    );
    let id = agent.conversation_id().expect("id de la conversación");
    let guardada = conversaciones(base.path()).load(&id).unwrap();
    assert!(
        guardada
            .messages
            .iter()
            .any(|mensaje| mensaje.content == "parcial"),
        "la conversación guardada perdió lo parcial: {:?}",
        guardada.messages
    );
    assert!(!agent.busy());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_is_not_timeout() {
    let base = tempfile::tempdir().unwrap();
    let runner = RunnerFalso::nuevo(Guion::EsperarCancelacion, (0, 0));
    let runner_del_agente: Arc<RunnerFalso> = Arc::clone(&runner);
    let agent = Arc::new(
        Agent::new(
            conversaciones(base.path()),
            Arc::new(ApprovalQueue::new()),
            runner_del_agente,
        )
        .with_turn_timeout(Duration::from_secs(5)),
    );
    let tarea = tokio::spawn({
        let agent = Arc::clone(&agent);
        async move { agent.send("hola", turno()).await }
    });
    runner.empezado.notified().await;

    let cancelador = tokio::spawn({
        let agent = Arc::clone(&agent);
        async move { agent.cancel() }
    });
    cancelador.await.unwrap();

    assert!(matches!(tarea.await.unwrap(), Err(AgentError::Cancelled)));
}
