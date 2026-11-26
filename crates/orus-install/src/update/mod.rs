//! Update completo sin ventana: manifiesto, firma Ed25519, descarga y swap.
//!
//! e
//! El cliente HTTP, la URL y el
//! destino se inyectan; este crate no elige ventana ni puerto. El estado "en
//! curso" es [`ApplyLock`], no un global del proceso.

mod check;
mod download;
mod keys;
mod manifest;
pub mod paquete;
mod signature;
mod swap;
#[cfg(test)]
mod tests;

use std::sync::atomic::{AtomicBool, Ordering};

pub use check::{UpdateInfo, check, check_with_keys};
pub use download::{download, download_con};
pub use keys::{Keys, TRUSTED_KEYS_B64};
pub use manifest::Manifest;
pub use swap::{responde_version, restart, rollback, swap};

/// Manifiesto que consulta la app.
pub const DEFAULT_URL: &str =
    "https://raw.githubusercontent.com/JuanMS20/oruscribe-agente-descargas/main/latest.json";

/// Únicos sitios desde los que se acepta un ejecutable
/// Loopback se acepta aparte para tests.
pub const ALLOWED_HOSTS: [&str; 2] = ["github.com", "app.oruscribe.com"];

pub(crate) const LIMITE_MANIFIESTO: usize = 1 << 20;
pub(crate) const LIMITE_FIRMA: usize = 4 << 10;
pub(crate) const LIMITE_DESCARGA: usize = 512 << 20;

/// Errores con los textos exactos del contrato.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("el servidor de versiones respondió {0}")]
    Servidor(u16),
    #[error("el manifiesto no está firmado por Oruscribe; no se actualiza")]
    NoFirmado,
    #[error("el manifiesto no se entiende: {0}")]
    Manifiesto(#[from] serde_json::Error),
    #[error("hay una versión nueva ({0}), pero todavía no está disponible para este sistema")]
    SinDescarga(String),
    #[error("el manifiesto no trae una huella válida para esta plataforma")]
    HuellaInvalida,
    #[error("la descarga no viene de un sitio permitido: {0}")]
    SitioNoPermitido(String),
    #[error("la descarga no viene del repositorio de Oruscribe: {0}")]
    RepositorioAjeno(String),
    #[error("la descarga respondió {0}")]
    Descarga(u16),
    #[error("la descarga no coincide con la firma del manifiesto")]
    DescargaNoCoincide,
    #[error("el paquete no trae el binario {0}")]
    PaqueteSinBinario(String),
    #[error("el paquete de actualización está roto: {0}")]
    Paquete(String),
    #[error("el paquete trae un binario más grande que el tope de descarga")]
    PaqueteDemasiadoGrande,
    #[error("una llave pública embebida está mal formada")]
    LlaveMalFormada,
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Candado de "una actualización a la vez".
///
/// Vive donde lo ponga el llamador: 7-04 decide si es un campo de su estado o
/// una pieza compartida. No hay estado global en este crate.
#[derive(Debug, Default)]
pub struct ApplyLock {
    busy: AtomicBool,
}

impl ApplyLock {
    pub fn new() -> Self {
        Self::default()
    }

    /// Toma el candado si está libre. El [`ApplyGuard`] lo suelta al caer.
    pub fn try_acquire(&self) -> Option<ApplyGuard<'_>> {
        self.busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| ApplyGuard { lock: self })
    }
}

/// Permiso vivo de [`ApplyLock`]; soltarlo libera el candado.
#[derive(Debug)]
#[must_use = "el candado se libera al soltar el guardia"]
pub struct ApplyGuard<'a> {
    lock: &'a ApplyLock,
}

impl Drop for ApplyGuard<'_> {
    fn drop(&mut self) {
        self.lock.busy.store(false, Ordering::Release);
    }
}

/// Lee un cuerpo hasta `limite` bytes, como `io.LimitReader`.
pub(crate) async fn leer_hasta(
    mut respuesta: reqwest::Response,
    limite: usize,
) -> Result<Vec<u8>, Error> {
    let mut cuerpo = Vec::new();
    while let Some(fragmento) = respuesta.chunk().await? {
        let espacio = limite.saturating_sub(cuerpo.len());
        if espacio == 0 {
            break;
        }
        cuerpo.extend_from_slice(&fragmento[..fragmento.len().min(espacio)]);
    }
    Ok(cuerpo)
}
