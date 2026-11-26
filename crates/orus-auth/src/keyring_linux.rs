//! Llaveros de Linux: Secret Service con respaldo en archivo privado.
//!
//! El llavero puede estar bloqueado: `get_secret` devuelve `NoEntry` mientras
//! `set_secret` devuelve `NoStorageAccess`. Por eso no se elige un backend al
//! construir; los dos caminos están siempre disponibles.

use std::path::{Path, PathBuf};

use keyring::Error as KeyringError;

use crate::file_store::{
    FileStore, Llavero, borrar_respaldo_marcado, cargar_con_llavero, cargar_con_llavero_marcado,
    guardar_con_llavero, guardar_respaldo_marcado, read_private,
};
use crate::{Error, Store, Token};

const SERVICIO: &str = "oruscribe-agent";
const USUARIO: &str = "session";

#[derive(Debug)]
enum KeyError {
    NoEntry,
    /// No hay llavero en el sistema: no puede quedar un token vivo allí.
    NoStore(String),
    /// El llavero existe pero la operación falló (por ejemplo, bloqueado).
    Fail(String),
}

impl KeyError {
    fn reason(self) -> String {
        match self {
            Self::NoEntry => "no hay entrada en el llavero".to_owned(),
            Self::NoStore(reason) | Self::Fail(reason) => reason,
        }
    }
}

/// Un bus D-Bus ausente o sin colección significa que no hay llavero, no que
/// el llavero esté bloqueado. El texto viene de `secret-service`/`zbus`.
/// Los fallos de transporte («could not connect», «connection refused») NO
/// están aquí: el bus existe pero no se pudo hablar con él, y el token puede
/// seguir vivo en el llavero; `clear` debe propagarlos, no responder Ok.
fn sin_servicio(reason: &str) -> bool {
    let reason = reason.to_ascii_lowercase();
    [
        "no default store",
        "serviceunknown",
        "service unknown",
        "not provided by any .service",
        "org.freedesktop.secrets",
        "no collection",
        "no such collection",
        "collection with alias",
        "no such service",
        "no service",
        // El socket del bus no existe: no hay infraestructura de llavero.
        "no such file or directory",
    ]
    .iter()
    .any(|fragmento| reason.contains(fragmento))
}

impl From<KeyringError> for KeyError {
    fn from(error: KeyringError) -> Self {
        let reason = error.to_string();
        match error {
            KeyringError::NoEntry => Self::NoEntry,
            KeyringError::NoDefaultStore => {
                // keyring 4 cachea la inicialización del store: si falló,
                // `Entry::new` devuelve NoDefaultStore sin el motivo. El
                // motivo real queda en `store_status`: un bus caído no es
                // ausencia de llavero y no puede dar Ok al cerrar sesión.
                match keyring::Entry::store_status() {
                    Err(error) if !sin_servicio(&error.to_string()) => {
                        Self::Fail(error.to_string())
                    }
                    _ => Self::NoStore(reason),
                }
            }
            KeyringError::PlatformFailure(_) | KeyringError::NoStorageAccess(_)
                if sin_servicio(&reason) =>
            {
                Self::NoStore(reason)
            }
            _ => Self::Fail(reason),
        }
    }
}

impl From<serde_json::Error> for KeyError {
    fn from(error: serde_json::Error) -> Self {
        Self::Fail(error.to_string())
    }
}

fn estado_llavero<T>(resultado: Result<T, KeyError>) -> Llavero<T> {
    match resultado {
        Ok(valor) => Llavero::Valido(valor),
        Err(KeyError::NoEntry) => Llavero::SinEntrada,
        Err(KeyError::NoStore(reason)) => Llavero::Ausente(reason),
        Err(KeyError::Fail(reason)) => Llavero::Fallo(reason),
    }
}

fn session_entry() -> Result<keyring::Entry, KeyError> {
    keyring::Entry::new(SERVICIO, USUARIO).map_err(KeyError::from)
}

/// El llavero de la sesión, sin respaldo. Lo envuelve `LinuxStore` en Linux y
/// el producto lo elige con `orus_auth::new_store`.
pub struct KeyringStore;

impl KeyringStore {
    fn write_token(&self, token: &Token) -> Result<(), KeyError> {
        let data = serde_json::to_vec(token)?;
        session_entry()?.set_secret(&data)?;
        Ok(())
    }

    fn read_token(&self) -> Result<Token, KeyError> {
        let data = session_entry()?.get_secret()?;
        Ok(serde_json::from_slice(&data)?)
    }

    fn remove_token(&self) -> Result<(), KeyError> {
        match session_entry() {
            Err(error) => Err(error),
            Ok(entry) => entry.delete_credential().map_err(KeyError::from),
        }
    }
}

impl Store for KeyringStore {
    fn save(&self, token: Token) -> Result<(), Error> {
        self.write_token(&token)
            .map_err(|error| Error::Unavailable(error.reason()))
    }

    fn load(&self) -> Result<Token, Error> {
        match self.read_token() {
            Ok(token) => Ok(token),
            Err(KeyError::NoEntry) => Err(Error::NoSession),
            Err(error) => Err(Error::Unavailable(error.reason())),
        }
    }

    fn clear(&self) -> Result<(), Error> {
        match self.remove_token() {
            Ok(()) | Err(KeyError::NoEntry) | Err(KeyError::NoStore(_)) => Ok(()),
            Err(error) => Err(Error::Unavailable(error.reason())),
        }
    }
}

/// Sesión de Linux: el llavero manda. El archivo es solo un respaldo para
/// cuando el llavero no existe o no tiene entrada.
pub struct LinuxStore {
    keyring: KeyringStore,
    file: FileStore,
}

impl LinuxStore {
    pub fn new(data_dir: impl AsRef<Path>) -> Self {
        Self {
            keyring: KeyringStore,
            file: FileStore::from_data_dir(data_dir),
        }
    }
}

impl Store for LinuxStore {
    fn save(&self, token: Token) -> Result<(), Error> {
        guardar_con_llavero(
            || self.keyring.write_token(&token).map_err(|e| e.reason()),
            || -> Result<Vec<u8>, Error> {
                let datos = serde_json::to_vec(&token)?;
                guardar_respaldo_marcado(self.file.path(), &datos)?;
                Ok(datos)
            },
            || borrar_respaldo_marcado(self.file.path()),
            self.file.path(),
            "la sesión",
        )
    }

    fn load(&self) -> Result<Token, Error> {
        cargar_con_llavero_marcado(
            estado_llavero(self.keyring.read_token()),
            || self.file.load(),
            self.file.path(),
        )
    }

    fn clear(&self) -> Result<(), Error> {
        let file_result = self.file.clear();
        let key_result = self.keyring.remove_token();
        match (file_result, key_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Ok(()), Err(KeyError::NoEntry | KeyError::NoStore(_))) => Ok(()),
            (Ok(()), Err(KeyError::Fail(reason))) => {
                Err(Error::Unavailable(format!("llavero: {reason}")))
            }
            (Err(file_error), Ok(())) => Err(file_error),
            (Err(file_error), Err(KeyError::NoEntry | KeyError::NoStore(_))) => Err(file_error),
            (Err(file_error), Err(KeyError::Fail(reason))) => Err(Error::Unavailable(format!(
                "archivo: {file_error}; llavero: {reason}"
            ))),
        }
    }
}

/// Llavero de un secreto suelto: target `service:name` y usuario `name`, como
/// en Windows. Con respaldo, `get` prefiere el llavero y `set` intenta el
/// llavero antes de caer al archivo.
pub struct SecretStore {
    target: String,
    user: String,
    fallback: Option<PathBuf>,
    solo_archivo: bool,
}

impl SecretStore {
    pub fn new(service: &str, name: &str) -> Self {
        Self {
            target: format!("{service}:{name}"),
            user: name.to_owned(),
            fallback: None,
            solo_archivo: false,
        }
    }

    pub fn with_file_fallback(service: &str, name: &str, data_dir: impl AsRef<Path>) -> Self {
        let saneado: String = name
            .chars()
            .filter(|caracter| {
                caracter.is_ascii_alphanumeric() || *caracter == '_' || *caracter == '-'
            })
            .collect();
        Self {
            target: format!("{service}:{name}"),
            user: name.to_owned(),
            fallback: Some(data_dir.as_ref().join(format!("secret-{saneado}.json"))),
            solo_archivo: crate::file_secrets_allowed(),
        }
    }

    #[cfg(test)]
    pub(crate) fn target(&self) -> &str {
        &self.target
    }

    #[cfg(test)]
    pub(crate) fn user(&self) -> &str {
        &self.user
    }

    fn entry(&self) -> Result<keyring::Entry, KeyringError> {
        keyring::Entry::new(&self.target, &self.user)
    }

    fn remove_keyring(&self) -> Result<(), KeyError> {
        match self.entry() {
            Err(error) => Err(KeyError::from(error)),
            Ok(entry) => entry.delete_credential().map_err(KeyError::from),
        }
    }

    fn leer_archivo(path: &Path) -> Result<String, Error> {
        if !path.exists() {
            return Err(Error::NoSession);
        }
        let datos = read_private(path)?;
        Ok(serde_json::from_slice(&datos)?)
    }

    pub fn get(&self) -> Result<Option<String>, Error> {
        if self.solo_archivo
            && let Some(path) = self.fallback.as_ref()
        {
            return match cargar_con_llavero_marcado::<String>(
                Llavero::SinEntrada,
                || Self::leer_archivo(path),
                path,
            ) {
                Ok(valor) => Ok(Some(valor)),
                Err(Error::NoSession) => Ok(None),
                Err(error) => Err(error),
            };
        }
        let del_llavero = self
            .entry()
            .map_err(KeyError::from)
            .and_then(|entry| entry.get_secret().map_err(KeyError::from))
            .and_then(|bytes| {
                String::from_utf8(bytes).map_err(|_| {
                    KeyError::Fail("la clave guardada no está codificada en UTF-8".to_owned())
                })
            });
        let obtenido = match &self.fallback {
            Some(path) => cargar_con_llavero_marcado(
                estado_llavero(del_llavero),
                || {
                    if path.exists() {
                        let data = read_private(path)?;
                        Ok(serde_json::from_slice(&data)?)
                    } else {
                        Err(Error::NoSession)
                    }
                },
                path,
            ),
            None => cargar_con_llavero(estado_llavero(del_llavero), || Err(Error::NoSession)),
        };
        match obtenido {
            Ok(valor) => Ok(Some(valor)),
            Err(Error::NoSession) => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub fn set(&self, value: &str) -> Result<(), Error> {
        if self.solo_archivo
            && let Some(path) = self.fallback.as_ref()
        {
            let datos = serde_json::to_vec(&value)?;
            guardar_respaldo_marcado(path, &datos)?;
            return Ok(());
        }
        let Some(path) = self.fallback.as_ref() else {
            return self
                .entry()
                .map_err(|error| Error::Unavailable(KeyError::from(error).reason()))
                .and_then(|entry| {
                    entry
                        .set_secret(value.as_bytes())
                        .map_err(|error| Error::Unavailable(KeyError::from(error).reason()))
                });
        };
        guardar_con_llavero(
            || {
                let entry = self
                    .entry()
                    .map_err(|error| KeyError::from(error).reason())?;
                entry
                    .set_secret(value.as_bytes())
                    .map_err(|error| KeyError::from(error).reason())
            },
            || -> Result<Vec<u8>, Error> {
                let data = serde_json::to_vec(&value)?;
                guardar_respaldo_marcado(path, &data)?;
                Ok(data)
            },
            || borrar_respaldo_marcado(path),
            path,
            "la clave",
        )
    }

    pub fn delete(&self) -> Result<(), Error> {
        if self.solo_archivo
            && let Some(path) = self.fallback.as_ref()
        {
            return Ok(borrar_respaldo_marcado(path)?);
        }
        let file_result = match &self.fallback {
            Some(path) => borrar_respaldo_marcado(path),
            None => Ok(()),
        };
        let key_result = self.remove_keyring();
        match (file_result, key_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Ok(()), Err(KeyError::NoEntry | KeyError::NoStore(_))) => Ok(()),
            (Ok(()), Err(KeyError::Fail(reason))) => {
                Err(Error::Unavailable(format!("llavero: {reason}")))
            }
            (Err(file_error), Ok(())) => Err(Error::Io(file_error)),
            (Err(file_error), Err(KeyError::NoEntry | KeyError::NoStore(_))) => {
                Err(Error::Io(file_error))
            }
            (Err(file_error), Err(KeyError::Fail(reason))) => Err(Error::Unavailable(format!(
                "archivo: {file_error}; llavero: {reason}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// El bus existe pero no se pudo hablar con él: no es ausencia de
    /// llavero. `clear` debe propagar el error, no responder Ok y dejar el
    /// token vivo para cuando el bus vuelva.
    #[test]
    fn transporte_caido_no_es_ausencia_de_llavero() {
        assert!(!sin_servicio("could not connect to bus"));
        assert!(!sin_servicio("Connection refused (os error 111)"));
    }

    /// La ausencia real del servicio sigue siendo ausencia: un Linux sin
    /// llavero puede cerrar sesión sin error.
    #[test]
    fn ausencia_de_servicio_sigue_siendo_ausencia() {
        assert!(sin_servicio("no default store"));
        assert!(sin_servicio(
            "ServiceUnknown: org.freedesktop.secrets not provided by any .service"
        ));
        // El socket del bus no existe: no hay infraestructura de llavero.
        assert!(sin_servicio("No such file or directory (os error 2)"));
    }
}
