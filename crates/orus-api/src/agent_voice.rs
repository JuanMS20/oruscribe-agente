//! Voz opcional: transcripción y lectura mediante un servidor compatible con OpenAI.
//!
//! La configuración sale de `settings_snapshot().agent` (`voice_stt_url`,
//! `voice_tts_url`); la clave del proveedor solo viaja si el servidor de voz
//! vive en el mismo host (`App::clave_de_voz`).

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::extract::{FromRequest, Multipart, Request, State as AxumState};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response;
use orus_app::App;
use serde::{Deserialize, Deserializer};
use serde_json::json;

use crate::{ErrorRuta, decode, write_json};

/// Tope del cuerpo multipart de audio (10 MiB).
pub(crate) const TOPE_AUDIO: usize = 10 << 20;
/// Tope de caracteres del texto para lectura en voz alta.
const TOPE_TEXTO: usize = 6000;
/// Tope de respuesta del servidor de voz (8 MiB).
const TOPE_RESPUESTA: usize = 8 << 20;
/// Tiempo máximo de espera por petición al servidor de voz.
const ESPERA_VOZ: Duration = Duration::from_secs(120);

const ERROR_AUDIO: &str = "el audio no se pudo leer o pasa de 10 MB";
const SIN_DICTADO: &str = "el dictado no está configurado: abre Ajustes del chat y escribe la dirección de tu servidor de voz";
const SIN_LECTURA: &str = "la lectura en voz alta no está configurada: abre Ajustes del chat y escribe la dirección de tu servidor de voz";

fn error_audio() -> ErrorRuta {
    ErrorRuta::Invalido(ERROR_AUDIO.to_owned())
}

struct ConfigVoz {
    stt_url: String,
    stt_model: String,
    tts_url: String,
    tts_model: String,
    tts_voice: String,
    locale: &'static str,
}

impl ConfigVoz {
    fn desde(app: &App) -> Self {
        let settings = app.settings_snapshot();
        Self {
            stt_url: settings.agent.voice_stt_url,
            stt_model: settings.agent.voice_stt_model,
            tts_url: settings.agent.voice_tts_url,
            tts_model: settings.agent.voice_tts_model,
            tts_voice: settings.agent.voice_tts_voice,
            locale: settings.locale.as_str(),
        }
    }
}

fn endpoint_de_voz(base: &str, recurso: &str) -> String {
    let base = base.trim_end_matches('/');
    if base.ends_with(&format!("/{recurso}")) {
        base.to_owned()
    } else {
        format!("{base}/{recurso}")
    }
}

fn cliente_de_voz() -> Result<reqwest::Client, ErrorRuta> {
    reqwest::Client::builder()
        .timeout(ESPERA_VOZ)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| ErrorRuta::Lectura(error.to_string()))
}

async fn leer_respuesta(mut respuesta: reqwest::Response) -> Result<Vec<u8>, ErrorRuta> {
    let mut datos = Vec::new();
    let mut excedido = false;
    while let Some(trozo) = respuesta.chunk().await.map_err(|error| {
        ErrorRuta::Lectura(format!("no se pudo hablar con el servidor de voz: {error}"))
    })? {
        let restante = TOPE_RESPUESTA.saturating_sub(datos.len());
        if trozo.len() > restante {
            excedido = true;
            break;
        }
        datos.extend_from_slice(&trozo);
    }
    if excedido {
        return Err(ErrorRuta::Lectura(format!(
            "la respuesta del servidor de voz pasa de {} MB",
            TOPE_RESPUESTA >> 20
        )));
    }
    Ok(datos)
}

fn error_de_red(error: reqwest::Error) -> ErrorRuta {
    ErrorRuta::Lectura(format!("no se pudo hablar con el servidor de voz: {error}"))
}

fn texto_de_json<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Deserialize, Default)]
struct SalidaTranscripcion {
    #[serde(default, deserialize_with = "texto_de_json")]
    text: String,
}

#[derive(Deserialize, Default)]
struct EntradaLectura {
    #[serde(default, deserialize_with = "texto_de_json")]
    text: String,
}

async fn transcribir(app: &App, audio: &[u8], nombre: &str) -> Result<String, ErrorRuta> {
    let cfg = ConfigVoz::desde(app);
    if cfg.stt_url.trim().is_empty() {
        return Err(ErrorRuta::Invalido(SIN_DICTADO.to_owned()));
    }
    let modelo = if cfg.stt_model.is_empty() {
        "whisper-1"
    } else {
        cfg.stt_model.as_str()
    };
    let mut formulario = reqwest::multipart::Form::new()
        .part(
            "file",
            reqwest::multipart::Part::bytes(audio.to_vec()).file_name(nombre.to_owned()),
        )
        .text("model", modelo.to_owned());
    if cfg.locale == "es" || cfg.locale == "en" {
        formulario = formulario.text("language", cfg.locale);
    }

    let url = endpoint_de_voz(&cfg.stt_url, "audio/transcriptions");
    let mut peticion = cliente_de_voz()?.post(&url).multipart(formulario);
    if let Some(clave) = app.clave_de_voz(&url) {
        peticion = peticion.bearer_auth(clave);
    }
    let respuesta = peticion.send().await.map_err(error_de_red)?;
    let estado = respuesta.status().as_u16();
    let datos = leer_respuesta(respuesta).await?;
    if estado != 200 {
        return Err(ErrorRuta::Lectura(format!(
            "el servidor de voz respondió {estado}"
        )));
    }
    let salida: Option<SalidaTranscripcion> = serde_json::from_slice(&datos).map_err(|_| {
        ErrorRuta::Lectura("el servidor de voz no devolvió texto entendible".to_owned())
    })?;
    let texto = salida.unwrap_or_default().text.trim().to_owned();
    if texto.is_empty() {
        return Err(ErrorRuta::Lectura(
            "el servidor de voz no entendió el audio".to_owned(),
        ));
    }
    Ok(texto)
}

async fn hablar(app: &App, texto: &str) -> Result<(Vec<u8>, String), ErrorRuta> {
    let texto = texto.trim();
    if texto.is_empty() {
        return Err(ErrorRuta::Invalido("no hay texto para leer".to_owned()));
    }
    let cfg = ConfigVoz::desde(app);
    if cfg.tts_url.is_empty() {
        return Err(ErrorRuta::Invalido(SIN_LECTURA.to_owned()));
    }
    let texto: String = texto.chars().take(TOPE_TEXTO).collect();
    let modelo = if cfg.tts_model.is_empty() {
        "tts-1"
    } else {
        cfg.tts_model.as_str()
    };
    let voz = if cfg.tts_voice.is_empty() {
        "alloy"
    } else {
        cfg.tts_voice.as_str()
    };
    let cuerpo = serde_json::to_vec(&json!({
        "model": modelo,
        "input": texto,
        "voice": voz,
        "response_format": "mp3",
    }))
    .map_err(|error| ErrorRuta::Lectura(error.to_string()))?;

    let url = endpoint_de_voz(&cfg.tts_url, "audio/speech");
    let mut peticion = cliente_de_voz()?
        .post(&url)
        .header(header::CONTENT_TYPE, "application/json")
        .body(cuerpo);
    if let Some(clave) = app.clave_de_voz(&url) {
        peticion = peticion.bearer_auth(clave);
    }
    let respuesta = peticion.send().await.map_err(error_de_red)?;
    let estado = respuesta.status().as_u16();
    let tipo = respuesta
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|valor| valor.to_str().ok())
        .filter(|valor| !valor.is_empty())
        .unwrap_or("audio/mpeg")
        .to_owned();
    let audio = leer_respuesta(respuesta).await?;
    if estado != 200 {
        return Err(ErrorRuta::Lectura(format!(
            "el servidor de voz respondió {estado}"
        )));
    }
    Ok((audio, tipo))
}

fn nombre_base(nombre: &str) -> String {
    Path::new(nombre)
        .file_name()
        .map(|parte| parte.to_string_lossy().into_owned())
        .unwrap_or_else(|| nombre.to_owned())
}

pub(crate) async fn transcribir_ruta(
    AxumState(app): AxumState<Arc<App>>,
    request: Request,
) -> Result<Response, ErrorRuta> {
    let (audio, nombre) = leer_audio(request).await?;
    let texto = transcribir(&app, &audio, &nombre).await?;
    Ok(write_json(StatusCode::OK, &json!({ "text": texto })))
}

async fn leer_audio(request: Request) -> Result<(Vec<u8>, String), ErrorRuta> {
    let (partes, cuerpo) = request.into_parts();
    let bytes = to_bytes(cuerpo, TOPE_AUDIO.saturating_add(1))
        .await
        .map_err(|_| error_audio())?;
    if bytes.len() > TOPE_AUDIO {
        return Err(error_audio());
    }
    let peticion = Request::from_parts(partes, Body::from(bytes));
    let mut multipart = Multipart::from_request(peticion, &())
        .await
        .map_err(|_| error_audio())?;

    let mut encontrado = None;
    while let Some(mut campo) = multipart.next_field().await.map_err(|_| error_audio())? {
        let nombre_campo = campo.name().unwrap_or_default().to_owned();
        let nombre_archivo = campo.file_name().map(nombre_base);
        let mut datos = Vec::new();
        while let Some(trozo) = campo.chunk().await.map_err(|_| error_audio())? {
            datos.extend_from_slice(&trozo);
        }
        if encontrado.is_none()
            && nombre_campo == "audio"
            && let Some(nombre) = nombre_archivo
            && !nombre.is_empty()
        {
            encontrado = Some((datos, nombre));
        }
    }
    encontrado.ok_or_else(|| ErrorRuta::Invalido("falta el audio".to_owned()))
}

pub(crate) async fn hablar_ruta(
    AxumState(app): AxumState<Arc<App>>,
    body: Body,
) -> Result<Response, ErrorRuta> {
    let entrada: Option<EntradaLectura> = decode(body).await?;
    let texto = entrada.unwrap_or_default().text;
    let (audio, tipo) = hablar(&app, &texto).await?;
    let mut respuesta = Response::new(Body::from(audio));
    *respuesta.status_mut() = StatusCode::OK;
    let tipo =
        HeaderValue::from_str(&tipo).unwrap_or_else(|_| HeaderValue::from_static("audio/mpeg"));
    respuesta.headers_mut().insert(header::CONTENT_TYPE, tipo);
    respuesta
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(respuesta)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_de_voz_completa_o_respeta_la_ruta() {
        assert_eq!(
            endpoint_de_voz("http://127.0.0.1:8080/v1/", "audio/transcriptions"),
            "http://127.0.0.1:8080/v1/audio/transcriptions"
        );
        assert_eq!(
            endpoint_de_voz(
                "http://127.0.0.1:8080/v1/audio/transcriptions",
                "audio/transcriptions"
            ),
            "http://127.0.0.1:8080/v1/audio/transcriptions"
        );
    }
}
