//! Cliente HTTP compatible con el chat de OpenAI.
//!
//! Incluye la petición, la validación de la base, el reintento, la respuesta
//! completa, los mensajes y las herramientas; el streaming vive en
//! `openai_stream.rs`.

use std::fmt;
use std::time::Duration;

use async_trait::async_trait;
use orus_core::{Message, ToolCall};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, RETRY_AFTER};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::{ProviderError, SIN_RESPUESTA};
use crate::reintento::{MAX_INTENTOS, espera_reintento};
use crate::types::{ChatReply, ChatRequest, Provider, ToolSpec};

/// Tope de la respuesta del proveedor.
pub(crate) const MAX_RESPUESTA: usize = 4 << 20;
/// Caracteres del cuerpo de un error que se muestran.
pub(crate) const MAX_CUERPO_ERR: usize = 300;
/// Tiempo máximo de una petición completa. En
/// streaming es el plazo por lectura, no un deadline total (issue #93).
const TIMEOUT_CLIENTE: Duration = Duration::from_secs(120);

/// Hook de las pruebas para no dormir entre reintentos. En producción es
/// `None` y manda [`espera_reintento`].
pub type RetryDelay = Box<dyn Fn(u32) -> Duration + Send + Sync>;

/// Habla con cualquier proveedor que use el formato de chat de OpenAI.
///
/// Los campos son públicos:
/// la fábrica de proveedores arma el cliente HTTP una sola vez y las
/// pruebas sustituyen la espera.
pub struct OpenAICompat {
    pub base_url: String,
    pub api_key: String,
    pub client: reqwest::Client,
    pub retry_delay: Option<RetryDelay>,
}

impl fmt::Debug for OpenAICompat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // La clave nunca se imprime, ni siquiera al depurar.
        f.debug_struct("OpenAICompat")
            .field("base_url", &self.base_url)
            .field("api_key", &"***")
            .field("retry_delay", &self.retry_delay.is_some())
            .finish()
    }
}

impl OpenAICompat {
    /// Crea el proveedor con el cliente de fábrica: 120 s y sin redirects.
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Result<Self, ProviderError> {
        Ok(Self {
            base_url: base_url.into(),
            api_key: api_key.into(),
            client: cliente_http()?,
            retry_delay: None,
        })
    }

    /// Manda la conversación al proveedor y devuelve su respuesta completa.
    ///
    /// Sin la
    /// rama de streaming.
    pub async fn chat(&self, req: ChatRequest) -> Result<ChatReply, ProviderError> {
        let base = validar_base(&self.base_url)?;
        let cuerpo = armar_cuerpo(&req);
        let payload = serde_json::to_vec(&cuerpo).map_err(|error| {
            ProviderError::local(format!("no se pudo armar la petición: {error}"))
        })?;
        let url = reqwest::Url::parse(&format!("{base}/chat/completions")).map_err(|error| {
            ProviderError::local(format!("no se pudo armar la petición: {error}"))
        })?;

        let respuesta = self.enviar(url, &payload, req.on_delta.is_some()).await?;
        match req.on_delta.as_deref() {
            Some(on_delta) => self.procesar_stream(respuesta, on_delta).await,
            None => self.procesar(respuesta).await,
        }
    }

    /// Lee una respuesta 2xx como eventos SSE, o devuelve el error HTTP antes
    /// de empezar a leer el cuerpo.
    ///
    /// Se ignora el error de lectura del cuerpo de error y se devuelve igual el
    /// código con lo que alcanzó a leer.
    async fn procesar_stream(
        &self,
        respuesta: reqwest::Response,
        on_delta: &(dyn Fn(&str) + Send + Sync),
    ) -> Result<ChatReply, ProviderError> {
        let status = respuesta.status().as_u16();
        if status / 100 != 2 {
            return Err(error_de_stream(respuesta, |datos| self.resumen(datos)).await);
        }
        crate::openai_stream::leer_stream(respuesta, self, on_delta).await
    }

    /// Manda la petición y comparte el bucle de reintento con Anthropic.
    async fn enviar(
        &self,
        url: reqwest::Url,
        payload: &[u8],
        con_stream: bool,
    ) -> Result<reqwest::Response, ProviderError> {
        // En streaming el plazo es por lectura: un stream sano puede durar
        // más que el plazo total del cliente de respuestas completas (#93).
        let cliente = if con_stream {
            cliente_http_stream()?
        } else {
            self.client.clone()
        };
        enviar_con_reintentos(
            || {
                cliente
                    .post(url.clone())
                    .header(CONTENT_TYPE, "application/json")
                    .header(AUTHORIZATION, format!("Bearer {}", self.api_key))
                    .body(payload.to_vec())
            },
            self.retry_delay.as_ref(),
        )
        .await
    }

    /// Lee el cuerpo, comprueba el tamaño y delega el parseo.
    ///
    /// El orden importa: el contrato
    /// prefiere el error de tamaño aunque el status no sea 2xx.
    async fn procesar(&self, respuesta: reqwest::Response) -> Result<ChatReply, ProviderError> {
        let datos = procesar_respuesta(respuesta, |datos| self.resumen(datos)).await?;
        self.leer_respuesta(&datos)
    }

    /// Deja el cuerpo de un error sin la clave y en 300 caracteres como máximo.
    pub(crate) fn resumen(&self, datos: &[u8]) -> String {
        resumen(datos, &self.api_key)
    }

    /// Lee una respuesta 2xx completa.
    fn leer_respuesta(&self, datos: &[u8]) -> Result<ChatReply, ProviderError> {
        let respuesta: OaiRespuesta = serde_json::from_slice(datos).map_err(|error| {
            ProviderError::local(format!("respuesta del proveedor inválida: {error}"))
        })?;

        let Some(eleccion) = respuesta.choices.first() else {
            // Algunos proveedores responden 200 con un error dentro del cuerpo.
            if let Some(error) = &respuesta.error
                && !error.message.is_empty()
            {
                return Err(ProviderError::local(format!(
                    "el proveedor devolvió un error: {}",
                    self.resumen(error.message.as_bytes())
                )));
            }
            return Err(ProviderError::local(SIN_RESPUESTA));
        };

        let mut llamadas = Vec::with_capacity(eleccion.message.tool_calls.len());
        for (indice, llamada) in eleccion.message.tool_calls.iter().enumerate() {
            // El contrato guarda los argumentos crudos y el bucle marca `invalidArgs`;
            // acá `Args` ya es un `Value`, así que la marca se pone al entrar.
            llamadas.push(ToolCall::con_argumentos(
                &llamada.id,
                &llamada.function.name,
                &llamada.function.arguments,
                indice,
            ));
        }

        let parcial = ChatReply {
            message: Message {
                role: "assistant".to_owned(),
                content: texto_de_contenido(eleccion.message.content.as_ref()),
                images: None,
                tool_calls: (!llamadas.is_empty()).then_some(llamadas),
                tool_call_id: String::new(),
            },
            prompt_tokens: respuesta.usage.prompt_tokens,
            completion_tokens: respuesta.usage.completion_tokens,
        };

        // Un 200 con mensaje vacío y sin tool_calls no es una respuesta: el
        // turno no puede cerrarse en blanco (issue #93).
        if parcial.message.content.is_empty() && parcial.message.tool_calls.is_none() {
            return Err(ProviderError::local(SIN_RESPUESTA));
        }

        if eleccion.finish_reason == "length" {
            // OpenAI no incluye max_tokens en el pedido, así que el límite es
            // el del proveedor; el texto que llegó es parcial y no se puede
            // presentar como respuesta completa.
            return Err(ProviderError::local(error_respuesta_cortada()).con_parcial(parcial));
        }
        Ok(parcial)
    }
}

#[async_trait]
impl Provider for OpenAICompat {
    async fn chat(&self, req: ChatRequest) -> Result<ChatReply, ProviderError> {
        OpenAICompat::chat(self, req).await
    }
}

/// Valida la dirección antes de mandar la clave: https, o http solo en
/// loopback. Quita la consulta, el fragmento y la barra final.
pub fn validar_base(raw: &str) -> Result<String, ProviderError> {
    let mut url = reqwest::Url::parse(raw)
        .map_err(|_| ProviderError::local("la dirección del proveedor no es válida"))?;
    if url.host_str().is_none_or(str::is_empty) {
        return Err(ProviderError::local(
            "la dirección del proveedor no es válida",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(ProviderError::local(
            "la dirección del proveedor no puede llevar usuario ni contraseña",
        ));
    }
    let scheme = url.scheme();
    if scheme != "https" && !(scheme == "http" && es_loopback(&url)) {
        return Err(ProviderError::local(
            "la dirección del proveedor debe usar https; http solo se permite en localhost",
        ));
    }
    // La consulta y el fragmento no son parte de la ruta: sin quitarlos,
    // /chat/completions quedaría detrás de ?x=1.
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

fn es_loopback(url: &reqwest::Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    // `host_str` conserva los corchetes de una IPv6; `IpAddr` los rechaza.
    host.trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<std::net::IpAddr>()
        .is_ok_and(|ip| ip.is_loopback())
}

/// Deja el cuerpo de un error sin la clave y en 300 caracteres como máximo.
///
/// La usan OpenAI y Anthropic: la redacción se escribe una sola vez.
pub(crate) fn resumen(datos: &[u8], api_key: &str) -> String {
    let texto = String::from_utf8_lossy(datos);
    // Con clave vacía, `replace` insertaría *** entre cada letra.
    let texto = if api_key.is_empty() {
        texto.into_owned()
    } else {
        texto.replace(api_key, "***")
    };
    texto.chars().take(MAX_CUERPO_ERR).collect()
}

pub(crate) fn cliente_http() -> Result<reqwest::Client, ProviderError> {
    reqwest::Client::builder()
        .timeout(TIMEOUT_CLIENTE)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| ProviderError::local(format!("no se pudo armar el cliente HTTP: {error}")))
}

/// Cliente para streaming: el plazo es por lectura y se reinicia con cada
/// trozo. El plazo total de [`cliente_http`] cortaría a mitad un stream sano
/// que dura más de 120 s (issue #93).
pub(crate) fn cliente_http_stream() -> Result<reqwest::Client, ProviderError> {
    reqwest::Client::builder()
        .read_timeout(TIMEOUT_CLIENTE)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| ProviderError::local(format!("no se pudo armar el cliente HTTP: {error}")))
}

pub(crate) fn es_transitorio(codigo: u16) -> bool {
    codigo == 429 || codigo >= 500
}

pub(crate) fn retry_after_de(headers: &reqwest::header::HeaderMap) -> Option<String> {
    headers
        .get(RETRY_AFTER)
        .and_then(|valor| valor.to_str().ok())
        .map(str::to_owned)
}

/// Espera entre reintentos: el hook de las pruebas manda sobre el reloj.
pub(crate) async fn esperar(
    intento: u32,
    retry_after: Option<&str>,
    retry_delay: Option<&RetryDelay>,
) {
    let espera = match retry_delay {
        Some(espera_fn) => espera_fn(intento),
        None => espera_reintento(intento, retry_after),
    };
    if !espera.is_zero() {
        tokio::time::sleep(espera).await;
    }
}

/// Manda la petición y reintenta solo lo que puede ser pasajero: 429, 5xx y
/// fallos de transporte. No reintenta una respuesta que ya empezó a leerse,
/// para no repetir texto en la ventana ni cobrar dos veces una respuesta que
/// sí llegó.
///
/// La clausura
/// arma la petición en cada intento porque el cuerpo no es clonable.
pub(crate) async fn enviar_con_reintentos(
    construir: impl Fn() -> reqwest::RequestBuilder,
    retry_delay: Option<&RetryDelay>,
) -> Result<reqwest::Response, ProviderError> {
    let mut intento = 1;
    loop {
        let peticion = construir();
        match peticion.send().await {
            Ok(respuesta) => {
                let status = respuesta.status().as_u16();
                if es_transitorio(status) && intento < MAX_INTENTOS {
                    let retry_after = retry_after_de(respuesta.headers());
                    // El cuerpo aún no se leyó: el reintento no repite
                    // texto que la persona haya visto.
                    drop(respuesta);
                    esperar(intento, retry_after.as_deref(), retry_delay).await;
                    intento += 1;
                    continue;
                }
                return Ok(respuesta);
            }
            Err(source) => {
                let error = ProviderError::transporte(source);
                if intento >= MAX_INTENTOS || !error.is_transient() {
                    return Err(error);
                }
                esperar(intento, None, retry_delay).await;
                intento += 1;
            }
        }
    }
}

/// Lee el cuerpo acotado y convierte el status no 2xx en error. El error de
/// tamaño gana al HTTP porque ese es el orden del contrato.
pub(crate) async fn procesar_respuesta(
    mut respuesta: reqwest::Response,
    resumir: impl Fn(&[u8]) -> String,
) -> Result<Vec<u8>, ProviderError> {
    let status = respuesta.status().as_u16();
    let (datos, excedido) = leer_cuerpo(&mut respuesta).await?;
    if excedido {
        return Err(ProviderError::local(
            "la respuesta del proveedor es demasiado grande (más de 4 MiB)",
        ));
    }
    if status / 100 != 2 {
        return Err(ProviderError::http(status, resumir(&datos)));
    }
    Ok(datos)
}

/// Convierte la respuesta no 2xx de un stream en error. El contrato ignora el fallo de
/// lectura del cuerpo y devuelve el código con lo que alcanzó a leer.
pub(crate) async fn error_de_stream(
    mut respuesta: reqwest::Response,
    resumir: impl Fn(&[u8]) -> String,
) -> ProviderError {
    let status = respuesta.status().as_u16();
    let datos = match leer_cuerpo(&mut respuesta).await {
        Ok((datos, _)) => datos,
        Err(_) => Vec::new(),
    };
    ProviderError::http(status, resumir(&datos))
}

/// Lee como máximo [`MAX_RESPUESTA`] + 1 bytes. El segundo valor dice si el
/// cuerpo superó el tope.
pub(crate) async fn leer_cuerpo(
    respuesta: &mut reqwest::Response,
) -> Result<(Vec<u8>, bool), ProviderError> {
    let mut datos = Vec::new();
    loop {
        match respuesta.chunk().await {
            Ok(Some(trozo)) => {
                datos.extend_from_slice(&trozo);
                if datos.len() > MAX_RESPUESTA {
                    return Ok((datos, true));
                }
            }
            Ok(None) => return Ok((datos, false)),
            Err(error) => {
                return Err(ProviderError::local(format!(
                    "no se pudo leer la respuesta del proveedor: {error}"
                )));
            }
        }
    }
}

pub(crate) fn error_respuesta_cortada() -> &'static str {
    "la respuesta se cortó por el límite de tokens del proveedor; pide una respuesta más corta o continúa en otro mensaje"
}

fn armar_cuerpo(req: &ChatRequest) -> OaiRequest {
    let con_herramientas = !req.tools.is_empty();
    let con_stream = req.on_delta.is_some();
    OaiRequest {
        model: req.model.clone(),
        messages: convertir_mensajes(&req.messages),
        tools: con_herramientas.then(|| convertir_herramientas(&req.tools)),
        tool_choice: con_herramientas.then_some("auto"),
        stream: con_stream.then_some(true),
        stream_options: con_stream.then_some(OaiStreamOptions {
            include_usage: true,
        }),
    }
}

fn convertir_mensajes(msgs: &[Message]) -> Vec<OaiMessage> {
    msgs.iter()
        .map(|m| {
            let tool_calls: Option<Vec<OaiToolCall>> = m.tool_calls.as_ref().map(|llamadas| {
                llamadas
                    .iter()
                    .map(|c| OaiToolCall {
                        id: c.id.clone(),
                        kind: "function",
                        function: OaiFunction {
                            name: c.name.clone(),
                            arguments: argumentos(c),
                        },
                    })
                    .collect()
            });
            OaiMessage {
                role: m.role.clone(),
                content: contenido_oai(m),
                tool_calls: tool_calls.filter(|llamadas| !llamadas.is_empty()),
                tool_call_id: (!m.tool_call_id.is_empty()).then(|| m.tool_call_id.clone()),
            }
        })
        .collect()
}

/// Los argumentos viajan como la cadena JSON que espera el proveedor. Un
/// `Value::Null` es el "sin argumentos" y se
/// manda como `{}`, igual que la normalización de las respuestas.
fn argumentos(llamada: &ToolCall) -> String {
    if llamada.args.is_null() {
        return "{}".to_owned();
    }
    serde_json::to_string(&llamada.args).unwrap_or_else(|_| "{}".to_owned())
}

/// Una cadena sin
/// imágenes y un arreglo de partes cuando las hay. El texto va primero.
fn contenido_oai(m: &Message) -> OaiContent {
    let imagenes = m.images.as_deref().unwrap_or_default();
    if imagenes.is_empty() {
        return OaiContent::Texto(m.content.clone());
    }
    let mut partes = Vec::with_capacity(imagenes.len() + 1);
    if !m.content.is_empty() {
        partes.push(OaiPart {
            kind: "text",
            text: Some(m.content.clone()),
            image_url: None,
        });
    }
    for imagen in imagenes {
        partes.push(OaiPart {
            kind: "image_url",
            text: None,
            image_url: Some(OaiImageUrl {
                url: format!("data:{};base64,{}", imagen.mime, imagen.data),
            }),
        });
    }
    OaiContent::Partes(partes)
}

/// Un
/// schema `null` se cambia por el objeto vacío que exige el proveedor.
fn convertir_herramientas(specs: &[ToolSpec]) -> Vec<OaiTool> {
    specs
        .iter()
        .map(|spec| OaiTool {
            kind: "function",
            function: OaiToolFunction {
                name: spec.name.clone(),
                description: spec.description.clone(),
                parameters: if spec.schema.is_null() {
                    esquema_vacio()
                } else {
                    spec.schema.clone()
                },
            },
        })
        .collect()
}

fn esquema_vacio() -> Value {
    json!({"type": "object", "properties": {}})
}

/// Acepta la
/// cadena normal y el arreglo de partes que algunos proveedores devuelven.
fn texto_de_contenido(contenido: Option<&Value>) -> String {
    match contenido {
        Some(Value::String(texto)) => texto.clone(),
        Some(Value::Array(partes)) => partes
            .iter()
            .filter_map(|parte| parte.get("text").and_then(Value::as_str))
            .collect(),
        _ => String::new(),
    }
}

#[derive(Debug, Serialize)]
struct OaiRequest {
    model: String,
    messages: Vec<OaiMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<OaiTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<OaiStreamOptions>,
}

#[derive(Debug, Serialize)]
struct OaiStreamOptions {
    include_usage: bool,
}

#[derive(Debug, Serialize)]
struct OaiMessage {
    role: String,
    content: OaiContent,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OaiToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum OaiContent {
    Texto(String),
    Partes(Vec<OaiPart>),
}

#[derive(Debug, Serialize)]
struct OaiPart {
    #[serde(rename = "type")]
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    image_url: Option<OaiImageUrl>,
}

#[derive(Debug, Serialize)]
struct OaiImageUrl {
    url: String,
}

#[derive(Debug, Serialize)]
struct OaiToolCall {
    id: String,
    #[serde(rename = "type")]
    kind: &'static str,
    function: OaiFunction,
}

#[derive(Debug, Serialize)]
struct OaiFunction {
    name: String,
    arguments: String,
}

#[derive(Debug, Serialize)]
struct OaiTool {
    #[serde(rename = "type")]
    kind: &'static str,
    function: OaiToolFunction,
}

#[derive(Debug, Serialize)]
struct OaiToolFunction {
    name: String,
    description: String,
    parameters: Value,
}

#[derive(Debug, Default, Deserialize)]
struct OaiRespuesta {
    #[serde(default)]
    choices: Vec<OaiChoice>,
    #[serde(default)]
    usage: OaiUsage,
    #[serde(default)]
    error: Option<OaiError>,
}

#[derive(Debug, Default, Deserialize)]
struct OaiChoice {
    #[serde(default)]
    message: OaiMensajeRespuesta,
    #[serde(default)]
    finish_reason: String,
}

#[derive(Debug, Default, Deserialize)]
struct OaiMensajeRespuesta {
    #[serde(default)]
    content: Option<Value>,
    #[serde(default)]
    tool_calls: Vec<OaiToolCallRespuesta>,
}

#[derive(Debug, Default, Deserialize)]
struct OaiToolCallRespuesta {
    #[serde(default)]
    id: String,
    #[serde(default)]
    function: OaiFunctionRespuesta,
}

#[derive(Debug, Default, Deserialize)]
struct OaiFunctionRespuesta {
    #[serde(default)]
    name: String,
    #[serde(default)]
    arguments: String,
}

#[derive(Debug, Default, Deserialize)]
struct OaiUsage {
    #[serde(default)]
    prompt_tokens: i64,
    #[serde(default)]
    completion_tokens: i64,
}

#[derive(Debug, Default, Deserialize)]
struct OaiError {
    #[serde(default)]
    message: String,
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use axum::body::to_bytes;
    use axum::extract::Request;
    use axum::http::{StatusCode, header};
    use axum::response::{IntoResponse, Response};
    use serde_json::{Value, json};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    const RESPUESTA_SIMPLE: &str = r#"{"choices":[{"message":{"role":"assistant","content":"hola"}}],"usage":{"prompt_tokens":11,"completion_tokens":5}}"#;

    /// Servidor loopback con un handler para cualquier ruta.
    async fn servidor<H, Fut>(handler: H) -> String
    where
        H: Fn(Request) -> Fut + Clone + Send + Sync + 'static,
        Fut: std::future::Future<Output = Response> + Send + 'static,
    {
        let app = axum::Router::new().fallback(handler);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("el servidor de prueba debe escuchar");
        let addr = listener.local_addr().expect("dirección del servidor");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{addr}")
    }

    fn con_json(status: u16, cuerpo: &str) -> Response {
        (
            StatusCode::from_u16(status).expect("código HTTP válido"),
            [(header::CONTENT_TYPE, "application/json")],
            cuerpo.to_owned(),
        )
            .into_response()
    }

    fn proveedor(base: &str) -> OpenAICompat {
        OpenAICompat::new(base, "sk-test").expect("el cliente HTTP debe armarse")
    }

    fn proveedor_con_clave(base: &str, clave: &str) -> OpenAICompat {
        OpenAICompat::new(base, clave).expect("el cliente HTTP debe armarse")
    }

    fn proveedor_sin_espera(base: &str) -> OpenAICompat {
        let mut proveedor = proveedor(base);
        proveedor.retry_delay = Some(Box::new(|_| Duration::ZERO));
        proveedor
    }

    fn peticion() -> ChatRequest {
        ChatRequest {
            model: "m".to_owned(),
            messages: vec![Message {
                role: "user".to_owned(),
                content: "hola".to_owned(),
                ..Message::default()
            }],
            tools: Vec::new(),
            on_delta: None,
        }
    }

    #[tokio::test]
    async fn envia_cabecera_ruta_y_herramientas() {
        struct Visto {
            ruta: String,
            consulta: String,
            auth: String,
            cuerpo: Value,
        }

        let visto = Arc::new(Mutex::new(None::<Visto>));
        let servidor_url = servidor({
            let visto = Arc::clone(&visto);
            move |peticion: Request| {
                let visto = Arc::clone(&visto);
                async move {
                    let ruta = peticion.uri().path().to_owned();
                    let consulta = peticion.uri().query().unwrap_or("").to_owned();
                    let auth = peticion
                        .headers()
                        .get(header::AUTHORIZATION)
                        .and_then(|valor| valor.to_str().ok())
                        .unwrap_or("")
                        .to_owned();
                    let bytes = to_bytes(peticion.into_body(), usize::MAX)
                        .await
                        .expect("cuerpo del pedido");
                    let cuerpo =
                        serde_json::from_slice(&bytes).expect("el pedido debe ser JSON válido");
                    *visto.lock().expect("estado compartido") = Some(Visto {
                        ruta,
                        consulta,
                        auth,
                        cuerpo,
                    });
                    con_json(200, RESPUESTA_SIMPLE)
                }
            }
        })
        .await;

        let esquema = json!({"type": "object", "properties": {"ruta": {"type": "string"}}});
        let respuesta = proveedor(&format!("{servidor_url}/v1/"))
            .chat(ChatRequest {
                model: "modelo-x".to_owned(),
                messages: vec![Message {
                    role: "user".to_owned(),
                    content: "hola".to_owned(),
                    ..Message::default()
                }],
                tools: vec![
                    ToolSpec {
                        name: "leer".to_owned(),
                        description: "lee un archivo".to_owned(),
                        schema: esquema.clone(),
                    },
                    ToolSpec {
                        name: "hora".to_owned(),
                        description: "dice la hora".to_owned(),
                        schema: Value::Null,
                    },
                ],
                on_delta: None,
            })
            .await
            .expect("Chat debe responder");

        let visto = visto
            .lock()
            .expect("estado compartido")
            .take()
            .expect("el servidor recibió el pedido");
        assert_eq!(visto.ruta, "/v1/chat/completions");
        assert_eq!(visto.consulta, "");
        assert_eq!(visto.auth, "Bearer sk-test");
        assert_eq!(visto.cuerpo["model"], "modelo-x");
        assert_eq!(visto.cuerpo["tool_choice"], "auto");
        let herramientas = visto.cuerpo["tools"].as_array().expect("tools");
        assert_eq!(herramientas.len(), 2);
        assert_eq!(herramientas[0]["type"], "function");
        assert_eq!(herramientas[0]["function"]["name"], "leer");
        assert_eq!(herramientas[0]["function"]["parameters"], esquema);
        assert_eq!(
            herramientas[1]["function"]["parameters"],
            json!({"type": "object", "properties": {}})
        );
        assert_eq!(respuesta.message.content, "hola");
    }

    #[tokio::test]
    async fn lee_tool_calls_y_usage() {
        const RESPUESTA: &str = r#"{"choices":[{"message":{"role":"assistant","content":null,"tool_calls":[
            {"id":"call_1","type":"function","function":{"name":"leer","arguments":"{\"ruta\":\"a.txt\"}"}},
            {"id":"call_2","type":"function","function":{"name":"hora","arguments":""}}]}}],
            "usage":{"prompt_tokens":40,"completion_tokens":7}}"#;
        let servidor_url = servidor(|_peticion: Request| async { con_json(200, RESPUESTA) }).await;

        let respuesta = proveedor(&servidor_url)
            .chat(peticion())
            .await
            .expect("Chat debe responder");

        let llamadas = respuesta
            .message
            .tool_calls
            .as_deref()
            .expect("la respuesta trae dos llamadas");
        assert_eq!(llamadas.len(), 2);
        assert_eq!(llamadas[0].id, "call_1");
        assert_eq!(llamadas[0].name, "leer");
        assert_eq!(llamadas[0].args, json!({"ruta": "a.txt"}));
        assert_eq!(llamadas[1].args, json!({}));
        assert_eq!(respuesta.message.role, "assistant");
        assert_eq!(respuesta.prompt_tokens, 40);
        assert_eq!(respuesta.completion_tokens, 7);
    }

    #[tokio::test]
    async fn no_muestra_la_clave_en_errores() {
        const CLAVE: &str = "sk-secreta";
        let cuerpo = format!("la clave {CLAVE} no sirve {}", "Z".repeat(1000));
        let servidor_url = servidor(move |_peticion: Request| {
            let cuerpo = cuerpo.clone();
            async move { con_json(401, &cuerpo) }
        })
        .await;

        let error = proveedor_con_clave(&servidor_url, CLAVE)
            .chat(peticion())
            .await
            .expect_err("un 401 debe fallar");
        let mensaje = error.to_string();
        assert!(
            !mensaje.contains(CLAVE),
            "el error muestra la clave: {mensaje}"
        );
        assert!(
            mensaje.contains("***"),
            "falta la clave cambiada por ***: {mensaje}"
        );
        assert!(mensaje.contains("401"), "falta el código 401: {mensaje}");
        let zetas = mensaje.matches('Z').count();
        assert!(
            zetas > 0 && zetas <= MAX_CUERPO_ERR,
            "el cuerpo tiene {zetas} caracteres, quería entre 1 y {MAX_CUERPO_ERR}"
        );
    }

    #[tokio::test]
    async fn asigna_id_a_llamadas_sin_id() {
        const RESPUESTA: &str = r#"{"choices":[{"message":{"role":"assistant","content":null,"tool_calls":[
            {"id":"","type":"function","function":{"name":"hora","arguments":"{}"}},
            {"id":"call_x","type":"function","function":{"name":"leer","arguments":"{}"}},
            {"type":"function","function":{"name":"leer","arguments":"{}"}}]}}]}"#;
        let servidor_url = servidor(|_peticion: Request| async { con_json(200, RESPUESTA) }).await;

        let respuesta = proveedor(&servidor_url)
            .chat(peticion())
            .await
            .expect("Chat debe responder");
        let ids: Vec<&str> = respuesta
            .message
            .tool_calls
            .as_deref()
            .expect("la respuesta trae llamadas")
            .iter()
            .map(|llamada| llamada.id.as_str())
            .collect();
        assert_eq!(ids, ["call_0", "call_x", "call_2"]);
    }

    #[tokio::test]
    async fn muestra_el_error_del_proveedor_con_respuesta_200() {
        const CLAVE: &str = "sk-secreta";
        const RESPUESTA: &str =
            r#"{"error":{"message":"la clave sk-secreta no tiene saldo","code":402}}"#;
        let servidor_url = servidor(|_peticion: Request| async { con_json(200, RESPUESTA) }).await;

        let error = proveedor_con_clave(&servidor_url, CLAVE)
            .chat(peticion())
            .await
            .expect_err("un 200 con error y sin choices debe fallar");
        let mensaje = error.to_string();
        assert!(
            mensaje.contains("no tiene saldo"),
            "el error no trae el mensaje del proveedor: {mensaje}"
        );
        assert!(
            !mensaje.contains(CLAVE),
            "el error muestra la clave: {mensaje}"
        );
        assert!(
            !mensaje.contains("ninguna respuesta"),
            "el error no usa el mensaje del proveedor: {mensaje}"
        );
    }

    #[tokio::test]
    async fn ignora_consulta_y_fragmento_de_la_base() {
        let visto = Arc::new(Mutex::new((String::new(), String::new())));
        let servidor_url = servidor({
            let visto = Arc::clone(&visto);
            move |peticion: Request| {
                let visto = Arc::clone(&visto);
                async move {
                    let ruta = peticion.uri().path().to_owned();
                    let consulta = peticion.uri().query().unwrap_or("").to_owned();
                    *visto.lock().expect("estado compartido") = (ruta, consulta);
                    con_json(200, RESPUESTA_SIMPLE)
                }
            }
        })
        .await;

        proveedor(&format!("{servidor_url}/v1?clave=1#seccion"))
            .chat(peticion())
            .await
            .expect("Chat debe responder");

        let visto = visto.lock().expect("estado compartido");
        assert_eq!(visto.0, "/v1/chat/completions");
        assert_eq!(visto.1, "");
    }

    #[tokio::test]
    async fn rechaza_direccion_insegura() {
        let llamadas = Arc::new(AtomicUsize::new(0));
        let servidor_url = servidor({
            let llamadas = Arc::clone(&llamadas);
            move |_peticion: Request| {
                let llamadas = Arc::clone(&llamadas);
                async move {
                    llamadas.fetch_add(1, Ordering::SeqCst);
                    con_json(200, RESPUESTA_SIMPLE)
                }
            }
        })
        .await;
        let host = servidor_url
            .strip_prefix("http://")
            .expect("el servidor de prueba usa http");

        let casos = [
            ("http://ejemplo.com".to_owned(), "debe usar https"),
            (format!("http://user:pass@{host}"), "usuario ni contraseña"),
            (String::new(), "no es válida"),
            ("ftp://x.com".to_owned(), "debe usar https"),
        ];
        for (base, esperado) in casos {
            let error = proveedor(&base)
                .chat(peticion())
                .await
                .expect_err("la dirección insegura debe rechazarse");
            assert!(
                error.to_string().contains(esperado),
                "base {base:?}: {error}"
            );
        }
        assert_eq!(
            llamadas.load(Ordering::SeqCst),
            0,
            "no debía contactar al proveedor"
        );
    }

    #[tokio::test]
    async fn acepta_http_en_loopback() {
        let servidor_url =
            servidor(|_peticion: Request| async { con_json(200, RESPUESTA_SIMPLE) }).await;

        let respuesta = proveedor(&servidor_url)
            .chat(peticion())
            .await
            .expect("http en 127.0.0.1 debía pasar");
        assert_eq!(respuesta.message.content, "hola");
    }

    #[tokio::test]
    async fn no_sigue_redirecciones() {
        let destino_llamadas = Arc::new(AtomicUsize::new(0));
        let destino = servidor({
            let destino_llamadas = Arc::clone(&destino_llamadas);
            move |_peticion: Request| {
                let destino_llamadas = Arc::clone(&destino_llamadas);
                async move {
                    destino_llamadas.fetch_add(1, Ordering::SeqCst);
                    con_json(200, RESPUESTA_SIMPLE)
                }
            }
        })
        .await;
        let origen = servidor(move |_peticion: Request| {
            let destino = destino.clone();
            async move {
                let mut respuesta = con_json(200, "");
                *respuesta.status_mut() = StatusCode::TEMPORARY_REDIRECT;
                respuesta.headers_mut().insert(
                    header::LOCATION,
                    format!("{destino}/v1/chat/completions")
                        .parse()
                        .expect("Location válido"),
                );
                respuesta
            }
        })
        .await;

        let error = proveedor(&origen)
            .chat(peticion())
            .await
            .expect_err("el proveedor redirigió y el cliente no debía seguir");
        assert_eq!(error.status, 307);
        assert_eq!(
            destino_llamadas.load(Ordering::SeqCst),
            0,
            "la petición con la clave llegó al destino de la redirección"
        );
    }

    #[tokio::test]
    async fn rechaza_respuesta_demasiado_grande() {
        let cuerpo = format!(
            r#"{{"choices":[{{"message":{{"role":"assistant","content":"{}"}}}}]}}"#,
            "a".repeat(MAX_RESPUESTA)
        );
        let servidor_url = servidor(move |_peticion: Request| {
            let cuerpo = cuerpo.clone();
            async move { con_json(200, &cuerpo) }
        })
        .await;

        let error = proveedor(&servidor_url)
            .chat(peticion())
            .await
            .expect_err("más de 4 MiB debe fallar");
        assert!(
            error.to_string().contains("demasiado grande"),
            "error = {error}"
        );
    }

    #[tokio::test]
    async fn reintenta_429_y_5xx() {
        let llamadas = Arc::new(AtomicUsize::new(0));
        let servidor_url = servidor({
            let llamadas = Arc::clone(&llamadas);
            move |_peticion: Request| {
                let llamadas = Arc::clone(&llamadas);
                async move {
                    match llamadas.fetch_add(1, Ordering::SeqCst) {
                        0 => con_json(429, r#"{"error":{"message":"espera"}}"#),
                        1 => con_json(502, r#"{"error":{"message":"caído"}}"#),
                        _ => con_json(200, RESPUESTA_SIMPLE),
                    }
                }
            }
        })
        .await;

        let respuesta = proveedor_sin_espera(&servidor_url)
            .chat(peticion())
            .await
            .expect("el tercer intento responde");
        assert_eq!(respuesta.message.content, "hola");
        assert_eq!(llamadas.load(Ordering::SeqCst), 3);
    }

    /// Un corte de la conexión vale reintentarlo: la persona no llegó a ver
    /// nada de la respuesta.
    #[tokio::test]
    async fn reintenta_un_fallo_de_red() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("el servidor de prueba debe escuchar");
        let addr = listener.local_addr().expect("dirección del servidor");
        let llamadas = Arc::new(AtomicUsize::new(0));
        let maximo = MAX_INTENTOS as usize;
        let tarea = tokio::spawn({
            let llamadas = Arc::clone(&llamadas);
            async move {
                loop {
                    let (mut socket, _) = listener.accept().await.expect("aceptar conexión");
                    let numero = llamadas.fetch_add(1, Ordering::SeqCst) + 1;
                    // Las dos primeras conexiones aceptan y cierran sin
                    // responder; la tercera contesta.
                    if numero < maximo {
                        drop(socket);
                        continue;
                    }
                    let mut buffer = [0u8; 4096];
                    let _ = socket.read(&mut buffer).await;
                    let respuesta = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        RESPUESTA_SIMPLE.len(),
                        RESPUESTA_SIMPLE
                    );
                    let _ = socket.write_all(respuesta.as_bytes()).await;
                    let _ = socket.shutdown().await;
                }
            }
        });

        let respuesta = proveedor_sin_espera(&format!("http://{addr}"))
            .chat(peticion())
            .await
            .expect("el tercer intento responde");
        assert_eq!(respuesta.message.content, "hola");
        assert_eq!(llamadas.load(Ordering::SeqCst), maximo);
        tarea.abort();
    }

    #[tokio::test]
    async fn no_reintenta_un_error_que_no_es_pasajero() {
        let llamadas = Arc::new(AtomicUsize::new(0));
        let servidor_url = servidor({
            let llamadas = Arc::clone(&llamadas);
            move |_peticion: Request| {
                let llamadas = Arc::clone(&llamadas);
                async move {
                    llamadas.fetch_add(1, Ordering::SeqCst);
                    con_json(401, "")
                }
            }
        })
        .await;

        proveedor_sin_espera(&servidor_url)
            .chat(peticion())
            .await
            .expect_err("un 401 no se reintenta");
        assert_eq!(llamadas.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn no_presenta_como_completa_una_respuesta_cortada() {
        let servidor_url = servidor(|_peticion: Request| async {
            con_json(
                200,
                r#"{"choices":[{"message":{"role":"assistant","content":"a medias"},"finish_reason":"length"}]}"#,
            )
        })
        .await;

        let error = proveedor(&servidor_url)
            .chat(peticion())
            .await
            .expect_err("una respuesta cortada no es completa");
        assert_eq!(
            error.to_string(),
            "la respuesta se cortó por el límite de tokens del proveedor; pide una respuesta más corta o continúa en otro mensaje"
        );
        let parcial = error
            .partial
            .as_deref()
            .expect("la respuesta parcial viaja en el error");
        assert_eq!(parcial.message.content, "a medias");
    }

    #[test]
    fn manda_la_imagen_como_parte() {
        const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAFgwJ/lU7c5wAAAABJRU5ErkJggg==";
        let mensajes = convertir_mensajes(&[Message {
            role: "user".to_owned(),
            content: "¿qué ves?".to_owned(),
            images: Some(vec![orus_core::Image {
                mime: "image/png".to_owned(),
                data: PNG.to_owned(),
                preview: String::new(),
            }]),
            ..Message::default()
        }]);

        let OaiContent::Partes(partes) = &mensajes[0].content else {
            panic!("quería texto e imagen");
        };
        assert_eq!(partes.len(), 2);
        assert_eq!(partes[0].kind, "text");
        assert_eq!(partes[0].text.as_deref(), Some("¿qué ves?"));
        assert_eq!(partes[1].kind, "image_url");
        let url = &partes[1]
            .image_url
            .as_ref()
            .expect("la parte de imagen trae URL")
            .url;
        assert!(url.starts_with("data:image/png;base64,"));
        assert!(url.ends_with(PNG));
    }

    #[test]
    fn sin_imagen_sigue_mandando_texto() {
        let mensajes = convertir_mensajes(&[Message {
            role: "user".to_owned(),
            content: "hola".to_owned(),
            ..Message::default()
        }]);
        let OaiContent::Texto(texto) = &mensajes[0].content else {
            panic!("sin imágenes el contenido es una cadena");
        };
        assert_eq!(texto, "hola");
    }

    /// La conversión de mensajes envía de vuelta el historial
    /// con sus llamadas y el `tool_call_id` del resultado.
    #[test]
    fn convierte_tool_calls_y_tool_call_id() {
        let mensajes = convertir_mensajes(&[
            Message {
                role: "assistant".to_owned(),
                tool_calls: Some(vec![ToolCall {
                    id: "call_1".to_owned(),
                    name: "leer".to_owned(),
                    args: json!({"ruta": "a"}),
                    invalid_args: false,
                }]),
                ..Message::default()
            },
            Message {
                role: "tool".to_owned(),
                content: "listo".to_owned(),
                tool_call_id: "call_1".to_owned(),
                ..Message::default()
            },
        ]);

        let llamada = serde_json::to_value(&mensajes[0]).expect("mensaje serializable");
        assert_eq!(
            llamada["tool_calls"],
            json!([{
                "id": "call_1",
                "type": "function",
                "function": {"name": "leer", "arguments": "{\"ruta\":\"a\"}"}
            }])
        );
        let resultado = serde_json::to_value(&mensajes[1]).expect("mensaje serializable");
        assert_eq!(resultado["tool_call_id"], "call_1");
        assert!(resultado.get("tool_calls").is_none());
    }

    /// Un 200 con mensaje vacío y sin tool_calls no es una respuesta: el
    /// turno no puede cerrarse en blanco (issue #93).
    #[tokio::test]
    async fn rechaza_un_200_vacio() {
        for cuerpo in [
            r#"{"choices":[{"message":{"role":"assistant","content":""}}]}"#,
            r#"{"choices":[{"message":{"role":"assistant","content":null}}]}"#,
        ] {
            let servidor_url =
                servidor(move |_peticion: Request| async move { con_json(200, cuerpo) }).await;
            let error = proveedor(&servidor_url)
                .chat(peticion())
                .await
                .expect_err("un 200 vacío debe fallar");
            assert!(
                error.to_string().contains("no devolvió ninguna respuesta"),
                "err = {error}"
            );
        }
    }

    async fn escribir_chunk(socket: &mut tokio::net::TcpStream, datos: &[u8]) {
        socket
            .write_all(format!("{:x}\r\n", datos.len()).as_bytes())
            .await
            .expect("tamaño del trozo");
        socket.write_all(datos).await.expect("datos del trozo");
        socket.write_all(b"\r\n").await.expect("fin del trozo");
        socket.flush().await.expect("enviar el trozo");
    }

    async fn aceptar_hasta_el_plazo(
        listener: tokio::net::TcpListener,
        cabecera: String,
        cuerpo: &'static str,
        llamadas: Arc<AtomicUsize>,
    ) {
        loop {
            let (mut socket, _) = listener.accept().await.expect("aceptar conexión");
            llamadas.fetch_add(1, Ordering::SeqCst);
            let mut buffer = [0u8; 4096];
            let _ = socket.read(&mut buffer).await;
            tokio::time::sleep(Duration::from_millis(250)).await;
            let _ = socket.write_all(cabecera.as_bytes()).await;
            let _ = socket.write_all(cuerpo.as_bytes()).await;
            let _ = socket.shutdown().await;
        }
    }

    /// El plazo de 120 s del cliente es TOTAL y corta un stream sano que dura
    /// más. En streaming el plazo debe ser por lectura (issue #93): acá el
    /// cliente de prueba lleva un plazo total de 120 ms y el stream dura
    /// 250 ms; con read_timeout el stream llega entero.
    #[tokio::test]
    async fn el_stream_no_se_corta_con_el_plazo_total() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("el servidor de prueba debe escuchar");
        let addr = listener.local_addr().expect("dirección del servidor");
        let tarea = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("aceptar conexión");
            let mut buffer = [0u8; 4096];
            let _ = socket.read(&mut buffer).await;
            let cabecera = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n";
            socket
                .write_all(cabecera.as_bytes())
                .await
                .expect("cabecera");
            escribir_chunk(
                &mut socket,
                b"data: {\"choices\":[{\"delta\":{\"content\":\"uno\"}}]}\n\n",
            )
            .await;
            tokio::time::sleep(Duration::from_millis(250)).await;
            escribir_chunk(
                &mut socket,
                b"data: {\"choices\":[{\"delta\":{\"content\":\" dos\"}}]}\n\n",
            )
            .await;
            escribir_chunk(&mut socket, b"data: [DONE]\n\n").await;
            socket
                .write_all(b"0\r\n\r\n")
                .await
                .expect("fin del cuerpo");
            socket.shutdown().await.expect("cerrar");
        });

        let cliente = OpenAICompat {
            base_url: format!("http://{addr}"),
            api_key: "sk-test".to_owned(),
            client: reqwest::Client::builder()
                .timeout(Duration::from_millis(120))
                .build()
                .expect("cliente de prueba"),
            retry_delay: None,
        };
        let visto = Arc::new(Mutex::new(Vec::new()));
        let copia = Arc::clone(&visto);
        let respuesta = cliente
            .chat(ChatRequest {
                on_delta: Some(Box::new(move |texto: &str| {
                    copia.lock().expect("fragmentos").push(texto.to_owned());
                })),
                ..peticion()
            })
            .await
            .expect("un stream sano no se corta por el plazo total");
        assert_eq!(respuesta.message.content, "uno dos");
        tarea.await.expect("el servidor termina");
    }

    /// La respuesta completa (sin stream) conserva el plazo total del cliente:
    /// el cambio a read_timeout es solo para streaming (issue #93).
    #[tokio::test]
    async fn la_respuesta_completa_conserva_el_plazo_total() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("el servidor de prueba debe escuchar");
        let addr = listener.local_addr().expect("dirección del servidor");
        let cabecera = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            RESPUESTA_SIMPLE.len()
        );
        let llamadas = Arc::new(AtomicUsize::new(0));
        let tarea = tokio::spawn(aceptar_hasta_el_plazo(
            listener,
            cabecera,
            RESPUESTA_SIMPLE,
            Arc::clone(&llamadas),
        ));

        let cliente = OpenAICompat {
            base_url: format!("http://{addr}"),
            api_key: "sk-test".to_owned(),
            client: reqwest::Client::builder()
                .timeout(Duration::from_millis(120))
                .build()
                .expect("cliente de prueba"),
            retry_delay: None,
        };
        let error = cliente
            .chat(peticion())
            .await
            .expect_err("sin stream el plazo total del cliente sigue aplicando");
        assert_eq!(
            error.status, 0,
            "el plazo vencido es de transporte: {error}"
        );
        tarea.abort();
        assert_eq!(
            llamadas.load(Ordering::SeqCst),
            1,
            "un vencimiento con reqwest no se reintenta"
        );
    }
}
