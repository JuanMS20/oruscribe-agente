use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("no se pudo escribir la configuración: {0}")]
    Io(#[from] std::io::Error),

    #[error("no se pudo serializar la configuración: {0}")]
    Json(#[from] serde_json::Error),

    #[error("no se pudo crear un temporal único en {0}")]
    TempFile(PathBuf),

    #[error("no se pudo leer {}: {fuente}", ruta.display())]
    ReadSettings {
        ruta: PathBuf,
        #[source]
        fuente: std::io::Error,
    },

    // `editar` no puede partir de los defaults cuando el archivo existe: al
    // guardar, los defaults borrarían lo que no se pudo interpretar.
    #[error(
        "no se puede interpretar {}: {fuente}; no se sobrescribe para no perder los ajustes guardados",
        ruta.display()
    )]
    UnreadableSettings {
        ruta: PathBuf,
        #[source]
        fuente: serde_json::Error,
    },
}
