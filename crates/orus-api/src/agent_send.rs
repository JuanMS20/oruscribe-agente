//! `POST /api/agent/send`: arranca un turno con texto e imágenes y contesta
//! antes de que termine.
//!
//! El orden de los errores es el del contrato: decode, ocupado, imágenes, texto, tope,
//! cerebro de la organización, configuración y arranque.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::State as AxumState;
use axum::http::StatusCode;
use axum::response::Response;
use orus_app::{Agent, AgentError, AgentImage, App, SessionError, normalizar_imagenes};
use serde::Deserialize;

use crate::{ErrorRuta, agent::de_agente, decode_con_limite, write_err, write_json};

/// El texto sobra, las imágenes en.
/// base64 necesitan aire.
const TOPE_ENVIAR: usize = 24 << 20;

#[derive(Deserialize)]
struct CuerpoEnviar {
    #[serde(default)]
    text: String,
    #[serde(default)]
    images: Vec<AgentImage>,
}

pub(crate) async fn enviar(AxumState(app): AxumState<Arc<App>>, body: Body) -> Response {
    let cuerpo: CuerpoEnviar = match decode_con_limite(body, TOPE_ENVIAR).await {
        Ok(cuerpo) => cuerpo,
        Err(error) => return write_err(error),
    };
    let Some(agente) = app.agent() else {
        return write_err(SessionError::Tool("el agente no está disponible".to_owned()).into());
    };
    // El 409 gana a las validaciones del cuerpo y llega antes de pedir red
    // `send` vuelve a mirar bajo su candado.
    if agente.ocupado() {
        return write_err(ErrorRuta::Ocupado(AgentError::Busy.to_string()));
    }
    let imagenes = match normalizar_imagenes(cuerpo.images) {
        Ok(imagenes) => imagenes,
        Err(error) => return write_err(de_agente(error)),
    };
    // `Agent::send` vuelve a validar bajo su candado; la ruta lo comprueba
    // antes de armar el turno. Sin imágenes, el texto
    // vacío sigue siendo un 400.
    let texto = match Agent::validar_mensaje_con_imagenes(&cuerpo.text, !imagenes.is_empty()) {
        Ok(texto) => texto,
        Err(error) => return write_err(de_agente(error)),
    };
    if app.settings_snapshot().agent.kind == "org" && !imagenes.is_empty() {
        return write_err(ErrorRuta::Invalido(
            "el cerebro de la organización todavía no recibe imágenes: cambia a tu propia clave o quita la imagen".to_owned(),
        ));
    }
    let mut turno = match app.turno_para_base(texto).await {
        Ok(turno) => turno,
        Err(error) => return write_err(de_agente(error)),
    };
    turno.imagenes = imagenes;
    // `empezar` reserva el agente antes de responder: si otro envío ganó la
    // carrera, devuelve `Busy` y la ruta lo pinta 409.
    // La corrida sigue aparte: su error queda en `Agent::error()`/`last_error`,
    // no en un 500 tardío.
    let app_org = Arc::clone(&app);
    match agente.empezar_con(texto, turno, move |turno| {
        let app = Arc::clone(&app_org);
        Box::pin(async move { app.preparar_org(turno).await })
    }) {
        Ok(()) => write_json(StatusCode::OK, &serde_json::json!({"ok": true})),
        Err(error) => write_err(de_agente(error)),
    }
}
