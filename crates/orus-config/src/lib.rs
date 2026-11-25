//! Configuración local del agente: carpeta de datos, idioma y `settings.json`.
//!
//! Port de `internal/config` (Go). Nada aquí habla con la red.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod error;
#[cfg(not(windows))]
mod locale_other;
#[cfg(windows)]
mod locale_windows;
mod paths;
mod settings;
mod store;

pub use error::Error;
pub use paths::{clean_path, data_dir, data_dir_removable, sin_prefijo};
pub use settings::{
    AGENT_SECRET_NAME, AgentSettings, AllowRule, DEFAULT_AGENT_BASE_URL, DEFAULT_AGENT_MODE,
    Locale, Settings, load,
};
pub use store::{editar, save};

#[cfg(not(windows))]
pub use locale_other::system_locale;
#[cfg(windows)]
pub use locale_windows::system_locale;
