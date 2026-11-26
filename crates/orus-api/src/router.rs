//! Registro de las rutas HTTP: `RUTAS`, `router`, `router_con` y `state`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Extension;
use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::extract::State as AxumState;
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::{any, get, post};
use orus_app::App;

use crate::agent_models::{CacheModelos, URL_CATALOGO};
use crate::agent_pick_folder::PickFolder;
use crate::events::{self, Hub};
use crate::quit::{self, Quit};
use crate::settings::{self, SinAutostart, SinTimbre};
use crate::setup::{self, Setup};
use crate::update::{self, Update};
use crate::workspace_write::{self, GitLogReal};
use crate::write_json;
use crate::{Autostart, BellStatus, ESPERA_EVENTOS, GitLog};
use crate::{
    agent, agent_config, agent_models, agent_pick_folder, agent_send, agent_test, agent_voice,
    auth, diagnostics, notes, org, uploads, workspace,
};

// Una sola tabla apunta la ruta, los métodos que el router registra y lo que
// `RUTAS` exporta: no se puede agregar una ruta sin anotarla aquí. Deliberadamente
// no es un build script: `RUTAS` sigue siendo datos estáticos para el gate de
// contrato.
macro_rules! registrar_rutas {
    ($(($ruta:literal, $($metodo:ident),+ => $handler:expr)),* $(,)?) => {
        /// Las 72 rutas en alcance que el router registra. Sale de la misma
        /// tabla que `router_con`.
        pub static RUTAS: &[(&str, &str)] = &[ $($((stringify!($metodo), $ruta)),*),* ];

        fn registrar_rutas(router: axum::Router<Arc<App>>) -> axum::Router<Arc<App>> {
            let mut router = router;
            $(router = router.route($ruta, $handler);)*
            router
        }
    };
}

registrar_rutas! {
    ("/api/state", GET => get(state)),
    ("/api/auth/state", GET => get(state)),
    ("/api/auth/start", POST => post(auth::start)),
    ("/api/auth/import", POST => post(auth::import)),
    ("/api/auth/logout", POST => post(auth::logout)),
    ("/api/org/seleccionar", POST => post(org::seleccionar)),
    ("/api/events", GET => get(events::handle_events)),
    // El contrato registraba sin método; el handler decide y contesta
    // 405 para el resto.
    ("/api/notes", GET, POST, DELETE => any(notes::notas)),
    ("/api/diagnostics", GET => get(diagnostics::diagnostico)),
    ("/api/diagnostics/report", POST => post(diagnostics::reportar)),
    ("/api/settings", GET, POST => get(settings::leer).post(settings::cambiar)),
    ("/api/setup", GET => get(setup::leer)),
    ("/api/setup/install", POST => post(setup::instalar)),
    ("/api/setup/uninstall", POST => post(setup::desinstalar)),
    ("/api/update", GET => get(update::leer)),
    ("/api/update/apply", POST => post(update::aplicar)),
    ("/api/update/progress", GET => get(update::progreso)),
    ("/api/tasks", GET, POST => get(workspace::tareas).post(workspace_write::crear_tarea)),
    ("/api/tasks/search", GET => get(workspace::buscar_tareas)),
    ("/api/tasks/files", GET, POST => get(workspace::archivos_de_tarea)
        .post(uploads::tarea)
        .layer(DefaultBodyLimit::disable())),
    ("/api/tasks/files/comment", POST => post(workspace_write::comentar_avance)),
    ("/api/tasks/status", POST => post(workspace_write::cambiar_estado)),
    ("/api/rooms", GET => get(workspace::salas)),
    ("/api/rooms/read", POST => post(workspace_write::marcar_sala)),
    ("/api/messages", GET, POST => get(workspace::mensajes).post(workspace_write::enviar_mensaje)),
    ("/api/chat/files", POST => post(uploads::chat).layer(DefaultBodyLimit::disable())),
    ("/api/alerts", GET => get(workspace::alertas)),
    ("/api/alerts/read", POST => post(workspace_write::marcar_alertas)),
    ("/api/mentions", GET => get(workspace::menciones)),
    ("/api/mentions/read", POST => post(workspace_write::marcar_menciones)),
    ("/api/goals", GET, POST => get(workspace::metas).post(workspace_write::crear_meta)),
    ("/api/goals/detail", GET => get(workspace::detalle_de_meta)),
    ("/api/periods", GET, POST => get(workspace::periodos).post(workspace_write::crear_periodo)),
    ("/api/capture/propose", POST => post(workspace_write::proponer_captura)),
    ("/api/capture/apply", POST => post(workspace_write::aplicar_captura)),
    ("/api/alerts/send", POST => post(workspace_write::enviar_aviso)),
    ("/api/messages/react", POST => post(workspace_write::reaccionar)),
    ("/api/context", POST => post(workspace_write::actualizar_contexto)),
    ("/api/activity", GET => get(workspace::actividad)),
    ("/api/decisions", GET => get(workspace::decisiones)),
    ("/api/team", GET => get(workspace::equipos)),
    ("/api/agent", GET => get(agent::estado)),
    ("/api/agent/config", POST => post(agent_config::configurar)),
    ("/api/agent/test", POST => post(agent_test::probar)),
    ("/api/agent/transcribe", POST => post(agent_voice::transcribir_ruta)
        .layer(DefaultBodyLimit::max(agent_voice::TOPE_AUDIO))),
    ("/api/agent/speak", POST => post(agent_voice::hablar_ruta)),
    ("/api/agent/providers", GET => get(agent::proveedores)),
    ("/api/agent/models", GET => get(agent_models::modelos)),
    ("/api/agent/pick-folder", POST => post(agent_pick_folder::seleccionar)),
    ("/api/agent/journal", GET => get(agent::diario)),
    ("/api/agent/undo", POST => post(agent::deshacer)),
    ("/api/agent/send", POST => post(agent_send::enviar)),
    ("/api/agent/rules", GET => get(agent::reglas)),
    ("/api/agent/rules/revoke", POST => post(agent::revocar)),
    ("/api/agent/cancel", POST => post(agent::cancelar)),
    ("/api/agent/approvals", GET => get(agent::aprobaciones)),
    ("/api/agent/approve", POST => post(agent::aprobar)),
    ("/api/agent/reset", POST => post(agent::reiniciar)),
    ("/api/agent/conversations", GET => get(agent::conversaciones)),
    ("/api/agent/conversations/open", POST => post(agent::abrir_conversacion)),
    ("/api/agent/conversations/delete", POST => post(agent::borrar_conversacion)),
    ("/api/quit", POST => post(quit::salir)),
}

/// El router con un hub propio.
pub fn router(app: Arc<App>) -> Router {
    router_con(
        app,
        Arc::new(Hub::new()),
        ESPERA_EVENTOS,
        Arc::new(GitLogReal),
        String::new(),
        Arc::new(SinTimbre),
        Arc::new(SinAutostart),
        URL_CATALOGO.to_owned(),
        reqwest::Client::new(),
        Arc::new(Mutex::new(CacheModelos::default())),
        None,
        Arc::new(setup::SinInstalador),
        Arc::new(update::SinActualizador),
        Arc::new(quit::SinQuit),
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn router_con(
    app: Arc<App>,
    hub: Arc<Hub>,
    wait: Duration,
    git_log: Arc<dyn GitLog>,
    version: String,
    bell_status: Arc<dyn BellStatus>,
    autostart: Arc<dyn Autostart>,
    models_url: String,
    http: reqwest::Client,
    modelos: Arc<Mutex<CacheModelos>>,
    pick_folder: Option<Arc<dyn PickFolder>>,
    instalador: Arc<dyn Setup>,
    actualizador: Arc<dyn Update>,
    salida: Arc<dyn Quit>,
) -> Router {
    registrar_rutas(Router::new())
        .layer(Extension(hub))
        .layer(Extension(wait))
        .layer(Extension(git_log))
        .layer(Extension(version))
        .layer(Extension(bell_status))
        .layer(Extension(autostart))
        .layer(Extension(agent_models::UrlCatalogo(models_url)))
        .layer(Extension(http))
        .layer(Extension(modelos))
        .layer(Extension(pick_folder))
        .layer(Extension(instalador))
        .layer(Extension(actualizador))
        .layer(Extension(salida))
        .layer(axum::middleware::from_fn_with_state(
            Arc::clone(&app),
            diagnostics::vigilar,
        ))
        .with_state(app)
}

async fn state(AxumState(app): AxumState<Arc<App>>) -> Response {
    write_json(StatusCode::OK, &app.state())
}
