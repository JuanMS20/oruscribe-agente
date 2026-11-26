//! Rutas del agente: vista, diario, reglas, aprobaciones y conversaciones.
//!
//! Las 12 rutas no configuran ni
//! envían: solo leen el estado y contestan lo que la ventana ya tiene. Los
//! topes y el formato de error son los del borde (O6-02).

use std::sync::Arc;

use axum::body::Body;
use axum::extract::State as AxumState;
use axum::http::StatusCode;
use axum::response::Response;
use orus_app::{AgentError, App, ProviderPreset, SessionError, presets_de_proveedores};
use serde::Deserialize;

use crate::{ErrorRuta, decode_con_limite, write_err, write_json};

const TOPE_REVOCAR: usize = 1 << 10;
const TOPE_APROBAR: usize = 4 << 10;
const TOPE_CONVERSACION: usize = 4 << 10;
const TOPE_UNDO: usize = 1 << 10;

fn sin_agente() -> Response {
    write_err(SessionError::Tool("el agente no está disponible".to_owned()).into())
}

/// El código HTTP sale del error, no de la ruta
/// `Busy` es 409, `Invalid` 400 y una sesión conserva
/// el código de `From<SessionError>` (401 sin login, 507 sin espacio).
pub(crate) fn de_agente(error: AgentError) -> ErrorRuta {
    match error {
        AgentError::Busy => ErrorRuta::Ocupado(AgentError::Busy.to_string()),
        AgentError::Invalid(texto) => ErrorRuta::Invalido(texto),
        AgentError::Sesion(error) => error.into(),
        otro => SessionError::Tool(otro.to_string()).into(),
    }
}

#[derive(Deserialize)]
struct CuerpoId {
    #[serde(default)]
    id: String,
}

#[derive(Deserialize)]
struct CuerpoAprobar {
    #[serde(default)]
    id: String,
    /// Es `*bool`: omitido y `null` valen `false`.
    #[serde(default)]
    approve: Option<bool>,
    #[serde(default)]
    remember: bool,
}

#[derive(Deserialize)]
struct CuerpoConversacion {
    #[serde(default)]
    id: String,
    #[serde(default)]
    all: bool,
}

#[derive(Deserialize)]
struct CuerpoUndo {
    #[serde(default)]
    n: i64,
    #[serde(default)]
    force: bool,
}

pub(crate) async fn estado(AxumState(app): AxumState<Arc<App>>) -> Response {
    match app.agent_view() {
        Ok(vista) => write_json(StatusCode::OK, &vista),
        Err(error) => write_err(error.into()),
    }
}

pub(crate) async fn proveedores() -> Response {
    let proveedores: Vec<ProviderPreset> = presets_de_proveedores();
    write_json(StatusCode::OK, &proveedores)
}

pub(crate) async fn diario(AxumState(app): AxumState<Arc<App>>) -> Response {
    match app.journal_entries() {
        Ok(entradas) => write_json(StatusCode::OK, &serde_json::json!({"entries": entradas})),
        Err(error) => write_err(error.into()),
    }
}

pub(crate) async fn deshacer(AxumState(app): AxumState<Arc<App>>, body: Body) -> Response {
    let cuerpo: CuerpoUndo = match decode_con_limite(body, TOPE_UNDO).await {
        Ok(cuerpo) => cuerpo,
        Err(error) => return write_err(error),
    };
    if cuerpo.n < 1 {
        return write_err(ErrorRuta::Invalido(
            "falta el número de cambios a deshacer".to_owned(),
        ));
    }
    let (n, force) = (cuerpo.n, cuerpo.force);
    let resultado = match tokio::task::spawn_blocking(move || app.agent_undo(n, force)).await {
        Ok(resultado) => resultado,
        Err(error) => {
            return write_err(ErrorRuta::Lectura(format!(
                "el deshacer se interrumpió: {error}"
            )));
        }
    };
    match resultado {
        Ok(hechas) => write_json(StatusCode::OK, &serde_json::json!({"undone": hechas})),
        Err(error) => write_err(error.into()),
    }
}

pub(crate) async fn reglas(AxumState(app): AxumState<Arc<App>>) -> Response {
    let reglas = app.rules();
    // Sin reglas la clave es null.
    let rules = (!reglas.is_empty()).then_some(reglas);
    write_json(StatusCode::OK, &serde_json::json!({"rules": rules}))
}

pub(crate) async fn revocar(AxumState(app): AxumState<Arc<App>>, body: Body) -> Response {
    let cuerpo: CuerpoId = match decode_con_limite(body, TOPE_REVOCAR).await {
        Ok(cuerpo) => cuerpo,
        Err(error) => return write_err(error),
    };
    let id = cuerpo.id.trim();
    if id.is_empty() {
        return write_err(ErrorRuta::Invalido("falta el id de la regla".to_owned()));
    }
    match app.revoke_rule(id) {
        Ok(true) => write_json(StatusCode::OK, &serde_json::json!({"ok": true})),
        Ok(false) => write_err(ErrorRuta::NoEncontrado("no existe la regla".to_owned())),
        Err(error) => write_err(error.into()),
    }
}

pub(crate) async fn cancelar(AxumState(app): AxumState<Arc<App>>) -> Response {
    if let Some(agente) = app.agent() {
        agente.cancel();
    }
    write_json(StatusCode::OK, &serde_json::json!({"ok": true}))
}

pub(crate) async fn aprobaciones(AxumState(app): AxumState<Arc<App>>) -> Response {
    let Some(agente) = app.agent() else {
        return sin_agente();
    };
    write_json(StatusCode::OK, &agente.approvals().pending())
}

pub(crate) async fn aprobar(AxumState(app): AxumState<Arc<App>>, body: Body) -> Response {
    let cuerpo: CuerpoAprobar = match decode_con_limite(body, TOPE_APROBAR).await {
        Ok(cuerpo) => cuerpo,
        Err(error) => return write_err(error),
    };
    if cuerpo.id.is_empty() {
        return write_err(ErrorRuta::Invalido(
            "falta el id de la aprobación".to_owned(),
        ));
    }
    let Some(agente) = app.agent() else {
        return sin_agente();
    };
    if let Err(error) =
        agente
            .approvals()
            .answer(&cuerpo.id, cuerpo.approve.unwrap_or(false), cuerpo.remember)
    {
        return write_err(SessionError::Tool(error.to_string()).into());
    }
    write_json(StatusCode::OK, &agente.approvals().pending())
}

pub(crate) async fn reiniciar(AxumState(app): AxumState<Arc<App>>) -> Response {
    let Some(agente) = app.agent() else {
        return sin_agente();
    };
    match agente.reset() {
        Ok(()) => write_json(StatusCode::OK, &serde_json::json!({"ok": true})),
        Err(error) => write_err(de_agente(error)),
    }
}

pub(crate) async fn conversaciones(AxumState(app): AxumState<Arc<App>>) -> Response {
    let Some(conversaciones) = app.conversations() else {
        return write_err(
            SessionError::Tool("las conversaciones no están disponibles".to_owned()).into(),
        );
    };
    match conversaciones.list() {
        Ok(lista) => write_json(StatusCode::OK, &lista),
        Err(error) => write_err(SessionError::Tool(error.to_string()).into()),
    }
}

pub(crate) async fn abrir_conversacion(
    AxumState(app): AxumState<Arc<App>>,
    body: Body,
) -> Response {
    let cuerpo: CuerpoId = match decode_con_limite(body, TOPE_CONVERSACION).await {
        Ok(cuerpo) => cuerpo,
        Err(error) => return write_err(error),
    };
    let Some(agente) = app.agent() else {
        return sin_agente();
    };
    if let Err(error) = agente.open_conversation(&cuerpo.id) {
        return write_err(de_agente(error));
    }
    match app.agent_view() {
        Ok(vista) => write_json(StatusCode::OK, &vista),
        Err(error) => write_err(error.into()),
    }
}

pub(crate) async fn borrar_conversacion(
    AxumState(app): AxumState<Arc<App>>,
    body: Body,
) -> Response {
    let cuerpo: CuerpoConversacion = match decode_con_limite(body, TOPE_CONVERSACION).await {
        Ok(cuerpo) => cuerpo,
        Err(error) => return write_err(error),
    };
    if !cuerpo.all && cuerpo.id.trim().is_empty() {
        return write_err(ErrorRuta::Invalido(
            "falta la conversación o la marca all".to_owned(),
        ));
    }
    let Some(agente) = app.agent() else {
        return sin_agente();
    };
    let id = if cuerpo.all { "" } else { cuerpo.id.as_str() };
    if let Err(error) = agente.delete_conversation(id) {
        return write_err(de_agente(error));
    }
    match app.agent_view() {
        Ok(vista) => write_json(StatusCode::OK, &vista),
        Err(error) => write_err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use orus_app::{AgentError, SessionError};

    use super::de_agente;
    use crate::write_err;

    /// El código HTTP sale del error, no de la ruta. Una sesión conserva `LoggedOut` 401, `Invalido`
    /// 400, `Ocupado` 409 y `SinEspacio` 507; lo demás es 500.
    #[test]
    fn sesion_conserva_el_codigo_de_write_err() {
        let casos = [
            (SessionError::LoggedOut, StatusCode::UNAUTHORIZED),
            (
                SessionError::Invalido("dato inválido".to_owned()),
                StatusCode::BAD_REQUEST,
            ),
            (
                SessionError::Ocupado("ocupado".to_owned()),
                StatusCode::CONFLICT,
            ),
            (
                SessionError::SinEspacio("sin cupo".to_owned()),
                StatusCode::INSUFFICIENT_STORAGE,
            ),
            (
                SessionError::Tool("falló la herramienta".to_owned()),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
        ];
        for (sesion, esperado) in casos {
            let descripcion = sesion.to_string();
            let obtenido = write_err(de_agente(AgentError::Sesion(sesion))).status();
            assert_eq!(
                obtenido, esperado,
                "Sesion({descripcion:?}) tiene que responder {esperado}, no {obtenido}"
            );
        }
    }
}
