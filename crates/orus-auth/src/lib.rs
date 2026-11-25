//! Sesión de la app: el `Token` de OAuth y el llavero del sistema.
//!
//! Port de `internal/auth/store.go` y de `Token` en `internal/auth/oauth.go`.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod file_store;
#[cfg(target_os = "linux")]
mod keyring_linux;
#[cfg(windows)]
mod keyring_windows;
mod login;
mod oauth;
mod pkce;
mod token;

pub use file_store::FileStore;
#[cfg(target_os = "linux")]
pub use keyring_linux::{KeyringStore, LinuxStore, SecretStore};
#[cfg(windows)]
pub use keyring_windows::{KeyringStore, SecretStore};
pub use login::{CALLBACK_PATH, authorize_url, exchange, register};
pub use oauth::{Config, ISSUER, Metadata, discover, refresh};
pub use pkce::{challenge, new_state, new_verifier};
pub use reqwest::Client as HttpClient;
pub use token::Token;

use std::path::Path;

/// Error de la frontera del crate.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no hay sesión guardada")]
    NoSession,
    #[error("este equipo no puede guardar la sesión de forma segura: {0}")]
    Unavailable(String),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error("descubrimiento del servidor OAuth: {0}")]
    Discover(String),
    #[error("el servidor OAuth no publica los endpoints necesarios")]
    IncompleteMetadata,
    #[error("no hay token de refresco")]
    NoRefreshToken,
    #[error("renovar la sesión: {0}")]
    Renew(String),
    #[error("la renovación no devolvió token de acceso")]
    NoAccessToken,
    #[error("el servidor rechazó renovar la sesión: {0}")]
    RefreshRejected(String),
    #[error("el servidor OAuth no permite registrar la app")]
    RegisterEndpoint,
    #[error("registro de la app: {0}")]
    Register(String),
    #[error("el registro no devolvió client_id")]
    NoClientId,
    #[error("intercambio del código: {0}")]
    Exchange(String),
    #[error("el servidor no devolvió token de acceso")]
    ExchangeNoAccessToken,
    #[error("dirección del servidor OAuth inválida: {0}")]
    InvalidUrl(String),
    #[cfg(windows)]
    #[error("no se pudo borrar la sesión: {0}")]
    Keyring(String),
}

/// Guarda la sesión entre ejecuciones. Es el `Store` de Go.
pub trait Store {
    fn save(&self, token: Token) -> Result<(), Error>;
    fn load(&self) -> Result<Token, Error>;
    fn clear(&self) -> Result<(), Error>;
}

/// Plataforma sin llavero: Linux usa `keyring_linux` y Windows
/// `keyring_windows`. Aquí solo se conserva la forma pública del crate.
#[cfg(not(any(windows, target_os = "linux")))]
const SIN_LLAVERO: &str = "el llavero solo está implementado en Windows y Linux";

#[cfg(not(any(windows, target_os = "linux")))]
pub struct KeyringStore;

#[cfg(not(any(windows, target_os = "linux")))]
impl Store for KeyringStore {
    fn save(&self, _: Token) -> Result<(), Error> {
        Err(Error::Unavailable(SIN_LLAVERO.into()))
    }

    fn load(&self) -> Result<Token, Error> {
        Err(Error::Unavailable(SIN_LLAVERO.into()))
    }

    fn clear(&self) -> Result<(), Error> {
        Ok(())
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
pub struct SecretStore {
    #[allow(dead_code)]
    target: String,
    #[allow(dead_code)]
    user: String,
}

#[cfg(not(any(windows, target_os = "linux")))]
impl SecretStore {
    pub fn new(service: &str, name: &str) -> Self {
        Self {
            target: format!("{service}:{name}"),
            user: name.to_owned(),
        }
    }

    pub fn with_file_fallback(service: &str, name: &str, _data_dir: impl AsRef<Path>) -> Self {
        Self::new(service, name)
    }

    #[cfg(test)]
    pub(crate) fn target(&self) -> &str {
        &self.target
    }

    #[cfg(test)]
    pub(crate) fn user(&self) -> &str {
        &self.user
    }

    pub fn get(&self) -> Result<Option<String>, Error> {
        Err(Error::Unavailable(SIN_LLAVERO.into()))
    }

    pub fn set(&self, _: &str) -> Result<(), Error> {
        Err(Error::Unavailable(SIN_LLAVERO.into()))
    }

    pub fn delete(&self) -> Result<(), Error> {
        Err(Error::Unavailable(SIN_LLAVERO.into()))
    }
}

/// En Windows el llavero del sistema basta: no hay respaldo en archivo.
#[cfg(windows)]
impl SecretStore {
    pub fn with_file_fallback(service: &str, name: &str, _data_dir: impl AsRef<Path>) -> Self {
        Self::new(service, name)
    }
}

/// Interruptor manual para usar un archivo como store principal
/// (`ORUSCRIBE_ALLOW_FILE_SECRETS=1`, `store.go:177-182`).
pub fn file_secrets_allowed() -> bool {
    std::env::var("ORUSCRIBE_ALLOW_FILE_SECRETS").is_ok_and(|value| value == "1")
}

/// Con el interruptor, `FileStore` es el store principal. En Linux, `LinuxStore`
/// intenta el llavero y cae a un archivo privado (0600) si falla o no existe.
pub fn new_store(data_dir: impl AsRef<Path>) -> Box<dyn Store + Send + Sync> {
    if file_secrets_allowed() {
        Box::new(FileStore::from_data_dir(data_dir))
    } else {
        #[cfg(windows)]
        {
            Box::new(KeyringStore)
        }
        #[cfg(target_os = "linux")]
        {
            Box::new(LinuxStore::new(data_dir))
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            Box::new(KeyringStore)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SecretStore;

    /// go-keyring compone `service + ":" + name`; la sesión conserva el suyo.
    #[test]
    fn secret_store_compone_target_y_usuario() {
        let store = SecretStore::new("oruscribe-agent", "agente-clave");
        assert_eq!(store.target(), "oruscribe-agent:agente-clave");
        assert_eq!(store.user(), "agente-clave");
    }
}
