//! Servidor headless de Oruscribe: el `App` real, el router de `orus-api` y
//! los archivos de `frontend/dist` (UI y fuente) embebidos.
//!
//! Port de `cmd/immuneserver/main.go:18-55` y de `main.go:28-32`. El Go monta
//! `a.Handler()` en `/api/` y `http.FileServer(http.Dir("frontend/dist"))` en
//! `/`; aquí el router de `orus-api` se fusiona con un fallback que sirve los
//! estáticos y responde 404 como `http.FileServer`, sin fallback de SPA.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod guardia;
mod timbre;

pub use guardia::{Guardia, proteger};

use std::sync::Arc;

use axum::Router;
use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use tokio::net::TcpListener;

const INDEX: &str = include_str!("../../../frontend/dist/index.html");
const APP_CSS: &str = include_str!("../../../frontend/dist/app.css");
const LOGO_SVG: &str = include_str!("../../../frontend/dist/logo.svg");
const LOGO_PNG: &[u8] = include_bytes!("../../../frontend/dist/logo-512.png");
const FONT_WOFF2: &[u8] = include_bytes!("../../../frontend/dist/fonts/inter.woff2");

/// Módulos ES nativos del frontend embebidos en el binario (sin Node ni bundler).
pub const JS_MODULOS: &[(&str, &str)] = &[
    (
        "/js/main.js",
        include_str!("../../../frontend/dist/js/main.js"),
    ),
    (
        "/js/i18n.js",
        include_str!("../../../frontend/dist/js/i18n.js"),
    ),
    (
        "/js/dom.js",
        include_str!("../../../frontend/dist/js/dom.js"),
    ),
    (
        "/js/api.js",
        include_str!("../../../frontend/dist/js/api.js"),
    ),
    (
        "/js/estados.mjs",
        include_str!("../../../frontend/dist/js/estados.mjs"),
    ),
    (
        "/js/state.js",
        include_str!("../../../frontend/dist/js/state.js"),
    ),
    (
        "/js/views/agent.js",
        include_str!("../../../frontend/dist/js/views/agent.js"),
    ),
    (
        "/js/views/brain.js",
        include_str!("../../../frontend/dist/js/views/brain.js"),
    ),
    (
        "/js/views/team.js",
        include_str!("../../../frontend/dist/js/views/team.js"),
    ),
    (
        "/js/views/work.js",
        include_str!("../../../frontend/dist/js/views/work.js"),
    ),
    (
        "/js/views/settings.js",
        include_str!("../../../frontend/dist/js/views/settings.js"),
    ),
];

const TEXTO_404: &str = "404 page not found\n";

/// Quien abre el navegador durante el login. El shell le pasa su abridor; el
/// servidor headless no abre nada.
pub type AbrirNavegador = Arc<dyn Fn(&str) + Send + Sync>;

/// Construye el borde HTTP con las piezas reales de `orus-app`.
///
/// `--data` fija `ORUSCRIBE_DATA_DIR` antes de llamar aquí; por eso
/// `data_dir()`, `load()` y `Conversations::new()` ven la carpeta elegida.
pub fn api(pick_folder: Option<String>) -> Result<orus_api::Api, orus_app::SessionError> {
    api_con_apertura(pick_folder, None)
}

/// Igual que [`api`], pero cablea quién abre el navegador en el login.
///
/// El shell le pasa su abridor; el servidor headless no abre nada. Sin esta
/// costura, `POST /api/auth/start` devolvía la URL OAuth y nadie la abría.
pub fn api_con_apertura(
    pick_folder: Option<String>,
    abrir: Option<AbrirNavegador>,
) -> Result<orus_api::Api, orus_app::SessionError> {
    let datos = orus_config::data_dir();
    let store = orus_auth::new_store(&datos);
    let ajustes = orus_config::load();
    let refresco = orus_app::OrgRefresh::new(ajustes.client_id.clone())?;

    let hub = Arc::new(orus_api::events::Hub::new());
    let watch = Arc::new(orus_app::Watch::new(timbre::conector_real()));
    let mut politica = orus_app::WakePolicy::new(Arc::new(orus_app::RelojLocal));
    politica.enabled = ajustes.agent.wake_enabled;
    let politica = Arc::new(tokio::sync::Mutex::new(politica));
    let timbre = Arc::new(timbre::Timbre::new(watch, Arc::clone(&hub), politica));

    let conversaciones = orus_app::Conversations::new();
    let aprobaciones = Arc::new(orus_app::ApprovalQueue::new());
    let agente = Arc::new(
        orus_app::Agent::new(
            conversaciones.clone(),
            Arc::clone(&aprobaciones),
            Arc::new(orus_app::HarnessRunner::new(Arc::clone(&aprobaciones))),
        )
        .with_on_finish(timbre.al_terminar())
        .with_telemetry(orus_app::Telemetry::compartida()),
    );

    let app = orus_app::App::new(
        store,
        ajustes.clone(),
        Arc::new(orus_app::OrgConnector),
        Arc::new(refresco),
    )
    .with_conversations(Arc::new(conversaciones))
    .with_secrets(Arc::new(orus_app::AgentSecrets::new()))
    .with_agent(Arc::clone(&agente))
    .with_wake_log(timbre.registro())
    .with_watch_spawner(Arc::clone(&timbre) as Arc<dyn orus_app::WatchSpawner>);
    let mut app = app;
    if let Some(abrir) = abrir {
        app.set_open_url(move |url| abrir(url));
    }
    let app = Arc::new(app);

    timbre.referencias().set_app(Arc::clone(&app));
    timbre.referencias().set_agent(Arc::clone(&agente));

    let mut api = orus_api::Api::with_hub(Arc::clone(&app), hub)
        .with_version(env!("CARGO_PKG_VERSION").to_owned())
        .with_bell_status(timbre);
    if let Some(ruta) = pick_folder.filter(|ruta| !ruta.is_empty()) {
        api = api.with_pick_folder(Arc::new(SelectorCarpeta(ruta)));
    }
    Ok(api)
}

/// El selector que cablea `--pick-folder`; la limpieza la hace el borde,
/// como el `filepath.Clean` de `immune_fake.go:591`.
struct SelectorCarpeta(String);

impl orus_api::PickFolder for SelectorCarpeta {
    fn elegir(&self) -> Result<Option<String>, String> {
        Ok(Some(self.0.clone()))
    }
}

/// El router del binario: `/api/*` del crate y el resto, estáticos.
///
/// `Api::serve` sirve solo su propio router, sin estáticos; por eso el borde
/// compone aquí y llama `axum::serve` con el router fusionado.
pub fn router(api: &orus_api::Api) -> Router {
    api.router().fallback(estatico)
}

/// Sirve hasta que el listener se cierra.
pub async fn servir(
    listener: TcpListener,
    api: orus_api::Api,
    guardia: Guardia,
) -> std::io::Result<()> {
    axum::serve(listener, proteger(router(&api), guardia)).await
}

/// Los archivos del `go:embed` (UI y fuente), con las rutas exactas del Go.
async fn estatico(uri: Uri) -> Response {
    let ruta = uri.path();
    if let Some((_, cuerpo)) = JS_MODULOS.iter().find(|(path, _)| *path == ruta) {
        return archivo(cuerpo.as_bytes(), "text/javascript; charset=utf-8");
    }
    match ruta {
        "/" => archivo(INDEX.as_bytes(), "text/html; charset=utf-8"),
        "/app.css" => archivo(APP_CSS.as_bytes(), "text/css; charset=utf-8"),
        "/logo.svg" => archivo(LOGO_SVG.as_bytes(), "image/svg+xml"),
        "/logo-512.png" => archivo(LOGO_PNG, "image/png"),
        "/fonts/inter.woff2" => archivo(FONT_WOFF2, "font/woff2"),
        // `http.FileServer` redirige `/index.html` a `./` con 301.
        "/index.html" => {
            (StatusCode::MOVED_PERMANENTLY, [(header::LOCATION, "./")]).into_response()
        }
        // `http.FileServer` responde este texto exacto cuando no hay archivo.
        _ => (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            TEXTO_404,
        )
            .into_response(),
    }
}

fn archivo(cuerpo: &'static [u8], tipo: &'static str) -> Response {
    (StatusCode::OK, [(header::CONTENT_TYPE, tipo)], cuerpo).into_response()
}
