//! Inicio de sesión OAuth 2.1: URL de autorización, registro y canje de código.
//!
//! Puerto de `AuthorizeURL` y `Exchange` (`oauth.go:84-157`) y de `Register`
//! (`register.go:18-58`). El callback loopback vive en `orus-app`.

use reqwest::header::{ACCEPT, CONTENT_TYPE};
use serde::{Deserialize, Serialize};

use crate::oauth::{expiracion_acotada, leer_cuerpo};
use crate::{Config, Error, Metadata, Token};

/// Ruta del callback loopback (`callback.go:15-135`).
pub const CALLBACK_PATH: &str = "/oauth_callback";

/// La dirección que abre el navegador (`oauth.go:86`). El verificador nunca
/// viaja aquí, solo su reto.
pub fn authorize_url(config: &Config, state: &str, challenge: &str) -> Result<String, Error> {
    let mut url = reqwest::Url::parse(&config.metadata.authorization_endpoint)
        .map_err(|error| Error::InvalidUrl(error.to_string()))?;
    // Go conserva la query que ya traiga el endpoint y la reordena en
    // `Values.Encode()`, que ordena claves (y valores) alfabéticamente.
    let mut pares: Vec<(String, String)> = url
        .query_pairs()
        .map(|(clave, valor)| (clave.into_owned(), valor.into_owned()))
        .collect();
    let scope = config.scopes.join(" ");
    let mut ajustes = vec![
        ("response_type", "code"),
        ("client_id", config.client_id.as_str()),
        ("redirect_uri", config.redirect_uri.as_str()),
        ("state", state),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256"),
    ];
    if !config.scopes.is_empty() {
        ajustes.push(("scope", scope.as_str()));
    }
    for (clave, valor) in ajustes {
        pares.retain(|(existente, _)| existente != clave);
        pares.push((clave.to_owned(), valor.to_owned()));
    }
    pares.sort_by(|a, b| a.0.cmp(&b.0));
    url.set_query(None);
    {
        let mut query = url.query_pairs_mut();
        for (clave, valor) in &pares {
            query.append_pair(clave, valor);
        }
    }
    Ok(url.to_string())
}

#[derive(Serialize)]
struct Registro<'a> {
    client_name: &'a str,
    grant_types: [&'a str; 2],
    redirect_uris: [&'a str; 1],
    response_types: [&'a str; 1],
    token_endpoint_auth_method: &'a str,
}

/// Registra un cliente público (`register.go:18`).
pub async fn register(
    http: &reqwest::Client,
    metadata: &Metadata,
    redirect_uri: &str,
    client_name: &str,
) -> Result<String, Error> {
    if metadata.registration_endpoint.is_empty() {
        return Err(Error::RegisterEndpoint);
    }
    let cuerpo = serde_json::to_vec(&Registro {
        client_name,
        grant_types: ["authorization_code", "refresh_token"],
        redirect_uris: [redirect_uri],
        response_types: ["code"],
        token_endpoint_auth_method: "none",
    })?;
    let respuesta = http
        .post(&metadata.registration_endpoint)
        .header(CONTENT_TYPE, "application/json")
        .header(ACCEPT, "application/json")
        .body(cuerpo)
        .send()
        .await?;
    let status = respuesta.status();
    let crudo = leer_cuerpo(respuesta).await?;
    let texto = String::from_utf8_lossy(&crudo).trim().to_owned();
    if !status.is_success() {
        return Err(Error::Register(format!("{status} {texto}")));
    }
    #[derive(Default, Deserialize)]
    #[serde(default)]
    struct Respuesta {
        client_id: String,
    }
    let salida: Respuesta = serde_json::from_slice(&crudo)?;
    if salida.client_id.is_empty() {
        return Err(Error::NoClientId);
    }
    Ok(salida.client_id)
}

/// Canjea el código por tokens (`oauth.go:107`). El `redirect_uri` y el
/// verificador se envían otra vez porque el servidor ata el código a ambos.
pub async fn exchange(
    http: &reqwest::Client,
    config: &Config,
    code: &str,
    verifier: &str,
) -> Result<Token, Error> {
    let mut url = reqwest::Url::parse(&config.metadata.token_endpoint)
        .map_err(|error| Error::InvalidUrl(error.to_string()))?;
    url.query_pairs_mut()
        .append_pair("client_id", &config.client_id)
        .append_pair("code", code)
        .append_pair("code_verifier", verifier)
        .append_pair("grant_type", "authorization_code")
        .append_pair("redirect_uri", &config.redirect_uri);
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
    if status != reqwest::StatusCode::OK {
        return Err(Error::Exchange(format!("{status} {cuerpo}")));
    }
    let payload: Canje = serde_json::from_slice(&crudo)?;
    if payload.access_token.is_empty() {
        return Err(Error::ExchangeNoAccessToken);
    }
    // Sin `expires_in` la expiración queda en cero, no en el pasado
    // (`oauth.go:143-149`): renovar en bucle sería peor. El tope evita el
    // panic con un valor enorme del servidor (#74).
    let token = Token {
        access_token: payload.access_token,
        refresh_token: payload.refresh_token,
        token_type: payload.token_type,
        scope: payload.scope,
        expiry: expiracion_acotada(payload.expires_in),
    };
    Ok(token)
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Canje {
    access_token: String,
    refresh_token: String,
    token_type: String,
    scope: String,
    expires_in: i64,
}
