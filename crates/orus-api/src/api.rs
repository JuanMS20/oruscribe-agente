//! El borde HTTP de la app: `Api`, sus constructores y `router()`/`serve()`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use orus_app::App;
use tokio::net::TcpListener;

use crate::agent_models::{CacheModelos, URL_CATALOGO};
use crate::agent_pick_folder::PickFolder;
use crate::events::Hub;
use crate::quit::Quit;
use crate::router::router_con;
use crate::settings::{SinAutostart, SinTimbre};
use crate::setup::Setup;
use crate::update::Update;
use crate::workspace_write::GitLogReal;
use crate::{Autostart, BellStatus, ESPERA_EVENTOS, GitLog};

/// El borde HTTP de la app.
pub struct Api {
    app: Arc<App>,
    hub: Arc<Hub>,
    wait: Duration,
    git_log: Arc<dyn GitLog>,
    version: String,
    bell_status: Arc<dyn BellStatus>,
    autostart: Arc<dyn Autostart>,
    http: reqwest::Client,
    models_url: String,
    modelos: Arc<Mutex<CacheModelos>>,
    pick_folder: Option<Arc<dyn PickFolder>>,
    setup: Arc<dyn Setup>,
    update: Arc<dyn Update>,
    quit: Arc<dyn Quit>,
}

impl Api {
    fn con(app: Arc<App>, hub: Arc<Hub>, git_log: Arc<dyn GitLog>) -> Self {
        Self {
            app,
            hub,
            wait: ESPERA_EVENTOS,
            git_log,
            version: String::new(),
            bell_status: Arc::new(SinTimbre),
            autostart: Arc::new(SinAutostart),
            http: reqwest::Client::new(),
            models_url: URL_CATALOGO.to_owned(),
            modelos: Arc::new(Mutex::new(CacheModelos::default())),
            pick_folder: None,
            setup: Arc::new(crate::setup::SinInstalador),
            update: Arc::new(crate::update::SinActualizador),
            quit: Arc::new(crate::quit::SinQuit),
        }
    }

    pub fn new(app: Arc<App>) -> Self {
        Self::con(app, Arc::new(Hub::new()), Arc::new(GitLogReal))
    }

    /// Restaura la sesión guardada en segundo plano, como el `Boot` del contrato
    /// Tiene que llamarse dentro de un runtime de Tokio:
    /// `App::boot` usa `tokio::spawn`. Sin esta llamada, una `App` con token en
    /// disco se queda en `connecting` para siempre.
    pub fn boot(&self) {
        self.app.boot();
    }

    /// Deja que el borde inyecte el hub que alimenta su `Watch`.
    pub fn with_hub(app: Arc<App>, hub: Arc<Hub>) -> Self {
        Self::con(app, hub, Arc::new(GitLogReal))
    }

    /// Inyecta la lectura de `git log`; los tests usan un fake.
    pub fn with_git_log(app: Arc<App>, git_log: Arc<dyn GitLog>) -> Self {
        Self::con(app, Arc::new(Hub::new()), git_log)
    }

    /// La versión que muestra el panel. El contrato usa `App.Version`;
    /// el binario la inyecta hasta que exista un constructor propio.
    pub fn with_version(mut self, version: String) -> Self {
        self.version = version;
        self
    }

    /// Inyecta el estado del timbre que publica el borde con el `Watch` real.
    pub fn with_bell_status(mut self, bell_status: Arc<dyn BellStatus>) -> Self {
        self.bell_status = bell_status;
        self
    }

    /// Inyecta el arranque con el sistema; el borde de escritorio lo cablea al
    /// registro de Windows y el servidor sin ventana conserva el aviso.
    pub fn with_autostart(mut self, autostart: Arc<dyn Autostart>) -> Self {
        self.autostart = autostart;
        self
    }

    /// Cambia el plazo del long poll; los tests usan uno corto.
    pub fn with_wait(mut self, wait: Duration) -> Self {
        self.wait = wait;
        self
    }

    /// Inyecta el catálogo de modelos; los tests usan un servidor local.
    pub fn with_models_url(mut self, url: String) -> Self {
        self.models_url = url;
        self
    }

    /// Cablea el selector de carpetas; sin él la ruta responde 400.
    pub fn with_pick_folder(mut self, selector: Arc<dyn PickFolder>) -> Self {
        self.pick_folder = Some(selector);
        self
    }

    /// Inyecta las acciones del instalador; sin ellas las tres rutas de setup
    /// responden 500.
    pub fn with_setup(mut self, setup: Arc<dyn Setup>) -> Self {
        self.setup = setup;
        self
    }

    /// Inyecta las acciones de actualización; sin ellas las dos rutas de
    /// update responden 500.
    pub fn with_update(mut self, update: Arc<dyn Update>) -> Self {
        self.update = update;
        self
    }

    /// Inyecta la salida; sin ella la ruta de `/api/quit` responde 500.
    pub fn with_quit(mut self, quit: Arc<dyn Quit>) -> Self {
        self.quit = quit;
        self
    }

    pub fn router(&self) -> Router {
        router_con(
            Arc::clone(&self.app),
            Arc::clone(&self.hub),
            self.wait,
            Arc::clone(&self.git_log),
            self.version.clone(),
            Arc::clone(&self.bell_status),
            Arc::clone(&self.autostart),
            self.models_url.clone(),
            self.http.clone(),
            Arc::clone(&self.modelos),
            self.pick_folder.clone(),
            Arc::clone(&self.setup),
            Arc::clone(&self.update),
            Arc::clone(&self.quit),
        )
    }

    pub async fn serve(self, listener: TcpListener) -> std::io::Result<()> {
        axum::serve(listener, self.router()).await
    }
}
