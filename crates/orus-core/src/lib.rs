//! Tipos compartidos del arnés. Sin UI, sin red, sin base de datos.

#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod error;
mod event;
mod message;

pub use error::Error;
pub use event::Event;
pub use message::{Image, Message, ToolCall};

pub type Result<T> = std::result::Result<T, Error>;
