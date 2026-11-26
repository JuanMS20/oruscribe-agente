//! Sesión de la app: token, refresco y cliente MCP.
//!

mod app;
mod backend;
mod cliente;
mod conector;
mod conexion;

pub use app::State;
pub use backend::{Backend, SessionError};
pub use conector::{Connector, OrgConnector, OrgRefresh, Refresh};

use backend::org_error;

use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use chrono::{DateTime, FixedOffset, TimeZone, Utc};
use orus_auth::{Store, Token};
use orus_config::Settings;
use orus_harness::mcptools::RegistroLecturas;
use orus_org::doorbell::Config as DoorbellConfig;
use orus_org::{DEFAULT_ENDPOINT, DigestSource, Me};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex as RefreshMutex;

use crate::agent::{Agent, ProveedorFn, Secrets, WakeLog};
use crate::conversations::Conversations;
use crate::login::LoginState;
use crate::watch::ClientProvider;

/// El estado que pinta la ventana. Cuatro valores, ni uno más.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    #[default]
    LoggedOut,
    Connecting,
    Ready,
    Failed,
}

/// Lee los commits de una carpeta local. Es la materia
/// prima de la captura y corre en la máquina, nunca contra el servidor.
pub fn git_log(dir: &str, since: Option<DateTime<Utc>>) -> Result<DigestSource, SessionError> {
    orus_org::captura::git_log(dir, since).map_err(org_error)
}

/// Como [`git_log`], con plazo: al vencerlo se mata el proceso git, en vez de
/// dejar un hilo bloqueante esperando a que termine solo.
pub fn git_log_con_plazo(
    dir: &str,
    since: Option<DateTime<Utc>>,
    plazo: Duration,
) -> Result<DigestSource, SessionError> {
    orus_org::captura::git_log_con_plazo(dir, since, plazo).map_err(org_error)
}

#[derive(Default)]
struct Estado {
    client: Option<Arc<dyn Backend>>,
    status: Status,
    me: Option<Me>,
    last_error: String,
    retrying: bool,
    token_efimero: Option<Token>,
}

/// La costura del timbre: el borde es dueño del `Watch` y de sus sinks
/// `App` solo decide cuándo arrancarlo y detenerlo.
pub trait WatchSpawner: Send + Sync {
    fn start(&self, generation: u64, client: Arc<dyn ClientProvider>);
    fn stop(&self);
}

/// La sesión de la app, sin pantalla ni avatares.
#[derive(Clone)]
pub struct App {
    pub(crate) store: Arc<dyn Store + Send + Sync>,
    pub(crate) settings: Arc<Mutex<Settings>>,
    estado: Arc<Mutex<Estado>>,
    refresh_mu: Arc<RefreshMutex<()>>,
    sesion_gen: Arc<AtomicU64>,
    connect_backoff: Duration,
    restore_backoff: Duration,
    /// Tope de una restauración y de lo que `client` espera bajo `refresh_mu`
    /// (`RESTORE_TIMEOUT`, inyectable en los tests).
    restore_timeout: Duration,
    connector: Arc<dyn Connector>,
    pub(crate) refresher: Arc<dyn Refresh>,
    pub(crate) login: LoginState,
    watch_mu: Arc<Mutex<()>>,
    watch_key: Arc<Mutex<String>>,
    watch_gen: Arc<AtomicU64>,
    watch_spawner: Option<Arc<dyn WatchSpawner>>,
    pub(crate) conversations: Option<Arc<Conversations>>,
    pub(crate) agent: Option<Arc<Agent>>,
    pub(crate) secrets: Option<Arc<dyn Secrets>>,
    pub(crate) wake_log: Option<Arc<dyn WakeLog>>,
    /// La última configuración del timbre que trajo el servidor; de acá sale
    /// la base del cerebro de la organización.
    pub(crate) bell_config: Arc<Mutex<Option<DoorbellConfig>>>,
    /// La costura del proveedor para probar el agente; sin ella se usa la
    /// fábrica real.
    pub(crate) proveedor: Option<ProveedorFn>,
    /// Tope de lecturas MCP compartido por todos los turnos de esta sesión.
    pub(crate) registro_lecturas: Arc<RegistroLecturas>,
    /// Caché de fotos de perfil por organización.
    pub(crate) avatares: Arc<crate::avatares::Avatares>,
    /// Colector de telemetría de errores (Sentry + anillo local redactado).
    pub(crate) telemetry: Arc<crate::telemetria::Telemetry>,
    /// Runtime donde vive el cliente MCP. El hilo del login trae el suyo, de un
    /// solo hilo, y muere al terminar: si el cliente se abre ahí, su servicio
    /// queda cerrado y todo responde «Transport closed» hasta reiniciar.
    pub(crate) runtime: std::sync::OnceLock<tokio::runtime::Handle>,
}

impl App {
    fn estado(&self) -> MutexGuard<'_, Estado> {
        crate::sync::lock(&self.estado)
    }

    pub(crate) fn aviso_persistencia(&self, mensaje: String) {
        let mut estado = self.estado();
        if estado.status == Status::Ready {
            estado.last_error = mensaje;
        }
    }

    /// Consumo actual del cliente MCP activo (solo memoria, sin red).
    pub fn mcp_quota(&self) -> Option<orus_org::McpQuota> {
        let cliente = self.estado().client.clone()?;
        cliente.mcp_quota()
    }
}

fn endpoint(settings: &Settings) -> String {
    match settings.endpoint.as_str() {
        "" => DEFAULT_ENDPOINT.to_owned(),
        other => other.to_owned(),
    }
}

/// El cero (`0001-01-01T00:00:00Z`) significa "sin vencimiento"
/// (`orus_auth/src/token.rs:27-32`).
fn sin_vencimiento() -> DateTime<FixedOffset> {
    let naive = chrono::NaiveDate::from_ymd_opt(1, 1, 1)
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .unwrap_or_default();
    Utc.from_utc_datetime(&naive).fixed_offset()
}

#[cfg(test)]
mod tests {
    use super::*;

    use async_trait::async_trait;
    use orus_auth::Token;
    use orus_config::Locale;

    /// Un `Store` sin sesión: `App::new` queda en `LoggedOut` sin tocar disco.
    struct StoreVacio;

    impl Store for StoreVacio {
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

    /// Costuras que solo fallan: `state()` no debe llamarlas.
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

    fn app_sin_sesion() -> App {
        App::new(
            Box::new(StoreVacio),
            Settings {
                locale: Locale::Spanish,
                ..Settings::default()
            },
            Arc::new(SinRed),
            Arc::new(SinRed),
        )
    }

    #[test]
    fn state_logged_out_refleja_el_estado() {
        let state = app_sin_sesion().state();

        assert_eq!(state.status, Status::LoggedOut);
        assert!(state.me.is_none());
        assert_eq!(state.error, "");
        assert!(!state.flow, "no hay login en curso");
        assert!(!state.retrying, "no hay restauración en curso");
    }

    #[test]
    fn state_json_omite_me_y_error_vacios() {
        let json =
            serde_json::to_value(app_sin_sesion().state()).unwrap_or(serde_json::Value::Null);

        assert!(json.is_object(), "el snapshot no serializó: {json}");
        assert!(
            json.get("me").is_none(),
            "me vacío no debe aparecer: {json}"
        );
        assert!(
            json.get("error").is_none(),
            "error vacío no debe aparecer: {json}"
        );
        assert_eq!(json["locale"], "es");
        assert_eq!(json["flow"], false);
        assert_eq!(json["retrying"], false);
    }

    #[test]
    fn state_con_flujo_pisa_status_y_error() {
        let app = app_sin_sesion();
        app.login
            .set_flow_for_test(Status::Connecting, Some("falló".to_owned()));

        let state = app.state();

        assert_eq!(state.status, Status::Connecting);
        assert_eq!(state.error, "falló");
        assert!(state.flow, "el flujo vivo se anuncia");
    }

    #[test]
    fn state_con_flujo_sin_error_borra_el_ultimo_error() {
        let app = app_sin_sesion();
        app.estado().last_error = "error viejo".to_owned();
        app.login.set_flow_for_test(Status::Ready, None);

        let state = app.state();

        assert_eq!(state.status, Status::Ready);
        assert_eq!(state.error, "", "el flujo manda aunque no traiga error");
        assert!(state.flow, "el flujo vivo se anuncia");
    }

    #[tokio::test]
    async fn last_error_sesion_conserva_el_error_crudo_sin_el_flujo_de_login() {
        let mut app = app_sin_sesion();
        app.set_connect_backoff(Duration::ZERO);
        let token = Token {
            access_token: "token-de-prueba".to_owned(),
            refresh_token: String::new(),
            token_type: "Bearer".to_owned(),
            scope: String::new(),
            expiry: sin_vencimiento(),
        };
        let resultado = app.connect(&token).await;
        assert!(resultado.is_err(), "SinRed no debería conectar");
        assert_eq!(
            resultado.err().map(|error| error.to_string()),
            Some("sin red".to_owned())
        );
        assert_eq!(app.last_error_sesion(), "sin red");

        app.login
            .set_flow_for_test(Status::Failed, Some("el navegador falló".to_owned()));
        assert_eq!(app.state().error, "el navegador falló");
        assert_eq!(
            app.last_error_sesion(),
            "sin red",
            "el flujo de login no pisa el error crudo que lee el diagnóstico"
        );
    }

    #[test]
    fn cerebro_base_quita_barras_finales_y_exige_timbre() {
        let app = app_sin_sesion();
        assert_eq!(app.cerebro_base(), "", "sin timbre no hay base");

        app.set_bell_config(DoorbellConfig {
            supabase_url: "https://proyecto.supabase.co///".to_owned(),
            ..DoorbellConfig::default()
        });
        assert_eq!(
            app.cerebro_base(),
            "https://proyecto.supabase.co/functions/v1"
        );
    }

    #[test]
    fn set_bell_config_reemplaza_la_copia_anterior() {
        let app = app_sin_sesion();
        app.set_bell_config(DoorbellConfig {
            supabase_url: "https://uno".to_owned(),
            ..DoorbellConfig::default()
        });
        app.set_bell_config(DoorbellConfig {
            supabase_url: "https://dos/".to_owned(),
            ..DoorbellConfig::default()
        });

        assert_eq!(app.cerebro_base(), "https://dos/functions/v1");

        app.set_bell_config(DoorbellConfig::default());
        assert_eq!(app.cerebro_base(), "", "un timbre sin supabase no da base");
    }

    /// O6-01b guarda `Arc<App>` y atiende peticiones en tareas: las firmas de
    /// auth no pueden pedir `&mut`, y el `App` tiene que cruzar hilos.
    #[tokio::test]
    async fn arc_app_atiende_auth_sin_pedir_mut() {
        let app = Arc::new(app_sin_sesion());
        assert_eq!(app.state().status, Status::LoggedOut);

        let compartida = Arc::clone(&app);
        let resultado = tokio::spawn(async move { compartida.import_token("").await }).await;

        assert!(
            matches!(
                &resultado,
                Ok(Err(SessionError::Connect(texto))) if texto == "el token está vacío"
            ),
            "Arc<App> no devolvió el error de token vacío: {resultado:?}"
        );
    }
}
