use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("json inválido: {0}")]
    Json(#[from] serde_json::Error),
}
