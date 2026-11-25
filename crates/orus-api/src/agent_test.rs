//! `POST /api/agent/test`: prueba el proveedor sin guardar nada.
//!
//! Port de `internal/app/http.go:852-867` y `agent.go:397-481`. El cuerpo se
//! decodifica con su tope; la sonda y sus validaciones viven en `orus-app`.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::State as AxumState;
use axum::http::StatusCode;
use axum::response::Response;
use orus_app::{AgentInput, App};

use crate::{agent::de_agente, decode_con_limite, write_err, write_json};

const TOPE_SONDA: usize = 64 << 10;

pub(crate) async fn probar(AxumState(app): AxumState<Arc<App>>, body: Body) -> Response {
    let entrada: AgentInput = match decode_con_limite(body, TOPE_SONDA).await {
        Ok(entrada) => entrada,
        Err(error) => return write_err(error),
    };
    match app.agent_test(entrada).await {
        Ok(sonda) => write_json(StatusCode::OK, &sonda),
        Err(error) => write_err(de_agente(error)),
    }
}
