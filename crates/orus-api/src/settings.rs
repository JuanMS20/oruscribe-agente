//! Ajustes del panel: leer y cambiar.
//!
//! Todo es local: no hay red ni llavero. La versión y el timbre llegan por
//! `Extension`, igual que `GitLog` (D182).

use std::sync::Arc;

use axum::Extension;
use axum::body::Body;
use axum::extract::State as AxumState;
use axum::http::StatusCode;
use axum::response::Response;
use orus_app::{App, SessionError};
use orus_config::{Locale, Settings};
use serde::{Deserialize, Serialize};

use crate::{ErrorRuta, decode, write_err, write_json};

#[derive(Serialize)]
struct McpQuotaView {
    #[serde(rename = "usedMinute")]
    used_minute: usize,
    #[serde(rename = "limitMinute")]
    limit_minute: usize,
    #[serde(rename = "usedHour")]
    used_hour: usize,
    #[serde(rename = "limitHour")]
    limit_hour: usize,
}

/// Lo que muestra el panel.
#[derive(Serialize)]
pub struct Info {
    version: String,
    #[serde(rename = "dataDir")]
    data_dir: String,
    endpoint: String,
    locale: Locale,
    autostart: bool,
    #[serde(rename = "autostartAsked")]
    autostart_asked: bool,
    #[serde(rename = "autoUpdate")]
    auto_update: bool,
    telemetry: bool,
    doorbell: bool,
    #[serde(rename = "bellError", skip_serializing_if = "String::is_empty")]
    bell_error: String,
    #[serde(rename = "mcpQuota", skip_serializing_if = "Option::is_none")]
    mcp_quota: Option<McpQuotaView>,
    platform: String,
}

/// El estado del timbre que inyecta el borde desde el `Watch` real
/// Acá no entra `orus-org` (D161).
pub trait BellStatus: Send + Sync {
    fn doorbell(&self) -> bool;
    fn error(&self) -> String;
}

/// El timbre de un borde que todavía no lo cableó: apagado y sin error.
pub(crate) struct SinTimbre;

impl BellStatus for SinTimbre {
    fn doorbell(&self) -> bool {
        false
    }

    fn error(&self) -> String {
        String::new()
    }
}

/// El arranque con el sistema. El borde del escritorio lo cablea al registro de
/// Windows; sin él, el panel conserva el aviso de que este equipo no puede.
pub trait Autostart: Send + Sync {
    fn activo(&self) -> bool;
    fn cambiar(&self, encendido: bool) -> Result<(), String>;
}

/// Un borde sin arranque automático (servidor sin ventana o Linux sin cablear).
pub(crate) struct SinAutostart;

impl Autostart for SinAutostart {
    fn activo(&self) -> bool {
        false
    }

    fn cambiar(&self, _encendido: bool) -> Result<(), String> {
        Err("este equipo no puede arrancar la app solo".to_owned())
    }
}

/// El cuerpo del `POST`. Los punteros del contrato significan "ausente = no tocar"
/// `locale` es texto y vacío tampoco cambia nada.
#[derive(Deserialize, Default)]
#[serde(default)]
struct CuerpoAjustes {
    // `Option` también acepta `null`, que deja el string vacío.
    locale: Option<String>,
    autostart: Option<bool>,
    #[serde(rename = "autostartAsked")]
    autostart_asked: Option<bool>,
    #[serde(rename = "autoUpdate")]
    auto_update: Option<bool>,
    telemetry: Option<bool>,
}

pub(crate) async fn leer(
    AxumState(app): AxumState<Arc<App>>,
    Extension(version): Extension<String>,
    Extension(timbre): Extension<Arc<dyn BellStatus>>,
    Extension(autostart): Extension<Arc<dyn Autostart>>,
) -> Response {
    let settings = app.settings_snapshot();
    write_json(
        StatusCode::OK,
        &info(
            &app,
            &settings,
            &version,
            timbre.as_ref(),
            autostart.as_ref(),
        ),
    )
}

pub(crate) async fn cambiar(
    AxumState(app): AxumState<Arc<App>>,
    Extension(version): Extension<String>,
    Extension(timbre): Extension<Arc<dyn BellStatus>>,
    Extension(autostart): Extension<Arc<dyn Autostart>>,
    body: Body,
) -> Response {
    let cuerpo: CuerpoAjustes = match decode(body).await {
        Ok(cuerpo) => cuerpo,
        Err(error) => return write_err(error),
    };
    let locale = cuerpo.locale.as_deref().filter(|valor| !valor.is_empty());
    // Guardar primero: si el arranque falla, ajustes y sistema no quedan
    // desincronizados después de reiniciar.
    let guardados = match app.change_settings(
        locale,
        cuerpo.autostart_asked,
        cuerpo.auto_update,
        cuerpo.telemetry,
    ) {
        Ok(settings) => settings,
        Err(error) => return write_err(ErrorRuta::Sesion(error)),
    };
    if let Some(encendido) = cuerpo.autostart
        && let Err(error) = autostart.cambiar(encendido)
    {
        return write_err(ErrorRuta::Sesion(SessionError::Tool(error)));
    }
    write_json(
        StatusCode::OK,
        &info(
            &app,
            &guardados,
            &version,
            timbre.as_ref(),
            autostart.as_ref(),
        ),
    )
}

fn info(
    app: &App,
    settings: &Settings,
    version: &str,
    timbre: &dyn BellStatus,
    autostart: &dyn Autostart,
) -> Info {
    Info {
        version: version.to_owned(),
        data_dir: orus_config::data_dir().display().to_string(),
        endpoint: app.endpoint_for(settings),
        locale: settings.locale,
        autostart: autostart.activo(),
        autostart_asked: settings.autostart_asked,
        auto_update: settings.auto_update_on(),
        telemetry: settings.telemetry_on(),
        doorbell: timbre.doorbell(),
        bell_error: timbre.error(),
        mcp_quota: app.mcp_quota().map(|q| McpQuotaView {
            used_minute: q.used_minute,
            limit_minute: q.limit_minute,
            used_hour: q.used_hour,
            limit_hour: q.limit_hour,
        }),
        platform: std::env::consts::OS.to_owned(),
    }
}
