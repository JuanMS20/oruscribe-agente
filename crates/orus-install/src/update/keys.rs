//! Llaves públicas embebidas.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::{Signature, VerifyingKey};

use super::Error;

/// Llave principal y de respaldo, en base64, exactas a.
pub const TRUSTED_KEYS_B64: [&str; 2] = [
    "yEi7clTFW6Db2yCIX0x/FcJDOB8avIcgj/RcTIid8W8=",
    "+TCfwJ1TAks48NM/wBGT7sInhSoSJ13XQYPB6m4sphk=",
];

/// Llaves ya decodificadas. Vacío significa "no exigir firma", como el
/// `trustedKeysB64 = nil` que usa el contrato solo en sus tests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Keys {
    keys: Vec<[u8; 32]>,
}

impl Keys {
    /// Decodifica las dos llaves del binario.
    pub fn embedded() -> Result<Self, Error> {
        Self::from_base64(&TRUSTED_KEYS_B64)
    }

    /// Decodifica una lista inyectada. Una llave mala invalida el chequeo
    /// entero: si se ignorara, la app dejaría de exigir firma.
    pub fn from_base64(lista: &[&str]) -> Result<Self, Error> {
        let mut keys = Vec::with_capacity(lista.len());
        for texto in lista {
            let crudo = STANDARD
                .decode(texto.as_bytes())
                .map_err(|_| Error::LlaveMalFormada)?;
            let llave: [u8; 32] = crudo.try_into().map_err(|_| Error::LlaveMalFormada)?;
            keys.push(llave);
        }
        Ok(Self { keys })
    }

    /// Juego sin llaves: no se exige firma.
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// Dice si alguna llave avala `body`. Basta una, igual que el contrato
    /// Una llave de 32 bytes que no forma punto válido
    /// simplemente no verifica; no invalida a las demás.
    pub fn verifies(&self, body: &[u8], signature: &[u8]) -> bool {
        let Ok(firma) = Signature::from_slice(signature) else {
            return false;
        };
        self.keys
            .iter()
            .any(|llave| match VerifyingKey::from_bytes(llave) {
                Ok(publica) => publica.verify_strict(body, &firma).is_ok(),
                Err(_) => false,
            })
    }
}
