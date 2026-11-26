//! Flujo de inicio de sesión OAuth de la app.
//!
//! Las primitivas PKCE, registro y canje viven en
//! `orus-auth`; acá solo se orquesta el listener, el flujo y la conexión.

use std::io;
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use orus_auth::Config;
use orus_config::Settings;

use crate::callback::wait;
use crate::sesion::{App, SessionError, Status};

/// El puerto fijo del callback. El servidor compara el
/// redirect byte a byte; cambiarlo obligaría a registrar un cliente nuevo.
pub const LOGIN_PORT: u16 = 47823;

const HOST: &str = "127.0.0.1";
const CLIENT_NAME: &str = "Oruscribe Agente";
const SCOPES: [&str; 3] = ["openid", "email", "offline_access"];
const LOGIN_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// El cliente HTTP del contrato usa 30 s.
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
const TEXTO_TIMEOUT: &str = "el inicio de sesión venció; vuelve a intentarlo";
const TEXTO_HILO: &str = "no se pudo lanzar el inicio de sesión";

/// El flujo que la ventana pinta.
pub struct LoginFlow {
    pub state: String,
    pub url: String,
    pub status: Status,
    pub error: Option<String>,
    pub cancel: Arc<AtomicBool>,
}

type OpenUrl = Arc<dyn Fn(&str) + Send + Sync>;
type SaveClient = Arc<dyn Fn(String, String) -> Result<Settings, String> + Send + Sync>;
type ListenerFactory = Arc<dyn Fn() -> io::Result<(TcpListener, String)> + Send + Sync>;

/// Lo que `App` guarda del login: el flujo vivo y las costuras del borde.
#[derive(Clone)]
pub(crate) struct LoginState {
    flow: Arc<Mutex<Option<LoginFlow>>>,
    issuer: String,
    listener: ListenerFactory,
    open_url: Option<OpenUrl>,
    save_client: SaveClient,
}

impl Default for LoginState {
    fn default() -> Self {
        Self {
            flow: Arc::new(Mutex::new(None)),
            issuer: orus_auth::ISSUER.to_owned(),
            listener: Arc::new(login_listener),
            open_url: None,
            save_client: Arc::new(guardar_cliente),
        }
    }
}

impl LoginState {
    fn lock(&self) -> MutexGuard<'_, Option<LoginFlow>> {
        crate::sync::lock(&self.flow)
    }

    /// Suelta el flujo vivo, como `a.flow = nil`. Si había un
    /// login esperando el callback, se cancela: sin esto el canje sigue y
    /// guarda una sesión después de cerrar sesión.
    pub(crate) fn clear_flow(&self) {
        if let Some(flow) = self.lock().take() {
            flow.cancel.store(true, Ordering::SeqCst);
        }
    }

    /// Copia `status`, `error` y `url` del flujo vivo en un solo lock, sin
    /// sacarlo. Un flujo sin error devuelve la cadena vacía.
    pub(crate) fn flow_snapshot(&self) -> Option<(Status, String, String)> {
        self.lock().as_ref().map(|flow| {
            (
                flow.status,
                flow.error.clone().unwrap_or_default(),
                flow.url.clone(),
            )
        })
    }
}

#[cfg(test)]
impl LoginState {
    /// Inyecta un flujo vivo para probar el snapshot sin abrir navegador.
    pub(crate) fn set_flow_for_test(&self, status: Status, error: Option<String>) {
        *self.lock() = Some(LoginFlow {
            state: "flujo-de-prueba".to_owned(),
            url: String::new(),
            status,
            error,
            cancel: Arc::new(AtomicBool::new(false)),
        });
    }

    /// Igual, pero con el `cancel` del llamador para comprobar que se dispara.
    pub(crate) fn set_flow_para_cancelar(&self, cancel: Arc<AtomicBool>) {
        *self.lock() = Some(LoginFlow {
            state: "flujo-de-prueba".to_owned(),
            url: String::new(),
            status: Status::Connecting,
            error: None,
            cancel,
        });
    }
}

/// Lo que el hilo de setup le devuelve a `start_login`.
struct Aviso {
    url: String,
    settings: Option<Settings>,
}

/// Setup terminado; el listener y el config siguen en el hilo del flujo.
struct Preparado {
    aviso: Aviso,
    job: LoginJob,
}

struct LoginJob {
    listener: TcpListener,
    http: reqwest::Client,
    config: Config,
    state: String,
    verifier: String,
    cancel: Arc<AtomicBool>,
}

impl App {
    /// El estado del flujo de login, o `None` si no hay ninguno.
    pub fn login_status(&self) -> Option<Status> {
        self.login.lock().as_ref().map(|flow| flow.status)
    }

    /// Costura del borde: sin ella, quien llama abre la URL que devuelve
    /// `start_login`.
    pub fn set_open_url(&mut self, open: impl Fn(&str) + Send + Sync + 'static) {
        self.login.open_url = Some(Arc::new(open));
    }

    /// Costura de pruebas: el issuer real es `orus_auth::ISSUER`.
    pub fn set_login_issuer(&mut self, issuer: impl Into<String>) {
        self.login.issuer = issuer.into();
    }

    /// Costura de pruebas: el listener real es `login_listener`.
    pub fn set_login_listener(
        &mut self,
        listener: impl Fn() -> io::Result<(TcpListener, String)> + Send + Sync + 'static,
    ) {
        self.login.listener = Arc::new(listener);
    }

    /// Costura de pruebas: el guardado real usa `orus_config::editar`.
    pub fn set_save_client(
        &mut self,
        save: impl Fn(String, String) -> Result<Settings, String> + Send + Sync + 'static,
    ) {
        self.login.save_client = Arc::new(save);
    }

    /// Descubre el servidor, registra el cliente si hace falta y devuelve la URL
    /// que abre el navegador.
    pub fn start_login(&self) -> Result<String, SessionError> {
        self.reemplazar_flujo();
        let state =
            orus_auth::new_state().map_err(|error| SessionError::Connect(error.to_string()))?;
        let verifier =
            orus_auth::new_verifier().map_err(|error| SessionError::Connect(error.to_string()))?;
        let cancel = Arc::new(AtomicBool::new(false));
        *self.login.lock() = Some(LoginFlow {
            state: state.clone(),
            url: String::new(),
            status: Status::Connecting,
            error: None,
            cancel: Arc::clone(&cancel),
        });
        let app = self.clone();
        let (envio, recibo) = std::sync::mpsc::channel::<Result<Aviso, SessionError>>();
        let estado = state.clone();
        let verificador = verifier.clone();
        let cancelado = Arc::clone(&cancel);
        std::thread::Builder::new()
            .name("orus-login".to_owned())
            .spawn(move || hilo_login(app, envio, estado, verificador, cancelado))
            .map_err(|error| {
                self.login.clear_flow();
                SessionError::Connect(error.to_string())
            })?;
        let aviso = match recibo
            .recv()
            .map_err(|_| SessionError::Connect(TEXTO_HILO.to_owned()))
            .and_then(|res| res)
        {
            Ok(aviso) => aviso,
            Err(SessionError::LoggedOut) => return Err(SessionError::LoggedOut),
            Err(error) => {
                self.report_error("auth.oauth", &error.to_string(), serde_json::Value::Null);
                return Err(error);
            }
        };
        if let Some(settings) = aviso.settings {
            self.replace_settings(settings);
        }
        if let Some(open) = &self.login.open_url {
            open(&aviso.url);
        }
        Ok(aviso.url)
    }

    /// Cancela el flujo anterior y lo borra.
    pub(crate) fn reemplazar_flujo(&self) {
        if let Some(anterior) = self.login.lock().take() {
            anterior.cancel.store(true, Ordering::SeqCst);
        }
    }

    async fn preparar_login(
        &mut self,
        state: String,
        verifier: String,
        cancel: Arc<AtomicBool>,
    ) -> Result<Preparado, SessionError> {
        let http = reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| SessionError::Connect(error.to_string()))?;
        let metadata = orus_auth::discover(&http, &self.login.issuer)
            .await
            .map_err(|error| SessionError::Connect(error.to_string()))?;
        if cancel.load(Ordering::SeqCst) {
            return Err(SessionError::LoggedOut);
        }
        let (listener, redirect) =
            (self.login.listener)().map_err(|error| SessionError::Connect(error.to_string()))?;
        if cancel.load(Ordering::SeqCst) {
            return Err(SessionError::LoggedOut);
        }

        let mut settings_guardados = None;
        let actuales = self.settings_snapshot();
        let client_id = if actuales.client_id.is_empty() || actuales.redirect_uri != redirect {
            let id = orus_auth::register(&http, &metadata, &redirect, CLIENT_NAME)
                .await
                .map_err(|error| SessionError::Connect(error.to_string()))?;
            if cancel.load(Ordering::SeqCst) {
                return Err(SessionError::LoggedOut);
            }
            // Si falla el guardado, la memoria queda como estaba.
            if let Ok(settings) = (self.login.save_client)(id.clone(), redirect.clone()) {
                self.replace_settings(settings.clone());
                settings_guardados = Some(settings);
            } else {
                let mut settings = self.settings_snapshot();
                settings.client_id = id.clone();
                settings.redirect_uri = redirect.clone();
                self.replace_settings(settings);
            }
            self.refresher.set_client_id(&id);
            id
        } else {
            actuales.client_id.clone()
        };
        if cancel.load(Ordering::SeqCst) {
            return Err(SessionError::LoggedOut);
        }

        let config = Config {
            metadata,
            client_id,
            redirect_uri: redirect,
            scopes: SCOPES.iter().map(|scope| (*scope).to_owned()).collect(),
        };
        let url = orus_auth::authorize_url(&config, &state, &orus_auth::challenge(&verifier))
            .map_err(|error| SessionError::Connect(error.to_string()))?;
        {
            let mut flujo = self.login.lock();
            if cancel.load(Ordering::SeqCst) {
                return Err(SessionError::LoggedOut);
            }
            match flujo.as_mut() {
                Some(actual) if actual.state == state => actual.url = url.clone(),
                _ => return Err(SessionError::LoggedOut),
            }
        }
        Ok(Preparado {
            aviso: Aviso {
                url: url.clone(),
                settings: settings_guardados,
            },
            job: LoginJob {
                listener,
                http,
                config,
                state,
                verifier,
                cancel,
            },
        })
    }

    /// Espera el código, lo canjea, guarda la sesión y conecta.
    async fn finish_login(&self, job: LoginJob) {
        let inicio = Instant::now();
        let LoginJob {
            listener,
            http,
            config,
            state,
            verifier,
            cancel,
        } = job;
        let codigo = match wait(listener, &state, &cancel, LOGIN_TIMEOUT) {
            Ok(codigo) => codigo,
            Err(error) => {
                self.end_flow(&state, Status::Failed, Some(error.to_string()));
                return;
            }
        };
        if cancel.load(Ordering::SeqCst) {
            self.end_flow(
                &state,
                Status::Failed,
                Some("el inicio de sesión se canceló".to_owned()),
            );
            return;
        }

        // El contexto del contrato cubre todo el flujo con 5 min.
        let restante = LOGIN_TIMEOUT.saturating_sub(inicio.elapsed());
        let resultado = tokio::time::timeout(restante, async {
            let token = orus_auth::exchange(&http, &config, &codigo, &verifier)
                .await
                .map_err(|error| error.to_string())?;
            if cancel.load(Ordering::SeqCst) {
                return Err("el inicio de sesión se canceló".to_owned());
            }
            let aviso_guardado = match self.store.save(token.clone()) {
                Ok(()) => {
                    self.limpiar_token_efimero();
                    None
                }
                Err(error) => {
                    self.usar_token_efimero(token.clone());
                    Some(format!(
                        "la sesión está activa, pero no se pudo guardar en este equipo: {error}"
                    ))
                }
            };
            if cancel.load(Ordering::SeqCst) {
                // El logout pudo correr entre el chequeo y el save: el token
                // quedaría escrito después del borrado y la sesión reviviría
                // al próximo arranque. Se limpia solo si lo guardado sigue
                // siendo este token, para no borrar un login más nuevo.
                let sigue = self
                    .store
                    .load()
                    .is_ok_and(|actual| actual.access_token.eq(&token.access_token));
                if sigue {
                    let _ = self.store.clear();
                }
                self.limpiar_token_efimero_si_coincide(&token);
                return Err("el inicio de sesión se canceló".to_owned());
            }
            // El cliente MCP tiene tareas en segundo plano: si se abre en este
            // hilo, su runtime muere al volver y el servicio queda cerrado
            // («Transport closed»). Se conecta en el runtime del servidor.
            if let Some(runtime) = self.runtime.get() {
                let app = self.clone();
                let (envio, recibo) = tokio::sync::oneshot::channel();
                runtime.spawn(async move {
                    let _ = envio.send(app.connect(&token).await);
                });
                recibo
                    .await
                    .map_err(|_| TEXTO_HILO.to_owned())?
                    .map_err(|error| error.to_string())?;
            } else {
                self.connect(&token)
                    .await
                    .map_err(|error| error.to_string())?;
            }
            if let Some(aviso) = aviso_guardado {
                self.aviso_persistencia(aviso);
            }
            Ok::<(), String>(())
        })
        .await;

        match resultado {
            Ok(Ok(())) => self.end_flow(&state, Status::Ready, None),
            Ok(Err(error)) => self.end_flow(&state, Status::Failed, Some(error)),
            Err(_) => self.end_flow(&state, Status::Failed, Some(TEXTO_TIMEOUT.to_owned())),
        }
    }

    /// Cierra el flujo con ese `state`. Un flujo viejo que
    /// termina tarde no toca al actual.
    pub(crate) fn end_flow(&self, state: &str, status: Status, error: Option<String>) {
        let mut flujo = self.login.lock();
        let coincide = flujo.as_ref().is_some_and(|actual| actual.state == state);
        if !coincide {
            return;
        }
        if status == Status::Ready {
            *flujo = None;
            return;
        }
        if let Some(actual) = flujo.as_mut() {
            actual.status = status;
            if let Some(err) = error {
                if status == Status::Failed
                    && err != crate::callback::LoginError::Cancelled.to_string()
                {
                    self.report_error("auth.oauth", &err, serde_json::Value::Null);
                }
                actual.error = Some(err);
            }
        }
    }
}

fn hilo_login(
    mut app: App,
    envio: std::sync::mpsc::Sender<Result<Aviso, SessionError>>,
    state: String,
    verifier: String,
    cancel: Arc<AtomicBool>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = envio.send(Err(SessionError::Connect(error.to_string())));
            return;
        }
    };
    match runtime.block_on(app.preparar_login(state, verifier, cancel)) {
        Ok(preparado) => {
            if envio.send(Ok(preparado.aviso)).is_err() {
                return;
            }
            runtime.block_on(app.finish_login(preparado.job));
        }
        Err(error) => {
            let _ = envio.send(Err(error));
        }
    }
}

/// El listener fijo, con el puerto efímero como respaldo.
pub fn login_listener() -> io::Result<(TcpListener, String)> {
    login_listener_on(LOGIN_PORT)
}

/// Intenta `port` y cae a uno efímero si está ocupado. Se factoriza para
/// probar el respaldo sin depender de que 47823 esté libre.
pub fn login_listener_on(port: u16) -> io::Result<(TcpListener, String)> {
    match escuchar(port) {
        Ok(par) => Ok(par),
        Err(_) => escuchar(0),
    }
}

fn escuchar(port: u16) -> io::Result<(TcpListener, String)> {
    let listener = TcpListener::bind((HOST, port))?;
    let puerto = listener.local_addr()?.port();
    Ok((
        listener,
        format!("http://{HOST}:{puerto}{}", orus_auth::CALLBACK_PATH),
    ))
}

/// Lee lo que hay en disco, aplica el cambio y guarda.
fn guardar_cliente(id: String, redirect: String) -> Result<Settings, String> {
    let mut resultado = Settings::default();
    orus_config::editar(|settings| {
        settings.client_id = id.clone();
        settings.redirect_uri = redirect.clone();
        resultado = settings.clone();
    })
    .map_err(|error| error.to_string())?;
    Ok(resultado)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cerrar_sesion_cancela_el_login_pendiente() {
        let estado = LoginState::default();
        let cancel = Arc::new(AtomicBool::new(false));
        estado.set_flow_para_cancelar(Arc::clone(&cancel));

        estado.clear_flow();

        assert!(
            cancel.load(Ordering::SeqCst),
            "clear_flow soltó el flujo sin cancelar el callback"
        );
        assert!(estado.flow_snapshot().is_none());
    }
}
