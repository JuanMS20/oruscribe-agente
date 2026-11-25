//! Selección de organización por conexión (#117).
//!
//! El slug viaja como `x-org-slug` en el cliente MCP que abre `orus-app`: esta
//! ruta nunca usa la herramienta global de cambio de organización, que
//! afectaría a todas las conexiones de la cuenta
//! (`docs/prompt-paridad-android.md:127`).

use std::sync::Arc;

use axum::body::Body;
use axum::extract::State as AxumState;
use axum::http::StatusCode;
use axum::response::Response;
use orus_app::App;
use serde::Deserialize;

use crate::{ErrorRuta, decode, write_err, write_json};

#[derive(Deserialize, Default)]
#[serde(default)]
struct CuerpoOrg {
    slug: String,
}

pub(crate) async fn seleccionar(AxumState(app): AxumState<Arc<App>>, body: Body) -> Response {
    let cuerpo: CuerpoOrg = match decode(body).await {
        Ok(cuerpo) => cuerpo,
        Err(error) => return write_err(error),
    };
    if cuerpo.slug.trim().is_empty() {
        return write_err(ErrorRuta::Invalido(
            "falta el slug de la organización".to_owned(),
        ));
    }
    match app.seleccionar_org(&cuerpo.slug).await {
        Ok(me) => write_json(StatusCode::OK, &serde_json::json!({"ok": true, "me": me})),
        Err(error) => write_err(ErrorRuta::from(error)),
    }
}
