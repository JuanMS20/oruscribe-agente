use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Local, TimeZone};
use orus_app::{
    BELL_RETRY_LIMIT, Backend, Bell, BellConnector, ClientProvider, Event, EventSink, RetryLimiter,
    RingSink, SessionError, Sleeper, WakeTrigger, Watch, backoff_delay,
};
use orus_org::doorbell::{Config, DoorbellError, Event as DoorbellEvent, OnEvent};
use orus_org::{JsonObject, Me, MessagesPage, Room, Status, Task, ToolSpec};
use serde_json::json;
use tokio::sync::{Notify, mpsc};

#[derive(Clone)]
struct BackendFalso {
    sin_timbre: bool,
    tasks: Vec<Task>,
    rooms: Vec<Room>,
}

#[async_trait]
impl Backend for BackendFalso {
    async fn me(&self) -> Result<Me, SessionError> {
        Ok(Me::default())
    }
    async fn tasks(&self) -> Result<Vec<Task>, SessionError> {
        Ok(self.tasks.clone())
    }
    async fn rooms(&self) -> Result<Vec<Room>, SessionError> {
        Ok(self.rooms.clone())
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
    async fn doorbell(&self) -> Result<Config, SessionError> {
        if self.sin_timbre {
            Err(SessionError::NoDoorbell)
        } else {
            Ok(Config::default())
        }
    }
}

#[async_trait]
impl ClientProvider for BackendFalso {
    async fn client(&self) -> Result<Arc<dyn Backend>, SessionError> {
        Ok(Arc::new(self.clone()))
    }
}

struct CampanaFalsa {
    done: Arc<Notify>,
}

#[async_trait]
impl Bell for CampanaFalsa {
    async fn done(&self) {
        self.done.notified().await;
    }
    async fn close(&self) {
        self.done.notify_waiters();
    }
}

struct ConectorFalso {
    on_event: Mutex<Option<OnEvent>>,
    calls: AtomicUsize,
    connected: Notify,
}

impl ConectorFalso {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            on_event: Mutex::new(None),
            calls: AtomicUsize::new(0),
            connected: Notify::new(),
        })
    }
    fn emit(&self, event: DoorbellEvent) {
        let mut on_event = self.on_event.lock().unwrap();
        let on_event = on_event.as_mut().expect("el timbre no está conectado");
        on_event(event);
    }
}

#[async_trait]
impl BellConnector for ConectorFalso {
    async fn connect(&self, _: Config, on_event: OnEvent) -> Result<Box<dyn Bell>, DoorbellError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self.on_event.lock().unwrap() = Some(on_event);
        self.connected.notify_one();
        Ok(Box::new(CampanaFalsa {
            done: Arc::new(Notify::new()),
        }))
    }
}

struct SleeperFalso {
    seen: Mutex<Vec<Duration>>,
    started: Notify,
    gate: Option<Arc<Notify>>,
}

impl SleeperFalso {
    fn new(gate: Option<Arc<Notify>>) -> Arc<Self> {
        Arc::new(Self {
            seen: Mutex::new(Vec::new()),
            started: Notify::new(),
            gate,
        })
    }
    fn count(&self, duration: Duration) -> usize {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|seen| **seen == duration)
            .count()
    }
    fn seen(&self) -> Vec<Duration> {
        self.seen.lock().unwrap().clone()
    }
}

#[async_trait]
impl Sleeper for SleeperFalso {
    async fn sleep(&self, duration: Duration) {
        self.seen.lock().unwrap().push(duration);
        self.started.notify_one();
        if let Some(gate) = &self.gate {
            gate.notified().await;
        }
    }
}

struct DespertarFalso {
    events: mpsc::UnboundedSender<DoorbellEvent>,
}

#[async_trait]
impl WakeTrigger for DespertarFalso {
    async fn despertar(&self, event: DoorbellEvent) {
        let _ = self.events.send(event);
    }
}

fn despertador() -> (Arc<DespertarFalso>, mpsc::UnboundedReceiver<DoorbellEvent>) {
    let (events, rx) = mpsc::unbounded_channel();
    (Arc::new(DespertarFalso { events }), rx)
}

fn eventos() -> (EventSink, mpsc::UnboundedReceiver<Event>) {
    let (tx, rx) = mpsc::unbounded_channel();
    (
        Arc::new(move |event| {
            let _ = tx.send(event);
        }),
        rx,
    )
}

fn anillos() -> (RingSink, mpsc::UnboundedReceiver<(Event, Option<String>)>) {
    let (tx, rx) = mpsc::unbounded_channel();
    (
        Arc::new(move |event, error| {
            let _ = tx.send((event, error));
        }),
        rx,
    )
}

fn ahora() -> DateTime<Local> {
    Local
        .with_ymd_and_hms(2026, 11, 12, 9, 0, 0)
        .single()
        .unwrap()
}

fn ping(id: &str) -> DoorbellEvent {
    DoorbellEvent {
        kind: "ping".to_owned(),
        name: "ping".to_owned(),
        raw: json!({ "id": id }),
    }
}

async fn recibir<T>(rx: &mut mpsc::UnboundedReceiver<T>) -> T {
    tokio::time::timeout(Duration::from_secs(1), rx.recv())
        .await
        .expect("timeout")
        .expect("canal cerrado")
}

async fn cerrar(watch: &Watch, task: tokio::task::JoinHandle<()>) {
    watch.close().await;
    tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .expect("el bucle no terminó")
        .expect("tarea");
}

#[test]
fn retry_limiter_allows_six_per_minute() {
    let now = ahora();
    let mut limiter = RetryLimiter::new(BELL_RETRY_LIMIT, Duration::from_secs(60));
    assert_eq!(BELL_RETRY_LIMIT, 6);
    for _ in 0..BELL_RETRY_LIMIT {
        assert!(limiter.wait(now).is_none());
    }
}

#[test]
fn retry_limiter_waits_after_limit() {
    let now = ahora();
    let mut limiter = RetryLimiter::new(BELL_RETRY_LIMIT, Duration::from_secs(60));
    for _ in 0..BELL_RETRY_LIMIT {
        limiter.wait(now);
    }
    assert_eq!(limiter.wait(now), Some(Duration::from_secs(60)));
    assert_eq!(
        limiter.wait(now + chrono::Duration::seconds(30)),
        Some(Duration::from_secs(30))
    );
    assert!(limiter.wait(now + chrono::Duration::seconds(60)).is_none());
}

#[test]
fn backoff_caps_at_eighty() {
    let seconds: Vec<u64> = (0..7)
        .map(|attempt| backoff_delay(attempt).as_secs())
        .collect();
    assert_eq!(seconds, [5, 10, 20, 40, 80, 80, 80]);
}

#[tokio::test]
async fn no_bell_stops_without_retry() {
    let connector = ConectorFalso::new();
    let watch = Arc::new(Watch::new(connector.clone()));
    let client = Arc::new(BackendFalso {
        sin_timbre: true,
        tasks: vec![],
        rooms: vec![],
    });
    let sleeper = SleeperFalso::new(None);
    let (on_event, mut events) = eventos();
    let (on_ring, mut rings) = anillos();
    let (wake, _woken) = despertador();
    let ctx = watch.begin_watch(1);
    tokio::time::timeout(
        Duration::from_secs(1),
        watch.watch_loop(ctx, 1, client, on_event, on_ring, wake, sleeper.clone()),
    )
    .await
    .expect("el bucle no terminó");
    assert_eq!(connector.calls.load(Ordering::SeqCst), 0);
    assert_eq!(watch.bell_error(), "este cliente no ofrece timbre");
    assert!(events.try_recv().is_err());
    assert!(rings.try_recv().is_err());
    assert!(sleeper.seen().is_empty());
}

#[tokio::test]
async fn ping_emits_and_wakes() {
    let connector = ConectorFalso::new();
    let watch = Arc::new(Watch::new(connector.clone()));
    let client = Arc::new(BackendFalso {
        sin_timbre: false,
        tasks: vec![],
        rooms: vec![],
    });
    let sleeper = SleeperFalso::new(None);
    let (on_event, mut events) = eventos();
    let (on_ring, mut rings) = anillos();
    let (wake, mut woken) = despertador();
    let ctx = watch.begin_watch(1);
    let task = tokio::spawn({
        let watch = watch.clone();
        async move {
            watch
                .watch_loop(ctx, 1, client, on_event, on_ring, wake, sleeper)
                .await;
        }
    });
    tokio::time::timeout(Duration::from_secs(1), connector.connected.notified())
        .await
        .expect("no conectó");
    connector.emit(DoorbellEvent {
        kind: "mention".to_owned(),
        name: "ping".to_owned(),
        raw: json!({ "id": "e1" }),
    });
    let event = recibir(&mut events).await;
    assert_eq!(event.kind, "mention");
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["open"], 0);
    assert_eq!(json["chat"], 0);
    assert!(json.get("at").is_some());
    let (ring, error) = recibir(&mut rings).await;
    assert_eq!(ring.kind, "mention");
    assert!(error.is_none());
    assert_eq!(recibir(&mut woken).await.kind, "mention");
    cerrar(&watch, task).await;
}

/// La campana que se cae una sola vez: el primer `done()` vuelve al instante,
/// el segundo queda pendiente como un canal sano.
struct ConectorQueCaeUnaVez {
    calls: AtomicUsize,
    connected: Notify,
}

impl ConectorQueCaeUnaVez {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            connected: Notify::new(),
        })
    }
}

struct CampanaCaida;

#[async_trait]
impl Bell for CampanaCaida {
    async fn done(&self) {}
    async fn close(&self) {}
}

#[async_trait]
impl BellConnector for ConectorQueCaeUnaVez {
    async fn connect(&self, _: Config, _: OnEvent) -> Result<Box<dyn Bell>, DoorbellError> {
        let llamada = self.calls.fetch_add(1, Ordering::SeqCst);
        self.connected.notify_one();
        if llamada == 0 {
            Ok(Box::new(CampanaCaida))
        } else {
            Ok(Box::new(CampanaFalsa {
                done: Arc::new(Notify::new()),
            }))
        }
    }
}

/// El hallazgo: cuando el timbre moría en silencio, `done()` no despertaba y
/// la app se quedaba "Conectada" para siempre. Al caerse, el bucle reconecta.
#[tokio::test]
async fn un_timbre_que_se_cae_reconecta() {
    let connector = ConectorQueCaeUnaVez::new();
    let watch = Arc::new(Watch::new(connector.clone()));
    let client = Arc::new(BackendFalso {
        sin_timbre: false,
        tasks: vec![],
        rooms: vec![],
    });
    let sleeper = SleeperFalso::new(None);
    let (on_event, _events) = eventos();
    let (on_ring, mut rings) = anillos();
    let (wake, _woken) = despertador();
    let ctx = watch.begin_watch(1);
    let task = tokio::spawn({
        let watch = watch.clone();
        async move {
            watch
                .watch_loop(ctx, 1, client, on_event, on_ring, wake, sleeper)
                .await;
        }
    });

    tokio::time::timeout(Duration::from_secs(1), async {
        while connector.calls.load(Ordering::SeqCst) < 2 {
            connector.connected.notified().await;
        }
    })
    .await
    .expect("el timbre caído no reconectó");

    let (ring, error) = recibir(&mut rings).await;
    assert_eq!(ring.kind, "desconectado");
    assert!(error.is_some(), "el aviso nombra la caída");

    tokio::time::timeout(Duration::from_secs(1), async {
        while !watch.conectado() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("quedó sin conexión después de reconectar");

    cerrar(&watch, task).await;
}

#[tokio::test]
async fn counts_collapse_burst() {
    let connector = ConectorFalso::new();
    let watch = Arc::new(Watch::new(connector.clone()));
    let client = Arc::new(BackendFalso {
        sin_timbre: false,
        tasks: vec![
            Task {
                status: Status::Pending,
                ..Task::default()
            },
            Task {
                status: Status::Done,
                ..Task::default()
            },
            Task {
                status: Status::InProgress,
                ..Task::default()
            },
        ],
        rooms: vec![
            Room {
                unread: 2,
                ..Room::default()
            },
            Room {
                unread: 3,
                ..Room::default()
            },
        ],
    });
    let gate = Arc::new(Notify::new());
    let sleeper = SleeperFalso::new(Some(gate.clone()));
    let (on_event, mut events) = eventos();
    let (on_ring, _rings) = anillos();
    let (wake, mut woken) = despertador();
    let ctx = watch.begin_watch(1);
    let task = tokio::spawn({
        let watch = watch.clone();
        let sleeper = sleeper.clone();
        async move {
            watch
                .watch_loop(ctx, 1, client, on_event, on_ring, wake, sleeper)
                .await;
        }
    });
    tokio::time::timeout(Duration::from_secs(1), connector.connected.notified())
        .await
        .expect("no conectó");
    connector.emit(ping("e1"));
    tokio::time::timeout(Duration::from_secs(1), sleeper.started.notified())
        .await
        .expect("no agendó conteos");
    assert_eq!(sleeper.count(Duration::from_secs(3)), 1);
    connector.emit(ping("e2"));
    connector.emit(ping("e3"));
    for _ in 0..3 {
        recibir(&mut woken).await;
    }
    assert_eq!(sleeper.count(Duration::from_secs(3)), 1);
    gate.notify_one();
    let counts = loop {
        let event = recibir(&mut events).await;
        if event.kind == "counts" {
            break event;
        }
    };
    assert_eq!((counts.open, counts.chat), (2, 5));
    cerrar(&watch, task).await;
}
