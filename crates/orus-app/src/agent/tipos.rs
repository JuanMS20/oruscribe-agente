//! Los tipos del agente: pasos, errores, entrada y estado compartido.

use chrono::{DateTime, Utc};
use orus_agent::CancelToken;
use orus_core::Message;
use serde::{Deserialize, Serialize};

use crate::sesion::SessionError;

use super::validacion::{
    deserializar_bool, deserializar_i64, deserializar_texto, deserializar_textos,
};

/// Un paso de la traza que pinta la ventana.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentStep {
    pub at: DateTime<Utc>,
    pub kind: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tool: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
}

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("ya hay una ejecución del agente en curso")]
    Busy,
    #[error("{0}")]
    Invalid(String),
    #[error("Se canceló la respuesta del agente.")]
    Cancelled,
    #[error("El agente tardó más de 5 minutos y se detuvo.")]
    TimedOut,
    #[error("{0}")]
    Turno(String),
    #[error("{0}")]
    Save(String),
    #[error("{0}")]
    Conversaciones(String),
    /// Un fallo de la sesión al armar el turno; `LoggedOut` conserva el 401
    #[error(transparent)]
    Sesion(#[from] SessionError),
}

/// Lo que manda el formulario de configuración. Un campo ausente o `null`
/// vale cero; una `api_key` vacía conserva la clave guardada
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct AgentInput {
    #[serde(default, deserialize_with = "deserializar_texto")]
    pub kind: String,
    #[serde(default, deserialize_with = "deserializar_texto")]
    pub base_url: String,
    #[serde(default, deserialize_with = "deserializar_texto")]
    pub model: String,
    #[serde(default, deserialize_with = "deserializar_texto")]
    pub mode: String,
    #[serde(default, deserialize_with = "deserializar_texto")]
    pub api_key: String,
    #[serde(default, deserialize_with = "deserializar_textos")]
    pub folders: Vec<String>,
    #[serde(default, deserialize_with = "deserializar_textos")]
    pub programs: Vec<String>,
    #[serde(default, deserialize_with = "deserializar_bool")]
    pub wake_enabled: bool,
    #[serde(default, deserialize_with = "deserializar_i64")]
    pub wake_per_minute: i64,
    #[serde(default, deserialize_with = "deserializar_i64")]
    pub wake_per_day: i64,
    #[serde(default, deserialize_with = "deserializar_i64")]
    pub wake_tokens_per_day: i64,
    #[serde(default, deserialize_with = "deserializar_texto")]
    pub voice_stt_url: String,
    #[serde(default, deserialize_with = "deserializar_texto")]
    pub voice_stt_model: String,
    #[serde(default, deserialize_with = "deserializar_texto")]
    pub voice_tts_url: String,
    #[serde(default, deserialize_with = "deserializar_texto")]
    pub voice_tts_model: String,
    #[serde(default, deserialize_with = "deserializar_texto")]
    pub voice_tts_voice: String,
}

/// El estado que comparten el turno, la ventana y el despertar.
#[derive(Clone, Default)]
pub(crate) struct EstadoAgente {
    pub(crate) conversation_id: String,
    pub(crate) created: Option<DateTime<Utc>>,
    pub(crate) model: String,
    pub(crate) history: Vec<Message>,
    pub(crate) tokens_in: i64,
    pub(crate) tokens_out: i64,
    pub(crate) cost_usd: f64,
    pub(crate) steps: Vec<AgentStep>,
    pub(crate) error: String,
    pub(crate) draft: String,
    pub(crate) busy: bool,
    /// Una operación exclusiva (configurar o deshacer) tomó el agente antes de
    /// empezar; `send` también la mira.
    pub(crate) setup: bool,
    pub(crate) cancel: Option<CancelToken>,
    pub(crate) generation: u64,
}
