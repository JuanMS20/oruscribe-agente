//! Arranque del shell de escritorio.
//!
//! La ventana, la bandeja y el servidor viven en módulos separados (R9);
//! `main` solo elige el camino de arranque y mantiene vivo el proceso.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

pub mod arranque;
pub mod aviso;
pub mod bandeja;
pub mod instancia;
pub mod navegador;
#[cfg(windows)]
pub mod selector_carpeta;
pub mod ventana;
