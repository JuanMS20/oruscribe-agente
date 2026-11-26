//! Diagnóstico que la persona copia cuando algo falla.
//!
//! Lleva versión, sistema y estados; no lleva nombres, correos, mensajes ni
//! claves, y todo el texto pasa por [`redactar`] antes de devolverse.

use std::sync::{Arc, LazyLock};

use axum::Extension;
use axum::body::Body;
use axum::extract::Request;
use axum::extract::State as AxumState;
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::Response;
use orus_app::{App, SaludTelemetria, Status};
use regex::Regex;
use serde::Deserialize;
use serde_json::json;

use crate::{BellStatus, decode_con_limite, write_err, write_json};

const TOPE_REPORTE: usize = 16 << 10;
const MAX_TRAIL: usize = 16;
const MAX_TRAIL_CHARS: usize = 160;
const TOPE_CUERPO_ERROR: usize = 64 << 10;

#[derive(Deserialize, Default)]
#[serde(default)]
struct CuerpoReporte {
    category: String,
    message: String,
    detail: String,
    trail: Vec<String>,
}

/// `POST /api/diagnostics/report`: recibe errores técnicos de la UI (JS,
/// micrófono, portapapeles) y los pasa al colector redactado y con tope. El
/// `trail` es el rastro de pasos de la pantalla justo antes del fallo.
pub(crate) async fn reportar(AxumState(app): AxumState<Arc<App>>, body: Body) -> Response {
    let cuerpo: CuerpoReporte = match decode_con_limite(body, TOPE_REPORTE).await {
        Ok(cuerpo) => cuerpo,
        Err(error) => return write_err(error),
    };
    let categoria = if cuerpo.category.trim().is_empty() {
        "ui.error"
    } else {
        cuerpo.category.trim()
    };
    let mensaje = cuerpo.message.trim();
    if !mensaje.is_empty() {
        let trail: Vec<String> = cuerpo
            .trail
            .iter()
            .rev()
            .take(MAX_TRAIL)
            .rev()
            .map(|paso| paso.chars().take(MAX_TRAIL_CHARS).collect())
            .collect();
        let mut extra = serde_json::Map::new();
        if !cuerpo.detail.trim().is_empty() {
            extra.insert("detail".to_owned(), json!(cuerpo.detail.trim()));
        }
        if !trail.is_empty() {
            extra.insert("ui_trail".to_owned(), json!(trail));
        }
        let extra = if extra.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::Value::Object(extra)
        };
        app.report_error(categoria, mensaje, extra);
    }
    write_json(StatusCode::OK, &json!({ "ok": true }))
}

/// Vigila cada respuesta del borde local: un 5xx es un error de la app y se
/// reporta con la ruta y el texto del error; las escrituras y los 4xx quedan
/// como migas para el siguiente reporte. Los GET correctos (sondeo) no.
pub(crate) async fn vigilar(
    AxumState(app): AxumState<Arc<App>>,
    peticion: Request,
    siguiente: Next,
) -> Response {
    let metodo = peticion.method().clone();
    let ruta = peticion.uri().path().to_owned();
    let respuesta = siguiente.run(peticion).await;
    let status = respuesta.status();
    if ruta == "/api/diagnostics/report" {
        return respuesta;
    }
    if status.is_server_error() {
        let (partes, cuerpo) = respuesta.into_parts();
        let bytes = axum::body::to_bytes(cuerpo, TOPE_CUERPO_ERROR)
            .await
            .unwrap_or_default();
        let texto = serde_json::from_slice::<serde_json::Value>(&bytes)
            .ok()
            .and_then(|valor| {
                valor
                    .get("error")
                    .and_then(|e| e.as_str().map(str::to_owned))
            })
            .unwrap_or_default();
        app.report_error(
            "api.http",
            &format!("{metodo} {ruta} respondió {}: {texto}", status.as_u16()),
            json!({ "kind": "http", "status": status.as_u16() }),
        );
        return Response::from_parts(partes, Body::from(bytes));
    }
    if status == StatusCode::TOO_MANY_REQUESTS {
        app.telemetry().report(
            "api.http",
            "warning",
            &format!("{metodo} {ruta} respondió 429: tope de llamadas alcanzado"),
            json!({ "kind": "rate_limit", "status": 429 }),
        );
    }
    if metodo != axum::http::Method::GET || status.is_client_error() {
        app.telemetry()
            .breadcrumb("http", &format!("{metodo} {ruta} {}", status.as_u16()));
    }
    respuesta
}

const PATRON_SECRETOS: &str = concat!(
    r"(?i)\bsk-[A-Za-z0-9_-]{12,}|",
    r"\bsyn_[A-Za-z0-9]{8,}|",
    r"\boru_[A-Za-z0-9]{8,}|",
    r"\bBearer\s+[A-Za-z0-9._+/=-]{8,}|",
    r"\bBasic\s+[A-Za-z0-9._+/=-]{8,}|",
    r"eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}|",
    r"\b(api[_-]?key|access[_-]?token|refresh[_-]?token|id[_-]?token|session[_-]?token|client[_-]?secret|secret|password|signature|sig|code|token)=[^\s&;]+|",
    r"\b(authorization|proxy[_-]?authorization|cookie|set-cookie|x-api-key)\s*:\s*[^\n;]+|",
    r"/(root|home)/[A-Za-z0-9._-]+|",
    r"[A-Za-z]:[\\/]+Users[\\/]+[A-Za-z0-9._-]+"
);

static SECRETOS: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(PATRON_SECRETOS).ok());
static USERINFO: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"(?i)(https?://)[^/@\s]+@").ok());

/// Tacha claves de proveedor, tokens de Oruscribe, portadores y JWT.
pub(crate) fn redactar(texto: &str) -> String {
    let tachado = match &*SECRETOS {
        Some(patrones) => patrones.replace_all(texto, "***").into_owned(),
        None => texto.to_owned(),
    };
    match &*USERINFO {
        Some(patron) => patron.replace_all(&tachado, "${1}***@").into_owned(),
        None => tachado,
    }
}

/// `GET /api/diagnostics`.
pub(crate) async fn diagnostico(
    AxumState(app): AxumState<Arc<App>>,
    Extension(version): Extension<String>,
    Extension(timbre): Extension<Arc<dyn BellStatus>>,
) -> Response {
    write_json(
        StatusCode::OK,
        &json!({ "text": texto(&app, &version, timbre.as_ref()) }),
    )
}

/// El texto completo, en el orden.
fn texto(app: &App, version: &str, timbre: &dyn BellStatus) -> String {
    let mut texto = String::new();
    texto.push_str(&format!("Oruscribe Agente {version}\n"));
    texto.push_str(&format!(
        "Sistema: {}/{}\n",
        std::env::consts::OS,
        nombre_de_arquitectura(std::env::consts::ARCH)
    ));
    texto.push_str("Motor: Rust\n");
    texto.push_str(&format!(
        "Estado de la sesión: {}\n",
        nombre_de_estado(app.state().status)
    ));
    // El servidor headless no pinta avatares.
    texto.push_str("Avatares: 0 cargados\n");
    let error_timbre = timbre.error();
    if error_timbre.is_empty() {
        texto.push_str("Timbre: sin errores\n");
    } else {
        texto.push_str(&format!("Timbre: {error_timbre}\n"));
    }
    if let Some(q) = app.mcp_quota() {
        texto.push_str(&format!(
            "Cuota MCP: {}/{} min · {}/{} h\n",
            q.used_minute, q.limit_minute, q.used_hour, q.limit_hour
        ));
    }
    let error_sesion = app.last_error_sesion();
    if !error_sesion.is_empty() {
        texto.push_str(&format!("Último error de sesión: {error_sesion}\n"));
    }
    let agente = app.agent();
    let ocupado = agente.as_ref().is_some_and(|agente| agente.busy());
    if ocupado {
        texto.push_str("Agente: ejecutando\n");
    } else {
        texto.push_str("Agente: en reposo\n");
    }
    if let Some(error) = agente
        .as_ref()
        .map(|agente| agente.error())
        .filter(|error| !error.is_empty())
    {
        texto.push_str(&format!("Último error del agente: {error}\n"));
    }
    let conversacion = agente
        .as_ref()
        .and_then(|agente| agente.conversation_id())
        .is_some();
    if conversacion {
        texto.push_str("Conversación abierta: sí\n");
    } else {
        texto.push_str("Conversación abierta: no\n");
    }
    // Sin `Watch` no hay cola de despertar ni contadores.
    texto.push_str("Despertar: 0 eventos en cola, 0 ejecuciones hoy, 0 tokens hoy\n");
    let tel = app.telemetry();
    match tel.salud() {
        SaludTelemetria::Local => {
            texto.push_str("Telemetría: local (sin consentimiento o sin DSN configurado).\n");
        }
        SaludTelemetria::Configurada => {
            let entrega = tel.entrega();
            texto.push_str(&format!(
                "Telemetría: configurada; {} entregados, {} fallos.\n",
                entrega.entregados, entrega.fallos
            ));
        }
        SaludTelemetria::Entregando => {
            texto.push_str("Telemetría: entregando (envíos en curso).\n");
        }
        SaludTelemetria::Fallando => {
            let entrega = tel.entrega();
            texto.push_str(&format!(
                "Telemetría: fallando; {} fallos; último: {}.\n",
                entrega.fallos, entrega.ultimo_error
            ));
        }
    }
    let recientes = tel.recent();
    if !recientes.is_empty() {
        texto.push_str("Últimos eventos locales:\n");
        for ev in recientes.iter().rev().take(5) {
            texto.push_str(&format!(
                "- [{}] {}: {}\n",
                ev.at.format("%H:%M:%S"),
                ev.category,
                ev.message
            ));
        }
    }
    let migas = tel.migas();
    if !migas.is_empty() {
        texto.push_str("Últimas migas:\n");
        for miga in migas.iter().rev().take(8).rev() {
            texto.push_str(&format!(
                "- [{}] {}: {}\n",
                miga.at.format("%H:%M:%S"),
                miga.category,
                miga.message
            ));
        }
    }
    texto.push_str("Sin telemetría: este texto no sale de este equipo salvo que lo pegues tú.\n");
    redactar(&texto)
}

/// El diagnóstico imprime `amd64`, no `x86_64`: sin el mapeo la
/// línea no coincide.
fn nombre_de_arquitectura(arch: &str) -> &str {
    match arch {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        "x86" => "386",
        "arm" => "arm",
        otro => otro,
    }
}

fn nombre_de_estado(status: Status) -> &'static str {
    match status {
        Status::LoggedOut => "logged_out",
        Status::Connecting => "connecting",
        Status::Ready => "ready",
        Status::Failed => "failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nombre_de_arquitectura_traduce_los_nombres_de_rust() {
        assert_eq!(nombre_de_arquitectura("x86_64"), "amd64");
        assert_eq!(nombre_de_arquitectura("aarch64"), "arm64");
        assert_eq!(nombre_de_arquitectura("x86"), "386");
        assert_eq!(nombre_de_arquitectura("arm"), "arm");
        assert_eq!(nombre_de_arquitectura("riscv64"), "riscv64");
    }

    #[test]
    fn nombre_de_estado_usa_los_cuatro_valores() {
        assert_eq!(nombre_de_estado(Status::LoggedOut), "logged_out");
        assert_eq!(nombre_de_estado(Status::Connecting), "connecting");
        assert_eq!(nombre_de_estado(Status::Ready), "ready");
        assert_eq!(nombre_de_estado(Status::Failed), "failed");
    }

    #[test]
    fn redactar_tacha_urls_headers_y_rutas_del_sistema() {
        let bruto = concat!(
            "GET https://api.test/v1?api_key=sk-proj-abcdef123456789&sig=firma123456 ",
            "Authorization: Basic dXNlcjpwYXNz en /root/app y /home/osman/app y C:\\Users\\Osman\\app"
        );
        let limpio = redactar(bruto);
        assert!(!limpio.contains("abcdef123456789"), "{limpio}");
        assert!(!limpio.contains("firma123456"), "{limpio}");
        assert!(!limpio.contains("dXNlcjpwYXNz"), "{limpio}");
        assert!(!limpio.contains("Osman"), "{limpio}");
        assert!(!limpio.contains("/root/"), "{limpio}");
        assert!(!limpio.contains("/home/osman"), "{limpio}");
        assert!(limpio.contains("***"), "{limpio}");
    }

    #[test]
    fn redactar_tacha_userinfo_del_dsn_en_un_error_de_red() {
        let error = concat!(
            "error sending request for url ",
            "(https://pubkey9f3a2b@o4501.ingest.sentry.io/api/98765/store/)"
        );
        let limpio = redactar(error);
        assert!(!limpio.contains("pubkey9f3a2b"), "{limpio}");
        assert!(
            limpio.contains("https://***@o4501.ingest.sentry.io/api/98765/store/"),
            "{limpio}"
        );
    }

    #[test]
    fn redactar_tacha_bearer_y_no_tacha_palabras_con_sk() {
        let token = "c".repeat(12);
        let con_bearer = format!("Authorization: Bearer {token}");
        let tachado = redactar(&con_bearer);
        assert!(!tachado.contains(&token));
        assert!(tachado.contains("***"));

        let normal = "task-management-system y disk-space-calculation";
        assert_eq!(redactar(normal), normal);
    }
}
