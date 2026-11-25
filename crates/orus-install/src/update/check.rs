//! Chequeo del manifiesto (`internal/update/update.go:44-121`).

use reqwest::header::CACHE_CONTROL;
use serde::Serialize;

use super::{Error, Keys, LIMITE_MANIFIESTO, Manifest, leer_hasta};
use crate::{newer, valid_sha};

/// Respuesta de `GET /api/update`, con las mismas claves y omisiones que el
/// `Result` de Go (`update.go:44-53`): `latest`, `notes`, `url` y `sha256`
/// desaparecen cuando están vacíos.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct UpdateInfo {
    pub configured: bool,
    pub available: bool,
    pub current: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub latest: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub notes: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub url: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub sha256: String,
}

/// Consulta el manifiesto con las llaves embebidas.
///
/// Una red caída es error, no un "estás al día" (`update.go:55-56`).
pub async fn check(http: &reqwest::Client, url: &str, current: &str) -> Result<UpdateInfo, Error> {
    if url.is_empty() {
        return Ok(UpdateInfo {
            configured: false,
            current: current.to_owned(),
            ..UpdateInfo::default()
        });
    }
    let keys = Keys::embedded()?;
    check_with_keys(http, url, current, &keys).await
}

/// Igual que [`check`] pero con las llaves inyectadas.
///
/// Es la costura que usan los tests para firmar con llaves desechables y para
/// reproducir el `trustedKeysB64 = nil` de `update_test.go`. En producción se
/// usa [`check`], que siempre lleva las dos llaves embebidas.
pub async fn check_with_keys(
    http: &reqwest::Client,
    url: &str,
    current: &str,
    keys: &Keys,
) -> Result<UpdateInfo, Error> {
    let mut info = UpdateInfo {
        configured: !url.is_empty(),
        current: current.to_owned(),
        ..UpdateInfo::default()
    };
    if url.is_empty() {
        return Ok(info);
    }

    let respuesta = http
        .get(url)
        .header(CACHE_CONTROL, "no-cache")
        .send()
        .await?;
    if respuesta.status() != reqwest::StatusCode::OK {
        return Err(Error::Servidor(respuesta.status().as_u16()));
    }
    let body = leer_hasta(respuesta, LIMITE_MANIFIESTO).await?;

    // Sin llaves no se exige firma: pasa solo en tests (`update.go:83-88`).
    if !keys.is_empty() {
        super::signature::verify_signature(http, url, &body, keys).await?;
    }

    let manifiesto: Manifest = serde_json::from_slice(&body)?;
    let hay_nueva = newer(&manifiesto.version, current);
    info.latest = manifiesto.version;
    info.notes = manifiesto.notes;
    if cfg!(windows) {
        info.url = manifiesto.windows;
        info.sha256 = manifiesto.sha256;
    } else {
        info.url = manifiesto.linux;
        info.sha256 = manifiesto.linux_sha256;
    }

    if hay_nueva && info.url.is_empty() {
        return Err(Error::SinDescarga(info.latest.clone()));
    }
    info.available = hay_nueva;
    if !hay_nueva {
        return Ok(info);
    }

    // Un manifiesto que anuncia algo que no puede entregar está roto, no al
    // día: falla acá y no reporta "al día" (`update.go:110-119`).
    super::download::check_download_url(&info.url)?;
    if !valid_sha(&info.sha256) {
        return Err(Error::HuellaInvalida);
    }
    Ok(info)
}
