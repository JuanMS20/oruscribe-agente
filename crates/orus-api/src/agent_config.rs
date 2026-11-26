//! `POST /api/agent/config`: valida, guarda y devuelve la vista.
//!
//! El cuerpo se
//! decodifica con su tope antes de reservar el agente; la reserva y la
//! validación viven en `orus-app`.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::State as AxumState;
use axum::http::StatusCode;
use axum::response::Response;
use orus_app::{AgentInput, App};

use crate::{agent::de_agente, decode_con_limite, write_err, write_json};

const TOPE_CONFIG: usize = 64 << 10;

pub(crate) async fn configurar(AxumState(app): AxumState<Arc<App>>, body: Body) -> Response {
    let entrada: AgentInput = match decode_con_limite(body, TOPE_CONFIG).await {
        Ok(entrada) => entrada,
        Err(error) => return write_err(error),
    };
    match app.agent_configure(entrada) {
        Ok(vista) => write_json(StatusCode::OK, &vista),
        Err(error) => write_err(de_agente(error)),
    }
}
