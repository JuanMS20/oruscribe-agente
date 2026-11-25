//! Contrato del login OAuth: callback loopback, flujo y conexión.
//!
//! Todo corre contra servidores `TcpListener` locales; no hay navegador ni red
//! real. El `settings.json` real nunca se toca: `start_login` usa la costura
//! `set_save_client`.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use orus_app::{
    App, Backend, CALLBACK_PATH, Connector, LOGIN_PORT, Refresh, SessionError, Status,
    login_listener_on,
};
use orus_auth::{Error as AuthError, FileStore, Store, Token};
use orus_config::Settings;
use orus_org::doorbell::Config as DoorbellConfig;
use orus_org::{JsonObject, Me, MessagesPage, Room, Task, ToolSpec};
use tempfile::{TempDir, tempdir};

struct FakeBackend;

#[async_trait]
impl Backend for FakeBackend {
    async fn me(&self) -> Result<Me, SessionError> {
        Ok(Me::default())
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
}

impl FakeConnector {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
        })
    }
}

#[async_trait]
impl Connector for FakeConnector {
    async fn open(&self, _: &str, _: &str, _: &str) -> Result<Arc<dyn Backend>, SessionError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Arc::new(FakeBackend))
    }
}

struct FakeRefresh;

#[async_trait]
impl Refresh for FakeRefresh {
    async fn refresh(&self, _: &Token) -> Result<Token, SessionError> {
        Err(SessionError::RefreshRejected)
    }
}

fn app_con(dir: &TempDir, connector: &Arc<FakeConnector>, settings: Settings) -> App {
    let refresher: Arc<dyn Refresh> = Arc::new(FakeRefresh);
    let mut app = App::new(
        Box::new(FileStore::from_data_dir(dir.path())),
        settings,
        connector.clone(),
        refresher,
    );
    app.set_connect_backoff(Duration::from_millis(1));
    app
}

fn listener_de_prueba() -> (TcpListener, String) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("puerto libre");
    let puerto = listener.local_addr().expect("dirección local").port();
    let redirect = format!("http://127.0.0.1:{puerto}{CALLBACK_PATH}");
    (listener, redirect)
}

fn listener_fijo(
    listener: TcpListener,
    redirect: String,
) -> impl Fn() -> std::io::Result<(TcpListener, String)> + Send + Sync {
    let celda = Arc::new(Mutex::new(Some((listener, redirect))));
    move || {
        celda
            .lock()
            .expect("el candado del listener")
            .take()
            .ok_or_else(|| std::io::Error::other("el listener de prueba ya se usó"))
    }
}

fn aceptar(listener: &TcpListener) -> (TcpStream, SocketAddr) {
    listener.set_nonblocking(true).expect("modo no bloqueante");
    let inicio = Instant::now();
    loop {
        match listener.accept() {
            Ok(par) => {
                par.0.set_nonblocking(false).expect("modo bloqueante");
                return par;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    inicio.elapsed() < Duration::from_secs(10),
                    "el servidor de prueba no recibió la petición"
                );
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("accept: {error}"),
        }
    }
}

fn leer_peticion(flujo: &mut TcpStream) -> String {
    flujo
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("tiempo de lectura");
    let mut lector = BufReader::new(flujo.try_clone().expect("clona el socket"));
    let mut cabeza = String::new();
    let mut largo = 0;
    loop {
        let mut linea = String::new();
        if lector.read_line(&mut linea).expect("lee la cabeza") == 0 {
            break;
        }
        if linea == "\r\n" || linea == "\n" {
            break;
        }
        if let Some(valor) = linea.to_ascii_lowercase().strip_prefix("content-length:") {
            largo = valor.trim().parse().expect("largo del cuerpo");
        }
        cabeza.push_str(&linea);
    }
    let mut cuerpo = vec![0; largo];
    lector.read_exact(&mut cuerpo).expect("lee el cuerpo");
    format!("{cabeza}\r\n{}", String::from_utf8_lossy(&cuerpo))
}

fn escribir_respuesta(flujo: &mut TcpStream, estado: u16, tipo: &str, cuerpo: &str) {
    let razon = match estado {
        200 => "OK",
        201 => "Created",
        _ => "Error",
    };
    let respuesta = format!(
        "HTTP/1.1 {estado} {razon}\r\nContent-Type: {tipo}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{cuerpo}",
        cuerpo.len()
    );
    flujo
        .write_all(respuesta.as_bytes())
        .expect("escribe la respuesta");
}

fn responder_oauth(base: &str, peticion: &str) -> (u16, &'static str, String) {
    let primera = peticion.lines().next().unwrap_or_default();
    let objetivo = primera.split_whitespace().nth(1).unwrap_or_default();
    let ruta = objetivo.split('?').next().unwrap_or_default();
    match ruta {
        "/.well-known/oauth-authorization-server" => (
            200,
            "application/json",
            format!(
                r#"{{"authorization_endpoint":"{base}/authorize","token_endpoint":"{base}/token","registration_endpoint":"{base}/register"}}"#
            ),
        ),
        "/register" => (
            201,
            "application/json",
            r#"{"client_id":"nuevo"}"#.to_owned(),
        ),
        "/token" => (
            200,
            "application/json",
            r#"{"access_token":"acceso","refresh_token":"refresco","token_type":"bearer","scope":"openid","expires_in":3600}"#
                .to_owned(),
        ),
        _ => (404, "text/plain; charset=utf-8", "404".to_owned()),
    }
}

struct ServidorOAuth {
    base: String,
    peticiones: Arc<Mutex<Vec<String>>>,
    hilo: Option<thread::JoinHandle<()>>,
}

impl ServidorOAuth {
    fn nuevo(esperadas: usize) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("puerto libre");
        let base = format!("http://{}", listener.local_addr().expect("dirección local"));
        let base_hilo = base.clone();
        let peticiones = Arc::new(Mutex::new(Vec::new()));
        let registro = Arc::clone(&peticiones);
        let hilo = thread::spawn(move || {
            for _ in 0..esperadas {
                let (mut flujo, _) = aceptar(&listener);
                let peticion = leer_peticion(&mut flujo);
                let (estado, tipo, cuerpo) = responder_oauth(&base_hilo, &peticion);
                escribir_respuesta(&mut flujo, estado, tipo, &cuerpo);
                registro.lock().expect("el registro").push(peticion);
            }
        });
        Self {
            base,
            peticiones,
            hilo: Some(hilo),
        }
    }

    fn base(&self) -> &str {
        &self.base
    }

    fn esperar(&mut self) -> Vec<String> {
        self.hilo
            .take()
            .expect("el hilo del servidor")
            .join()
            .expect("el servidor termina");
        self.peticiones.lock().expect("las peticiones").clone()
    }
}

/// Hace un GET y devuelve (estado, cabeceras, cuerpo).
fn pedir(url: &str) -> (u16, String, String) {
    let resto = url.strip_prefix("http://").expect("URL loopback");
    let (host, ruta) = resto.split_once('/').expect("host y ruta");
    let mut flujo = TcpStream::connect(host).expect("conecta al callback");
    flujo
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("tiempo de lectura");
    write!(
        flujo,
        "GET /{ruta} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
    )
    .expect("escribe la petición");
    flujo.flush().expect("manda la petición");
    let mut respuesta = String::new();
    flujo
        .read_to_string(&mut respuesta)
        .expect("lee la respuesta");
    let (cabeza, cuerpo) = respuesta.split_once("\r\n\r\n").expect("cabeza y cuerpo");
    let estado = cabeza
        .lines()
        .next()
        .and_then(|linea| linea.split_whitespace().nth(1))
        .and_then(|valor| valor.parse().ok())
        .expect("estado HTTP");
    (estado, cabeza.to_owned(), cuerpo.to_owned())
}

#[test]
fn login_falls_back_to_ephemeral_port() {
    assert_eq!(LOGIN_PORT, 47823);
    let ocupado = TcpListener::bind(("127.0.0.1", 0)).expect("puerto ocupado");
    let puerto_ocupado = ocupado.local_addr().expect("dirección").port();

    let (listener, redirect) = login_listener_on(puerto_ocupado).expect("respaldo efímero");
    let puerto = listener.local_addr().expect("dirección").port();

    assert_ne!(puerto, puerto_ocupado);
    assert!(redirect.ends_with(CALLBACK_PATH), "{redirect}");
    assert!(redirect.contains(&format!(":{puerto}")), "{redirect}");
}

#[test]
fn start_login_uses_existing_client() {
    let dir = tempdir().expect("carpeta temporal");
    let connector = FakeConnector::new();
    let mut servidor = ServidorOAuth::nuevo(1);
    let (listener, redirect) = listener_de_prueba();
    let settings = Settings {
        client_id: "cliente-existente".to_owned(),
        redirect_uri: redirect.clone(),
        ..Settings::default()
    };
    let mut app = app_con(&dir, &connector, settings);
    app.set_login_issuer(servidor.base().to_owned());
    app.set_login_listener(listener_fijo(listener, redirect));

    let guardado = Arc::new(Mutex::new(None));
    let guardado_hilo = Arc::clone(&guardado);
    app.set_save_client(move |id, uri| {
        *guardado_hilo.lock().expect("el guardado") = Some((id, uri));
        Ok(Settings::default())
    });

    let url = app.start_login().expect("inicia el login");

    assert!(url.contains("client_id=cliente-existente"), "{url}");
    assert!(
        guardado.lock().expect("el guardado").is_none(),
        "no debía registrar un cliente nuevo"
    );
    let peticiones = servidor.esperar();
    assert_eq!(peticiones.len(), 1);
    assert!(
        peticiones[0].starts_with("GET /.well-known/oauth-authorization-server"),
        "{}",
        peticiones[0]
    );
}

/// El fallo original: `start_login` devolvía la URL y nadie abría el
/// navegador. La costura del borde tiene que recibir exactamente esa URL.
#[test]
fn start_login_opens_the_authorization_url() {
    let dir = tempdir().expect("carpeta temporal");
    let connector = FakeConnector::new();
    let mut servidor = ServidorOAuth::nuevo(1);
    let (listener, redirect) = listener_de_prueba();
    let settings = Settings {
        client_id: "cliente-existente".to_owned(),
        redirect_uri: redirect.clone(),
        ..Settings::default()
    };
    let mut app = app_con(&dir, &connector, settings);
    app.set_login_issuer(servidor.base().to_owned());
    app.set_login_listener(listener_fijo(listener, redirect));

    let abierta = Arc::new(Mutex::new(None));
    let abierta_hilo = Arc::clone(&abierta);
    app.set_open_url(move |url| {
        *abierta_hilo.lock().expect("la URL abierta") = Some(url.to_owned());
    });

    let url = app.start_login().expect("inicia el login");
    let abierta = abierta
        .lock()
        .expect("la URL abierta")
        .clone()
        .expect("el borde abrió el navegador");

    assert_eq!(abierta, url);
    assert!(
        url.starts_with(&format!("{}/authorize", servidor.base())),
        "{url}"
    );
    let peticiones = servidor.esperar();
    assert_eq!(peticiones.len(), 1);
}

#[test]
fn start_login_registers_when_redirect_changes() {
    let dir = tempdir().expect("carpeta temporal");
    let connector = FakeConnector::new();
    let mut servidor = ServidorOAuth::nuevo(2);
    let (listener, redirect) = listener_de_prueba();
    let settings = Settings {
        client_id: "viejo".to_owned(),
        redirect_uri: "http://127.0.0.1:1/oauth_callback".to_owned(),
        ..Settings::default()
    };
    let mut app = app_con(&dir, &connector, settings);
    app.set_login_issuer(servidor.base().to_owned());
    app.set_login_listener(listener_fijo(listener, redirect.clone()));

    let guardado = Arc::new(Mutex::new(None));
    let guardado_hilo = Arc::clone(&guardado);
    app.set_save_client(move |id, uri| {
        *guardado_hilo.lock().expect("el guardado") = Some((id.clone(), uri.clone()));
        Ok(Settings {
            client_id: id,
            redirect_uri: uri,
            ..Settings::default()
        })
    });

    let url = app.start_login().expect("inicia el login");

    assert!(url.contains("client_id=nuevo"), "{url}");
    let (id, uri) = guardado
        .lock()
        .expect("el guardado")
        .clone()
        .expect("guardó el cliente");
    assert_eq!(id, "nuevo");
    assert_eq!(uri, redirect);
    let peticiones = servidor.esperar();
    assert_eq!(peticiones.len(), 2);
    assert!(
        peticiones[1].starts_with("POST /register"),
        "{}",
        peticiones[1]
    );
    assert!(peticiones[1].contains(&redirect), "{}", peticiones[1]);
}

#[test]
fn finish_login_saves_and_connects() {
    let dir = tempdir().expect("carpeta temporal");
    let connector = FakeConnector::new();
    let mut servidor = ServidorOAuth::nuevo(3);
    let (listener, redirect) = listener_de_prueba();
    let mut app = app_con(&dir, &connector, Settings::default());
    app.set_login_issuer(servidor.base().to_owned());
    app.set_login_listener(listener_fijo(listener, redirect));
    app.set_save_client(|id, uri| {
        Ok(Settings {
            client_id: id,
            redirect_uri: uri,
            ..Settings::default()
        })
    });

    let url = app.start_login().expect("inicia el login");
    assert_eq!(app.login_status(), Some(Status::Connecting));
    let autorizado = reqwest::Url::parse(&url).expect("URL de autorización");
    let state = autorizado
        .query_pairs()
        .find(|(clave, _)| clave == "state")
        .expect("state en la URL")
        .1
        .into_owned();
    let redirect_url = autorizado
        .query_pairs()
        .find(|(clave, _)| clave == "redirect_uri")
        .expect("redirect en la URL")
        .1
        .into_owned();

    let (estado, _, cuerpo) = pedir(&format!("{redirect_url}?code=codigo&state={state}"));
    assert_eq!(estado, 200);
    assert!(cuerpo.contains("Ya puedes cerrar esta pestaña"));

    let inicio = Instant::now();
    while (app.status() != Status::Ready || app.login_status().is_some())
        && inicio.elapsed() < Duration::from_secs(5)
    {
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(app.status(), Status::Ready);
    assert!(app.login_status().is_none());

    let guardada = FileStore::from_data_dir(dir.path())
        .load()
        .expect("la sesión quedó guardada");
    assert_eq!(guardada.access_token, "acceso");
    assert_eq!(guardada.refresh_token, "refresco");
    assert_eq!(connector.calls.load(Ordering::SeqCst), 1);

    let peticiones = servidor.esperar();
    assert_eq!(peticiones.len(), 3);
    assert!(
        peticiones[2].starts_with("POST /token"),
        "{}",
        peticiones[2]
    );
}

/// Store que ejecuta el logout justo antes de escribir el token: reproduce
/// la ventana entre el chequeo de cancel y el `save` del canje, donde el
/// logout borra el store y el token queda escrito después del borrado.
#[derive(Clone)]
struct StoreConLogout {
    file: Arc<FileStore>,
    compartido: Arc<Compartido>,
}

struct Compartido {
    app: OnceLock<App>,
    guardado: AtomicUsize,
}

impl StoreConLogout {
    fn nuevo(dir: &TempDir) -> Self {
        Self {
            file: Arc::new(FileStore::from_data_dir(dir.path())),
            compartido: Arc::new(Compartido {
                app: OnceLock::new(),
                guardado: AtomicUsize::new(0),
            }),
        }
    }

    fn set_app(&self, app: App) {
        let _ = self.compartido.app.set(app);
    }

    fn guardados(&self) -> usize {
        self.compartido.guardado.load(Ordering::SeqCst)
    }
}

impl Store for StoreConLogout {
    fn save(&self, token: Token) -> Result<(), orus_auth::Error> {
        if let Some(app) = self.compartido.app.get() {
            app.logout().expect("logout durante el canje");
        }
        self.file.save(token)?;
        self.compartido.guardado.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn load(&self) -> Result<Token, orus_auth::Error> {
        self.file.load()
    }

    fn clear(&self) -> Result<(), orus_auth::Error> {
        self.file.clear()
    }
}

/// El logout durante el canje no puede dejar el token guardado: la sesión
/// reviviría al próximo arranque y en un equipo compartido aparecería la
/// cuenta que la persona cerró.
#[test]
fn logout_durante_canje_no_revive_la_sesion() {
    let dir = tempdir().expect("carpeta temporal");
    let connector = FakeConnector::new();
    let mut servidor = ServidorOAuth::nuevo(3);
    let (listener, redirect) = listener_de_prueba();
    let espia = StoreConLogout::nuevo(&dir);
    let refresher: Arc<dyn Refresh> = Arc::new(FakeRefresh);
    let mut app = App::new(
        Box::new(espia.clone()),
        Settings::default(),
        connector.clone(),
        refresher,
    );
    app.set_connect_backoff(Duration::from_millis(1));
    espia.set_app(app.clone());
    app.set_login_issuer(servidor.base().to_owned());
    app.set_login_listener(listener_fijo(listener, redirect));
    app.set_save_client(|id, uri| {
        Ok(Settings {
            client_id: id,
            redirect_uri: uri,
            ..Settings::default()
        })
    });

    let url = app.start_login().expect("inicia el login");
    let autorizado = reqwest::Url::parse(&url).expect("URL de autorización");
    let state = autorizado
        .query_pairs()
        .find(|(clave, _)| clave == "state")
        .expect("state en la URL")
        .1
        .into_owned();
    let redirect_url = autorizado
        .query_pairs()
        .find(|(clave, _)| clave == "redirect_uri")
        .expect("redirect en la URL")
        .1
        .into_owned();
    let (estado, _, _) = pedir(&format!("{redirect_url}?code=codigo&state={state}"));
    assert_eq!(estado, 200);
    let peticiones = servidor.esperar();
    assert_eq!(peticiones.len(), 3);

    // El token llegó al disco con el logout ya ejecutado dentro del save.
    let inicio = Instant::now();
    while espia.guardados() == 0 && inicio.elapsed() < Duration::from_secs(5) {
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(espia.guardados(), 1, "el canje no llegó a guardar");
    assert_eq!(app.status(), Status::LoggedOut, "el logout no corrió");

    // El chequeo posterior al save debe limpiar lo que se escribió.
    let store = FileStore::from_data_dir(dir.path());
    let inicio = Instant::now();
    while store.load().is_ok() && inicio.elapsed() < Duration::from_secs(5) {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        matches!(store.load(), Err(orus_auth::Error::NoSession)),
        "el token sobrevivió al logout: la sesión revive al próximo arranque"
    );
    assert_eq!(
        connector.calls.load(Ordering::SeqCst),
        0,
        "no debía conectar tras el logout"
    );
}

#[test]
fn logout_durante_descubrimiento_cancela_el_login() {
    let dir = tempdir().expect("carpeta temporal");
    let connector = FakeConnector::new();
    let listener_oauth = TcpListener::bind(("127.0.0.1", 0)).expect("puerto libre");
    let base = format!("http://{}", listener_oauth.local_addr().expect("dirección"));
    let (entrado_tx, entrado_rx) = std::sync::mpsc::channel();
    let (suelta_tx, suelta_rx) = std::sync::mpsc::channel::<()>();
    let base_hilo = base.clone();
    let servidor = thread::spawn(move || {
        let (mut flujo, _) = aceptar(&listener_oauth);
        let peticion = leer_peticion(&mut flujo);
        entrado_tx.send(()).expect("avisar del descubrimiento");
        suelta_rx.recv().expect("liberar la respuesta");
        let (estado, tipo, cuerpo) = responder_oauth(&base_hilo, &peticion);
        escribir_respuesta(&mut flujo, estado, tipo, &cuerpo);
    });

    let (listener, redirect) = listener_de_prueba();
    let mut app = app_con(
        &dir,
        &connector,
        Settings {
            client_id: "cliente-existente".to_owned(),
            redirect_uri: redirect.clone(),
            ..Settings::default()
        },
    );
    app.set_login_issuer(base);
    app.set_login_listener(listener_fijo(listener, redirect));
    app.set_save_client(|id, uri| {
        Ok(Settings {
            client_id: id,
            redirect_uri: uri,
            ..Settings::default()
        })
    });
    let compartida = Arc::new(app);
    let (resultado_tx, resultado_rx) = std::sync::mpsc::channel();
    let en_hilo = Arc::clone(&compartida);
    thread::spawn(move || {
        let resultado = en_hilo.start_login();
        let _ = resultado_tx.send(resultado);
    });

    entrado_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("el descubrimiento está en curso");
    compartida
        .logout()
        .expect("logout durante el descubrimiento");
    suelta_tx.send(()).expect("liberar el descubrimiento");
    servidor.join().expect("el servidor respondió");

    let resultado = resultado_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("el login cancelado termina pronto");
    assert!(
        matches!(resultado, Err(SessionError::LoggedOut)),
        "{resultado:?}"
    );
    assert!(
        compartida.login_status().is_none(),
        "el flujo cancelado no debe publicarse"
    );
    assert_eq!(compartida.status(), Status::LoggedOut);
    assert_eq!(connector.calls.load(Ordering::SeqCst), 0);
}

#[cfg(target_os = "linux")]
#[test]
#[serial_test::serial]
fn import_token_usa_el_respaldo_en_linux() {
    temp_env::with_var(
        "DBUS_SESSION_BUS_ADDRESS",
        Some("unix:path=/nonexistent"),
        || {
            temp_env::with_var_unset("ORUSCRIBE_ALLOW_FILE_SECRETS", || {
                let dir = tempdir().expect("carpeta temporal");
                let refresher: Arc<dyn Refresh> = Arc::new(FakeRefresh);
                let app = App::new(
                    orus_auth::new_store(dir.path()),
                    Settings::default(),
                    FakeConnector::new(),
                    refresher,
                );
                tokio::runtime::Runtime::new()
                    .expect("runtime")
                    .block_on(app.import_token("token-de-prueba"))
                    .expect("importar el token con el respaldo en archivo");
                let guardado = FileStore::from_data_dir(dir.path())
                    .load()
                    .expect("leer el respaldo");
                assert_eq!(guardado.access_token, "token-de-prueba");
            });
        },
    );
}

struct StoreQueNoGuarda {
    archivo: FileStore,
    motivo: String,
}

impl Store for StoreQueNoGuarda {
    fn save(&self, _: Token) -> Result<(), AuthError> {
        Err(AuthError::Unavailable(self.motivo.clone()))
    }

    fn load(&self) -> Result<Token, AuthError> {
        self.archivo.load()
    }

    fn clear(&self) -> Result<(), AuthError> {
        self.archivo.clear()
    }
}

#[test]
fn tabla_canje_valido_sobrevive_a_fallo_de_persistencia() {
    let casos = [
        ("llavero", "llavero bloqueado"),
        ("permisos", "Permiso denegado"),
    ];
    for (nombre, motivo) in casos {
        let dir = tempdir().expect("carpeta temporal");
        let connector = FakeConnector::new();
        let mut servidor = ServidorOAuth::nuevo(3);
        let (listener, redirect) = listener_de_prueba();
        let mut app = App::new(
            Box::new(StoreQueNoGuarda {
                archivo: FileStore::from_data_dir(dir.path()),
                motivo: motivo.to_owned(),
            }),
            Settings::default(),
            connector.clone(),
            Arc::new(FakeRefresh) as Arc<dyn Refresh>,
        );
        app.set_connect_backoff(Duration::from_millis(1));
        app.set_login_issuer(servidor.base().to_owned());
        app.set_login_listener(listener_fijo(listener, redirect.clone()));
        app.set_save_client(|id, uri| {
            Ok(Settings {
                client_id: id,
                redirect_uri: uri,
                ..Settings::default()
            })
        });

        let url = app.start_login().expect("inicia el login");
        let autorizado = reqwest::Url::parse(&url).expect("URL de autorización");
        let state = autorizado
            .query_pairs()
            .find(|(clave, _)| clave == "state")
            .expect("state en la URL")
            .1
            .into_owned();
        let redirect_url = autorizado
            .query_pairs()
            .find(|(clave, _)| clave == "redirect_uri")
            .expect("redirect en la URL")
            .1
            .into_owned();
        let (estado, _, _) = pedir(&format!("{redirect_url}?code=codigo&state={state}"));
        assert_eq!(estado, 200, "{nombre}");

        let inicio = Instant::now();
        while (app.status() != Status::Ready || app.login_status().is_some())
            && inicio.elapsed() < Duration::from_secs(5)
        {
            thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            app.status(),
            Status::Ready,
            "{nombre}: el canje válido no entró"
        );
        assert_eq!(
            connector.calls.load(Ordering::SeqCst),
            1,
            "{nombre}: no conectó con el token canjeado"
        );
        let aviso = app.last_error_sesion();
        assert!(aviso.contains("sesión está activa"), "{nombre}: {aviso}");
        assert!(aviso.contains(motivo), "{nombre}: {aviso}");
        assert_eq!(servidor.esperar().len(), 3);
    }
}

struct SpyRefresh(Arc<Mutex<Vec<String>>>);

#[async_trait]
impl Refresh for SpyRefresh {
    async fn refresh(&self, _: &Token) -> Result<Token, SessionError> {
        Err(SessionError::RefreshRejected)
    }

    fn set_client_id(&self, client_id: &str) {
        self.0
            .lock()
            .expect("el registro de clientes")
            .push(client_id.to_owned());
    }
}

#[test]
fn tabla_refresco_usa_el_client_id_del_login() {
    let casos = [
        ("cliente existente", "cliente-existente", None, 1),
        ("cliente registrado", "viejo", Some("nuevo"), 2),
    ];
    for (nombre, inicial, registrado, esperadas) in casos {
        let dir = tempdir().expect("carpeta temporal");
        let mut servidor = ServidorOAuth::nuevo(esperadas);
        let (listener, redirect) = listener_de_prueba();
        let ids = Arc::new(Mutex::new(Vec::new()));
        let settings = Settings {
            client_id: inicial.to_owned(),
            redirect_uri: if registrado.is_some() {
                "http://127.0.0.1:1/oauth_callback".to_owned()
            } else {
                redirect.clone()
            },
            ..Settings::default()
        };
        let mut app = App::new(
            Box::new(FileStore::from_data_dir(dir.path())),
            settings,
            FakeConnector::new(),
            Arc::new(SpyRefresh(Arc::clone(&ids))) as Arc<dyn Refresh>,
        );
        app.set_login_issuer(servidor.base().to_owned());
        app.set_login_listener(listener_fijo(listener, redirect.clone()));
        app.set_save_client(|id, uri| {
            Ok(Settings {
                client_id: id,
                redirect_uri: uri,
                ..Settings::default()
            })
        });

        let url = app.start_login().expect("inicia el login");
        assert!(
            url.contains(&format!("client_id={}", registrado.unwrap_or(inicial))),
            "{nombre}: {url}"
        );
        assert_eq!(servidor.esperar().len(), esperadas);
        let esperado = registrado
            .map(str::to_owned)
            .unwrap_or_else(|| inicial.to_owned());
        assert_eq!(
            app.settings_snapshot().client_id,
            esperado,
            "{nombre}: los ajustes en memoria no siguen al login"
        );
        if let Some(registrado) = registrado {
            assert_eq!(
                ids.lock().expect("el registro de clientes").clone(),
                vec![registrado.to_owned()],
                "{nombre}: el refresco no recibió el client_id registrado"
            );
        }
    }
}
