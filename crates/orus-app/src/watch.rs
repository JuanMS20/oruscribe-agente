//! Timbre de la app: port de `internal/app/watch.go:15-303`. El hub HTTP no se porta.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::{DateTime, Local};
use orus_org::doorbell::{self, Config, DoorbellError, Event as DoorbellEvent, OnEvent};
use rand::Rng;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex as AsyncMutex, mpsc, watch};

use crate::sesion::{App, Backend, SessionError};
use crate::sync::lock;
use crate::wake::{Despertador, WakePolicy};

pub const BELL_RETRY_LIMIT: usize = 6;
const RETRY_WINDOW: Duration = Duration::from_secs(60);
const BACKOFF_INITIAL: Duration = Duration::from_secs(5);
const BACKOFF_MAX: Duration = Duration::from_secs(80);
const COUNTS_DEBOUNCE: Duration = Duration::from_secs(3);
const COUNTS_TIMEOUT: Duration = Duration::from_secs(30);
/// Dos lecturas de counts nunca quedan a menos de esto: cada una cuesta
/// 10-20 consultas en el servidor y el timbre puede tocar muy seguido.
const COUNTS_MIN: Duration = Duration::from_secs(60);
const CONEXION_ESTABLE: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub kind: String,
    pub at: DateTime<Local>,
    #[serde(default)]
    pub open: i64,
    #[serde(default)]
    pub chat: i64,
}

impl Event {
    fn new(kind: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            at: Local::now(),
            open: 0,
            chat: 0,
        }
    }
}

pub type EventSink = Arc<dyn Fn(Event) + Send + Sync>;
pub type RingSink = Arc<dyn Fn(Event, Option<String>) + Send + Sync>;

#[derive(Debug, Clone)]
pub struct RetryLimiter {
    limit: usize,
    window: chrono::Duration,
    at: Vec<DateTime<Local>>,
}

impl RetryLimiter {
    pub fn new(limit: usize, window: Duration) -> Self {
        Self {
            limit: limit.max(1),
            window: chrono::Duration::from_std(window).unwrap_or_default(),
            at: Vec::new(),
        }
    }
    pub fn wait(&mut self, now: DateTime<Local>) -> Option<Duration> {
        let cut = now - self.window;
        self.at.retain(|at| *at > cut);
        if self.at.len() < self.limit {
            self.at.push(now);
            return None;
        }
        (self.at[0] + self.window - now).to_std().ok()
    }
    /// Una conexión sana borra las marcas: abrir y caer no es un reintento
    /// fallido y no debe agotar la ventana de 60 s (#127).
    fn reset(&mut self) {
        self.at.clear();
    }
}

pub fn backoff_delay(attempt: u32) -> Duration {
    let seconds = BACKOFF_INITIAL
        .as_secs()
        .saturating_mul(1u64 << attempt.min(5));
    Duration::from_secs(seconds.min(BACKOFF_MAX.as_secs()))
}

fn with_jitter(backoff: Duration) -> Duration {
    let max_ms = u64::try_from(backoff.as_millis() / 4).unwrap_or(u64::MAX);
    backoff + Duration::from_millis(rand::thread_rng().gen_range(0..=max_ms))
}

#[async_trait]
pub trait Sleeper: Send + Sync {
    async fn sleep(&self, duration: Duration);
}

#[derive(Debug, Default, Clone, Copy)]
pub struct TokioSleeper;

#[async_trait]
impl Sleeper for TokioSleeper {
    async fn sleep(&self, duration: Duration) {
        tokio::time::sleep(duration).await;
    }
}

#[async_trait]
pub trait ClientProvider: Send + Sync {
    async fn client(&self) -> Result<Arc<dyn Backend>, SessionError>;

    /// Cachea la configuración del timbre que acaba de traer el servidor. El
    /// default deja intactos los fakes que no la usan (`watch.go:151-155`).
    fn set_bell_config(&self, _config: Config) {}

    /// El id de la persona de la sesión, para no despertar por lo que ella
    /// misma escribió. Sale del estado local: ninguna llamada al servidor.
    fn me(&self) -> String {
        String::new()
    }
}

#[async_trait]
impl ClientProvider for App {
    async fn client(&self) -> Result<Arc<dyn Backend>, SessionError> {
        App::client(self).await
    }

    fn set_bell_config(&self, config: Config) {
        App::set_bell_config(self, config);
    }

    fn me(&self) -> String {
        self.state()
            .me
            .as_ref()
            .map(|me| me.user_id.clone())
            .unwrap_or_default()
    }
}

#[async_trait]
pub trait Bell: Send + Sync {
    async fn done(&self);
    async fn close(&self);
}

#[async_trait]
impl Bell for doorbell::Doorbell {
    async fn done(&self) {
        doorbell::Doorbell::done(self).await;
    }
    async fn close(&self) {
        let _ = doorbell::Doorbell::close(self).await;
    }
}

#[async_trait]
pub trait BellConnector: Send + Sync {
    async fn connect(
        &self,
        config: Config,
        on_event: OnEvent,
    ) -> Result<Box<dyn Bell>, DoorbellError>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct RealBellConnector;

#[async_trait]
impl BellConnector for RealBellConnector {
    async fn connect(
        &self,
        config: Config,
        on_event: OnEvent,
    ) -> Result<Box<dyn Bell>, DoorbellError> {
        doorbell::dial(config, Some(on_event))
            .await
            .map(|bell| Box::new(bell) as Box<dyn Bell>)
    }
}

#[async_trait]
pub trait WakeTrigger: Send + Sync {
    async fn despertar(&self, event: DoorbellEvent);
}

pub struct WakePolicyTrigger {
    pub policy: Arc<AsyncMutex<WakePolicy>>,
    pub me: String,
    pub agent: Arc<dyn Despertador>,
}

#[async_trait]
impl WakeTrigger for WakePolicyTrigger {
    async fn despertar(&self, event: DoorbellEvent) {
        self.policy
            .lock()
            .await
            .despertar(event, &self.me, self.agent.as_ref())
            .await;
    }
}

#[derive(Clone)]
struct PingDeps {
    client: Arc<dyn ClientProvider>,
    on_event: EventSink,
    on_ring: RingSink,
    wake: Arc<dyn WakeTrigger>,
    sleeper: Arc<dyn Sleeper>,
}

#[derive(Debug, thiserror::Error)]
enum WatchError {
    #[error("este cliente no ofrece timbre")]
    NoDoorbell,
    #[error("el timbre se cayó")]
    Caido(Duration),
    #[error("{0}")]
    Other(String),
}

pub struct Watch {
    connector: Arc<dyn BellConnector>,
    generation: AtomicU64,
    cancel: Mutex<Option<watch::Sender<bool>>>,
    bell: Mutex<Option<Arc<dyn Bell>>>,
    bell_error: Mutex<String>,
    counts: Mutex<Option<(u64, watch::Sender<bool>)>>,
    counts_seq: AtomicU64,
    counts_pending: AtomicBool,
    ultima_counts: Mutex<Option<Instant>>,
}

impl Watch {
    pub fn new(connector: Arc<dyn BellConnector>) -> Self {
        Self {
            connector,
            generation: AtomicU64::new(0),
            cancel: Mutex::new(None),
            bell: Mutex::new(None),
            bell_error: Mutex::new(String::new()),
            counts: Mutex::new(None),
            counts_seq: AtomicU64::new(0),
            counts_pending: AtomicBool::new(false),
            ultima_counts: Mutex::new(None),
        }
    }
    pub fn begin_watch(&self, generation: u64) -> watch::Receiver<bool> {
        self.stop_watch();
        self.generation.store(generation, Ordering::SeqCst);
        let (cancel, rx) = watch::channel(false);
        *lock(&self.cancel) = Some(cancel);
        rx
    }
    pub fn bell_error(&self) -> String {
        lock(&self.bell_error).clone()
    }

    /// Si el canal en vivo está abierto ahora. Lo lee el estado de ajustes.
    pub fn conectado(&self) -> bool {
        lock(&self.bell).is_some()
    }
    fn set_bell_error(&self, error: &str) {
        *lock(&self.bell_error) = error.to_owned();
    }
    pub fn end_watch(&self, generation: u64) {
        if self.generation.load(Ordering::SeqCst) == generation {
            let _ = lock(&self.cancel).take();
        }
    }
    pub fn stop_watch(&self) {
        if let Some(cancel) = lock(&self.cancel).take() {
            cancel.send_replace(true);
        }
        // El logout pasa por aquí: cortar también los contadores evita que
        // `publish_counts` siga consultando el servidor con la sesión cerrada.
        if let Some((_, counts)) = lock(&self.counts).take() {
            counts.send_replace(true);
        }
        self.counts_pending.store(false, Ordering::SeqCst);
        if let Some(bell) = lock(&self.bell).clone() {
            tokio::spawn(async move {
                bell.close().await;
            });
        }
    }
    pub async fn close(&self) {
        self.stop_watch();
        let bell = lock(&self.bell).clone();
        if let Some(bell) = bell {
            bell.close().await;
        }
    }
    #[allow(clippy::too_many_arguments)]
    pub async fn watch_loop(
        self: &Arc<Self>,
        ctx: watch::Receiver<bool>,
        generation: u64,
        client: Arc<dyn ClientProvider>,
        on_event: EventSink,
        on_ring: RingSink,
        wake: Arc<dyn WakeTrigger>,
        sleeper: Arc<dyn Sleeper>,
    ) {
        let deps = PingDeps {
            client,
            on_event,
            on_ring,
            wake,
            sleeper,
        };
        let mut limiter = RetryLimiter::new(BELL_RETRY_LIMIT, RETRY_WINDOW);
        let mut attempt = 0u32;
        while !*ctx.borrow() {
            if let Some(wait) = limiter.wait(Local::now()) {
                tokio::select! {
                    _ = cancelled(&ctx) => break,
                    _ = deps.sleeper.sleep(wait) => {}
                }
                continue;
            }
            match self.watch_once(&ctx, &deps).await {
                Ok(()) => break,
                Err(WatchError::NoDoorbell) => {
                    self.set_bell_error("este cliente no ofrece timbre");
                    break;
                }
                Err(error) => {
                    self.set_bell_error(&format!("timbre: {error}"));
                    crate::telemetria::Telemetry::compartida().report(
                        "watch.bell",
                        "warning",
                        &format!("timbre: {error}"),
                        serde_json::json!({ "attempt": attempt }),
                    );
                    (deps.on_ring)(Event::new("desconectado"), Some(error.to_string()));
                    // Una conexión sana reinicia el backoff y el limitador:
                    // la caída posterior no hereda los intentos de antes.
                    if let WatchError::Caido(estuvo) = error
                        && estuvo >= CONEXION_ESTABLE
                    {
                        attempt = 0;
                        limiter.reset();
                    }
                    let wait = with_jitter(backoff_delay(attempt));
                    attempt = attempt.saturating_add(1);
                    tokio::select! {
                        _ = cancelled(&ctx) => break,
                        _ = deps.sleeper.sleep(wait) => {}
                    }
                }
            }
        }
        self.end_watch(generation);
    }
    async fn watch_once(
        self: &Arc<Self>,
        ctx: &watch::Receiver<bool>,
        deps: &PingDeps,
    ) -> Result<(), WatchError> {
        let generation = self.generation.load(Ordering::SeqCst);
        let backend = deps
            .client
            .client()
            .await
            .map_err(|e| WatchError::Other(e.to_string()))?;
        let config = backend.doorbell().await.map_err(|error| match error {
            SessionError::NoDoorbell => WatchError::NoDoorbell,
            other => WatchError::Other(other.to_string()),
        })?;
        // Go guarda la copia recién cuando `doorbell.Dial` tuvo éxito
        // (`watch.go:151-155`): un timbre que no abre no deja base al cerebro.
        let config_para_cache = config.clone();
        let (events, mut incoming) = mpsc::unbounded_channel();
        let watch = Arc::clone(self);
        let ping_deps = deps.clone();
        let pings = tokio::spawn(async move {
            while let Some(event) = incoming.recv().await {
                watch.on_ping(event, &ping_deps).await;
            }
        });
        let connected = tokio::select! {
            _ = cancelled(ctx) => {
                pings.abort();
                return Ok(());
            }
            result = self.connector.connect(config, Box::new(move |event| {
                let _ = events.send(event);
            })) => result,
        };
        let bell: Arc<dyn Bell> = match connected {
            Ok(bell) => {
                // El logout pudo llegar mientras conectaba: el canal de la
                // sesión cerrada no se guarda ni se cachea; se cierra ahora.
                if *ctx.borrow() || self.generation.load(Ordering::SeqCst) != generation {
                    pings.abort();
                    bell.close().await;
                    return Ok(());
                }
                deps.client.set_bell_config(config_para_cache);
                Arc::from(bell)
            }
            Err(error) => {
                pings.abort();
                return Err(WatchError::Other(error.to_string()));
            }
        };
        let conectado_en = Instant::now();
        *lock(&self.bell) = Some(Arc::clone(&bell));
        self.set_bell_error("");
        bell.done().await;
        let mut current = lock(&self.bell);
        if current
            .as_ref()
            .is_some_and(|stored| Arc::ptr_eq(stored, &bell))
        {
            *current = None;
        }
        drop(current);
        pings.abort();
        if *ctx.borrow() {
            return Ok(());
        }
        Err(WatchError::Caido(conectado_en.elapsed()))
    }
    async fn on_ping(self: &Arc<Self>, event: DoorbellEvent, deps: &PingDeps) {
        let kind = if event.kind.is_empty() {
            "ping".to_owned()
        } else {
            event.kind.clone()
        };
        let published = Event::new(kind);
        (deps.on_event)(published.clone());
        (deps.on_ring)(published, None);
        self.schedule_counts(deps);
        let wake = Arc::clone(&deps.wake);
        tokio::spawn(async move {
            wake.despertar(event).await;
        });
    }
    fn soltar_counts(&self, id: u64) {
        let mut counts = lock(&self.counts);
        if counts.as_ref().is_some_and(|(actual, _)| *actual == id) {
            *counts = None;
        }
    }
    fn schedule_counts(self: &Arc<Self>, deps: &PingDeps) {
        let mut counts = lock(&self.counts);
        if counts.is_some() {
            // Llegó un ping mientras se publicaba. Se anota y se reprograma al
            // terminar: no se publica ahora porque dos lecturas de salas se
            // solaparían, y cada una cuesta 10-20 consultas en el servidor.
            self.counts_pending.store(true, Ordering::SeqCst);
            return;
        }
        self.counts_pending.store(false, Ordering::SeqCst);
        let id = self.counts_seq.fetch_add(1, Ordering::SeqCst) + 1;
        let (cancel, rx) = watch::channel(false);
        *counts = Some((id, cancel));
        drop(counts);
        let watch = Arc::clone(self);
        let deps = deps.clone();
        tokio::spawn(async move {
            let espera = espera_counts(COUNTS_DEBOUNCE, *lock(&watch.ultima_counts));
            tokio::select! {
                _ = cancelled(&rx) => {
                    watch.soltar_counts(id);
                    return;
                }
                _ = deps.sleeper.sleep(espera) => {}
            }
            let _ = tokio::time::timeout(COUNTS_TIMEOUT, publish_counts(&deps)).await;
            // Cuenta como intento aunque falle: sin esto, un servidor caído
            // se leería en cada ping.
            *lock(&watch.ultima_counts) = Some(Instant::now());
            // La marca se suelta al terminar, no al empezar, para no solapar.
            watch.soltar_counts(id);
            if watch.counts_pending.swap(false, Ordering::SeqCst) {
                watch.schedule_counts(&deps);
            }
        });
    }
}

/// La espera antes de leer counts: el debounce, estirado para que dos
/// lecturas nunca queden a menos de `COUNTS_MIN`.
fn espera_counts(debounce: Duration, ultima: Option<Instant>) -> Duration {
    match ultima {
        Some(fue) => debounce.max(COUNTS_MIN.saturating_sub(fue.elapsed())),
        None => debounce,
    }
}

async fn publish_counts(deps: &PingDeps) {
    let Ok(backend) = deps.client.client().await else {
        return;
    };
    let (tasks, rooms) = tokio::join!(backend.tasks(), backend.rooms());
    if tasks.is_err() && rooms.is_err() {
        return;
    }
    let mut event = Event::new("counts");
    if let Ok(tasks) = tasks {
        event.open = tasks.iter().filter(|task| task.status.open()).count() as i64;
    }
    if let Ok(rooms) = rooms {
        event.chat = rooms.iter().map(|room| room.unread).sum();
    }
    (deps.on_event)(event.clone());
    (deps.on_ring)(event, None);
}

async fn cancelled(ctx: &watch::Receiver<bool>) {
    let mut ctx = ctx.clone();
    if *ctx.borrow() {
        return;
    }
    while ctx.changed().await.is_ok() {
        if *ctx.borrow() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicU32, AtomicUsize};

    use orus_org::{JsonObject, Me, MessagesPage, Room, Task, ToolSpec};
    use tokio::sync::Notify;

    use super::*;

    struct ClienteConTimbre {
        config: Config,
        guardadas: Arc<StdMutex<Vec<Config>>>,
    }

    #[async_trait]
    impl ClientProvider for ClienteConTimbre {
        async fn client(&self) -> Result<Arc<dyn Backend>, SessionError> {
            Ok(Arc::new(BackendDePrueba {
                config: self.config.clone(),
            }) as Arc<dyn Backend>)
        }

        fn set_bell_config(&self, config: Config) {
            self.guardadas
                .lock()
                .expect("configuraciones guardadas")
                .push(config);
        }
    }

    struct BackendDePrueba {
        config: Config,
    }

    #[async_trait]
    impl Backend for BackendDePrueba {
        async fn me(&self) -> Result<Me, SessionError> {
            Err(SessionError::Connect("sin red".to_owned()))
        }

        async fn tasks(&self) -> Result<Vec<Task>, SessionError> {
            Err(SessionError::Connect("sin red".to_owned()))
        }

        async fn rooms(&self) -> Result<Vec<Room>, SessionError> {
            Err(SessionError::Connect("sin red".to_owned()))
        }

        async fn messages(&self, _: &str, _: i64, _: i64) -> Result<MessagesPage, SessionError> {
            Err(SessionError::Connect("sin red".to_owned()))
        }

        async fn send(&self, _: &str, _: &str, _: &str) -> Result<(), SessionError> {
            Err(SessionError::Connect("sin red".to_owned()))
        }

        async fn tool_specs(&self) -> Result<Vec<ToolSpec>, SessionError> {
            Err(SessionError::Connect("sin red".to_owned()))
        }

        async fn call_tool(&self, _: &str, _: JsonObject) -> Result<String, SessionError> {
            Err(SessionError::Connect("sin red".to_owned()))
        }

        async fn doorbell(&self) -> Result<Config, SessionError> {
            Ok(self.config.clone())
        }
    }

    struct SinDespertar;

    #[async_trait]
    impl WakeTrigger for SinDespertar {
        async fn despertar(&self, _: DoorbellEvent) {}
    }

    struct TimbreQueTermina;

    #[async_trait]
    impl Bell for TimbreQueTermina {
        async fn done(&self) {}
        async fn close(&self) {}
    }

    struct ConectorQueTermina;

    #[async_trait]
    impl BellConnector for ConectorQueTermina {
        async fn connect(&self, _: Config, _: OnEvent) -> Result<Box<dyn Bell>, DoorbellError> {
            Ok(Box::new(TimbreQueTermina))
        }
    }

    struct ConectorQueFalla;

    #[async_trait]
    impl BellConnector for ConectorQueFalla {
        async fn connect(&self, _: Config, _: OnEvent) -> Result<Box<dyn Bell>, DoorbellError> {
            Err(DoorbellError::Connect("sin red".to_owned()))
        }
    }

    /// Go cachea la copia cuando `Dial` ya abrió el canal; si se moviera antes,
    /// un timbre que no conecta dejaría al cerebro con una base que no existe.
    #[tokio::test]
    async fn watch_once_cachea_el_timbre_al_conectar() {
        let guardadas = Arc::new(StdMutex::new(Vec::new()));
        let config = Config {
            supabase_url: "https://proyecto.supabase.co".to_owned(),
            ..Config::default()
        };
        let deps = PingDeps {
            client: Arc::new(ClienteConTimbre {
                config: config.clone(),
                guardadas: Arc::clone(&guardadas),
            }),
            on_event: Arc::new(|_| {}),
            on_ring: Arc::new(|_, _| {}),
            wake: Arc::new(SinDespertar),
            sleeper: Arc::new(TokioSleeper),
        };
        let timbre = Arc::new(Watch::new(Arc::new(ConectorQueTermina)));
        let (_cancelar, ctx) = tokio::sync::watch::channel(false);

        let _ = timbre.watch_once(&ctx, &deps).await;

        let guardadas = guardadas.lock().expect("configuraciones guardadas");
        assert_eq!(
            guardadas.len(),
            1,
            "watch_once tiene que cachear el timbre una sola vez"
        );
        assert_eq!(guardadas[0].supabase_url, "https://proyecto.supabase.co");
    }

    #[tokio::test]
    async fn watch_once_no_cachea_si_la_conexion_falla() {
        let guardadas = Arc::new(StdMutex::new(Vec::new()));
        let deps = PingDeps {
            client: Arc::new(ClienteConTimbre {
                config: Config {
                    supabase_url: "https://proyecto.supabase.co".to_owned(),
                    ..Config::default()
                },
                guardadas: Arc::clone(&guardadas),
            }),
            on_event: Arc::new(|_| {}),
            on_ring: Arc::new(|_, _| {}),
            wake: Arc::new(SinDespertar),
            sleeper: Arc::new(TokioSleeper),
        };
        let timbre = Arc::new(Watch::new(Arc::new(ConectorQueFalla)));
        let (_cancelar, ctx) = tokio::sync::watch::channel(false);

        let resultado = timbre.watch_once(&ctx, &deps).await;

        assert!(resultado.is_err(), "la conexión falló");
        assert!(
            guardadas
                .lock()
                .expect("configuraciones guardadas")
                .is_empty(),
            "sin conexión no se cachea la base del cerebro"
        );
    }

    #[test]
    fn espera_counts_estira_el_debounce_al_minimo() {
        assert_eq!(espera_counts(COUNTS_DEBOUNCE, None), COUNTS_DEBOUNCE);
        let recien = espera_counts(COUNTS_DEBOUNCE, Some(Instant::now()));
        assert!(
            (Duration::from_secs(59)..=COUNTS_MIN).contains(&recien),
            "recién leído estira al minuto: {recien:?}"
        );
        let hace_50 = Instant::now() - Duration::from_secs(50);
        let resta = espera_counts(COUNTS_DEBOUNCE, Some(hace_50));
        assert!(
            (Duration::from_secs(9)..=Duration::from_secs(10)).contains(&resta),
            "completa el minuto que falta: {resta:?}"
        );
        assert_eq!(
            espera_counts(COUNTS_DEBOUNCE, Some(Instant::now() - COUNTS_MIN)),
            COUNTS_DEBOUNCE
        );
    }

    /// Registra cada espera y corta el bucle al llegar al tope pedido.
    struct SleeperQueRegistra {
        esperas: Arc<StdMutex<Vec<Duration>>>,
        cancelar: watch::Sender<bool>,
        cortar_en: usize,
    }

    #[async_trait]
    impl Sleeper for SleeperQueRegistra {
        async fn sleep(&self, duration: Duration) {
            let mut esperas = self.esperas.lock().expect("esperas registradas");
            esperas.push(duration);
            if esperas.len() >= self.cortar_en {
                self.cancelar.send_replace(true);
            }
        }
    }

    struct TimbreSano;

    #[async_trait]
    impl Bell for TimbreSano {
        async fn done(&self) {
            tokio::time::sleep(CONEXION_ESTABLE + Duration::from_millis(50)).await;
        }
        async fn close(&self) {}
    }

    #[derive(Default)]
    struct ConectorConHistorial {
        intentos: AtomicU32,
    }

    #[async_trait]
    impl BellConnector for ConectorConHistorial {
        async fn connect(&self, _: Config, _: OnEvent) -> Result<Box<dyn Bell>, DoorbellError> {
            let intento = self.intentos.fetch_add(1, Ordering::SeqCst);
            if intento == 2 {
                Ok(Box::new(TimbreSano))
            } else {
                Err(DoorbellError::Connect("sin red".to_owned()))
            }
        }
    }

    /// El fallo que esto cubre: `attempt` solo subía; tras una conexión sana
    /// que se cae horas después, la reconexión esperaba el backoff acumulado
    /// (tope 80 s) en vez de empezar de nuevo en 5 s.
    #[tokio::test]
    async fn el_backoff_se_reinicia_tras_una_conexion_sana() {
        let (cancelar, ctx) = tokio::sync::watch::channel(false);
        let esperas = Arc::new(StdMutex::new(Vec::new()));
        let sleeper = Arc::new(SleeperQueRegistra {
            esperas: Arc::clone(&esperas),
            cancelar,
            cortar_en: 4,
        });
        let cliente = Arc::new(ClienteConTimbre {
            config: Config::default(),
            guardadas: Arc::new(StdMutex::new(Vec::new())),
        });
        let timbre = Arc::new(Watch::new(Arc::new(ConectorConHistorial::default())));

        timbre
            .watch_loop(
                ctx,
                1,
                cliente,
                Arc::new(|_| {}),
                Arc::new(|_, _| {}),
                Arc::new(SinDespertar),
                sleeper,
            )
            .await;

        let esperas = esperas.lock().expect("esperas registradas");
        assert_eq!(
            esperas.len(),
            4,
            "cuatro esperas: fallo, fallo, caída tras conectar, fallo"
        );
        // El jitter agrega hasta un 25%: 5 s -> [5, 6.25] s, 10 s -> [10, 12.5] s.
        assert!(
            (Duration::from_secs(5)..=Duration::from_millis(6250)).contains(&esperas[0]),
            "el primer fallo espera el inicio del backoff: {:?}",
            esperas[0]
        );
        assert!(
            (Duration::from_secs(10)..=Duration::from_millis(12500)).contains(&esperas[1]),
            "el segundo fallo sube un escalón: {:?}",
            esperas[1]
        );
        assert!(
            (Duration::from_secs(5)..=Duration::from_millis(6250)).contains(&esperas[2]),
            "la caída tras una conexión sana no hereda el backoff acumulado: {:?}",
            esperas[2]
        );
        assert!(
            (Duration::from_secs(10)..=Duration::from_millis(12500)).contains(&esperas[3]),
            "el fallo siguiente sube un escalón desde el inicio: {:?}",
            esperas[3]
        );
    }

    #[tokio::test]
    async fn caidas_inmediatas_agotan_la_ventana_del_limitador() {
        let (cancelar, ctx) = tokio::sync::watch::channel(false);
        let esperas = Arc::new(StdMutex::new(Vec::new()));
        let sleeper = Arc::new(SleeperQueRegistra {
            esperas: Arc::clone(&esperas),
            cancelar,
            cortar_en: 7,
        });
        let cliente = Arc::new(ClienteConTimbre {
            config: Config::default(),
            guardadas: Arc::new(StdMutex::new(Vec::new())),
        });
        let timbre = Arc::new(Watch::new(Arc::new(ConectorQueTermina)));

        timbre
            .watch_loop(
                ctx,
                1,
                cliente,
                Arc::new(|_| {}),
                Arc::new(|_, _| {}),
                Arc::new(SinDespertar),
                sleeper,
            )
            .await;

        let esperas = esperas.lock().expect("esperas registradas");
        assert_eq!(esperas.len(), 7, "siete reconexiones registradas");
        for (i, espera) in esperas.iter().take(6).enumerate() {
            let base = backoff_delay(u32::try_from(i).expect("escalón"));
            let tope = base + base / 4;
            assert!(
                (base..=tope).contains(espera),
                "la caída inmediata {i} tiene que subir el backoff: {espera:?}"
            );
        }
        assert!(
            (Duration::from_secs(59)..=Duration::from_secs(60)).contains(&esperas[6]),
            "la séptima reconexión cae en la ventana del limitador: {:?}",
            esperas[6]
        );
    }

    struct TimbreQueEsperaCierre {
        cerrados: Arc<AtomicUsize>,
        fin: Arc<Notify>,
    }

    #[async_trait]
    impl Bell for TimbreQueEsperaCierre {
        async fn done(&self) {
            self.fin.notified().await;
        }
        async fn close(&self) {
            self.cerrados.fetch_add(1, Ordering::SeqCst);
            self.fin.notify_one();
        }
    }

    /// Marca el cancel y devuelve el timbre sin ningún await: la rama de
    /// connect gana la carrera del `select!` con el logout ya pedido.
    struct ConectorQueConectaEnLogout {
        cancelar: watch::Sender<bool>,
        cerrados: Arc<AtomicUsize>,
        fin: Arc<Notify>,
    }

    #[async_trait]
    impl BellConnector for ConectorQueConectaEnLogout {
        async fn connect(&self, _: Config, _: OnEvent) -> Result<Box<dyn Bell>, DoorbellError> {
            self.cancelar.send_replace(true);
            Ok(Box::new(TimbreQueEsperaCierre {
                cerrados: Arc::clone(&self.cerrados),
                fin: Arc::clone(&self.fin),
            }))
        }
    }

    /// El fallo que esto cubre: un connect que completa durante el logout
    /// guardaba el timbre, cacheaba la config y se quedaba en `done()`:
    /// `conectado()` mentía y el canal de la sesión cerrada seguía abierto.
    #[tokio::test]
    async fn un_timbre_que_conecta_durante_el_logout_se_cierra() {
        let (cancelar, ctx) = tokio::sync::watch::channel(false);
        let cerrados = Arc::new(AtomicUsize::new(0));
        let fin = Arc::new(Notify::new());
        let guardadas = Arc::new(StdMutex::new(Vec::new()));
        let conector = Arc::new(ConectorQueConectaEnLogout {
            cancelar,
            cerrados: Arc::clone(&cerrados),
            fin,
        });
        let timbre = Arc::new(Watch::new(conector));
        let deps = PingDeps {
            client: Arc::new(ClienteConTimbre {
                config: Config::default(),
                guardadas: Arc::clone(&guardadas),
            }),
            on_event: Arc::new(|_| {}),
            on_ring: Arc::new(|_, _| {}),
            wake: Arc::new(SinDespertar),
            sleeper: Arc::new(TokioSleeper),
        };
        let watch = Arc::clone(&timbre);
        let tarea = tokio::spawn(async move { watch.watch_once(&ctx, &deps).await });

        let resultado = tokio::time::timeout(Duration::from_secs(2), tarea)
            .await
            .expect("watch_once no terminó tras el logout")
            .expect("la tarea del watch no entró en pánico");
        assert!(resultado.is_ok(), "el logout cancela el watch sin error");
        assert!(
            !timbre.conectado(),
            "conectado() no puede mentir tras el logout"
        );
        assert_eq!(
            cerrados.load(Ordering::SeqCst),
            1,
            "el timbre que conectó durante el logout se cierra"
        );
        assert!(
            guardadas
                .lock()
                .expect("configuraciones guardadas")
                .is_empty(),
            "la sesión cerrada no cachea la config del timbre"
        );
    }

    #[tokio::test]
    async fn una_counts_vieja_no_quita_la_nueva() {
        let deps = PingDeps {
            client: Arc::new(ClienteConTimbre {
                config: Config::default(),
                guardadas: Arc::new(StdMutex::new(Vec::new())),
            }),
            on_event: Arc::new(|_| {}),
            on_ring: Arc::new(|_, _| {}),
            wake: Arc::new(SinDespertar),
            sleeper: Arc::new(TokioSleeper),
        };
        let timbre = Arc::new(Watch::new(Arc::new(ConectorQueTermina)));
        timbre.schedule_counts(&deps);
        let vieja = lock(&timbre.counts).as_ref().expect("counts").0;
        timbre.stop_watch();
        timbre.schedule_counts(&deps);
        let nueva = lock(&timbre.counts).as_ref().expect("counts").0;

        timbre.soltar_counts(vieja);
        assert!(
            lock(&timbre.counts).is_some(),
            "la tarea cancelada no puede quitar el cancelador nuevo"
        );
        timbre.soltar_counts(nueva);
        assert!(lock(&timbre.counts).is_none());
    }
}
