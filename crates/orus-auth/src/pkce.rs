//! Puerto: verificador, reto S256 y estado.

use rand::{RngCore, rngs::OsRng};
use sha2::{Digest, Sha256};

use crate::Error;

const ALFABETO: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// 32 bytes aleatorios en base64url sin relleno: 43 caracteres.
pub fn new_verifier() -> Result<String, Error> {
    let mut bytes = [0u8; 32];
    aleatorio(&mut bytes)?;
    Ok(base64url(&bytes))
}

/// Deriva el reto S256 con SHA-256 y base64url.
pub fn challenge(verifier: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(verifier.as_bytes());
    base64url(&hash.finalize())
}

/// 16 bytes aleatorios en base64url sin relleno: 22 caracteres.
pub fn new_state() -> Result<String, Error> {
    let mut bytes = [0u8; 16];
    aleatorio(&mut bytes)?;
    Ok(base64url(&bytes))
}

/// `crypto/rand.Read`: si el sistema no da entropía, es un error de E/S.
fn aleatorio(bytes: &mut [u8]) -> Result<(), Error> {
    OsRng
        .try_fill_bytes(bytes)
        .map_err(|error| Error::Io(std::io::Error::other(error.to_string())))
}

/// `base64.RawURLEncoding`: sin `=` y con `-`/`_` en vez de `+`/`/`.
fn base64url(bytes: &[u8]) -> String {
    let mut salida = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for trozo in bytes.chunks(3) {
        let mut bloque = [0u8; 3];
        bloque[..trozo.len()].copy_from_slice(trozo);
        let valor =
            (u32::from(bloque[0]) << 16) | (u32::from(bloque[1]) << 8) | u32::from(bloque[2]);
        salida.push(ALFABETO[((valor >> 18) & 63) as usize] as char);
        salida.push(ALFABETO[((valor >> 12) & 63) as usize] as char);
        if trozo.len() > 1 {
            salida.push(ALFABETO[((valor >> 6) & 63) as usize] as char);
        }
        if trozo.len() > 2 {
            salida.push(ALFABETO[(valor & 63) as usize] as char);
        }
    }
    salida
}
