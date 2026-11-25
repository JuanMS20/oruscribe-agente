//! El borde es dueño del timbre: conecta el websocket, reparte cada evento al
//! `Hub` del long poll y despierta al agente. `App` solo decide cuándo arrancar
//! y detener (`watch.rs:60-65`).

use std::sync::{Arc, Mutex as StdMutex, OnceLock, Weak};

use orus_api::BellStatus;
use orus_api::events::Hub;
use orus_app::{
    Agent, AgentError, App, ClientProvider, Despertador, DoorbellEvent, Event, EventSink,
    RealBellConnector, RingSink, Sleeper, TokioSleeper, WakeEntry, WakeError, WakeLog, WakePolicy,
    WakeTrigger, Watch, WatchSpawner,
};
use tokio::sync::Mutex as AsyncMutex;

/// El diario que se lee sin el candado async mientras corre un turno.
type Diario = Arc<StdMutex<Vec<WakeEntry>>>;

/// Referencias débiles a propósito: el `Agent` guarda el callback de fin de
/// turno, que guarda este despertador. Guardar `Arc` aquí cerraría el ciclo y
/// dejaría la app viva después de cerrarla.
#[derive(Default)]
pub struct Referencias {
    app: OnceLock<Weak<App>>,
    agent: OnceLock<Weak<Agent>>,
}

impl Referencias {
    pub fn set_app(&self, app: Arc<App>) {
        let _ = self.app.set(Arc::downgrade(&app));
    }

    pub fn set_agent(&self, agent: Arc<Agent>) {
        let _ = self.agent.set(Arc::downgrade(&agent));
    }
}

#[derive(Default)]
struct DespertadorReal {
    refs: Arc<Referencias>,
}

#[async_trait::async_trait]
impl Despertador for DespertadorReal {
    async fn send(&self, prompt: &str) -> Result<(), WakeError> {
        let (Some(app), Some(agent)) = (
            self.refs.app.get().and_then(Weak::upgrade),
            self.refs.agent.get().and_then(Weak::upgrade),
        ) else {
            return Err(WakeError::Other("la sesión ya se cerró".to_owned()));
        };
        let turno = app
            .turno_para(prompt)
            .await
            .map_err(|error| WakeError::Other(error.to_string()))?;
        agent.empezar(prompt, turno).map_err(|error| match error {
            AgentError::Busy => WakeError::Busy,
            other => WakeError::Other(other.to_string()),
        })
    }

    fn busy(&self) -> bool {
        self.refs
            .agent
            .get()
            .and_then(Weak::upgrade)
            .map(|agent| agent.busy())
            .unwrap_or(true)
    }

    fn generation(&self) -> u64 {
        self.refs
            .agent
            .get()
            .and_then(Weak::upgrade)
            .map(|agent| agent.generation())
            .unwrap_or(0)
    }
}

struct Despertar {
    policy: Arc<AsyncMutex<WakePolicy>>,
    diario: Diario,
    me: String,
    agente: Arc<dyn Despertador>,
}

#[async_trait::async_trait]
impl WakeTrigger for Despertar {
    async fn despertar(&self, event: DoorbellEvent) {
        let encolado = {
            let mut policy = self.policy.lock().await;
            // Tras un logout el agente sube su generación y cancela el turno en
            // vuelo, cuyo cierre no llega. Alinear antes evita el `active` colgado.
            policy.alinear_generacion(self.agente.generation());
            // El ajuste puede cambiar mientras la app está abierta; se lee del
            // archivo local, sin ninguna llamada al servidor.
            let ajustes = orus_config::load().agent;
            policy.enabled = ajustes.wake_enabled;
            policy.per_minute = u32::try_from(ajustes.wake_per_minute).unwrap_or(0);
            policy.per_day = u32::try_from(ajustes.wake_per_day).unwrap_or(0);
            policy.tokens_per_day = u64::try_from(ajustes.wake_tokens_per_day).unwrap_or(0);
            policy.compartir(Arc::clone(&self.diario));
            policy.encolar(event, &self.me)
        };
        if !encolado {
            return;
        }
        // El turno es la red: corre con el candado suelto para que el diario se
        // pueda leer y el próximo ping solo se encole.
        drenar(Arc::clone(&self.policy), Arc::clone(&self.agente)).await;
    }
}

/// Ejecuta la cola con el candado suelto: se toma para marcar el turno, se
/// suelta para el envío y se vuelve a tomar solo para cerrarlo.
async fn drenar(policy: Arc<AsyncMutex<WakePolicy>>, agente: Arc<dyn Despertador>) {
    loop {
        let turno = {
            let mut politica = policy.lock().await;
            politica.tomar(agente.as_ref())
        };
        let Some(turno) = turno else {
            return;
        };
        match agente.send(&turno.prompt()).await {
            Ok(()) => return,
            Err(WakeError::Busy) => {
                policy.lock().await.devolver(turno);
                return;
            }
            Err(error) => policy.lock().await.fallar(&turno, &error),
        }
    }
}

struct RegistroDespertar {
    diario: Diario,
}

impl WakeLog for RegistroDespertar {
    /// Lee el espejo con un candado corto: no espera al `AsyncMutex` de la
    /// política, así que no devuelve vacío mientras corre un turno.
    fn entries(&self) -> Vec<WakeEntry> {
        let diario = self
            .diario
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        diario.clone()
    }
}

pub struct Timbre {
    watch: Arc<Watch>,
    hub: Arc<Hub>,
    policy: Arc<AsyncMutex<WakePolicy>>,
    diario: Diario,
    despertador: Arc<DespertadorReal>,
}

impl Timbre {
    pub fn new(watch: Arc<Watch>, hub: Arc<Hub>, policy: Arc<AsyncMutex<WakePolicy>>) -> Self {
        Self {
            watch,
            hub,
            policy,
            diario: Arc::new(StdMutex::new(Vec::new())),
            despertador: Arc::new(DespertadorReal::default()),
        }
    }

    /// Las referencias se llenan cuando `App` y `Agent` ya existen.
    pub fn referencias(&self) -> &Arc<Referencias> {
        &self.despertador.refs
    }

    /// Cierra el turno del despertar y encola el siguiente, si lo hay.
    pub fn al_terminar(&self) -> Box<dyn Fn(u64, i64, Option<String>) + Send + Sync> {
        let policy = Arc::clone(&self.policy);
        let diario = self.diario.clone();
        let agente: Arc<dyn Despertador> = self.despertador.clone();
        Box::new(move |generacion, tokens, error| {
            let error = error.map(WakeError::Other);
            let tokens = u64::try_from(tokens.max(0)).unwrap_or(u64::MAX);
            let policy = Arc::clone(&policy);
            let diario = diario.clone();
            let agente = agente.clone();
            tokio::spawn(async move {
                let drenar_cola = {
                    let mut politica = policy.lock().await;
                    politica.compartir(Arc::clone(&diario));
                    politica.cerrar(generacion, tokens, error.as_ref())
                };
                if drenar_cola {
                    drenar(policy, agente).await;
                }
            });
        })
    }

    pub fn registro(&self) -> Arc<dyn WakeLog> {
        Arc::new(RegistroDespertar {
            diario: Arc::clone(&self.diario),
        })
    }
}

impl WatchSpawner for Timbre {
    fn start(&self, generation: u64, client: Arc<dyn ClientProvider>) {
        let ctx = self.watch.begin_watch(generation);
        let me = client.me();
        let watch = Arc::clone(&self.watch);

        let publica = Arc::clone(&self.hub);
        let on_event: EventSink = Arc::new(move |event: Event| publica.publish(event));
        let publica = Arc::clone(&self.hub);
        // `on_ping` y `publish_counts` publican el mismo aviso por `on_event` y
        // otra vez por `on_ring`: publicarlo aquí duplicaría cada evento en el
        // long poll. El único que no pasa por `on_event` es el de desconexión,
        // que llega con error.
        let on_ring: RingSink = Arc::new(move |event: Event, error: Option<String>| {
            if error.is_some() {
                publica.publish(event);
            }
        });

        let despertar: Arc<dyn WakeTrigger> = Arc::new(Despertar {
            policy: Arc::clone(&self.policy),
            diario: self.diario.clone(),
            me,
            agente: self.despertador.clone(),
        });
        let sleeper: Arc<dyn Sleeper> = Arc::new(TokioSleeper);

        tokio::spawn(async move {
            watch
                .watch_loop(
                    ctx, generation, client, on_event, on_ring, despertar, sleeper,
                )
                .await;
        });
    }

    fn stop(&self) {
        self.watch.stop_watch();
        // Logout o cambio de sesión: la copia del timbre es de la org anterior
        // y no puede quedar como base del cerebro de la sesión nueva.
        if let Some(app) = self.despertador.refs.app.get().and_then(Weak::upgrade) {
            app.clear_bell_config();
        }
    }
}

impl BellStatus for Timbre {
    fn doorbell(&self) -> bool {
        self.watch.conectado()
    }

    fn error(&self) -> String {
        self.watch.bell_error()
    }
}

/// El conector real que usa el borde. Separándolo del `Watch` se puede probar
/// el cableado sin abrir un websocket.
pub fn conector_real() -> Arc<dyn orus_app::BellConnector> {
    Arc::new(RealBellConnector)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use async_trait::async_trait;
    use orus_app::{AgentInput, Backend, Bell, BellConnector, Connector, Refresh, SessionError};
    use orus_auth::{Store, Token};
    use orus_org::doorbell::{Config, DoorbellError, Event as DoorbellEvent, OnEvent};
    use orus_org::{JsonObject, Me, MessagesPage, Room, Task, ToolSpec};
    use serde_json::json;
    use tokio::sync::Notify;

    use super::*;

    struct ClienteFalso {
        config: Config,
    }

    #[async_trait]
    impl Backend for ClienteFalso {
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

    struct ProveedorFalso {
        config: Config,
    }

    #[async_trait]
    impl ClientProvider for ProveedorFalso {
        async fn client(&self) -> Result<Arc<dyn Backend>, SessionError> {
            Ok(Arc::new(ClienteFalso {
                config: self.config.clone(),
            }))
        }
    }

    struct CampanaFalsa {
        fin: Arc<Notify>,
    }

    #[async_trait]
    impl Bell for CampanaFalsa {
        async fn done(&self) {
            self.fin.notified().await;
        }

        async fn close(&self) {
            self.fin.notify_one();
        }
    }

    struct ConectorFalso {
        eventos: Arc<StdMutex<Option<OnEvent>>>,
        fin: Arc<Notify>,
    }

    #[async_trait]
    impl BellConnector for ConectorFalso {
        async fn connect(
            &self,
            _: Config,
            on_event: OnEvent,
        ) -> Result<Box<dyn Bell>, DoorbellError> {
            *self.eventos.lock().expect("guardar callback") = Some(on_event);
            Ok(Box::new(CampanaFalsa {
                fin: Arc::clone(&self.fin),
            }))
        }
    }

    #[tokio::test]
    async fn el_timbre_publica_en_el_hub_y_informa_del_estado() {
        let config = Config {
            supabase_url: "https://ejemplo.supabase.co".to_owned(),
            topic: "abc".to_owned(),
            ..Config::default()
        };
        let hub = Arc::new(Hub::new());
        let (id, mut rx) = hub.subscribe();
        let eventos: Arc<StdMutex<Option<OnEvent>>> = Arc::new(StdMutex::new(None));
        let fin = Arc::new(Notify::new());
        let watch = Arc::new(Watch::new(Arc::new(ConectorFalso {
            eventos: Arc::clone(&eventos),
            fin: Arc::clone(&fin),
        })));
        let politica = Arc::new(AsyncMutex::new(WakePolicy::new(Arc::new(
            orus_app::RelojLocal,
        ))));
        let timbre = Arc::new(Timbre::new(watch, Arc::clone(&hub), politica));

        timbre.start(1, Arc::new(ProveedorFalso { config }));

        let mut callback = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some(cb) = eventos.lock().expect("leer callback").take() {
                    return cb;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("el borde conectó el timbre");

        assert!(
            BellStatus::doorbell(timbre.as_ref()),
            "el estado de ajustes ve el canal abierto"
        );

        callback(DoorbellEvent {
            kind: "mention".to_owned(),
            name: "mention".to_owned(),
            raw: json!({"id": "e1"}),
        });
        let evento = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("el evento llega al hub")
            .expect("el hub sigue abierto");
        assert_eq!(evento.kind, "mention");
        assert!(
            rx.try_recv().is_err(),
            "un ping publica un solo aviso en el hub"
        );

        hub.unsubscribe(id);
        timbre.stop();
        fin.notify_one();
    }

    /// Un `Store` sin sesión: la app queda `LoggedOut` sin tocar disco.
    struct StoreSinSesion;

    impl Store for StoreSinSesion {
        fn save(&self, _: Token) -> Result<(), orus_auth::Error> {
            Err(orus_auth::Error::Unavailable("sin disco".to_owned()))
        }
        fn load(&self) -> Result<Token, orus_auth::Error> {
            Err(orus_auth::Error::NoSession)
        }
        fn clear(&self) -> Result<(), orus_auth::Error> {
            Ok(())
        }
    }

    /// Costuras que solo fallan: la prueba no abre ninguna conexión.
    struct SinRed;

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

    /// El fallo que esto cubre: `bell_config` sobrevivía al logout y al cambio
    /// de org; la prueba del cerebro seguía viendo la base de la org anterior.
    /// `stop` es la costura que el `App` llama en ambos caminos.
    #[tokio::test]
    async fn stop_suelta_la_base_del_timbre_de_la_org_anterior() {
        let app = Arc::new(App::new(
            Box::new(StoreSinSesion),
            orus_config::Settings::default(),
            Arc::new(SinRed),
            Arc::new(SinRed),
        ));
        app.set_bell_config(Config {
            supabase_url: "https://vieja.supabase.co".to_owned(),
            ..Config::default()
        });
        let watch = Arc::new(Watch::new(Arc::new(ConectorFalso {
            eventos: Arc::new(StdMutex::new(None)),
            fin: Arc::new(Notify::new()),
        })));
        let politica = Arc::new(AsyncMutex::new(WakePolicy::new(Arc::new(
            orus_app::RelojLocal,
        ))));
        let timbre = Timbre::new(watch, Arc::new(Hub::new()), politica);
        timbre.referencias().set_app(Arc::clone(&app));

        let entrada = AgentInput {
            kind: "org".to_owned(),
            ..AgentInput::default()
        };
        // Con la base vieja cacheada, la prueba la da por buena y falla más
        // adelante (sin agente disponible).
        assert!(
            app.agent_test(entrada.clone()).await.is_err(),
            "la base vieja sigue disponible antes del stop"
        );
        timbre.stop();
        let probe = app
            .agent_test(entrada)
            .await
            .expect("sin base, la prueba responde sin necesitar agente");
        assert_eq!(
            probe.error,
            "aún no hay conexión con Oruscribe: espera a estar en línea"
        );
    }

    #[derive(Default)]
    struct AgenteQueDespierta;

    #[async_trait]
    impl Despertador for AgenteQueDespierta {
        async fn send(&self, _: &str) -> Result<(), WakeError> {
            Ok(())
        }
        fn busy(&self) -> bool {
            false
        }
    }

    /// #111: el evento del timbre respeta el tope guardado en ajustes, no el
    /// de fábrica de la política. Con per_minute=1 el segundo ping se ignora.
    #[tokio::test]
    async fn el_evento_del_timbre_usa_el_tope_configurado() {
        let datos = tempfile::tempdir().expect("carpeta temporal");
        temp_env::async_with_vars(
            [("ORUSCRIBE_DATA_DIR", Some(datos.path().as_os_str()))],
            async {
                orus_config::editar(|settings| {
                    settings.agent.wake_enabled = true;
                    settings.agent.wake_per_minute = 1;
                })
                .expect("guardar ajustes");

                let policy = Arc::new(AsyncMutex::new(WakePolicy::new(Arc::new(
                    orus_app::RelojLocal,
                ))));
                let despertar = Despertar {
                    policy: Arc::clone(&policy),
                    diario: Arc::new(StdMutex::new(Vec::new())),
                    me: String::new(),
                    agente: Arc::new(AgenteQueDespierta),
                };
                let evento = |id: &str| DoorbellEvent {
                    kind: "mention".to_owned(),
                    name: "mention".to_owned(),
                    raw: json!({"id": id}),
                };

                despertar.despertar(evento("e1")).await;
                despertar.despertar(evento("e2")).await;

                let entradas = policy.lock().await.wake_log();
                assert!(
                    entradas
                        .iter()
                        .any(|e| e.result == "ignorado: límite por minuto"),
                    "el segundo ping no respetó el tope por minuto de 1: {entradas:?}"
                );
            },
        )
        .await;
    }

    /// Un agente que se queda dentro del envío hasta que la prueba lo suelta:
    /// así se mira el estado justo mientras corre el turno, sin dormir a ciegas.
    struct AgenteEnVuelo {
        dentro: Arc<Notify>,
        soltar: Arc<Notify>,
        llamadas: Arc<AtomicUsize>,
    }

    impl AgenteEnVuelo {
        fn nuevo() -> Arc<Self> {
            Arc::new(Self {
                dentro: Arc::new(Notify::new()),
                soltar: Arc::new(Notify::new()),
                llamadas: Arc::new(AtomicUsize::new(0)),
            })
        }
    }

    #[async_trait]
    impl Despertador for AgenteEnVuelo {
        async fn send(&self, _: &str) -> Result<(), WakeError> {
            self.llamadas.fetch_add(1, Ordering::SeqCst);
            self.dentro.notify_one();
            self.soltar.notified().await;
            Ok(())
        }

        fn busy(&self) -> bool {
            false
        }
    }

    fn evento(id: &str, actor: &str) -> DoorbellEvent {
        DoorbellEvent {
            kind: "mention".to_owned(),
            name: "mention".to_owned(),
            raw: json!({"id": id, "actor_id": actor, "body": "hola"}),
        }
    }

    /// #151: el diario se leía con `try_lock`; mientras corría el turno (la red)
    /// devolvía `Vec::new()` y `/api/agent/journal` se veía vacío. El diario
    /// tiene que conservar lo último aunque el turno esté en vuelo.
    #[tokio::test]
    async fn el_diario_no_se_vacia_mientras_corre_un_despertar() {
        let datos = tempfile::tempdir().expect("carpeta temporal");
        temp_env::async_with_vars(
            [("ORUSCRIBE_DATA_DIR", Some(datos.path().as_os_str()))],
            async {
                orus_config::editar(|settings| {
                    settings.agent.wake_enabled = true;
                    settings.agent.wake_per_minute = 10;
                    settings.agent.wake_per_day = 10;
                })
                .expect("guardar ajustes");

                let policy = Arc::new(AsyncMutex::new(WakePolicy::new(Arc::new(
                    orus_app::RelojLocal,
                ))));
                let timbre = Timbre::new(
                    Arc::new(Watch::new(Arc::new(ConectorFalso {
                        eventos: Arc::new(StdMutex::new(None)),
                        fin: Arc::new(Notify::new()),
                    }))),
                    Arc::new(Hub::new()),
                    Arc::clone(&policy),
                );
                let registro = timbre.registro();
                let agente = AgenteEnVuelo::nuevo();
                let despertar = Arc::new(Despertar {
                    policy: Arc::clone(&policy),
                    diario: timbre.diario.clone(),
                    me: "yo".to_owned(),
                    agente: Arc::clone(&agente) as Arc<dyn Despertador>,
                });

                // Un ping propio no despierta, pero deja historia en el diario.
                despertar.despertar(evento("e0", "yo")).await;
                assert_eq!(
                    registro.entries().len(),
                    1,
                    "el diario arranca con el aviso anterior"
                );

                let turno = tokio::spawn({
                    let despertar = Arc::clone(&despertar);
                    async move { despertar.despertar(evento("e1", "u2")).await }
                });
                tokio::time::timeout(Duration::from_secs(2), agente.dentro.notified())
                    .await
                    .expect("el turno entró al envío");

                let durante = registro.entries();
                assert_eq!(
                    durante.len(),
                    1,
                    "mientras corre el despertar el diario conserva lo último, no se vacía: {durante:?}"
                );

                agente.soltar.notify_one();
                tokio::time::timeout(Duration::from_secs(2), turno)
                    .await
                    .expect("el despertar termina")
                    .expect("la tarea no entra en pánico");
            },
        )
        .await;
    }

    /// #151: el candado se retenía durante la red, así que un segundo ping
    /// esperaba el turno entero para poder encolarse. Con el turno en vuelo, el
    /// segundo ping tiene que volver enseguida y sin abrir otro turno.
    #[tokio::test]
    async fn dos_pings_no_se_serializan_detras_del_turno() {
        let datos = tempfile::tempdir().expect("carpeta temporal");
        temp_env::async_with_vars(
            [("ORUSCRIBE_DATA_DIR", Some(datos.path().as_os_str()))],
            async {
                orus_config::editar(|settings| {
                    settings.agent.wake_enabled = true;
                    settings.agent.wake_per_minute = 10;
                    settings.agent.wake_per_day = 10;
                })
                .expect("guardar ajustes");

                let policy = Arc::new(AsyncMutex::new(WakePolicy::new(Arc::new(
                    orus_app::RelojLocal,
                ))));
                let agente = AgenteEnVuelo::nuevo();
                let despertar = Arc::new(Despertar {
                    policy: Arc::clone(&policy),
                    diario: Arc::new(StdMutex::new(Vec::new())),
                    me: String::new(),
                    agente: Arc::clone(&agente) as Arc<dyn Despertador>,
                });

                let primero = tokio::spawn({
                    let despertar = Arc::clone(&despertar);
                    async move { despertar.despertar(evento("e1", "u2")).await }
                });
                tokio::time::timeout(Duration::from_secs(2), agente.dentro.notified())
                    .await
                    .expect("el turno entró al envío");

                tokio::time::timeout(
                    Duration::from_secs(2),
                    despertar.despertar(evento("e2", "u3")),
                )
                .await
                .expect(
                    "el segundo ping no puede esperar al turno: el candado está tomado por la red",
                );
                assert_eq!(
                    agente.llamadas.load(Ordering::SeqCst),
                    1,
                    "el segundo ping se encola, no abre otro turno"
                );

                agente.soltar.notify_one();
                tokio::time::timeout(Duration::from_secs(2), primero)
                    .await
                    .expect("el primer turno termina")
                    .expect("la tarea no entra en pánico");
            },
        )
        .await;
    }
}
