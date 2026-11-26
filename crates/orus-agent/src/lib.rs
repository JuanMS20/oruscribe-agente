//! Núcleo del agente: el trait de proveedor, la política de permisos, los
//! errores, el reintento, los clientes de OpenAI y Anthropic y el bucle del
//! runner.
//!

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Los tests sí pueden usar `unwrap` y `expect`: son la red que prueba el
// contrato, no código que atiende a una persona.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

mod anthropic;
mod anthropic_stream;
mod catalog;
mod cloud;
mod compact;
mod error;
mod factory;
mod openai;
mod openai_stream;
mod policy;
mod reintento;
mod runner;
mod runner_tools;
mod sse;
mod subagent;
mod types;

pub use anthropic::Anthropic;
pub use catalog::{
    PROTOCOLO_ANTHROPIC_MESSAGES, PROTOCOLO_BEDROCK_CONVERSE, PROTOCOLO_CONFIGURADO,
    PROTOCOLO_COPILOT, PROTOCOLO_GOOGLE_GENERATIVE_AI, PROTOCOLO_GOOGLE_VERTEX,
    PROTOCOLO_MISTRAL_CONVERSATIONS, PROTOCOLO_OPENAI_CHAT, PROTOCOLO_OPENAI_CODEX,
    PROTOCOLO_OPENAI_RESPONSES, PROTOCOLO_PI_MESSAGES, PROTOCOLO_TYPESAFE_SYSTEM_ONE,
    ProviderPreset, buscar_preset_proveedor, presets_de_proveedores,
};
pub use cloud::{CloudBrain, TokenFn};
pub use error::{ProviderError, costo_estimado};
pub use factory::{Proveedor, nuevo_proveedor, nuevo_proveedor_por_url};
pub use openai::{OpenAICompat, RetryDelay, validar_base};
pub use policy::{Decision, Mode, Risk, decide};
pub use reintento::{ESPERA_MAXIMA, MAX_INTENTOS, espera_reintento};
pub use runner::{
    AllowRuleFn, ApproveFn, CancelToken, EmitFn, HERRAMIENTA_PROPOSE_PLAN, OnTokenFn,
    PASOS_POR_DEFECTO, PAUSA_REINTENTO_TURNO, PlanApproveFn, Runner, RunnerError, Step, StepKind,
    Usage,
};
pub use subagent::{DEFAULT_STEPS, MAX_PROMPT, Subagent, read_tools};
pub use types::{ChatReply, ChatRequest, OnDelta, Provider, Tool, ToolError, ToolSpec};
