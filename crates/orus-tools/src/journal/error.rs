use std::path::PathBuf;

use thiserror::Error;

/// Errores del diario. Los textos son parte del contrato: la ventana y el CLI
/// los muestran tal cual.
#[derive(Debug, Error)]
pub enum Error {
    #[error("el diario necesita una carpeta")]
    EmptyDir,

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error("no se pudo serializar la entrada del diario: {0}")]
    Json(#[from] serde_json::Error),

    #[error("no se pudo respaldar {}: {source}", .path.display())]
    Backup {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("no hay nada que deshacer")]
    NothingToUndo,

    #[error("la entrada pendiente {0} ya no está en el diario")]
    PendingMissing(String),

    #[error("{}: {message}", .path.display())]
    Guard { path: PathBuf, message: String },

    #[error(
        "{} cambió después del agente; usa forzar para restaurarlo de todos modos",
        .path.display()
    )]
    Changed { path: PathBuf },

    #[error("{} ya no existe; no se puede restaurar", .path.display())]
    Missing { path: PathBuf },

    #[error("no se encontró el respaldo de {}: {source}", .path.display())]
    BackupMissing {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error(
        "el respaldo de {} no coincide con lo que había antes de la escritura; no se restaura",
        .path.display()
    )]
    BackupCorrupt { path: PathBuf },

    #[error("no se pudo generar un identificador para el diario: {0}")]
    Entropy(#[from] rand::Error),

    #[error("{undo}\n{consume}")]
    UndoAndConsume {
        undo: Box<Error>,
        consume: Box<Error>,
    },
}
