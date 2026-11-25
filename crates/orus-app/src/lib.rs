//! Sesión de la app: token, refresco y cliente MCP. Port de `internal/app/app.go:33-40,487-535,680,730-800`.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod agent;
mod agent_conversations;
mod approvals;
pub mod avatares;
mod callback;
mod conversations;
mod login;
mod sesion;
mod sync;
pub mod telemetria;
mod wake;
mod watch;

pub use agent::{
    Agent, AgentError, AgentImage, AgentInput, AgentMessage, AgentProbe, AgentSecrets, AgentStep,
    AgentView, Emisor, HarnessRunner, JournalEntryView, NoSecrets, ProveedorFn, Secrets, Toolset,
    TurnRunner, Turno, WakeLog, normalizar_imagenes,
};
pub use approvals::{AgentApproval, ApprovalError, ApprovalQueue, nombre_accion};
pub use callback::{LoginError, wait};
pub use conversations::{ConversationInfo, Conversations, conversation_store, conversations_dir};
pub use login::{LOGIN_PORT, LoginFlow, login_listener, login_listener_on};
pub use orus_agent::{
    CancelToken, ChatReply, ChatRequest, Provider, ProviderError, ProviderPreset,
    presets_de_proveedores,
};
pub use orus_auth::CALLBACK_PATH;
pub use orus_core::Message as ChatMessage;
pub use orus_harness::ErrorTurno;
pub use orus_harness::sessions::Session;
pub use orus_org::Pagina;
// `orus-api` no depende de `orus-org` (D161); el borde usa estos tipos a través
// de `orus-app`.
pub use orus_org::doorbell::Event as DoorbellEvent;
pub use orus_org::{
    Digest, DigestSource, GoalInput, McpQuota, PeriodInput, Progress, Status as TaskStatus,
    TaskInput, parse_status,
};
pub use sesion::{
    App, Backend, Connector, OrgConnector, OrgRefresh, Refresh, SessionError, State, Status,
    WatchSpawner, git_log, git_log_con_plazo,
};
pub use telemetria::{EntregaTelemetria, SaludTelemetria, SentryDsn, Telemetry, TelemetryEvent};
pub use wake::{
    Clock, Despertador, RelojLocal, WakeEntry, WakeError, WakeEvent, WakePolicy, leer_evento,
    prompt_despertar,
};
pub use watch::{
    BELL_RETRY_LIMIT, Bell, BellConnector, ClientProvider, Event, EventSink, RealBellConnector,
    RetryLimiter, RingSink, Sleeper, TokioSleeper, WakePolicyTrigger, WakeTrigger, Watch,
    backoff_delay,
};
