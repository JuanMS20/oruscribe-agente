//! API HTTP del cliente: adaptadores finos sobre `orus-app`.
//!
//! `Api` y sus constructores
//! viven en `api.rs`, el registro de rutas en `router.rs`; las rutas de
//! autenticación están en `auth.rs`, el long poll en `events.rs` y la lectura
//! del espacio de trabajo en `workspace.rs`.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod agent;
mod agent_config;
mod agent_models;
mod agent_pick_folder;
mod agent_send;
mod agent_test;
mod agent_voice;
mod api;
mod auth;
mod diagnostics;
pub mod events;
mod notes;
mod org;
mod quit;
mod router;
mod settings;
mod setup;
mod update;
mod uploads;
mod workspace;
mod workspace_write;

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use orus_app::{App, SessionError};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::net::TcpListener;

pub use crate::agent_pick_folder::PickFolder;
pub use crate::api::Api;
pub use crate::quit::Quit;
pub use crate::router::{RUTAS, router};
pub use crate::settings::{Autostart, BellStatus};
pub use crate::setup::Setup;
pub use crate::update::{Fase, Progreso, Update};
pub use crate::workspace_write::GitLog;

const CONTENT_TYPE_JSON: &str = "application/json; charset=utf-8";
const MENSAJE_SERIALIZACION: &str = "{\"error\":\"no se pudo serializar la respuesta\"}\n";
const TOPE_CUERPO: usize = 1 << 20;
const TEXTO_CUERPO_GRANDE: &str = "el cuerpo de la petición pasa del máximo de la ruta";

/// El plazo del long poll.
pub(crate) const ESPERA_EVENTOS: Duration = Duration::from_secs(25);

/// Sirve el router hasta que el listener se cierra.
pub async fn serve(listener: TcpListener, app: Arc<App>) -> std::io::Result<()> {
    axum::serve(listener, router(app)).await
}

/// Lo que `decode` y las rutas convierten en respuesta de error
#[derive(Debug)]
pub(crate) enum ErrorRuta {
    Sesion(SessionError),
    CuerpoGrande,
    ArchivoGrande,
    SinEspacio(String),
    Json(serde_json::Error),
    Lectura(String),
    Invalido(String),
    NoEncontrado(String),
    Ocupado(String),
}

impl From<SessionError> for ErrorRuta {
    fn from(error: SessionError) -> Self {
        match error {
            SessionError::Invalido(texto) => Self::Invalido(texto),
            SessionError::Ocupado(texto) => Self::Ocupado(texto),
            SessionError::SinEspacio(detalle) => Self::SinEspacio(detalle),
            otro => Self::Sesion(otro),
        }
    }
}

/// Deja que los handlers usen `?` y devuelvan la respuesta de error.
impl IntoResponse for ErrorRuta {
    fn into_response(self) -> Response {
        write_err(self)
    }
}

/// Lee hasta el tope más uno para
/// distinguir "justo cabe" de "se pasó" y trata el cuerpo vacío como `{}`.
pub(crate) async fn decode<T: DeserializeOwned>(body: Body) -> Result<T, ErrorRuta> {
    decode_con_limite(body, TOPE_CUERPO).await
}

/// Igual que `decode`, con el tope que declara la ruta.
pub(crate) async fn decode_con_limite<T: DeserializeOwned>(
    body: Body,
    tope: usize,
) -> Result<T, ErrorRuta> {
    let datos = match axum::body::to_bytes(body, tope.saturating_add(1)).await {
        Ok(datos) => datos,
        Err(error) => {
            // `to_bytes` envuelve `LengthLimitError`; su texto es el contrato
            // estable de `http-body-util` para este caso (ver axum 0.8).
            let limite = std::error::Error::source(&error)
                .is_some_and(|fuente| fuente.to_string() == "length limit exceeded");
            return Err(if limite {
                ErrorRuta::CuerpoGrande
            } else {
                ErrorRuta::Lectura(error.to_string())
            });
        }
    };
    if datos.len() > tope {
        return Err(ErrorRuta::CuerpoGrande);
    }
    let entrada: &[u8] = if datos.is_empty() { b"{}" } else { &datos };
    serde_json::from_slice(entrada).map_err(ErrorRuta::Json)
}

/// `LoggedOut` y `Auth` son 401, la
/// cuota agotada 507 y el resto de `SessionError` es 500.
pub(crate) fn write_err(error: ErrorRuta) -> Response {
    // Cualquier camino que traiga la cuota agotada responde 507.
    let error = match error {
        ErrorRuta::Sesion(SessionError::SinEspacio(detalle)) => ErrorRuta::SinEspacio(detalle),
        otro => otro,
    };
    match error {
        ErrorRuta::Sesion(error) => {
            let status = match &error {
                SessionError::LoggedOut | SessionError::Auth => StatusCode::UNAUTHORIZED,
                SessionError::RateLimited(_) => StatusCode::TOO_MANY_REQUESTS,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            write_json(status, &serde_json::json!({"error": error.to_string()}))
        }
        ErrorRuta::CuerpoGrande => write_json(
            StatusCode::PAYLOAD_TOO_LARGE,
            &serde_json::json!({"error": TEXTO_CUERPO_GRANDE, "code": "cuerpo_grande"}),
        ),
        ErrorRuta::ArchivoGrande => write_json(
            StatusCode::BAD_REQUEST,
            &serde_json::json!({"error": "el archivo supera los 25 MB", "code": "archivo_grande"}),
        ),
        ErrorRuta::SinEspacio(detalle) => {
            // El Display de `SessionError` es el texto completo del contrato
            // No se duplica aquí.
            let texto = SessionError::SinEspacio(detalle).to_string();
            write_json(
                StatusCode::INSUFFICIENT_STORAGE,
                &serde_json::json!({"error": texto, "code": "sin_espacio"}),
            )
        }
        // El texto del error es el de serde.
        ErrorRuta::Json(error) => write_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            &serde_json::json!({"error": error.to_string()}),
        ),
        ErrorRuta::Lectura(error) => write_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            &serde_json::json!({"error": error}),
        ),
        ErrorRuta::Invalido(error) => write_json(
            StatusCode::BAD_REQUEST,
            &serde_json::json!({"error": error}),
        ),
        ErrorRuta::NoEncontrado(error) => {
            write_json(StatusCode::NOT_FOUND, &serde_json::json!({"error": error}))
        }
        ErrorRuta::Ocupado(error) => {
            write_json(StatusCode::CONFLICT, &serde_json::json!({"error": error}))
        }
    }
}

/// Un fallo de una acción inyectada (instalador o update): como no es de sesión, responde 500 con el texto de la
/// implementación.
pub(crate) fn error_de_accion(texto: String) -> Response {
    write_json(
        StatusCode::INTERNAL_SERVER_ERROR,
        &serde_json::json!({"error": texto}),
    )
}

/// Serializa antes de escribir, y el
/// cuerpo lleva el `\n` que el cliente espera.
pub(crate) fn write_json<T: Serialize>(status: StatusCode, value: &T) -> Response {
    match serde_json::to_vec(value) {
        Ok(mut body) => {
            body.push(b'\n');
            let mut response = Response::new(Body::from(body));
            *response.status_mut() = status;
            cabeceras_json(response.headers_mut());
            response
        }
        Err(_) => error_de_serializacion(),
    }
}

/// La rama de error de la serialización: solo se fija `Content-Type`.
fn error_de_serializacion() -> Response {
    let mut response = Response::new(Body::from(MENSAJE_SERIALIZACION));
    *response.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(CONTENT_TYPE_JSON),
    );
    response
}

fn cabeceras_json(headers: &mut HeaderMap) {
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(CONTENT_TYPE_JSON),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
}

#[cfg(test)]
mod tests {
    use serde::ser::Error as _;

    use super::*;

    #[derive(Serialize)]
    struct Ejemplo {
        ok: bool,
    }

    struct Roto;

    impl Serialize for Roto {
        fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
            Err(S::Error::custom("no serializable"))
        }
    }

    async fn cuerpo(response: Response) -> Vec<u8> {
        axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap_or_default()
            .to_vec()
    }

    #[tokio::test]
    async fn write_json_agrega_salto_y_cabeceras() {
        let response = write_json(StatusCode::OK, &Ejemplo { ok: true });

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|valor| valor.to_str().ok()),
            Some(CONTENT_TYPE_JSON)
        );
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|valor| valor.to_str().ok()),
            Some("no-store")
        );
        let body = cuerpo(response).await;
        let texto = String::from_utf8_lossy(&body);
        assert!(
            texto.ends_with('\n'),
            "el cuerpo no termina en \\n: {texto:?}"
        );
        let value: serde_json::Value =
            serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
        assert_eq!(value["ok"], true, "el cuerpo no es JSON válido: {texto:?}");
    }

    #[tokio::test]
    async fn write_json_serializa_antes_de_escribir() {
        let response = write_json(StatusCode::OK, &Roto);

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|valor| valor.to_str().ok()),
            Some(CONTENT_TYPE_JSON)
        );
        assert_eq!(
            String::from_utf8_lossy(&cuerpo(response).await),
            MENSAJE_SERIALIZACION
        );
    }

    #[tokio::test]
    async fn write_err_manda_401_cuando_el_servidor_no_autoriza() {
        let response = write_err(SessionError::Auth.into());

        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "una sesión no autorizada debe mandar al login, no parecer un fallo del servidor"
        );
        let texto = String::from_utf8_lossy(&cuerpo(response).await).to_string();
        assert!(
            texto.contains("autorizada"),
            "el cuerpo no explica que la sesión no está autorizada: {texto:?}"
        );
    }

    #[tokio::test]
    async fn decode_con_limite_rechaza_el_cuerpo_que_pasa_el_tope() {
        let body = Body::from("12345");

        let resultado = decode_con_limite::<serde_json::Value>(body, 4).await;

        assert!(
            matches!(resultado, Err(ErrorRuta::CuerpoGrande)),
            "un cuerpo de 5 bytes con tope 4 no dio CuerpoGrande"
        );
    }
}
