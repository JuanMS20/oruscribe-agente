//! Telemetría de errores hacia Sentry sin dependencias externas.
//!
//! - Solo envía errores técnicos (panics, fallo al abrir navegador, OAuth,
//!   micrófono, errores JS no controlados, fallos de actualización o agente).
//! - Nunca envía mensajes del chat, contenido de archivos ni secretos: todo
//!   pasa por [`redactar_evento`] (claves API, tokens, JWT y rutas de usuario).
//! - Tope estricto de 10 eventos por hora + deduplicación de 5 minutos para no
//!   saturar red ni cuota.
//! - Si el usuario apaga el interruptor en Ajustes o no hay DSN configurado,
//!   conserva solo el anillo local de los últimos 20 errores para Diagnóstico y
//!   no hace ninguna petición de red.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use rand::Rng as _;
use serde::Serialize;
use serde_json::{Map, Value, json};

const MAX_EVENTOS_POR_HORA: usize = 10;
const VENTANA_HORA: Duration = Duration::from_secs(3600);
const VENTANA_DEDUP: Duration = Duration::from_secs(300);
const MAX_RECIENTES: usize = 20;
const MAX_MENSAJE_CHARS: usize = 512;
const TIMEOUT_ASYNC: Duration = Duration::from_secs(5);
const MAX_FRAMES_STACK: usize = 32;
const MAX_FRAMES_CRUDOS: usize = 96;
const MAX_ERROR_CHARS: usize = 160;
const MAX_MIGAS: usize = 50;
const MAX_MIGA_CHARS: usize = 200;
const MAX_FATALES_POR_HORA: usize = 5;
const MAX_AVISOS_POR_HORA: usize = 5;
const MAX_CLAVES_DEDUP: usize = 256;

/// DSN público de Sentry ya validado y descompuesto en URL de envío y cabecera.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SentryDsn {
    pub store_url: String,
    pub public_key: String,
}

impl SentryDsn {
    /// Parsea un DSN estándar de Sentry (`https://<key>@<host>[/prefix]/<project_id>`).
    /// Acepta `http://127.0.0.1` o `http://localhost` solo para pruebas locales.
    pub fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        if raw.is_empty() || raw.chars().any(char::is_control) {
            return None;
        }
        let parsed = reqwest::Url::parse(raw).ok()?;
        let scheme = parsed.scheme();
        let host = parsed.host_str()?;
        let loopback = matches!(host, "127.0.0.1" | "localhost" | "::1");
        if scheme != "https" && !(scheme == "http" && loopback) {
            return None;
        }
        let public_key = parsed.username();
        if public_key.is_empty() || public_key.chars().any(|c| !c.is_ascii_alphanumeric()) {
            return None;
        }
        let segmentos: Vec<&str> = parsed.path_segments()?.filter(|s| !s.is_empty()).collect();
        let (&project_id, prefijo) = segmentos.split_last()?;
        if project_id.is_empty() || !project_id.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        let host_port = match parsed.port() {
            Some(puerto) => format!("{host}:{puerto}"),
            None => host.to_owned(),
        };
        let ruta_prefijo = if prefijo.is_empty() {
            String::new()
        } else {
            format!("/{}", prefijo.join("/"))
        };
        Some(Self {
            store_url: format!("{scheme}://{host_port}{ruta_prefijo}/api/{project_id}/store/"),
            public_key: public_key.to_owned(),
        })
    }

    pub fn auth_header(&self, version: &str) -> String {
        format!(
            "Sentry sentry_version=7, sentry_client=oruscribe-agente/{version}, sentry_key={}",
            self.public_key
        )
    }
}

/// Resuelve el DSN desde entorno (`ORUSCRIBE_SENTRY_DSN`) o valor embebido al compilar.
pub fn dsn_configurado() -> Option<SentryDsn> {
    if let Ok(env_val) = std::env::var("ORUSCRIBE_SENTRY_DSN") {
        let trimmed = env_val.trim();
        if !trimmed.is_empty() {
            return SentryDsn::parse(trimmed);
        }
    }
    option_env!("ORUSCRIBE_SENTRY_DSN").and_then(SentryDsn::parse)
}

/// Evento técnico redactado conservado en el anillo local de diagnóstico.
#[derive(Debug, Clone, Serialize)]
pub struct TelemetryEvent {
    pub at: DateTime<Utc>,
    pub category: String,
    pub level: String,
    pub message: String,
}

/// Paso previo a un error: qué hacía la app justo antes de fallar. Solo texto
/// técnico redactado (nombres de herramienta, estados), nunca contenido de
/// chat ni argumentos.
#[derive(Debug, Clone, Serialize)]
pub struct Miga {
    pub at: DateTime<Utc>,
    pub category: String,
    pub message: String,
}

#[derive(Default)]
struct Estado {
    enviados: VecDeque<Instant>,
    enviados_fatales: VecDeque<Instant>,
    enviados_avisos: VecDeque<Instant>,
    dedup: HashMap<String, Instant>,
    suprimidos: HashMap<String, u64>,
    descartados_por_tope: u64,
    migas: VecDeque<Miga>,
    recientes: VecDeque<TelemetryEvent>,
    en_vuelo: usize,
    entregados: u64,
    fallos: u64,
    ultimo_error: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaludTelemetria {
    Local,
    Configurada,
    Entregando,
    Fallando,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EntregaTelemetria {
    pub entregados: u64,
    pub fallos: u64,
    pub ultimo_error: String,
}

static COMPARTIDA: OnceLock<Arc<Telemetry>> = OnceLock::new();

/// Colector y enviador de errores con tope horario, deduplicación y redacción.
pub struct Telemetry {
    enabled: AtomicBool,
    dsn: Option<SentryDsn>,
    version: String,
    estado: Mutex<Estado>,
    sin_vuelo: Condvar,
    http: Option<reqwest::Client>,
}

impl Telemetry {
    pub fn new(enabled: bool, version: impl Into<String>, dsn: Option<SentryDsn>) -> Arc<Self> {
        let http = reqwest::Client::builder()
            .timeout(TIMEOUT_ASYNC)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .ok();
        Arc::new(Self {
            enabled: AtomicBool::new(enabled),
            dsn,
            version: version.into(),
            estado: Mutex::new(Estado::default()),
            sin_vuelo: Condvar::new(),
            http,
        })
    }

    pub fn compartida() -> Arc<Self> {
        Arc::clone(
            COMPARTIDA
                .get_or_init(|| Self::new(false, env!("CARGO_PKG_VERSION"), dsn_configurado())),
        )
    }

    pub fn from_env(enabled: bool) -> Arc<Self> {
        let tel = Self::compartida();
        tel.set_enabled(enabled);
        tel
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    pub fn salud(&self) -> SaludTelemetria {
        let st = crate::sync::lock(&self.estado);
        if !self.enabled.load(Ordering::Relaxed) || self.dsn.is_none() {
            SaludTelemetria::Local
        } else if st.en_vuelo > 0 {
            SaludTelemetria::Entregando
        } else if !st.ultimo_error.is_empty() {
            SaludTelemetria::Fallando
        } else {
            SaludTelemetria::Configurada
        }
    }

    pub fn entrega(&self) -> EntregaTelemetria {
        let st = crate::sync::lock(&self.estado);
        EntregaTelemetria {
            entregados: st.entregados,
            fallos: st.fallos,
            ultimo_error: st.ultimo_error.clone(),
        }
    }

    fn registrar_resultado(&self, resultado: Result<(), String>) {
        {
            let mut st = crate::sync::lock(&self.estado);
            st.en_vuelo = st.en_vuelo.saturating_sub(1);
            match resultado {
                Ok(()) => {
                    st.entregados += 1;
                    st.ultimo_error.clear();
                }
                Err(error) => {
                    st.fallos += 1;
                    st.ultimo_error = recortar(&error, MAX_ERROR_CHARS);
                }
            }
        }
        self.sin_vuelo.notify_all();
    }

    pub fn flush(&self, timeout: Duration) {
        let limite = Instant::now() + timeout;
        let mut st = crate::sync::lock(&self.estado);
        while st.en_vuelo > 0 {
            let restante = limite.saturating_duration_since(Instant::now());
            if restante.is_zero() {
                break;
            }
            let (siguiente, _) = self
                .sin_vuelo
                .wait_timeout(st, restante)
                .unwrap_or_else(PoisonError::into_inner);
            st = siguiente;
        }
    }

    /// Últimos eventos redactados en memoria para `GET /api/diagnostics`.
    pub fn recent(&self) -> Vec<TelemetryEvent> {
        crate::sync::lock(&self.estado)
            .recientes
            .iter()
            .cloned()
            .collect()
    }

    /// Registra un error técnico y, si la telemetría está activa y dentro de
    /// cuota, lo envía en segundo plano sin bloquear el flujo principal.
    pub fn report(self: &Arc<Self>, category: &str, level: &str, message: &str, extra: Value) {
        let Some((dsn, payload)) = self.preparar_envio(category, level, message, extra) else {
            return;
        };
        let auth = dsn.auth_header(&self.version);
        let url = dsn.store_url;
        let http = self.http.clone();
        let envio = async move { enviar_payload(http, url, auth, payload).await };

        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let guarda = Vuelo::nuevo(self);
            handle.spawn(async move {
                let resultado = envio.await;
                guarda.cerrar(resultado);
            });
            return;
        }
        let guarda = Vuelo::nuevo(self);
        let hilo = std::thread::Builder::new()
            .name("orus-telemetry".to_owned())
            .spawn(move || {
                match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => guarda.cerrar(rt.block_on(envio)),
                    Err(_) => guarda.cerrar(Err("sin runtime para telemetría".to_owned())),
                }
            });
        drop(hilo);
    }

    pub fn report_text(self: &Arc<Self>, category: &str, level: &str, message: &str) {
        self.report(category, level, message, Value::Null);
    }

    pub fn report_panic(self: &Arc<Self>, message: &str) {
        self.report_panic_con_plazo(message, Duration::from_secs(2));
    }

    fn report_panic_con_plazo(self: &Arc<Self>, message: &str, plazo: Duration) {
        let traza = std::backtrace::Backtrace::force_capture().to_string();
        let frames = sin_internos_de_panico(frames_de_backtrace(&traza));
        let extra = json!({ "stacktrace": { "frames": frames } });
        let Some((dsn, payload)) = self.preparar_envio("shell.panic", "fatal", message, extra)
        else {
            return;
        };
        let auth = dsn.auth_header(&self.version);
        let url = dsn.store_url;
        let http = self.http.clone();
        let (tx, rx) = mpsc::channel::<Result<(), String>>();
        let hilo = std::thread::Builder::new()
            .name("orus-panic-report".to_owned())
            .spawn(move || {
                let resultado = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt.block_on(enviar_payload(http, url, auth, payload)),
                    Err(error) => Err(format!("sin runtime para telemetría: {error}")),
                };
                let _ = tx.send(resultado);
            });
        if hilo.is_err() {
            self.registrar_resultado(Err("sin hilo para reporte de pánico".to_owned()));
            return;
        }
        let resultado = match rx.recv_timeout(plazo) {
            Ok(resultado) => resultado,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                Err("reporte de pánico agotó el timeout".to_owned())
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err("hilo de reporte de pánico desconectado".to_owned())
            }
        };
        self.registrar_resultado(resultado);
    }

    fn preparar_envio(
        &self,
        category: &str,
        level: &str,
        message: &str,
        extra: Value,
    ) -> Option<(SentryDsn, Value)> {
        let categoria = recortar(category.trim(), 64);
        if categoria.is_empty() {
            return None;
        }
        let nivel = match level {
            "fatal" | "error" | "warning" | "info" => level,
            _ => "error",
        };
        let limpio = redactar_evento(message);
        if limpio.is_empty() {
            return None;
        }
        let clave_dedup = format!("{categoria}|{limpio}");
        let ahora = Instant::now();
        let fatal = nivel == "fatal";

        let permitido = {
            let mut st = crate::sync::lock(&self.estado);
            if st.recientes.len() >= MAX_RECIENTES {
                st.recientes.pop_front();
            }
            st.recientes.push_back(TelemetryEvent {
                at: Utc::now(),
                category: categoria.clone(),
                level: nivel.to_owned(),
                message: limpio.clone(),
            });

            if !self.enabled.load(Ordering::Relaxed) || self.dsn.is_none() {
                None
            } else {
                st.enviados
                    .retain(|t| ahora.saturating_duration_since(*t) < VENTANA_HORA);
                st.enviados_fatales
                    .retain(|t| ahora.saturating_duration_since(*t) < VENTANA_HORA);
                st.enviados_avisos
                    .retain(|t| ahora.saturating_duration_since(*t) < VENTANA_HORA);
                st.dedup
                    .retain(|_, t| ahora.saturating_duration_since(*t) < VENTANA_DEDUP);
                if st.dedup.contains_key(&clave_dedup) {
                    if st.suprimidos.len() < MAX_CLAVES_DEDUP
                        || st.suprimidos.contains_key(&clave_dedup)
                    {
                        *st.suprimidos.entry(clave_dedup).or_insert(0) += 1;
                    }
                    None
                } else {
                    let aviso = matches!(nivel, "warning" | "info");
                    let (cola, tope) = if fatal {
                        (&mut st.enviados_fatales, MAX_FATALES_POR_HORA)
                    } else if aviso {
                        (&mut st.enviados_avisos, MAX_AVISOS_POR_HORA)
                    } else {
                        (&mut st.enviados, MAX_EVENTOS_POR_HORA)
                    };
                    if cola.len() >= tope {
                        st.descartados_por_tope += 1;
                        None
                    } else {
                        cola.push_back(ahora);
                        let repetidos = st.suprimidos.remove(&clave_dedup).unwrap_or(0);
                        st.dedup.insert(clave_dedup, ahora);
                        let descartados = std::mem::take(&mut st.descartados_por_tope);
                        st.en_vuelo += 1;
                        Some((repetidos, descartados, st.migas.iter().cloned().collect()))
                    }
                }
            }
        };

        let (repetidos, descartados, migas): (u64, u64, Vec<Miga>) = permitido?;
        let dsn = self.dsn.clone()?;
        let mut extra = extra;
        if repetidos > 0 {
            extra = con_campo(extra, "suppressed_duplicates", json!(repetidos));
        }
        if descartados > 0 {
            extra = con_campo(extra, "dropped_by_rate_limit", json!(descartados));
        }
        let payload = armar_payload(&self.version, &categoria, nivel, &limpio, extra, &migas);
        Some((dsn, payload))
    }

    /// Anota un paso previo en el anillo de migas (últimas 50). No hace red:
    /// viaja dentro de los eventos que se envíen después.
    pub fn breadcrumb(&self, category: &str, message: &str) {
        let categoria = recortar(category.trim(), 48);
        let mensaje = recortar(&redactar_evento(message), MAX_MIGA_CHARS);
        if categoria.is_empty() || mensaje.is_empty() {
            return;
        }
        let mut st = crate::sync::lock(&self.estado);
        if st.migas.len() >= MAX_MIGAS {
            st.migas.pop_front();
        }
        st.migas.push_back(Miga {
            at: Utc::now(),
            category: categoria,
            message: mensaje,
        });
    }

    /// Las migas en orden cronológico, para Diagnóstico.
    pub fn migas(&self) -> Vec<Miga> {
        crate::sync::lock(&self.estado)
            .migas
            .iter()
            .cloned()
            .collect()
    }
}

fn con_campo(extra: Value, clave: &str, valor: Value) -> Value {
    let mut mapa = match extra {
        Value::Object(mapa) => mapa,
        Value::Null => Map::new(),
        otro => {
            let mut mapa = Map::new();
            mapa.insert("value".to_owned(), otro);
            mapa
        }
    };
    mapa.insert(clave.to_owned(), valor);
    Value::Object(mapa)
}

struct Vuelo {
    tel: Arc<Telemetry>,
    motivo: &'static str,
    activo: bool,
}

impl Vuelo {
    fn nuevo(tel: &Arc<Telemetry>) -> Self {
        Self {
            tel: Arc::clone(tel),
            motivo: "envío cancelado",
            activo: true,
        }
    }

    fn cerrar(mut self, resultado: Result<(), String>) {
        self.activo = false;
        self.tel.registrar_resultado(resultado);
    }
}

impl Drop for Vuelo {
    fn drop(&mut self) {
        if self.activo {
            self.tel.registrar_resultado(Err(self.motivo.to_owned()));
        }
    }
}

async fn enviar_payload(
    http: Option<reqwest::Client>,
    url: String,
    auth: String,
    payload: Value,
) -> Result<(), String> {
    let Some(cliente) = http else {
        return Err("cliente HTTP no disponible".to_owned());
    };
    let respuesta = cliente
        .post(url)
        .header("Content-Type", "application/json")
        .header("X-Sentry-Auth", auth)
        .json(&payload)
        .send()
        .await
        .map_err(|error| error.to_string())?;
    let estado = respuesta.status();
    if estado.is_success() {
        Ok(())
    } else {
        Err(format!("Sentry respondió {estado}"))
    }
}

fn armar_payload(
    version: &str,
    category: &str,
    level: &str,
    message: &str,
    extra: Value,
    migas: &[Miga],
) -> Value {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill(&mut bytes);
    let event_id = hex::encode(bytes);

    let mut tags = Map::new();
    tags.insert(
        "os".to_owned(),
        Value::String(std::env::consts::OS.to_owned()),
    );
    tags.insert(
        "arch".to_owned(),
        Value::String(std::env::consts::ARCH.to_owned()),
    );
    tags.insert("category".to_owned(), Value::String(category.to_owned()));
    if let Ok(desktop) = std::env::var("XDG_CURRENT_DESKTOP")
        && !desktop.trim().is_empty()
    {
        tags.insert(
            "desktop".to_owned(),
            Value::String(recortar(desktop.trim(), 48)),
        );
    }
    if let Ok(session) = std::env::var("XDG_SESSION_TYPE")
        && !session.trim().is_empty()
    {
        tags.insert(
            "session_type".to_owned(),
            Value::String(recortar(session.trim(), 24)),
        );
    }

    let frames = extra
        .get("stacktrace")
        .and_then(|valor| valor.get("frames"))
        .and_then(Value::as_array)
        .filter(|frames| !frames.is_empty())
        .map(|frames| {
            let mut frames: Vec<Value> = frames.iter().take(MAX_FRAMES_STACK).cloned().collect();
            frames.reverse();
            frames
        });
    let mut extra_limpio = redactar_valor(extra);
    if let Some(mapa) = extra_limpio.as_object_mut() {
        mapa.remove("stacktrace");
    }
    for clave in ["kind", "status", "provider", "mcp_tool"] {
        let valor = match extra_limpio.get(clave) {
            Some(Value::String(texto)) if !texto.is_empty() => recortar(texto, 64),
            Some(Value::Number(numero)) => numero.to_string(),
            _ => continue,
        };
        tags.insert(clave.to_owned(), Value::String(valor));
    }
    let mensaje = recortar(message, MAX_MENSAJE_CHARS);
    let mut payload = json!({
        "event_id": event_id,
        "timestamp": Utc::now().to_rfc3339(),
        "platform": "native",
        "level": level,
        "release": format!("oruscribe-agente@{version}"),
        "environment": if cfg!(debug_assertions) { "debug" } else { "release" },
        "sdk": { "name": "oruscribe-sentry", "version": version },
        "logger": category,
        "tags": Value::Object(tags),
        "message": { "formatted": mensaje },
        "extra": extra_limpio,
    });
    if !migas.is_empty() {
        let valores: Vec<Value> = migas
            .iter()
            .map(|miga| {
                json!({
                    "timestamp": miga.at.to_rfc3339(),
                    "type": "default",
                    "level": "info",
                    "category": miga.category,
                    "message": miga.message,
                })
            })
            .collect();
        payload["breadcrumbs"] = json!({ "values": valores });
    }
    if let Some(frames) = frames {
        payload["exception"] = json!({
            "values": [{
                "type": "panic",
                "value": mensaje.clone(),
                "mechanism": { "type": "panic", "handled": false },
                "stacktrace": { "frames": frames }
            }]
        });
    }
    payload
}

fn redactar_valor(valor: Value) -> Value {
    match valor {
        Value::String(s) => Value::String(redactar_evento(&s)),
        Value::Array(items) => {
            Value::Array(items.into_iter().take(16).map(redactar_valor).collect())
        }
        Value::Object(map) => {
            let mut limpio = Map::new();
            for (k, v) in map.into_iter().take(16) {
                let k_low = k.to_ascii_lowercase();
                if k_low.contains("token")
                    || k_low.contains("secret")
                    || k_low.contains("key")
                    || k_low.contains("password")
                    || k_low.contains("passwd")
                    || k_low.contains("auth")
                    || k_low.contains("credential")
                    || k_low.contains("cookie")
                    || k_low.contains("signature")
                    || k_low.contains("sig")
                    || k_low.contains("code")
                {
                    limpio.insert(k, Value::String("***".to_owned()));
                } else {
                    limpio.insert(k, redactar_valor(v));
                }
            }
            Value::Object(limpio)
        }
        otro => otro,
    }
}

pub fn redactar_evento(texto: &str) -> String {
    let mut salida = texto.trim().to_owned();
    if salida.is_empty() {
        return salida;
    }
    salida = redactar_urls(&salida);
    salida = redactar_rutas(&salida);
    salida = redactar_pares(&salida);
    salida = redactar_formas(&salida);
    salida = redactar_portador(&salida);
    recortar(salida.trim(), MAX_MENSAJE_CHARS)
}

fn redactar_urls(texto: &str) -> String {
    let mut out = String::with_capacity(texto.len());
    let mut resto = texto;
    loop {
        let minusculas = resto.to_ascii_lowercase();
        let ini = match (minusculas.find("https://"), minusculas.find("http://")) {
            (Some(https), Some(http)) => https.min(http),
            (Some(https), None) => https,
            (None, Some(http)) => http,
            (None, None) => {
                out.push_str(resto);
                return out;
            }
        };
        let tras = &resto[ini..];
        let fin = tras
            .char_indices()
            .find(|(_, c)| es_delimitador_url(*c))
            .map(|(i, _)| i)
            .unwrap_or(tras.len());
        if let Ok(mut url) = reqwest::Url::parse(&tras[..fin]) {
            if !url.username().is_empty() || url.password().is_some() {
                let _ = url.set_username("***");
                let _ = url.set_password(Some("***"));
            }
            if url.query().is_some() {
                redactar_query(&mut url);
            }
            out.push_str(&resto[..ini]);
            out.push_str(url.as_ref());
            resto = &tras[fin..];
        } else {
            let prefijo = if minusculas[ini..].starts_with("https://") {
                8
            } else {
                7
            };
            out.push_str(&tras[..prefijo]);
            resto = &tras[prefijo..];
        }
    }
}

fn es_delimitador_url(c: char) -> bool {
    c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '`' | ')' | ']' | '}')
}

fn redactar_query(url: &mut reqwest::Url) {
    let Some(query) = url.query().map(str::to_owned) else {
        return;
    };
    let crudos: Vec<&str> = query.split('&').collect();
    let sensibles: Vec<(String, bool)> = url
        .query_pairs()
        .map(|(clave, _)| {
            let sensible = clave_sensible(&clave);
            (clave.into_owned(), sensible)
        })
        .collect();
    let mut limpio = String::with_capacity(query.len());
    for (indice, (clave, sensible)) in sensibles.iter().enumerate() {
        if indice > 0 {
            limpio.push('&');
        }
        if *sensible {
            limpio.push_str(clave);
            limpio.push_str("=***");
        } else if let Some(crudo) = crudos.get(indice) {
            limpio.push_str(crudo);
        }
    }
    for crudo in crudos.iter().skip(sensibles.len()) {
        if !limpio.is_empty() {
            limpio.push('&');
        }
        limpio.push_str(crudo);
    }
    url.set_query(Some(&limpio));
}

fn clave_sensible(clave: &str) -> bool {
    let normalizada: String = clave
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    [
        "token",
        "secret",
        "password",
        "passwd",
        "credential",
        "cookie",
        "signature",
        "sig",
        "apikey",
        "key",
        "auth",
        "code",
    ]
    .iter()
    .any(|sensible| normalizada.contains(sensible))
}

const CLAVES_SENSIBLES: &[&str] = &[
    "access_token",
    "refresh_token",
    "session_token",
    "id_token",
    "code_challenge",
    "code_verifier",
    "client_secret",
    "x-sentry-auth",
    "x-auth-token",
    "x-api-key",
    "authorization",
    "credentials",
    "credential",
    "password",
    "passwd",
    "signature",
    "apikey",
    "api_key",
    "api-key",
    "secret",
    "token",
    "cookie",
    "set-cookie",
    "code",
    "sig",
    "key",
    "auth",
];

fn redactar_rutas(texto: &str) -> String {
    let mut salida = texto.to_owned();
    for prefijo in [
        r"C:\\Users\\",
        r"C:\Users\",
        "C:/Users/",
        "/home/",
        "/Users/",
    ] {
        salida = reemplazar_usuario_ruta(&salida, prefijo);
    }
    salida = salida.replace("/root/", "<home>/");
    reemplazar_root_suelto(&salida)
}

fn reemplazar_usuario_ruta(texto: &str, prefijo: &str) -> String {
    let buscado = prefijo.to_ascii_lowercase();
    let mut out = String::with_capacity(texto.len());
    let mut resto = texto;
    while let Some(ini) = resto.to_ascii_lowercase().find(&buscado) {
        let tras = &resto[ini + prefijo.len()..];
        let fin = tras
            .find(['\\', '/', ' ', '"', '\'', ')', ',', ';', ']', '}'])
            .unwrap_or(tras.len());
        out.push_str(&resto[..ini]);
        out.push_str("<home>");
        resto = &tras[fin..];
    }
    out.push_str(resto);
    out
}

fn reemplazar_root_suelto(texto: &str) -> String {
    let mut out = String::with_capacity(texto.len());
    let mut resto = texto;
    while let Some(ini) = resto.find("/root") {
        let tras = &resto[ini + 5..];
        let suelto = tras.chars().next().is_none_or(|c| {
            !c.is_ascii_alphanumeric() && c != '_' && c != '-' && c != '/' && c != '\\'
        });
        if suelto {
            out.push_str(&resto[..ini]);
            out.push_str("<home>");
        } else {
            out.push_str(&resto[..ini + 5]);
        }
        resto = tras;
    }
    out.push_str(resto);
    out
}

fn redactar_pares(texto: &str) -> String {
    let mut out = String::with_capacity(texto.len());
    let mut resto = texto;
    while let Some((_, fin_clave)) = encontrar_clave(resto) {
        let cola = &resto[fin_clave..];
        let bytes = cola.as_bytes();
        let mut idx = 0;
        let mut separador = false;
        while idx < bytes.len() {
            match bytes[idx] {
                b'=' | b':' => {
                    separador = true;
                    idx += 1;
                }
                b' ' | b'\t' | b'"' | b'\'' if separador => idx += 1,
                _ => break,
            }
        }
        if !separador || idx == 0 || idx >= bytes.len() {
            out.push_str(&resto[..fin_clave]);
            resto = cola;
            continue;
        }
        let mut fin = idx;
        if matches!(bytes.get(fin), Some(b'"') | Some(b'\'')) {
            let comilla = bytes[fin];
            fin += 1;
            while fin < bytes.len() && bytes[fin] != comilla {
                fin += 1;
            }
            if fin < bytes.len() {
                fin += 1;
            }
        } else {
            while fin < bytes.len() && !es_delimitador_valor(bytes[fin]) {
                fin += 1;
            }
            let primero = &cola[idx..fin];
            if primero.eq_ignore_ascii_case("bearer") || primero.eq_ignore_ascii_case("basic") {
                let mut salto = fin;
                while salto < bytes.len() && matches!(bytes[salto], b' ' | b'\t') {
                    salto += 1;
                }
                while salto < bytes.len() && !es_delimitador_valor(bytes[salto]) {
                    salto += 1;
                }
                fin = salto;
            }
        }
        out.push_str(&resto[..fin_clave]);
        out.push_str(&cola[..idx]);
        out.push_str("***");
        resto = &cola[fin..];
    }
    out.push_str(resto);
    out
}

fn es_delimitador_valor(b: u8) -> bool {
    matches!(
        b,
        b' ' | b'\t' | b'&' | b'"' | b'\'' | b',' | b';' | b')' | b'}' | b']' | b'\n'
    )
}

fn encontrar_clave(texto: &str) -> Option<(usize, usize)> {
    let minusculas = texto.to_ascii_lowercase();
    let mut mejor: Option<(usize, usize)> = None;
    for clave in CLAVES_SENSIBLES {
        let mut desde = 0;
        while let Some(rel) = minusculas[desde..].find(clave) {
            let ini = desde + rel;
            let fin = ini + clave.len();
            let antes = ini
                .checked_sub(1)
                .and_then(|i| texto.as_bytes().get(i))
                .copied();
            let despues = texto.as_bytes().get(fin).copied();
            if antes.is_none_or(|c| !c.is_ascii_alphanumeric() && c != b'_' && c != b'-')
                && despues.is_none_or(|c| matches!(c, b'=' | b':' | b' ' | b'\t' | b'"' | b'\''))
                && mejor.is_none_or(|(m, _)| ini < m)
            {
                mejor = Some((ini, fin));
            }
            desde = ini + 1;
        }
    }
    mejor
}

fn redactar_formas(texto: &str) -> String {
    let mut out = String::with_capacity(texto.len());
    let mut resto = texto;
    while let Some((ini, fin)) = encontrar_forma(resto) {
        out.push_str(&resto[..ini]);
        out.push_str("***");
        resto = &resto[fin..];
    }
    out.push_str(resto);
    out
}

fn encontrar_forma(texto: &str) -> Option<(usize, usize)> {
    let mut mejor = None;
    for (prefijo, minimo) in [("sk-", 12usize), ("syn_", 8), ("oru_", 8)] {
        let mut desde = 0;
        while let Some(rel) = texto[desde..].find(prefijo) {
            let ini = desde + rel;
            let fin = ini + prefijo.len() + consumir_valor(&texto[ini + prefijo.len()..]);
            let borde = ini == 0 || !texto.as_bytes()[ini - 1].is_ascii_alphanumeric();
            if borde && fin - ini >= prefijo.len() + minimo && mejor.is_none_or(|(m, _)| ini < m) {
                mejor = Some((ini, fin));
            }
            desde = ini + prefijo.len();
        }
    }
    let mut desde = 0;
    while let Some(rel) = texto[desde..].find("eyJ") {
        let ini = desde + rel;
        let borde = ini == 0 || !texto.as_bytes()[ini - 1].is_ascii_alphanumeric();
        if borde
            && let Some(fin) = fin_jwt(texto, ini)
            && mejor.is_none_or(|(m, _)| ini < m)
        {
            mejor = Some((ini, fin));
        }
        desde = ini + 3;
    }
    mejor
}

fn consumir_valor(texto: &str) -> usize {
    texto
        .bytes()
        .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'-')
        .count()
}

fn fin_jwt(texto: &str, ini: usize) -> Option<usize> {
    let mut cursor = ini + 3;
    for segmento in 0..3 {
        let comienzo = cursor;
        while cursor < texto.len() {
            let b = texto.as_bytes()[cursor];
            if b.is_ascii_alphanumeric() || b == b'_' || b == b'-' {
                cursor += 1;
            } else {
                break;
            }
        }
        if cursor - comienzo < 8 {
            return None;
        }
        if segmento < 2 {
            if texto.as_bytes().get(cursor) != Some(&b'.') {
                return None;
            }
            cursor += 1;
        }
    }
    Some(cursor)
}

fn redactar_portador(texto: &str) -> String {
    let mut out = String::with_capacity(texto.len());
    let mut resto = texto;
    while let Some(ini) = encontrar_portador(resto) {
        let tras = &resto[ini..];
        let fin = tras.find(char::is_whitespace).unwrap_or(tras.len());
        out.push_str(&resto[..ini]);
        out.push_str("***");
        resto = &tras[fin..];
    }
    out.push_str(resto);
    out
}

fn encontrar_portador(texto: &str) -> Option<usize> {
    let minusculas = texto.to_ascii_lowercase();
    let mut mejor = None;
    for palabra in ["bearer", "basic"] {
        let mut desde = 0;
        while let Some(rel) = minusculas[desde..].find(palabra) {
            let ini = desde + rel;
            let fin = ini + palabra.len();
            let borde = ini == 0 || !texto.as_bytes()[ini - 1].is_ascii_alphanumeric();
            if borde
                && texto
                    .as_bytes()
                    .get(fin)
                    .is_some_and(|c| c.is_ascii_whitespace())
            {
                let valor = fin + texto[fin..].len() - texto[fin..].trim_start().len();
                if valor < texto.len() && mejor.is_none_or(|m| valor < m) {
                    mejor = Some(valor);
                }
            }
            desde = ini + palabra.len();
        }
    }
    mejor
}

fn frames_de_backtrace(texto: &str) -> Vec<Value> {
    let mut frames: Vec<Value> = Vec::new();
    for linea in texto.lines() {
        let linea = linea.trim();
        if linea.is_empty() || linea.starts_with("stack backtrace:") {
            continue;
        }
        if let Some(lugar) = linea.strip_prefix("at ") {
            let partes: Vec<&str> = lugar.rsplitn(3, ':').collect();
            if partes.len() == 3 {
                let columna = partes[0].parse::<u64>().ok();
                let fila = partes[1].parse::<u64>().ok();
                let archivo = redactar_evento(partes[2]);
                if let Some(ultimo) = frames.last_mut() {
                    ultimo["filename"] = json!(archivo);
                    ultimo["lineno"] = json!(fila);
                    ultimo["colno"] = json!(columna);
                } else if !archivo.is_empty() {
                    frames.push(json!({ "filename": archivo, "lineno": fila, "colno": columna }));
                }
            }
            continue;
        }
        let cuerpo = match linea.split_once(':') {
            Some((numero, resto))
                if !numero.is_empty()
                    && numero.len() <= 3
                    && numero.chars().all(|c| c.is_ascii_digit()) =>
            {
                resto.trim()
            }
            _ => linea,
        };
        if cuerpo.is_empty() || frames.len() >= MAX_FRAMES_CRUDOS {
            continue;
        }
        let funcion = redactar_evento(cuerpo);
        if !funcion.is_empty() && funcion != "***" {
            frames.push(json!({ "function": funcion }));
        }
    }
    frames
}

fn funcion_de_frame(frame: &Value) -> &str {
    frame.get("function").and_then(Value::as_str).unwrap_or("")
}

/// Quita del arranque del backtrace los frames del propio hook de pánico
/// (captura, `std::panicking`, `core::panicking`) para que el tope de frames
/// alcance al código de la app que falló.
fn sin_internos_de_panico(frames: Vec<Value>) -> Vec<Value> {
    let marcas = [
        "rust_begin_unwind",
        "core::panicking::",
        "std::panicking::",
        "std::backtrace::",
        "orus_app::telemetria::",
        "instalar_hook_panic",
    ];
    let ultimo_interno = frames.iter().take(24).rposition(|frame| {
        marcas
            .iter()
            .any(|marca| funcion_de_frame(frame).contains(marca))
    });
    let desde = ultimo_interno.map_or(0, |indice| indice + 1);
    frames
        .into_iter()
        .skip(desde)
        .take(MAX_FRAMES_STACK)
        .collect()
}

fn recortar(texto: &str, max_chars: usize) -> String {
    let mut out: String = texto.chars().take(max_chars).collect();
    if texto.chars().count() > max_chars {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    use super::*;

    const JWT: &str = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";

    #[test]
    fn parsea_dsn_y_redacta_secretos_y_rutas_de_usuario() {
        let dsn = SentryDsn::parse("https://abc123def@o4501.ingest.us.sentry.io/98765")
            .expect("DSN válido");
        assert_eq!(
            dsn.store_url,
            "https://o4501.ingest.us.sentry.io/api/98765/store/"
        );
        assert!(dsn.auth_header("0.8.3").contains("sentry_key=abc123def"));
        assert!(SentryDsn::parse("http://remote.example/42").is_none());
        assert!(SentryDsn::parse("https://abc@host/no-num").is_none());

        assert_eq!(
            redactar_pares("api_key=abc&sig=zzz9999999"),
            "api_key=***&sig=***"
        );
        let bruto = format!(
            r"GET https://api.proveedor.test/v1/models?api_key=sk-proj-XYZ1234567890abc&sig=zzz9999999 falló: Authorization: Basic dXNlcjpwYXNz en {} y /root/.ssh/id_rsa con `syn_abc123def456` y {}",
            r"C:\\Users\\Osman\\Documents", JWT
        );
        let limpio = redactar_evento(&bruto);
        assert!(!limpio.contains("Osman"), "filtró usuario: {limpio}");
        assert!(
            !limpio.contains("XYZ1234567890abc"),
            "filtró clave: {limpio}"
        );
        assert!(!limpio.contains("zzz9999999"), "filtró firma: {limpio}");
        assert!(!limpio.contains("dXNlcjpwYXNz"), "filtró basic: {limpio}");
        assert!(
            !limpio.contains("syn_abc123def456"),
            "filtró token: {limpio}"
        );
        assert!(!limpio.contains("SflKxwRJSMeKKF2"), "filtró JWT: {limpio}");
        assert!(!limpio.contains("/root/"), "filtró /root: {limpio}");
        assert!(limpio.contains("api_key=***"), "{limpio}");
        assert!(limpio.contains("Authorization: ***"), "{limpio}");
        assert!(limpio.contains("<home>"), "no sustituyó home: {limpio}");

        let con_url = redactar_evento(
            "GET https://usuario:claveSecreta@example.test/ruta?accessToken=valorOpaco&apiToken=otroSecreto&keep=visible falló",
        );
        assert!(
            !con_url.contains("claveSecreta"),
            "filtró userinfo: {con_url}"
        );
        assert!(
            !con_url.contains("valorOpaco"),
            "filtró accessToken: {con_url}"
        );
        assert!(
            !con_url.contains("otroSecreto"),
            "filtró apiToken: {con_url}"
        );
        assert!(
            con_url.contains("keep=visible"),
            "redactó de más: {con_url}"
        );

        let codificada = redactar_evento("https://example.test/?access%54oken=claveCodificada");
        assert!(
            !codificada.contains("claveCodificada"),
            "filtró clave con nombre codificado: {codificada}"
        );

        let oauth =
            redactar_evento("cb?code=secret123&id_token=otroSecreto456 Bearer abcdef123456");
        assert!(!oauth.contains("secret123"), "filtró code: {oauth}");
        assert!(
            !oauth.contains("otroSecreto456"),
            "filtró id_token: {oauth}"
        );
        assert!(!oauth.contains("abcdef123456"), "filtró bearer: {oauth}");
    }

    #[test]
    fn tope_de_diez_por_hora_y_deduplicacion() {
        let dsn = SentryDsn::parse("http://pub@127.0.0.1:9/42").expect("loopback DSN");
        let tel = Telemetry::new(true, "0.8.3", Some(dsn));

        assert!(
            tel.preparar_envio("ui.mic", "error", "NotAllowedError", Value::Null)
                .is_some()
        );
        assert!(
            tel.preparar_envio("ui.mic", "error", "NotAllowedError", Value::Null)
                .is_none(),
            "el duplicado en 5 min no debe enviarse"
        );

        for i in 1..=9 {
            assert!(
                tel.preparar_envio("ui.js", "error", &format!("fallo-{i}"), Value::Null)
                    .is_some()
            );
        }
        assert!(
            tel.preparar_envio("ui.js", "error", "fallo-11", Value::Null)
                .is_none(),
            "el evento 11 en la misma hora debe frenarse"
        );
        assert!(tel.recent().len() >= 11);
    }

    #[test]
    fn envia_evento_a_sentry_y_respeta_apagado() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock sentry");
        let puerto = listener.local_addr().expect("addr").port();
        let (tx, rx) = mpsc::channel::<String>();

        let srv = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buf = vec![0u8; 8192];
            let n = stream.read(&mut buf).expect("read");
            let req = String::from_utf8_lossy(&buf[..n]).into_owned();
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}");
            let _ = tx.send(req);
        });

        let dsn =
            SentryDsn::parse(&format!("http://pub123key@127.0.0.1:{puerto}/77")).expect("DSN mock");
        let tel = Telemetry::new(false, "0.8.3", Some(dsn));

        tel.report_text("shell.browser", "error", "no abre");
        assert!(rx.try_recv().is_err(), "apagada no debe enviar nada");

        tel.set_enabled(true);
        tel.report_text(
            "shell.browser",
            "error",
            r"xdg-open falló en /home/osman/app",
        );
        tel.flush(Duration::from_secs(3));
        let recibido = rx
            .recv_timeout(Duration::from_secs(3))
            .expect("mock sentry recibió el POST");
        srv.join().expect("hilo servidor");

        assert!(
            recibido.starts_with("POST /api/77/store/ HTTP/1.1"),
            "{recibido}"
        );
        let lower = recibido.to_lowercase();
        assert!(lower.contains("x-sentry-auth: sentry"), "{recibido}");
        assert!(lower.contains("sentry_key=pub123key"), "{recibido}");
        assert!(recibido.contains("oruscribe-agente@0.8.3"), "{recibido}");
        assert!(recibido.contains("<home>"), "{recibido}");
        assert!(!recibido.contains("osman"), "{recibido}");
        assert_eq!(tel.entrega().entregados, 1);
        assert_eq!(tel.salud(), SaludTelemetria::Configurada);
    }

    #[test]
    fn compartida_reutiliza_la_misma_instancia() {
        let tel = Telemetry::compartida();
        assert!(Arc::ptr_eq(&tel, &Telemetry::compartida()));
        assert!(Arc::ptr_eq(&tel, &Telemetry::from_env(false)));
    }

    #[test]
    fn salud_distingue_local_configurada_entregando_y_fallando() {
        let dsn = SentryDsn::parse("http://pub@127.0.0.1:9/42").expect("loopback DSN");
        assert_eq!(
            Telemetry::new(false, "0.8.3", Some(dsn.clone())).salud(),
            SaludTelemetria::Local
        );
        assert_eq!(
            Telemetry::new(true, "0.8.3", None).salud(),
            SaludTelemetria::Local
        );

        let tel = Telemetry::new(true, "0.8.3", Some(dsn));
        assert_eq!(tel.salud(), SaludTelemetria::Configurada);
        assert!(
            tel.preparar_envio("shell.panic", "fatal", "boom", Value::Null)
                .is_some()
        );
        assert_eq!(tel.salud(), SaludTelemetria::Entregando);
        tel.registrar_resultado(Ok(()));
        assert_eq!(tel.salud(), SaludTelemetria::Configurada);
        assert_eq!(tel.entrega().entregados, 1);
        tel.registrar_resultado(Err("429 Too Many Requests".to_owned()));
        assert_eq!(tel.salud(), SaludTelemetria::Fallando);
        assert_eq!(tel.entrega().fallos, 1);
        assert!(tel.entrega().ultimo_error.contains("429"));
    }

    #[test]
    fn fallo_http_se_contabiliza() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock sentry");
        let puerto = listener.local_addr().expect("addr").port();
        let srv = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buf = vec![0u8; 4096];
            let _ = stream.read(&mut buf).expect("read");
            let _ = stream
                .write_all(b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n");
        });

        let dsn = SentryDsn::parse(&format!("http://pub@127.0.0.1:{puerto}/77")).expect("DSN mock");
        let tel = Telemetry::new(true, "0.8.3", Some(dsn));
        tel.report_text("shell.update", "error", "fallo de actualización");
        tel.flush(Duration::from_secs(3));
        srv.join().expect("hilo servidor");

        assert_eq!(tel.entrega().fallos, 1);
        assert_eq!(tel.salud(), SaludTelemetria::Fallando);
    }

    #[test]
    fn vuelo_cancelado_libera_el_contador() {
        let dsn = SentryDsn::parse("http://pub@127.0.0.1:9/42").expect("loopback DSN");
        let tel = Telemetry::new(true, "0.8.3", Some(dsn));
        assert!(
            tel.preparar_envio("ui.mic", "error", "cancelado", Value::Null)
                .is_some()
        );
        assert_eq!(tel.salud(), SaludTelemetria::Entregando);
        let guarda = Vuelo::nuevo(&tel);
        drop(guarda);
        assert_eq!(tel.entrega().fallos, 1);
        assert_eq!(tel.salud(), SaludTelemetria::Fallando);
    }

    #[test]
    fn reporte_de_panico_espera_la_entrega_acotada() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock sentry");
        let puerto = listener.local_addr().expect("addr").port();
        let (tx, rx) = mpsc::channel::<String>();
        let srv = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buf = vec![0u8; 8192];
            let n = stream.read(&mut buf).expect("read");
            let req = String::from_utf8_lossy(&buf[..n]).into_owned();
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}");
            let _ = tx.send(req);
        });
        let dsn = SentryDsn::parse(&format!("http://pub@127.0.0.1:{puerto}/88")).expect("DSN mock");
        let tel = Telemetry::new(true, "0.8.3", Some(dsn));
        let inicio = Instant::now();
        tel.report_panic_con_plazo("boom", Duration::from_secs(2));
        let transcurrido = inicio.elapsed();
        let recibido = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("POST recibido");
        srv.join().expect("hilo servidor");
        assert!(
            transcurrido < Duration::from_secs(2),
            "esperó demasiado: {transcurrido:?}"
        );
        assert!(
            recibido.contains("\"category\":\"shell.panic\""),
            "{recibido}"
        );
        assert!(recibido.contains("stacktrace"), "{recibido}");
        assert_eq!(tel.entrega().entregados, 1);
        assert_eq!(tel.salud(), SaludTelemetria::Configurada);
    }

    #[test]
    fn reporte_de_panico_corta_el_timeout_sin_respuesta() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock sentry");
        let puerto = listener.local_addr().expect("addr").port();
        let (aceptado_tx, aceptado_rx) = mpsc::channel::<()>();
        let (liberar_tx, liberar_rx) = mpsc::channel::<()>();
        let (liberado_tx, liberado_rx) = mpsc::channel::<()>();
        let srv = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            aceptado_tx.send(()).expect("avisar petición aceptada");
            liberar_rx.recv().expect("liberar conexión");
            drop(stream);
            liberado_tx.send(()).expect("avisar conexión cerrada");
        });
        let dsn = SentryDsn::parse(&format!("http://pub@127.0.0.1:{puerto}/88")).expect("DSN mock");
        let tel = Telemetry::new(true, "0.8.3", Some(dsn));
        tel.report_panic_con_plazo("boom", Duration::from_millis(120));
        aceptado_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("la petición llegó al mock");
        let entrega = tel.entrega();
        let salud = tel.salud();
        assert!(
            liberado_rx.try_recv().is_err(),
            "el mock cerró antes de que venciera el plazo"
        );
        liberar_tx.send(()).expect("liberar el mock");
        srv.join().expect("hilo servidor");
        assert_eq!(entrega.fallos, 1, "{entrega:?}");
        assert!(entrega.ultimo_error.contains("timeout"), "{entrega:?}");
        assert_eq!(salud, SaludTelemetria::Fallando);
    }

    #[test]
    fn payload_lleva_stacktrace_cuando_hay_frames() {
        let bruto = "stack backtrace:\n   0: orus_app::agent::run\n             at /root/proyecto/src/agent.rs:12:5\n";
        let frames = frames_de_backtrace(bruto);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0]["function"], "orus_app::agent::run");
        assert_eq!(frames[0]["filename"], "<home>/proyecto/src/agent.rs");
        assert_eq!(frames[0]["lineno"], 12);

        let payload = armar_payload(
            "0.8.3",
            "shell.panic",
            "fatal",
            "boom",
            json!({ "stacktrace": { "frames": frames } }),
            &[],
        );
        assert_eq!(payload["platform"], "native");
        assert_eq!(
            payload["exception"]["values"][0]["stacktrace"]["frames"][0]["function"],
            "orus_app::agent::run"
        );
        assert_eq!(
            payload["exception"]["values"][0]["mechanism"]["handled"],
            false
        );
        let sin = armar_payload("0.8.3", "ui.mic", "error", "boom", Value::Null, &[]);
        assert!(sin.get("exception").is_none());
        assert!(sin.get("breadcrumbs").is_none());
    }

    #[test]
    fn extra_redacta_claves_y_valores_anidados() {
        let limpio = redactar_valor(json!({
            "api_key": "abc123secreto",
            "nested": [{ "sig": "firma123456", "ok": "visible" }]
        }));
        assert_eq!(limpio["api_key"], "***");
        assert_eq!(limpio["nested"][0]["sig"], "***");
        assert_eq!(limpio["nested"][0]["ok"], "visible");
    }

    #[test]
    fn los_avisos_no_gastan_el_cupo_de_errores() {
        let dsn = SentryDsn::parse("http://pub@127.0.0.1:9/42").expect("loopback DSN");
        let tel = Telemetry::new(true, "0.8.3", Some(dsn));
        let avisos = (0..30)
            .filter(|i| {
                tel.preparar_envio("watch.bell", "warning", &format!("corte {i}"), Value::Null)
                    .is_some()
            })
            .count();
        assert_eq!(avisos, MAX_AVISOS_POR_HORA);
        assert!(
            tel.preparar_envio("agent.chat", "error", "el chat falló", Value::Null)
                .is_some(),
            "el error de chat quedó sin cupo por culpa de los avisos"
        );
    }

    #[test]
    fn una_palabra_sensible_sin_valor_no_frena_la_redaccion_del_resto() {
        for (entrada, secreto) in [
            ("auth failed password=hunter2xyz", "hunter2xyz"),
            (
                "key was rejected client_secret: s3cretvalue9",
                "s3cretvalue9",
            ),
            ("token expired, cookie=sesion-abc-123", "sesion-abc-123"),
        ] {
            let salida = redactar_evento(entrada);
            assert!(!salida.contains(secreto), "{entrada} -> {salida}");
        }
    }
}
