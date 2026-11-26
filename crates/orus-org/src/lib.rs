//! Cliente de la organización.
//!
//! Habla MCP Streamable HTTP con `rmcp`; cada método llama una
//! herramienta del servidor. Este crate expone los tipos, los errores, la
//! apertura de sesión y la llamada genérica a herramientas.

#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use chrono::{DateTime, SecondsFormat, Utc};

pub mod captura;
mod client;
pub mod doorbell;
mod error;
pub mod mensajes;
pub mod subidas;
pub mod tareas;
pub mod types;

pub use client::{Client, DEFAULT_ENDPOINT, JsonObject, McpQuota, ToolSpec};
pub use error::{OrgError, Result};
pub use mensajes::{MessagesPage, ToolCaller};
pub use types::*;

/// Marca de tiempo con segundos y `Z`, el formato que espera el servidor.
pub(crate) fn rfc3339(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Secs, true)
}

pub(crate) fn es_loopback(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    let host = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    host.parse::<std::net::IpAddr>()
        .is_ok_and(|ip| ip.is_loopback())
}

pub(crate) fn url_segura(url: &reqwest::Url) -> bool {
    matches!(url.scheme(), "https")
        || (url.scheme() == "http" && url.host_str().is_some_and(es_loopback))
}
