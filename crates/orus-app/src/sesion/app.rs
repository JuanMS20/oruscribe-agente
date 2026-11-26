use std::collections::HashMap;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use orus_auth::Store;
use orus_config::{Locale, Settings};
use orus_harness::mcptools::RegistroLecturas;
use orus_org::Me;
use orus_org::doorbell::Config as DoorbellConfig;
use serde::Serialize;
use tokio::sync::Mutex as RefreshMutex;

use crate::agent::{Agent, ProveedorFn, Secrets, WakeLog};
use crate::conversations::Conversations;
use crate::login::LoginState;

use super::backend::SessionError;
use super::conector::{Connector, Refresh};
use super::{App, Estado, Status, WatchSpawner, endpoint};

/// Lo que la ventana pinta al abrirse.
#[derive(Debug, Clone, Serialize)]
pub struct State {
    pub status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub me: Option<Me>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub error: String,
    pub locale: Locale,
    /// Hay un inicio de sesión con navegador en curso.
    pub flow: bool,
    /// Enlace OAuth del flujo activo para mostrarlo o copiarlo si el navegador
    /// no se abrió solo o si la ventana recargó a mitad del inicio de sesión.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub login_url: String,
    /// La sesión en disco se está restaurando en segundo plano.
    pub retrying: bool,
}

impl App {
    pub fn new(
        store: Box<dyn Store + Send + Sync>,
        settings: Settings,
        connector: Arc<dyn Connector>,
        refresher: Arc<dyn Refresh>,
    ) -> Self {
        // `Boot` deja `connecting` si hay sesión en disco.
        let status = if store.load().is_ok() {
            Status::Connecting
        } else {
            Status::LoggedOut
        };
        let telemetry = crate::telemetria::Telemetry::from_env(settings.telemetry_on());
        let store: Arc<dyn Store + Send + Sync> = Arc::from(store);
        Self {
            store,
            settings: Arc::new(Mutex::new(settings)),
            estado: Arc::new(Mutex::new(Estado {
                status,
                ..Estado::default()
            })),
            refresh_mu: Arc::new(RefreshMutex::new(())),
            sesion_gen: Arc::new(AtomicU64::new(0)),
            connect_backoff: Duration::from_secs(1),
            restore_backoff: Duration::from_secs(10),
            restore_timeout: super::conexion::RESTORE_TIMEOUT,
            connector,
            refresher,
            login: LoginState::default(),
            watch_mu: Arc::new(Mutex::new(())),
            watch_key: Arc::new(Mutex::new(String::new())),
            watch_gen: Arc::new(AtomicU64::new(0)),
            watch_spawner: None,
            conversations: None,
            agent: None,
            secrets: None,
            wake_log: None,
            bell_config: Arc::new(Mutex::new(None)),
            proveedor: None,
            registro_lecturas: Arc::new(RegistroLecturas::new()),
            avatares: Arc::new(crate::avatares::Avatares::new()),
            telemetry,
            runtime: {
                let celda = std::sync::OnceLock::new();
                if let Ok(handle) = tokio::runtime::Handle::try_current() {
                    let _ = celda.set(handle);
                }
                celda
            },
        }
    }

    /// Costura del timbre para el borde; sin ella, `kick_watch` solo mueve la
    /// clave y la generación.
    pub fn with_watch_spawner(mut self, spawner: Arc<dyn WatchSpawner>) -> Self {
        self.watch_spawner = Some(spawner);
        self
    }

    pub fn with_conversations(mut self, conversations: Arc<Conversations>) -> Self {
        self.conversations = Some(conversations);
        self
    }

    pub fn with_agent(mut self, agent: Arc<Agent>) -> Self {
        self.agent = Some(agent);
        self
    }

    /// El llavero del proveedor. Sin él, `AgentView` no tiene clave y las rutas
    /// que la necesiten lo dicen al guardar.
    pub fn with_secrets(mut self, secrets: Arc<dyn Secrets>) -> Self {
        self.secrets = Some(secrets);
        self
    }

    /// El registro de lo que hizo el agente por su cuenta. Sin proveedor, la
    /// vista lo devuelve vacío.
    pub fn with_wake_log(mut self, wake_log: Arc<dyn WakeLog>) -> Self {
        self.wake_log = Some(wake_log);
        self
    }

    /// La fábrica de proveedores que usa `agent_test`. Sin ella se construye el
    /// proveedor real desde la base y la clave.
    pub fn with_proveedor(mut self, proveedor: ProveedorFn) -> Self {
        self.proveedor = Some(proveedor);
        self
    }

    pub fn agent(&self) -> Option<Arc<Agent>> {
        self.agent.clone()
    }

    pub fn conversations(&self) -> Option<Arc<Conversations>> {
        self.conversations.clone()
    }

    pub fn set_connect_backoff(&mut self, backoff: Duration) {
        self.connect_backoff = backoff;
    }

    pub fn set_restore_backoff(&mut self, backoff: Duration) {
        self.restore_backoff = backoff;
    }

    /// El plazo de restauración, para probar el vencimiento sin esperar 45 s.
    pub fn set_restore_timeout(&mut self, plazo: Duration) {
        self.restore_timeout = plazo;
    }

    pub fn status(&self) -> Status {
        self.estado().status
    }

    pub fn me(&self) -> Option<Me> {
        self.estado().me.clone()
    }

    /// Las fotos cacheadas de la organización.
    /// actual, para inyectarlas donde el backend no las trae. Nunca espera a
    /// la red: si el refresco toca, corre en segundo plano.
    pub fn avatar_map(&self) -> HashMap<String, String> {
        let org = self
            .estado()
            .me
            .as_ref()
            .map(|me| me.organization.id.clone())
            .unwrap_or_default();
        if org.is_empty() {
            return HashMap::new();
        }
        let (base, anon) = match crate::sync::lock(&self.bell_config).as_ref() {
            Some(config) => (config.supabase_url.clone(), config.anon_key.clone()),
            None => (String::new(), String::new()),
        };
        let Ok(token) = self.load_token() else {
            return HashMap::new();
        };
        self.avatares.mapa(&org, &base, &anon, &token.access_token)
    }

    /// El último error crudo de la sesión, sin el flujo de login encima.
    ///
    /// `state().error` no sirve para el diagnóstico: un flujo vivo lo pisa
    /// aunque el error real siga ahí.
    /// El contrato lee `a.lastErr` directo.
    pub fn last_error_sesion(&self) -> String {
        self.estado().last_error.clone()
    }

    /// El snapshot que pinta la ventana. El lock de `Estado`
    /// se suelta antes de leer ajustes y login para no cruzarlo con `logout`.
    pub fn state(&self) -> State {
        let (mut status, me, mut error, retrying) = {
            let estado = self.estado();
            (
                estado.status,
                estado.me.clone(),
                estado.last_error.clone(),
                estado.retrying,
            )
        };
        // el flujo vivo pisa `status` y `error`, aunque su
        // error venga vacío. El lock de `Estado` ya se soltó.
        let flow = self.login.flow_snapshot();
        let mut login_url = String::new();
        if let Some((flow_status, flow_error, flow_url)) = &flow {
            status = *flow_status;
            error = flow_error.clone();
            login_url = flow_url.clone();
        }
        // La foto propia no viene del backend: sale de la caché de avatares
        let mut me = me;
        if let Some(me) = me.as_mut()
            && me.avatar_url.as_deref().unwrap_or("").is_empty()
        {
            let urls = self.avatar_map();
            if let Some(url) = urls.get(&me.user_id) {
                me.avatar_url = Some(url.clone());
            }
        }
        State {
            status,
            me,
            error,
            locale: self.settings_snapshot().locale,
            flow: flow.is_some(),
            login_url,
            retrying,
        }
    }

    pub fn settings_snapshot(&self) -> Settings {
        crate::sync::lock(&self.settings).clone()
    }

    pub(crate) fn replace_settings(&self, settings: Settings) {
        *crate::sync::lock(&self.settings) = settings;
    }

    /// El endpoint efectivo del panel: un ajuste vacío usa el de producción
    pub fn endpoint(&self) -> String {
        self.endpoint_for(&self.settings_snapshot())
    }

    /// El endpoint de un `Settings` ya leído, para que el borde arme la vista
    /// con un solo candado.
    pub fn endpoint_for(&self, settings: &Settings) -> String {
        endpoint(settings)
    }

    /// Aplica lo que manda el panel. Valida antes de
    /// tocar disco y solo reemplaza la memoria si `editar` devolvió `Ok`: si
    /// el guardado falla, la memoria queda como estaba.
    pub fn telemetry(&self) -> Arc<crate::telemetria::Telemetry> {
        Arc::clone(&self.telemetry)
    }

    pub fn report_error(&self, category: &str, message: &str, extra: serde_json::Value) {
        self.telemetry.report(category, "error", message, extra);
    }

    pub fn change_settings(
        &self,
        locale: Option<&str>,
        autostart_asked: Option<bool>,
        auto_update: Option<bool>,
        telemetry: Option<bool>,
    ) -> Result<Settings, SessionError> {
        let locale = match locale.filter(|valor| !valor.is_empty()) {
            Some("es") => Some(Locale::Spanish),
            Some("en") => Some(Locale::English),
            Some(otro) => {
                return Err(SessionError::Invalido(format!(
                    "idioma desconocido: {otro:?}"
                )));
            }
            None => None,
        };

        let mut resultado = None;
        orus_config::editar(|settings| {
            if let Some(locale) = locale {
                settings.locale = locale;
            }
            if let Some(asked) = autostart_asked {
                settings.autostart_asked = asked;
            }
            if let Some(auto_update) = auto_update {
                settings.auto_update = Some(auto_update);
            }
            if let Some(telemetry) = telemetry {
                settings.telemetry = Some(telemetry);
            }
            resultado = Some(settings.clone());
        })
        .map_err(|error| SessionError::Tool(error.to_string()))?;

        let settings = resultado
            .ok_or_else(|| SessionError::Tool("no se pudo leer la configuración".to_owned()))?;
        self.telemetry.set_enabled(settings.telemetry_on());
        self.replace_settings(settings.clone());
        Ok(settings)
    }

    /// Guarda la copia del timbre que trajo la última conexión. La lee el
    /// cerebro de la organización.
    pub fn set_bell_config(&self, config: DoorbellConfig) {
        *crate::sync::lock(&self.bell_config) = Some(config);
    }

    /// Suelta la copia del timbre al cerrar sesión o cambiar de organización:
    /// la base de la org anterior no puede sobrevivir a la sesión que la trajo.
    /// La repone la próxima conexión que abra el canal.
    pub fn clear_bell_config(&self) {
        *crate::sync::lock(&self.bell_config) = None;
    }

    /// La raíz de las funciones de Supabase que ya trae la configuración del
    /// timbre. Vacío significa que aún no hay conexión.
    pub(crate) fn cerebro_base(&self) -> String {
        let config = crate::sync::lock(&self.bell_config);
        match config.as_ref() {
            Some(config) if !config.supabase_url.is_empty() => {
                format!("{}/functions/v1", config.supabase_url.trim_end_matches('/'))
            }
            _ => String::new(),
        }
    }

    /// El token vivo de la sesión, vacío si no hay. Nunca se guarda en los ajustes del agente.
    pub(crate) fn token_de_sesion(&self) -> String {
        match self.load_token() {
            Ok(token) => token.access_token,
            Err(_) => String::new(),
        }
    }
}
