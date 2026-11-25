//! Tipos del proveedor y de las herramientas. Port de
//! `internal/agent/types.go:38-72`.

use std::fmt;
use std::time::Duration;

use async_trait::async_trait;
use orus_core::Message;
use serde_json::Value;
use thiserror::Error;

use crate::error::ProviderError;
use crate::policy::Risk;
use crate::runner::Usage;

/// Describe una herramienta para el modelo. `schema` es el JSON Schema de los
/// argumentos.
///
/// Copia de `internal/agent/types.go:39-43`. En Go `Schema` es
/// `json.RawMessage`; acá es un valor JSON, así que se conserva la estructura
/// pero no el orden de las claves.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub schema: Value,
}

/// Cualquier servicio que responde un turno de chat.
///
/// Copia de `internal/agent/types.go:46-48`. `Send + Sync` no está en el
/// interface de Go: es lo que Rust necesita para mover el proveedor entre
/// tareas.
#[async_trait]
pub trait Provider: Send + Sync {
    async fn chat(&self, req: ChatRequest) -> Result<ChatReply, ProviderError>;
}

/// Callback de streaming: recibe cada fragmento de texto según llega.
pub type OnDelta = Box<dyn Fn(&str) + Send + Sync>;

/// Lo que se le manda al proveedor en cada vuelta.
///
/// Copia de `internal/agent/types.go:51-58`.
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    /// Si no es `None`, pide la respuesta por partes y se llama con cada
    /// fragmento de texto según llega. Con `None` se espera la respuesta
    /// entera.
    pub on_delta: Option<OnDelta>,
}

impl fmt::Debug for ChatRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChatRequest")
            .field("model", &self.model)
            .field("messages", &self.messages)
            .field("tools", &self.tools)
            .field("on_delta", &self.on_delta.is_some())
            .finish()
    }
}

/// La respuesta del proveedor con el consumo de tokens.
///
/// Copia de `internal/agent/types.go:61-65`.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatReply {
    pub message: Message,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
}

/// Error de una herramienta. En Go, `Tool.Run` devuelve `error`
/// (`internal/agent/types.go:71`) y el bucle solo usa su texto; acá el enum
/// permite que las herramientas de T2.4 agreguen variantes sin cambiar el
/// trait.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ToolError {
    #[error("{0}")]
    Message(String),
}

/// Una herramienta que el agente puede correr.
///
/// Copia de `internal/agent/types.go:68-72`.
#[async_trait]
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;

    /// Riesgo de la herramienta. `None` es un riesgo desconocido y la
    /// política lo niega, igual que un valor fuera de la lista en Go
    /// (`policy.go:32-34`).
    fn risk(&self) -> Option<Risk>;

    async fn run(&self, args: Value) -> Result<String, ToolError>;

    /// Plazo propio de esta llamada, declarado en sus argumentos. El runner lo
    /// usa antes que su plazo base; `None` conserva el base.
    fn plazo(&self, _args: &Value) -> Option<Duration> {
        None
    }

    /// Consumo acumulado por esta herramienta desde la última lectura. El
    /// runner lo suma al uso del turno para que presupuesto y coste vean lo
    /// que gastó un subagente (issue #126). Las herramientas sin proveedor
    /// propio devuelven cero.
    fn consumo(&self) -> Usage {
        Usage::default()
    }
}
