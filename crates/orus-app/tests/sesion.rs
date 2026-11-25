use std::fs;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use orus_agent::CancelToken;
use orus_app::{
    Agent, App, ApprovalQueue, Backend, ClientProvider, Connector, Conversations, Emisor, Refresh,
    SessionError, Status, TurnRunner, Turno, WatchSpawner, conversations_dir,
};
use orus_auth::{FileStore, Store, Token};
use orus_config::Settings;
use orus_core::Message;
use orus_harness::ErrorTurno;
use orus_harness::sessions::{Session, VERSION};
use orus_org::doorbell::Config as DoorbellConfig;
use orus_org::{JsonObject, Me, MessagesPage, Org, Room, Task, ToolSpec};
use serial_test::serial;
use tempfile::{TempDir, tempdir};
use tokio::sync::Notify;

#[derive(Default)]
struct FakeBackend {
    me: Me,
}

#[async_trait]
impl Backend for FakeBackend {
    async fn me(&self) -> Result<Me, SessionError> {
        Ok(self.me.clone())
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
        Err(SessionError::NoDoorbell)
    }
}

struct FakeConnector {
    calls: AtomicUsize,
    failures: usize,
}

impl FakeConnector {
    fn new(failures: usize) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            failures,
        })
    }
}

#[async_trait]
impl Connector for FakeConnector {
    async fn open(&self, _: &str, _: &str, _: &str) -> Result<Arc<dyn Backend>, SessionError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call < self.failures {
            return Err(SessionError::Connect("sin red".to_owned()));
        }
        Ok(Arc::new(FakeBackend::default()))
    }
}

/// Abre el cliente solo cuando la prueba lo suelta: deja la conexión
/// dentro de `connect` mientras se cierra la sesión.
struct ConnectorBloqueado {
    entrado: Arc<Notify>,
    suelta: Arc<Notify>,
}

#[async_trait]
impl Connector for ConnectorBloqueado {
    async fn open(&self, _: &str, _: &str, _: &str) -> Result<Arc<dyn Backend>, SessionError> {
        self.entrado.notify_one();
        self.suelta.notified().await;
        Ok(Arc::new(FakeBackend::default()))
    }
}

struct SlugConnector {
    slugs: Mutex<Vec<String>>,
}

impl SlugConnector {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            slugs: Mutex::new(Vec::new()),
        })
    }

    fn slugs(&self) -> Vec<String> {
        self.slugs.lock().unwrap().clone()
    }
}

#[async_trait]
impl Connector for SlugConnector {
    async fn open(
        &self,
        _: &str,
        _: &str,
        org_slug: &str,
    ) -> Result<Arc<dyn Backend>, SessionError> {
        self.slugs.lock().unwrap().push(org_slug.to_owned());
        Ok(Arc::new(FakeBackend {
            me: Me {
                organization: Org {
                    slug: "nuevo".to_owned(),
                    ..Org::default()
                },
                ..Me::default()
            },
        }))
    }
}

struct FakeRefresh {
    calls: AtomicUsize,
    reply: Result<Token, ()>,
}

impl FakeRefresh {
    fn new(reply: Result<Token, ()>) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            reply,
        })
    }
}

#[async_trait]
impl Refresh for FakeRefresh {
    async fn refresh(&self, _: &Token) -> Result<Token, SessionError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.reply
            .clone()
            .map_err(|()| SessionError::RefreshRejected)
    }
}

/// Un `Store` que siempre falla al guardar: para el error crudo de import.
struct StoreQueFalla;

impl Store for StoreQueFalla {
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

struct WatchSpy {
    starts: Mutex<Vec<u64>>,
    stops: AtomicUsize,
}

impl WatchSpy {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            starts: Mutex::new(Vec::new()),
            stops: AtomicUsize::new(0),
        })
    }

    fn starts(&self) -> Vec<u64> {
        self.starts.lock().unwrap().clone()
    }

    fn stops(&self) -> usize {
        self.stops.load(Ordering::SeqCst)
    }
}

impl WatchSpawner for WatchSpy {
    fn start(&self, generation: u64, _client: Arc<dyn ClientProvider>) {
        self.starts.lock().unwrap().push(generation);
    }

    fn stop(&self) {
        self.stops.fetch_add(1, Ordering::SeqCst);
    }
}

fn token(access: &str, hours: i64, refresh: &str) -> Token {
    Token {
        access_token: access.to_owned(),
        refresh_token: refresh.to_owned(),
        token_type: "Bearer".to_owned(),
        scope: String::new(),
        expiry: (Utc::now() + chrono::Duration::hours(hours)).fixed_offset(),
    }
}

fn file_store(dir: &TempDir) -> FileStore {
    FileStore::from_data_dir(dir.path())
}

fn app(dir: &TempDir, connector: &Arc<FakeConnector>, refresher: &Arc<FakeRefresh>) -> App {
    let connector: Arc<dyn Connector> = connector.clone();
    let refresher: Arc<dyn Refresh> = refresher.clone();
    let mut app = App::new(
        Box::new(file_store(dir)),
        Settings::default(),
        connector,
        refresher,
    );
    app.set_connect_backoff(Duration::from_millis(1));
    app.set_restore_backoff(Duration::from_millis(1));
    app
}
async fn esperar(app: &App, buscado: Status) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while app.status() != buscado {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("el estado final llegó a tiempo");
}
#[tokio::test]
async fn no_session_is_logged_out() {
    let dir = tempdir().unwrap();
    let connector = FakeConnector::new(0);
    let refresh = FakeRefresh::new(Err(()));
    let app = app(&dir, &connector, &refresh);

    assert!(matches!(app.client().await, Err(SessionError::LoggedOut)));
    assert_eq!(app.status(), Status::LoggedOut);
    assert_eq!(connector.calls.load(Ordering::SeqCst), 0);
    assert_eq!(refresh.calls.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn client_returns_cached_when_token_valid() {
    let dir = tempdir().unwrap();
    file_store(&dir).save(token("vigente", 1, "")).unwrap();
    let connector = FakeConnector::new(0);
    let refresh = FakeRefresh::new(Err(()));
    let app = app(&dir, &connector, &refresh);

    app.client().await.expect("primera conexión");
    app.client().await.expect("cliente cacheado");

    assert_eq!(connector.calls.load(Ordering::SeqCst), 1);
    assert_eq!(refresh.calls.load(Ordering::SeqCst), 0);
    assert_eq!(app.status(), Status::Ready);
}
#[tokio::test]
async fn expired_token_refreshes_and_reconnects() {
    let dir = tempdir().unwrap();
    file_store(&dir).save(token("viejo", 1, "")).unwrap();
    let connector = FakeConnector::new(0);
    let refresh = FakeRefresh::new(Ok(token("nuevo", 1, "r2")));
    let app = app(&dir, &connector, &refresh);

    app.client().await.expect("conexión inicial");
    file_store(&dir)
        .save(token("viejo", -1, "r1"))
        .expect("token vencido");

    app.client().await.expect("reconexión");
    assert_eq!(connector.calls.load(Ordering::SeqCst), 2);
    assert_eq!(refresh.calls.load(Ordering::SeqCst), 1);
    assert_eq!(file_store(&dir).load().unwrap().access_token, "nuevo");
    assert_eq!(app.status(), Status::Ready);
}
#[tokio::test]
async fn refresh_rejected_deja_logged_out_y_borra_token() {
    let dir = tempdir().unwrap();
    file_store(&dir).save(token("viejo", 1, "")).unwrap();
    let connector = FakeConnector::new(0);
    let refresh = FakeRefresh::new(Err(()));
    let app = app(&dir, &connector, &refresh);

    app.client().await.expect("conexión inicial");
    file_store(&dir)
        .save(token("viejo", -1, "r1"))
        .expect("token vencido");

    assert!(matches!(
        app.client().await,
        Err(SessionError::RefreshRejected)
    ));
    // #77: la ventana no puede seguir diciendo «conectada» mientras toda
    // acción falla. Mismo trato que el boot: el servidor ya no acepta esta
    // sesión y el token sale del disco para que no se restaure.
    assert_eq!(app.status(), Status::LoggedOut);
    assert_eq!(connector.calls.load(Ordering::SeqCst), 1);
    assert_eq!(refresh.calls.load(Ordering::SeqCst), 1);
    assert!(matches!(
        file_store(&dir).load(),
        Err(orus_auth::Error::NoSession)
    ));
    assert!(matches!(app.client().await, Err(SessionError::LoggedOut)));
}

/// Un fallo al guardar el token renovado tampoco puede dejar `Ready`.
struct GuardadoRoto {
    actual: Mutex<Token>,
    save_falla: AtomicBool,
}

#[derive(Clone)]
struct StoreConGuardadoRoto(Arc<GuardadoRoto>);

impl Store for StoreConGuardadoRoto {
    fn save(&self, token: Token) -> Result<(), orus_auth::Error> {
        if self.0.save_falla.load(Ordering::SeqCst) {
            return Err(orus_auth::Error::Unavailable("disco lleno".to_owned()));
        }
        *self.0.actual.lock().unwrap() = token;
        Ok(())
    }

    fn load(&self) -> Result<Token, orus_auth::Error> {
        Ok(self.0.actual.lock().unwrap().clone())
    }

    fn clear(&self) -> Result<(), orus_auth::Error> {
        Ok(())
    }
}

#[tokio::test]
async fn refresh_con_guardado_roto_mantiene_la_sesion_en_memoria() {
    let store = StoreConGuardadoRoto(Arc::new(GuardadoRoto {
        actual: Mutex::new(token("vigente", 1, "r1")),
        save_falla: AtomicBool::new(false),
    }));
    let connector = FakeConnector::new(0);
    let refresh = FakeRefresh::new(Ok(token("nuevo", 1, "r2")));
    let app = App::new(
        Box::new(store.clone()),
        Settings::default(),
        connector.clone(),
        refresh.clone(),
    );
    app.client().await.expect("conexión inicial");
    assert_eq!(app.status(), Status::Ready);

    *store.0.actual.lock().unwrap() = token("viejo", -1, "r1");
    store.0.save_falla.store(true, Ordering::SeqCst);

    app.client().await.expect("la renovación sigue en memoria");
    assert_eq!(app.status(), Status::Ready);
}
#[tokio::test]
async fn connect_retries_transient() {
    let dir = tempdir().unwrap();
    file_store(&dir).save(token("vigente", 1, "")).unwrap();
    let connector = FakeConnector::new(2);
    let refresh = FakeRefresh::new(Err(()));
    let app = app(&dir, &connector, &refresh);

    app.client().await.expect("con reintentos");

    assert_eq!(connector.calls.load(Ordering::SeqCst), 3);
    assert_eq!(app.status(), Status::Ready);
    assert!(app.me().is_some());
}
#[tokio::test]
async fn logout_clears_token() {
    let dir = tempdir().unwrap();
    file_store(&dir).save(token("vigente", 1, "")).unwrap();
    let connector = FakeConnector::new(0);
    let refresh = FakeRefresh::new(Err(()));
    let app = app(&dir, &connector, &refresh);

    app.client().await.expect("conexión");
    app.logout().expect("logout");

    assert_eq!(app.status(), Status::LoggedOut);
    assert!(matches!(
        file_store(&dir).load(),
        Err(orus_auth::Error::NoSession)
    ));
    assert!(matches!(app.client().await, Err(SessionError::LoggedOut)));
}
#[tokio::test]
#[serial]
async fn logout_clears_token_and_conversations() {
    let dir = tempdir().unwrap();
    let datos = tempdir().unwrap();
    let datos_path = datos.path().to_str().expect("ruta utf-8").to_owned();
    let connector = FakeConnector::new(0);
    let refresh = FakeRefresh::new(Err(()));

    temp_env::async_with_vars([("ORUSCRIBE_DATA_DIR", Some(datos_path.as_str()))], async {
        file_store(&dir).save(token("vigente", 1, "")).unwrap();
        let spy = WatchSpy::new();
        let app = app(&dir, &connector, &refresh)
            .with_watch_spawner(spy.clone())
            .with_conversations(Arc::new(Conversations::new()));
        let carpeta = orus_app::conversations_dir();
        fs::create_dir_all(&carpeta).unwrap();
        let archivo = carpeta.join("c-1.json");
        fs::write(&archivo, b"{}").unwrap();

        app.client().await.expect("conexión");
        app.logout().expect("logout");

        assert_eq!(app.status(), Status::LoggedOut);
        assert_eq!(spy.stops(), 1, "logout no detuvo el timbre");
        assert!(!archivo.exists(), "logout no borró las conversaciones");
        assert!(matches!(
            file_store(&dir).load(),
            Err(orus_auth::Error::NoSession)
        ));
    })
    .await;
}

#[tokio::test]
async fn kick_watch_deduplicates_same_session() {
    let dir = tempdir().unwrap();
    let connector = FakeConnector::new(0);
    let refresh = FakeRefresh::new(Err(()));
    let spy = WatchSpy::new();
    let app = app(&dir, &connector, &refresh).with_watch_spawner(spy.clone());

    app.import_token("token-uno").await.expect("primer timbre");
    app.import_token("token-uno").await.expect("mismo timbre");

    assert_eq!(spy.starts(), vec![1]);
    assert_eq!(spy.stops(), 0);
}

#[tokio::test]
async fn kick_watch_replaces_previous() {
    let dir = tempdir().unwrap();
    let connector = FakeConnector::new(0);
    let refresh = FakeRefresh::new(Err(()));
    let spy = WatchSpy::new();
    let app = app(&dir, &connector, &refresh).with_watch_spawner(spy.clone());

    app.import_token("token-uno").await.expect("primer timbre");
    app.import_token("token-dos").await.expect("timbre nuevo");

    assert_eq!(spy.starts(), vec![1, 2]);
    assert_eq!(spy.stops(), 1);
}

#[tokio::test]
#[serial]
async fn connect_saves_server_org_slug() {
    let dir = tempdir().unwrap();
    let datos = tempdir().unwrap();
    let datos_path = datos.path().to_str().expect("ruta utf-8").to_owned();
    let connector = SlugConnector::new();
    let refresh = FakeRefresh::new(Err(()));
    file_store(&dir).save(token("vigente", 1, "")).unwrap();

    temp_env::async_with_vars([("ORUSCRIBE_DATA_DIR", Some(datos_path.as_str()))], async {
        let app = App::new(
            Box::new(file_store(&dir)),
            Settings {
                org_slug: "viejo".to_owned(),
                ..Settings::default()
            },
            connector.clone(),
            refresh.clone(),
        );
        app.client().await.expect("conexión con el slug viejo");
        assert_eq!(orus_config::load().org_slug, "nuevo");

        let siguiente = App::new(
            Box::new(file_store(&dir)),
            orus_config::load(),
            connector.clone(),
            refresh.clone(),
        );
        siguiente.client().await.expect("siguiente apertura");
    })
    .await;

    // #125: la primera conexión se reabre con el slug que respondió get_me;
    // la apertura siguiente ya nace con ese slug.
    assert_eq!(
        connector.slugs(),
        vec!["viejo".to_owned(), "nuevo".to_owned(), "nuevo".to_owned()]
    );
}

#[tokio::test]
async fn boot_without_token_is_logged_out() {
    let dir = tempdir().unwrap();
    let connector = FakeConnector::new(0);
    let refresh = FakeRefresh::new(Err(()));
    let app = app(&dir, &connector, &refresh);

    app.boot();
    tokio::time::sleep(Duration::from_millis(20)).await;

    assert_eq!(app.status(), Status::LoggedOut);
    assert_eq!(connector.calls.load(Ordering::SeqCst), 0);
    assert_eq!(refresh.calls.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn boot_restores_valid_session() {
    let dir = tempdir().unwrap();
    file_store(&dir).save(token("vigente", 1, "")).unwrap();
    let connector = FakeConnector::new(0);
    let refresh = FakeRefresh::new(Err(()));
    let app = app(&dir, &connector, &refresh);

    app.boot();
    esperar(&app, Status::Ready).await;

    assert_eq!(connector.calls.load(Ordering::SeqCst), 1);
    assert!(app.me().is_some());
}
#[tokio::test]
async fn boot_retries_transient() {
    let dir = tempdir().unwrap();
    file_store(&dir).save(token("vigente", 1, "")).unwrap();
    let connector = FakeConnector::new(2);
    let refresh = FakeRefresh::new(Err(()));
    let app = app(&dir, &connector, &refresh);

    app.boot();
    esperar(&app, Status::Ready).await;

    assert_eq!(connector.calls.load(Ordering::SeqCst), 3);
    assert!(app.me().is_some());
}
#[tokio::test]
async fn boot_retries_after_all_connects_fail() {
    let dir = tempdir().unwrap();
    file_store(&dir).save(token("vigente", 1, "")).unwrap();
    let connector = FakeConnector::new(5);
    let refresh = FakeRefresh::new(Err(()));
    let app = app(&dir, &connector, &refresh);

    app.boot();
    esperar(&app, Status::Ready).await;

    assert_eq!(connector.calls.load(Ordering::SeqCst), 6);
    assert!(app.me().is_some());
}
#[tokio::test]
async fn boot_stops_on_refresh_rejected() {
    let dir = tempdir().unwrap();
    file_store(&dir)
        .save(token("viejo", -1, "r1"))
        .expect("token vencido");
    let connector = FakeConnector::new(0);
    let refresh = FakeRefresh::new(Err(()));
    let app = app(&dir, &connector, &refresh);

    app.boot();
    esperar(&app, Status::LoggedOut).await;
    // Un ciclo de reintento ya habría vuelto a pedir el refresh.
    tokio::time::sleep(Duration::from_millis(20)).await;

    assert_eq!(app.status(), Status::LoggedOut);
    assert_eq!(refresh.calls.load(Ordering::SeqCst), 1);
    assert_eq!(connector.calls.load(Ordering::SeqCst), 0);
    assert!(matches!(
        file_store(&dir).load(),
        Err(orus_auth::Error::NoSession)
    ));
}
#[tokio::test]
async fn import_token_rejects_empty() {
    let dir = tempdir().unwrap();
    let connector = FakeConnector::new(0);
    let refresh = FakeRefresh::new(Err(()));
    let app = app(&dir, &connector, &refresh);

    for vacio in ["", "   ", "\t\n "] {
        let error = app.import_token(vacio).await.expect_err("token vacío");
        assert_eq!(error.to_string(), "el token está vacío");
    }

    assert_eq!(connector.calls.load(Ordering::SeqCst), 0);
    assert_eq!(app.status(), Status::LoggedOut);
}
#[tokio::test]
async fn import_token_returns_raw_save_error() {
    let connector: Arc<dyn Connector> = FakeConnector::new(0);
    let refresher: Arc<dyn Refresh> = FakeRefresh::new(Err(()));
    let app = App::new(
        Box::new(StoreQueFalla),
        Settings::default(),
        connector,
        refresher,
    );

    let error = app
        .import_token("token")
        .await
        .expect_err("el guardado falla");
    let mensaje = error.to_string();
    let esperado = orus_auth::Error::Unavailable("sin disco".to_owned()).to_string();
    match &error {
        SessionError::GuardarToken(texto) => {
            assert_eq!(texto, &esperado, "el texto es el crudo del store");
        }
        otro => panic!("se esperaba GuardarToken: {otro:?}"),
    }
    assert!(
        !mensaje.contains("sesión renovada"),
        "no debe envolver el error: {mensaje}"
    );
}
#[tokio::test]
async fn import_token_saves_and_ready() {
    let dir = tempdir().unwrap();
    let connector = FakeConnector::new(0);
    let refresh = FakeRefresh::new(Err(()));
    let app = app(&dir, &connector, &refresh);

    app.import_token(" token-importado ")
        .await
        .expect("importa el token");

    assert_eq!(app.status(), Status::Ready);
    assert!(app.me().is_some());
    let guardado = file_store(&dir).load().expect("token guardado");
    assert_eq!(guardado.access_token, "token-importado");
    assert_eq!(guardado.token_type, "Bearer");
    app.client().await.expect("cliente listo");
    assert_eq!(connector.calls.load(Ordering::SeqCst), 1);
}
#[tokio::test]
#[serial]
async fn import_token_switches_org() {
    let dir = tempdir().unwrap();
    let datos = tempdir().unwrap();
    let datos_path = datos.path().to_str().expect("ruta utf-8").to_owned();
    let connector = SlugConnector::new();
    let refresher: Arc<dyn Refresh> = FakeRefresh::new(Err(()));
    let app = App::new(
        Box::new(file_store(&dir)),
        Settings {
            org_slug: "viejo".to_owned(),
            ..Settings::default()
        },
        connector.clone(),
        refresher,
    );

    temp_env::async_with_vars([("ORUSCRIBE_DATA_DIR", Some(datos_path.as_str()))], async {
        app.import_token(" token-nuevo ")
            .await
            .expect("importa el token");
        assert_eq!(orus_config::load().org_slug, "nuevo");
    })
    .await;

    assert_eq!(
        connector.slugs(),
        vec!["viejo".to_owned(), "nuevo".to_owned()]
    );
    assert_eq!(app.status(), Status::Ready);
    assert!(app.me().is_some());
    assert_eq!(
        file_store(&dir)
            .load()
            .expect("token guardado")
            .access_token,
        "token-nuevo"
    );
}

#[tokio::test]
async fn logout_durante_conexion_no_repone_cliente_revocado() {
    let dir = tempdir().unwrap();
    file_store(&dir).save(token("viejo", 1, "")).unwrap();
    let entrado = Arc::new(Notify::new());
    let suelta = Arc::new(Notify::new());
    let connector: Arc<dyn Connector> = Arc::new(ConnectorBloqueado {
        entrado: entrado.clone(),
        suelta: suelta.clone(),
    });
    let refresher: Arc<dyn Refresh> = FakeRefresh::new(Err(()));
    let spy = WatchSpy::new();
    let app = App::new(
        Box::new(file_store(&dir)),
        Settings::default(),
        connector,
        refresher,
    )
    .with_watch_spawner(spy.clone());

    let mut conexion = Box::pin(app.client());
    tokio::select! {
        _ = entrado.notified() => {}
        _ = &mut conexion => panic!("el cliente terminó antes de abrir la conexión"),
    }
    app.logout().expect("logout durante la conexión");
    suelta.notify_one();

    // Esperar el futuro completo garantiza que la conexión en vuelo terminó.
    assert!(matches!(conexion.await, Err(SessionError::LoggedOut)));
    assert_eq!(app.status(), Status::LoggedOut);
    assert!(
        spy.starts().is_empty(),
        "la conexión en vuelo repuso el cliente tras el logout"
    );
    assert!(matches!(
        file_store(&dir).load(),
        Err(orus_auth::Error::NoSession)
    ));
}

#[tokio::test]
async fn logout_durante_import_no_persiste_la_sesion() {
    let dir = tempdir().unwrap();
    let entrado = Arc::new(Notify::new());
    let suelta = Arc::new(Notify::new());
    let connector: Arc<dyn Connector> = Arc::new(ConnectorBloqueado {
        entrado: entrado.clone(),
        suelta: suelta.clone(),
    });
    let refresher: Arc<dyn Refresh> = FakeRefresh::new(Err(()));
    let app = App::new(
        Box::new(file_store(&dir)),
        Settings::default(),
        connector,
        refresher,
    );

    let mut importacion = Box::pin(app.import_token("token-importado"));
    tokio::select! {
        _ = entrado.notified() => {}
        _ = &mut importacion => panic!("la importación terminó antes de abrir el cliente"),
    }
    app.logout().expect("logout durante la importación");
    suelta.notify_one();

    assert!(matches!(importacion.await, Err(SessionError::LoggedOut)));
    assert_eq!(app.status(), Status::LoggedOut);
    assert!(matches!(
        file_store(&dir).load(),
        Err(orus_auth::Error::NoSession)
    ));
}

/// Runner que no hace nada: las pruebas de #78 solo necesitan historial.
struct RunnerMudo;

#[async_trait]
impl TurnRunner for RunnerMudo {
    async fn run(
        &self,
        _: Turno,
        _: &mut Session,
        _: CancelToken,
        _: Emisor,
    ) -> Result<(), ErrorTurno> {
        Ok(())
    }
}

/// Agente con una conversación abierta de la organización anterior.
fn agente_con_historial() -> Arc<Agent> {
    let conversaciones = Conversations::new();
    let mut sesion = Session {
        version: VERSION,
        id: "101".to_owned(),
        title: String::new(),
        cwd: String::new(),
        model: "modelo".to_owned(),
        created: Utc::now(),
        updated: Utc::now(),
        messages: vec![Message {
            role: "user".to_owned(),
            content: "secreto de la org vieja".to_owned(),
            ..Message::default()
        }],
        tokens_in: 0,
        tokens_out: 0,
        cost_usd: 0.0,
    };
    conversaciones
        .save(&mut sesion)
        .expect("guardar la conversación");
    let agent = Arc::new(Agent::new(
        conversaciones,
        Arc::new(ApprovalQueue::new()),
        Arc::new(RunnerMudo),
    ));
    agent
        .open_conversation("101")
        .expect("abrir la conversación");
    assert!(!agent.history().is_empty(), "la prueba necesita historial");
    agent
}

/// #78: importar un token de otra organización no puede llevarse la
/// conversación anterior: el historial y los resultados de herramienta de la
/// org vieja viajarían al cerebro de la nueva en el próximo turno.
#[tokio::test]
#[serial]
async fn import_token_cambio_org_limpia_conversacion() {
    let dir = tempdir().unwrap();
    let datos = tempdir().unwrap();
    let datos_path = datos.path().to_str().expect("ruta utf-8").to_owned();
    let connector = SlugConnector::new();
    let refresher: Arc<dyn Refresh> = FakeRefresh::new(Err(()));

    temp_env::async_with_vars([("ORUSCRIBE_DATA_DIR", Some(datos_path.as_str()))], async {
        let agent = agente_con_historial();
        let generacion = agent.generation();
        let app = App::new(
            Box::new(file_store(&dir)),
            Settings {
                org_slug: "viejo".to_owned(),
                ..Settings::default()
            },
            connector.clone(),
            refresher,
        )
        .with_agent(agent.clone())
        .with_conversations(Arc::new(Conversations::new()));

        app.import_token("token-nuevo")
            .await
            .expect("importa el token");

        assert!(
            agent.history().is_empty(),
            "el historial de la org vieja viajó a la nueva"
        );
        assert_eq!(agent.conversation_id(), None);
        assert!(agent.generation() > generacion, "el agente no se reinició");
        assert!(
            !conversations_dir().join("c-101.json").exists(),
            "la conversación de la org vieja sigue en disco"
        );
    })
    .await;
}

/// #78 también en la restauración: `connect` cambia el slug cuando el
/// servidor manda otro y debe limpiar igual que el import.
#[tokio::test]
#[serial]
async fn connect_cambio_org_limpia_conversacion() {
    let dir = tempdir().unwrap();
    let datos = tempdir().unwrap();
    let datos_path = datos.path().to_str().expect("ruta utf-8").to_owned();
    let connector = SlugConnector::new();
    let refresher: Arc<dyn Refresh> = FakeRefresh::new(Err(()));
    file_store(&dir).save(token("vigente", 1, "")).unwrap();

    temp_env::async_with_vars([("ORUSCRIBE_DATA_DIR", Some(datos_path.as_str()))], async {
        let agent = agente_con_historial();
        let app = App::new(
            Box::new(file_store(&dir)),
            Settings {
                org_slug: "viejo".to_owned(),
                ..Settings::default()
            },
            connector.clone(),
            refresher,
        )
        .with_agent(agent.clone())
        .with_conversations(Arc::new(Conversations::new()));

        app.client().await.expect("conexión con el slug viejo");

        assert!(
            agent.history().is_empty(),
            "el historial de la org vieja viajó a la nueva"
        );
        assert_eq!(agent.conversation_id(), None);
        assert!(
            !conversations_dir().join("c-101.json").exists(),
            "la conversación de la org vieja sigue en disco"
        );
    })
    .await;
}

/// Cambiar a la misma organización no borra nada: la limpieza solo aplica
/// cuando el slug cambia de verdad.
#[tokio::test]
#[serial]
async fn import_token_misma_org_conserva_conversacion() {
    let dir = tempdir().unwrap();
    let datos = tempdir().unwrap();
    let datos_path = datos.path().to_str().expect("ruta utf-8").to_owned();
    let connector = FakeConnector::new(0);
    let refresher: Arc<dyn Refresh> = FakeRefresh::new(Err(()));

    temp_env::async_with_vars([("ORUSCRIBE_DATA_DIR", Some(datos_path.as_str()))], async {
        let agent = agente_con_historial();
        let app = App::new(
            Box::new(file_store(&dir)),
            Settings {
                org_slug: "mismo".to_owned(),
                ..Settings::default()
            },
            connector.clone(),
            refresher,
        )
        .with_agent(agent.clone())
        .with_conversations(Arc::new(Conversations::new()));

        app.import_token("token").await.expect("importa el token");

        assert!(
            !agent.history().is_empty(),
            "la misma org no debe borrar la conversación"
        );
        assert!(conversations_dir().join("c-101.json").exists());
    })
    .await;
}
