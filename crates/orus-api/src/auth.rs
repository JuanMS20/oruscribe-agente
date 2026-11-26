//! Rutas de autenticación.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::State as AxumState;
use axum::http::StatusCode;
use axum::response::Response;
use orus_app::{App, SessionError};
use serde::Deserialize;

use crate::{ErrorRuta, decode, write_err, write_json};

#[derive(Deserialize)]
struct ImportBody {
    #[serde(default)]
    token: String,
}

/// `POST /api/auth/start`: no lee cuerpo. El login bloquea
/// esperando el descubrimiento, así que va a un hilo aparte.
pub(crate) async fn start(AxumState(app): AxumState<Arc<App>>) -> Response {
    match tokio::task::spawn_blocking(move || app.start_login()).await {
        Ok(Ok(url)) => write_json(StatusCode::OK, &serde_json::json!({"url": url})),
        Ok(Err(error)) => write_err(error.into()),
        Err(error) => write_err(SessionError::Connect(error.to_string()).into()),
    }
}

/// `POST /api/auth/import`: cuerpo `{token}` y responde el `State`.
pub(crate) async fn import(AxumState(app): AxumState<Arc<App>>, body: Body) -> Response {
    let cuerpo: ImportBody = match decode(body).await {
        Ok(cuerpo) => cuerpo,
        Err(error) => return write_err(error),
    };
    // Un token vacío es entrada inválida del panel, no un fallo de conexión.
    if cuerpo.token.trim().is_empty() {
        return write_err(ErrorRuta::Invalido("el token está vacío".to_owned()));
    }
    match app.import_token(&cuerpo.token).await {
        Ok(()) => write_json(StatusCode::OK, &app.state()),
        Err(error) => write_err(error.into()),
    }
}

/// `POST /api/auth/logout`.
pub(crate) async fn logout(AxumState(app): AxumState<Arc<App>>) -> Response {
    match app.logout() {
        Ok(()) => write_json(StatusCode::OK, &serde_json::json!({"ok": true})),
        Err(error) => write_err(error.into()),
    }
}
