use std::ffi::c_void;
use std::ptr;

use windows_sys::Win32::Foundation::{ERROR_NOT_FOUND, GetLastError};
use windows_sys::Win32::Security::Credentials::{
    CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredFree, CredReadW,
    CredWriteW,
};

use crate::{Error, Store, Token};

// go-keyring compone `service + ":" + username` (`keyring_windows.go:96-99`).
pub(crate) const KEYRING_TARGET: &str = "oruscribe-agent:session";
const KEYRING_USER: &str = "session";

/// Llavero de un secreto suelto: target `service:name` y usuario `name`, como
/// go-keyring (`secret.go:13-49`). `get` distingue "no hay" con `Ok(None)`.
pub struct SecretStore {
    target: String,
    user: String,
}

impl SecretStore {
    pub fn new(service: &str, name: &str) -> Self {
        Self {
            target: format!("{service}:{name}"),
            user: name.to_owned(),
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

    pub fn get(&self) -> Result<Option<String>, Error> {
        match read_credential(&self.target) {
            Ok(datos) => String::from_utf8(datos).map(Some).map_err(|_| {
                Error::Unavailable("la clave guardada no está codificada en UTF-8".to_owned())
            }),
            Err(KeyringError::Windows(ERROR_NOT_FOUND)) => Ok(None),
            Err(error) => Err(Error::Unavailable(error.to_string())),
        }
    }

    pub fn set(&self, value: &str) -> Result<(), Error> {
        write_credential(&self.target, &self.user, value.as_bytes())
            .map_err(|error| Error::Unavailable(error.to_string()))
    }

    pub fn delete(&self) -> Result<(), Error> {
        match delete_credential(&self.target) {
            Ok(()) | Err(KeyringError::Windows(ERROR_NOT_FOUND)) => Ok(()),
            Err(error) => Err(Error::Unavailable(error.to_string())),
        }
    }
}

/// Llavero del Administrador de credenciales de Windows.
pub struct KeyringStore;

impl Store for KeyringStore {
    fn save(&self, token: Token) -> Result<(), Error> {
        let data = serde_json::to_vec(&token)?;
        write_credential(KEYRING_TARGET, KEYRING_USER, &data)
            .map_err(|error| Error::Unavailable(error.to_string()))
    }

    fn load(&self) -> Result<Token, Error> {
        match read_credential(KEYRING_TARGET) {
            Ok(data) => Ok(serde_json::from_slice(&data)?),
            Err(KeyringError::Windows(ERROR_NOT_FOUND)) => Err(Error::NoSession),
            Err(error) => Err(Error::Unavailable(error.to_string())),
        }
    }

    fn clear(&self) -> Result<(), Error> {
        match delete_credential(KEYRING_TARGET) {
            Ok(()) | Err(KeyringError::Windows(ERROR_NOT_FOUND)) => Ok(()),
            Err(error) => Err(Error::Keyring(error.to_string())),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum KeyringError {
    #[error("error de Windows {0}")]
    Windows(u32),
    #[error("la credencial no trae datos")]
    Empty,
}

fn write_credential(target: &str, user: &str, blob: &[u8]) -> Result<(), KeyringError> {
    let target = wide(target);
    let user = wide(user);
    let credential = CREDENTIALW {
        Type: CRED_TYPE_GENERIC,
        TargetName: target.as_ptr() as *mut u16,
        CredentialBlobSize: blob.len() as u32,
        CredentialBlob: blob.as_ptr() as *mut u8,
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        UserName: user.as_ptr() as *mut u16,
        ..Default::default()
    };
    // SAFETY: target y user son buffers UTF-16 terminados en NUL y blob apunta
    // a blob.len() bytes; los tres viven hasta que CredWriteW retorna.
    let ok = unsafe { CredWriteW(&credential, 0) };
    if ok == 0 {
        return Err(KeyringError::Windows(last_error()));
    }
    Ok(())
}

fn read_credential(target: &str) -> Result<Vec<u8>, KeyringError> {
    let target = wide(target);
    let mut credential: *mut CREDENTIALW = ptr::null_mut();
    // SAFETY: target es UTF-16 terminado en NUL y credential es un puntero de
    // salida válido; CredReadW devuelve un bloque que libera CredFree.
    let ok = unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut credential) };
    if ok == 0 {
        return Err(KeyringError::Windows(last_error()));
    }
    if credential.is_null() {
        return Err(KeyringError::Empty);
    }

    // SAFETY: tras el éxito, CredReadW devuelve un CREDENTIALW vivo hasta que
    // se libere con CredFree; no se libera antes de copiar el blob.
    let entry = unsafe { &*credential };
    let data = if entry.CredentialBlob.is_null() {
        Vec::new()
    } else {
        // SAFETY: CredentialBlob apunta a CredentialBlobSize bytes válidos
        // mientras la credencial siga viva.
        unsafe {
            std::slice::from_raw_parts(entry.CredentialBlob, entry.CredentialBlobSize as usize)
        }
        .to_vec()
    };
    // SAFETY: credential salió de CredReadW y no se liberó todavía.
    unsafe { CredFree(credential.cast::<c_void>()) };
    Ok(data)
}

fn delete_credential(target: &str) -> Result<(), KeyringError> {
    let target = wide(target);
    // SAFETY: target es un buffer UTF-16 terminado en NUL que vive durante la
    // llamada; CredDeleteW no retiene el puntero.
    let ok = unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) };
    if ok == 0 {
        return Err(KeyringError::Windows(last_error()));
    }
    Ok(())
}

fn last_error() -> u32 {
    // SAFETY: GetLastError no recibe punteros y devuelve el error del hilo.
    unsafe { GetLastError() }
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};
    use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_NO_SUCH_LOGON_SESSION};

    #[test]
    fn keyring_target_matches_go() {
        assert_eq!(KEYRING_TARGET, "oruscribe-agent:session");
        assert_eq!(KEYRING_USER, "session");

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0);
        let target = format!("oruscribe-agent-test:{}-{unique}", std::process::id());
        let blob = b"valor falso";
        if let Err(error) = write_credential(&target, KEYRING_USER, blob) {
            let unavailable = matches!(
                &error,
                KeyringError::Windows(code)
                    if *code == ERROR_NO_SUCH_LOGON_SESSION || *code == ERROR_ACCESS_DENIED
            );
            if unavailable {
                eprintln!(
                    "se omite keyring_target_matches_go: Credential Manager no disponible ({error})"
                );
                return;
            }
            panic!("no se pudo crear la credencial de prueba: {error}");
        }

        let read = read_credential(&target);
        delete_credential(&target).expect("borrar la credencial de prueba");
        assert_eq!(read.expect("leer la credencial de prueba"), blob);
    }
}
