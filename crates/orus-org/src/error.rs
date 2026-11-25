//! Errores del cliente de la organización.

use rmcp::service::{ClientInitializeError, ServiceError};
use thiserror::Error;

/// Error de `orus-org`.
///
/// Los textos conservan los del Go: los lee una persona y algunos tests los
/// comparan.
#[derive(Debug, Error)]
pub enum OrgError {
    #[error("configuración MCP incompleta")]
    IncompleteConfig,

    #[error("endpoint MCP inválido: {0}")]
    InvalidEndpoint(String),

    #[error("endpoint MCP inseguro, se exige https salvo localhost: {0}")]
    InsecureEndpoint(String),

    /// La respuesta JSON-RPC no llegó antes del plazo total de la llamada.
    #[error("{tool}: la llamada MCP no respondió en {plazo} s")]
    Timeout { tool: String, plazo: u64 },

    /// Un servidor agotó un tope de seguridad de paginación del cliente.
    #[error("límite de seguridad MCP: {0}")]
    LimitExceeded(String),

    /// Una operación de escritura no puede reintentarse sin riesgo de duplicar.
    #[error("resultado incierto, no se reintentó para no duplicar: {0}")]
    ResultadoIncierto(String),

    #[error("x-org-slug inválido: {0}")]
    InvalidOrgSlug(String),

    #[error("crear cliente HTTP: {0}")]
    HttpClient(#[source] reqwest::Error),

    #[error("conectar con el servidor MCP: {0}")]
    Connect(#[source] Box<ClientInitializeError>),

    #[error("sin conexión con Oruscribe")]
    NotConnected,

    /// Límite de seguridad global en el cliente MCP para proteger la cuota de la cuenta.
    #[error(
        "límite de seguridad MCP alcanzado ({limit} llamadas en {window_secs} s): la app espera avisos por el timbre para proteger tu cuota"
    )]
    RateLimited { limit: usize, window_secs: u64 },

    #[error("{tool}: {message}")]
    Tool { tool: String, message: String },

    #[error("{tool}: {source}")]
    Call {
        tool: String,
        #[source]
        source: Box<ServiceError>,
    },

    #[error("{tool}: respuesta no es JSON: {source}")]
    InvalidJson {
        tool: String,
        #[source]
        source: serde_json::Error,
    },

    #[error("cerrar sesión MCP: {0}")]
    Close(String),

    #[error("estado desconocido: {0:?}")]
    UnknownStatus(String),

    /// Una entrada que la persona todavía puede corregir: un nombre vacío, una
    /// fecha que falta. El texto es el mismo que muestra el Go.
    #[error("{0}")]
    InvalidInput(String),

    /// La carpeta que se pidió leer no existe o no es una carpeta.
    #[error("esa carpeta no existe: {0}")]
    MissingFolder(String),

    /// `git` no está en el PATH de este equipo.
    #[error("git no está instalado en este equipo")]
    GitMissing,

    /// git respondió con un error; se conserva su stderr recortado.
    #[error("git no pudo leer la carpeta: {0}")]
    GitFailed(String),

    /// El rango no tiene commits, con la fecha desde la que se buscó.
    #[error("no hay commits en esa carpeta desde {0}")]
    NoCommits(String),

    /// La cuota de archivos de la organización está agotada. Envuelve el
    /// detalle original para no perder qué paso la reportó.
    #[error("la organización se quedó sin espacio para archivos: {detalle}")]
    SinEspacio { detalle: String },

    /// La dirección de subida no se puede interpretar o no tiene host.
    #[error("la dirección de subida no es válida")]
    InvalidUploadUrl,

    /// La dirección de subida no usa https ni apunta a loopback.
    #[error("la dirección de subida debe usar https; http solo se permite en localhost")]
    InsecureUploadUrl,

    /// Contexto de la herramienta sobre un error de subida, como el `%s: %w`
    /// del Go.
    #[error("{tool}: {source}")]
    ToolContext {
        tool: String,
        #[source]
        source: Box<OrgError>,
    },

    /// La respuesta de la herramienta de subida no trae lo necesario.
    #[error("{tool}: faltan campos (attachment_id/upload_url)")]
    MissingUploadFields { tool: String },

    /// El servidor pidió un método HTTP que no es un token válido.
    #[error("net/http: invalid method {0:?}")]
    InvalidUploadMethod(String),

    /// La URL firmada respondió un estado que no es 2xx.
    #[error("subir archivo: {status} {body}")]
    UploadStatus { status: u16, body: String },

    /// La petición a la URL firmada no se pudo completar.
    #[error("subir archivo: {0}")]
    UploadHttp(#[source] reqwest::Error),

    /// `attach_task_files` no devolvió el avance creado.
    #[error("attach_task_files: sin update_id")]
    MissingUpdateId,

    #[error("{tool}: sin id en la respuesta")]
    MissingId { tool: String },
}

/// Resultado con el error del crate.
pub type Result<T> = std::result::Result<T, OrgError>;
