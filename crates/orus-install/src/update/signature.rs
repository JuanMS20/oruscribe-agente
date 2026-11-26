//! Firma Ed25519 de `latest.json`.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use reqwest::header::CACHE_CONTROL;

use super::{Error, Keys, LIMITE_FIRMA, leer_hasta};

/// Baja `url + ".sig"`, decodifica base64 y exige que alguna llave firme los
/// bytes exactos del manifiesto. Cualquier fallo es [`Error::NoFirmado`], como
pub(crate) async fn verify_signature(
    http: &reqwest::Client,
    url: &str,
    body: &[u8],
    keys: &Keys,
) -> Result<(), Error> {
    let respuesta = http
        .get(format!("{url}.sig"))
        .header(CACHE_CONTROL, "no-cache")
        .send()
        .await?;
    if respuesta.status() != reqwest::StatusCode::OK {
        return Err(Error::NoFirmado);
    }
    let crudo = leer_hasta(respuesta, LIMITE_FIRMA).await?;
    let texto = String::from_utf8_lossy(&crudo);
    let firma = STANDARD
        .decode(texto.trim())
        .map_err(|_| Error::NoFirmado)?;
    if keys.verifies(body, &firma) {
        Ok(())
    } else {
        Err(Error::NoFirmado)
    }
}
