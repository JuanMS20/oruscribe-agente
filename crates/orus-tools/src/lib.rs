//! Herramientas locales del agente. Ola 1: el diario (`journal`); Ola 3: las
//! carpetas, la resolución de rutas y las herramientas de lectura
//! (`localtools`).

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

pub mod journal;
pub mod localtools;
pub mod permissions;
