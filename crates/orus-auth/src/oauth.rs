//! Puerto.

use chrono::{DateTime, FixedOffset, Utc};
use reqwest::header::{ACCEPT, CONTENT_TYPE};
use serde::{Deserialize, Serialize};

use crate::{Error, Token, token::zero_expiry};

/// El servidor de OAuth de la app.
pub const ISSUER: &str = "https://cfamprdygpngrjtaxzes.supabase.co/auth/v1";

/// Lo que el servidor publica de sí mismo.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Metadata {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub userinfo_endpoint: String,
    pub registration_endpoint: String,
    pub scopes_supported: Vec<String>,
    pub code_challenge_methods_supported: Vec<String>,
    pub token_endpoint_auth_methods_supported: Vec<String>,
    pub grant_types_supported: Vec<String>,
    pub response_types_supported: Vec<String>,
}

/// Lo que necesita un inicio de sesión.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub metadata: Metadata,
    pub client_id: String,
    pub redirect_uri: String,
    pub scopes: Vec<String>,
}

const LIMITE_CUERPO: usize = 1 << 20;

/// Tope de `expires_in` (1 año): `TimeDelta::seconds` hace panic por encima
/// de `i64::MAX/1000` y la suma con la fecha actual también desborda. Una
/// respuesta rota o maliciosa no puede tumbar el canje ni la renovación.
const TOPE_EXPIRES_IN: i64 = 365 * 24 * 60 * 60;

/// Expiración a partir de `expires_in`, acotada a un máximo razonable.
/// Sin valor positivo queda el cero («sin vencimiento»): renovar en
/// bucle sería peor.
pub(crate) fn expiracion_acotada(expires_in: i64) -> DateTime<FixedOffset> {
    if expires_in <= 0 {
        return zero_expiry();
    }
    (Utc::now() + chrono::Duration::seconds(expires_in.min(TOPE_EXPIRES_IN))).fixed_offset()
}

/// Lee la metadata desde su ubicación conocida.
pub async fn discover(http: &reqwest::Client, issuer: &str) -> Result<Metadata, Error> {
    let endpoint = format!(
        "{}/.well-known/oauth-authorization-server",
        issuer.trim_end_matches('/')
    );
    let respuesta = http
        .get(endpoint)
        .header(ACCEPT, "application/json")
        .send()
        .await?;
    let status = respuesta.status();
    if status != reqwest::StatusCode::OK {
        return Err(Error::Discover(status.to_string()));
    }
    let cuerpo = leer_cuerpo(respuesta).await?;
    let metadata: Metadata = serde_json::from_slice(&cuerpo)?;
    if metadata.authorization_endpoint.is_empty() || metadata.token_endpoint.is_empty() {
        return Err(Error::IncompleteMetadata);
    }
    Ok(metadata)
}

/// Cambia el token de refresco por uno de acceso.
pub async fn refresh(
    http: &reqwest::Client,
    config: Config,
    refresh_token: &str,
) -> Result<Token, Error> {
    if refresh_token.is_empty() {
        return Err(Error::NoRefreshToken);
    }
    let mut url = reqwest::Url::parse(&config.metadata.token_endpoint)
        .map_err(|error| Error::InvalidUrl(error.to_string()))?;
    url.query_pairs_mut()
        .append_pair("grant_type", "refresh_token")
        .append_pair("refresh_token", refresh_token)
        .append_pair("client_id", &config.client_id);
    let formulario = url.query().unwrap_or_default().to_owned();
    url.set_query(None);
    let respuesta = http
        .post(url)
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(ACCEPT, "application/json")
        .body(formulario)
        .send()
        .await?;
    let status = respuesta.status();
    let crudo = leer_cuerpo(respuesta).await?;
    let cuerpo = String::from_utf8_lossy(&crudo).trim().to_owned();
    if status == reqwest::StatusCode::BAD_REQUEST || status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(Error::RefreshRejected(cuerpo));
    }
    if status != reqwest::StatusCode::OK {
        return Err(Error::Renew(format!("{status} {cuerpo}")));
    }
    let payload: Renovacion = serde_json::from_slice(&crudo)?;
    if payload.access_token.is_empty() {
        return Err(Error::NoAccessToken);
    }
    // Sin `expires_in` la expiración queda en cero, no en el pasado
    // Renovar en bucle sería peor. El tope evita
    // el panic con un valor enorme del servidor.
    let mut next = Token {
        access_token: payload.access_token,
        refresh_token: payload.refresh_token,
        token_type: payload.token_type,
        scope: payload.scope,
        expiry: expiracion_acotada(payload.expires_in),
    };
    if next.refresh_token.is_empty() {
        next.refresh_token = refresh_token.to_owned();
    }
    Ok(next)
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Renovacion {
    access_token: String,
    refresh_token: String,
    token_type: String,
    scope: String,
    expires_in: i64,
}

/// Equivale a `io.LimitReader(res.Body, 1<<20)`.
pub(crate) async fn leer_cuerpo(mut respuesta: reqwest::Response) -> Result<Vec<u8>, Error> {
    let mut cuerpo = Vec::new();
    while let Some(fragmento) = respuesta.chunk().await? {
        let espacio = LIMITE_CUERPO.saturating_sub(cuerpo.len());
        if espacio == 0 {
            break;
        }
        cuerpo.extend_from_slice(&fragmento[..fragmento.len().min(espacio)]);
    }
    Ok(cuerpo)
}
