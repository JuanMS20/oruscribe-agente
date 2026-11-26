//! Cliente de la Messages API de Anthropic.
//!
//! Incluye los tipos, `Chat`, `resumen` y la traducción de mensajes y
//! herramientas. El SSE vive en [`crate::anthropic_stream`].

use std::fmt;

use async_trait::async_trait;
use orus_core::{Message, ToolCall};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::{ProviderError, SIN_RESPUESTA};
// La costura de `openai.rs`: validación, cliente HTTP, lectura acotada, bucle de
// reintento y redacción se escriben una sola vez. Cada proveedor conserva su
// clausura de cabeceras.
use crate::openai::{
    RetryDelay, cliente_http, cliente_http_stream, enviar_con_reintentos, error_de_stream,
    procesar_respuesta, resumen, validar_base,
};
use crate::types::{ChatReply, ChatRequest, Provider, ToolSpec};

/// Cabecera `anthropic-version` que este adaptador habla.
const VERSION_ANTHROPIC: &str = "2023-06-01";
/// La API exige `max_tokens`; ningún ajuste de ventana lo cambia.
const MAX_TOKENS_DEFECTO: i64 = 4096;

/// Habla con la Messages API de Anthropic.
///
/// Los campos son públicos: la fábrica arma el cliente una vez y
/// las pruebas sustituyen la espera.
pub struct Anthropic {
    pub base_url: String,
    pub api_key: String,
    pub client: reqwest::Client,
    pub retry_delay: Option<RetryDelay>,
    /// Cero usa [`MAX_TOKENS_DEFECTO`].
    pub max_tokens: i64,
    /// Manda la clave como `Authorization: Bearer` en vez de `x-api-key`.
    pub auth_bearer: bool,
}

impl fmt::Debug for Anthropic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // La clave nunca se imprime, ni siquiera al depurar.
        f.debug_struct("Anthropic")
            .field("base_url", &self.base_url)
            .field("api_key", &"***")
            .field("retry_delay", &self.retry_delay.is_some())
            .field("max_tokens", &self.max_tokens)
            .field("auth_bearer", &self.auth_bearer)
            .finish()
    }
}

impl Anthropic {
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
            max_tokens: 0,
            auth_bearer: false,
        })
    }

    /// Tope de tokens de la respuesta, con el valor por defecto de la API.
    /// `limite`.
    pub(crate) fn limite(&self) -> i64 {
        if self.max_tokens > 0 {
            self.max_tokens
        } else {
            MAX_TOKENS_DEFECTO
        }
    }

    /// Manda la conversación al proveedor y devuelve su respuesta.
    ///
    /// Con `on_delta` pide el SSE y
    /// devuelve cada fragmento; sin él espera la respuesta entera.
    pub async fn chat(&self, req: ChatRequest) -> Result<ChatReply, ProviderError> {
        let base = validar_base(&self.base_url)?;
        let (system, messages) = convertir_mensajes_anthropic(&req.messages);
        let con_herramientas = !req.tools.is_empty();
        let cuerpo = AnthRequest {
            model: req.model.clone(),
            max_tokens: self.limite(),
            system,
            messages,
            tools: con_herramientas.then(|| convertir_herramientas(&req.tools)),
            tool_choice: con_herramientas.then_some(AnthChoice { kind: "auto" }),
            // `omitempty`: sin delta el campo no viaja y el pedido es el de
            // T2.6a.
            stream: req.on_delta.is_some().then_some(true),
        };
        let payload = serde_json::to_vec(&cuerpo).map_err(|error| {
            ProviderError::local(format!("no se pudo armar la petición: {error}"))
        })?;
        let url = reqwest::Url::parse(&format!("{base}/v1/messages")).map_err(|error| {
            ProviderError::local(format!("no se pudo armar la petición: {error}"))
        })?;

        let respuesta = self.enviar(url, &payload, req.on_delta.is_some()).await?;
        match req.on_delta.as_deref() {
            Some(on_delta) => self.procesar_stream(respuesta, on_delta).await,
            None => self.procesar(respuesta).await,
        }
    }

    /// Con `on_delta` el status no 2xx se trata antes de abrir el SSE: El contrato lee
    /// el cuerpo acotado y devuelve el código con lo que alcanzó a leer
    async fn procesar_stream(
        &self,
        respuesta: reqwest::Response,
        on_delta: &(dyn Fn(&str) + Send + Sync),
    ) -> Result<ChatReply, ProviderError> {
        let status = respuesta.status().as_u16();
        if status / 100 != 2 {
            return Err(error_de_stream(respuesta, |datos| resumen(datos, &self.api_key)).await);
        }
        crate::anthropic_stream::leer_stream(respuesta, self, on_delta).await
    }

    /// Reintenta solo lo pasajero: 429, 5xx y fallos de transporte. El bucle
    /// vive en `openai.rs`; esta clausura conserva la cabecera de Anthropic.
    ///
    /// Aplicado por
    /// `Chat`.
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
                let peticion = cliente
                    .post(url.clone())
                    .header(CONTENT_TYPE, "application/json")
                    .header("anthropic-version", VERSION_ANTHROPIC)
                    .body(payload.to_vec());
                if self.auth_bearer {
                    peticion.header(AUTHORIZATION, format!("Bearer {}", self.api_key))
                } else {
                    peticion.header("x-api-key", self.api_key.as_str())
                }
            },
            self.retry_delay.as_ref(),
        )
        .await
    }

    /// Lee el cuerpo, comprueba el tamaño y delega el parseo.
    ///
    /// El orden importa: el contrato prefiere el error
    /// de tamaño aunque el status no sea 2xx.
    async fn procesar(&self, respuesta: reqwest::Response) -> Result<ChatReply, ProviderError> {
        let datos = procesar_respuesta(respuesta, |datos| resumen(datos, &self.api_key)).await?;
        self.leer_respuesta(&datos)
    }

    /// Lee una respuesta 2xx completa.
    fn leer_respuesta(&self, datos: &[u8]) -> Result<ChatReply, ProviderError> {
        let respuesta: AnthRespuesta = serde_json::from_slice(datos).map_err(|error| {
            ProviderError::local(format!("respuesta del proveedor inválida: {error}"))
        })?;
        if let Some(error) = &respuesta.error
            && !error.message.is_empty()
        {
            return Err(ProviderError::local(format!(
                "el proveedor devolvió un error: {}",
                resumen(error.message.as_bytes(), &self.api_key)
            )));
        }
        // El contrato acepta `content: null` como slice nil: también es "sin respuesta".
        let contenido = respuesta.content.as_deref().unwrap_or_default();
        if contenido.is_empty() {
            return Err(ProviderError::local(SIN_RESPUESTA));
        }

        let mut texto = String::new();
        let mut llamadas = Vec::new();
        for (indice, bloque) in contenido.iter().enumerate() {
            match bloque.kind.as_str() {
                "text" => texto.push_str(&bloque.text),
                "tool_use" => {
                    // `null` o ausente es la herramienta sin argumentos.
                    llamadas.push(ToolCall::desde_valor(
                        &bloque.id,
                        &bloque.name,
                        bloque.input.clone().unwrap_or(Value::Null),
                        indice,
                    ));
                }
                _ => {}
            }
        }

        // Un 200 con solo bloques de tipo desconocido (o texto vacío) tampoco
        // es una respuesta: el turno no puede cerrarse en blanco (issue #93).
        if texto.is_empty() && llamadas.is_empty() {
            return Err(ProviderError::local(SIN_RESPUESTA));
        }

        // El contrato acepta `usage: null` como el struct cero.
        let usage = respuesta.usage.unwrap_or_default();
        let reply = ChatReply {
            message: Message {
                role: "assistant".to_owned(),
                content: texto,
                images: None,
                // El contrato deja `nil` sin tool_use: `None`, no
                // `Some(vec![])`; esa asimetría es la del stream (D28).
                tool_calls: (!llamadas.is_empty()).then_some(llamadas),
                tool_call_id: String::new(),
            },
            prompt_tokens: usage.input_tokens,
            completion_tokens: usage.output_tokens,
        };
        if respuesta.stop_reason == "max_tokens" {
            // El texto llegó incompleto: devolverlo como respuesta entera haría
            // creer que el modelo terminó. D24: la parcial viaja en el error.
            return Err(
                ProviderError::local(error_respuesta_cortada(self.limite())).con_parcial(reply)
            );
        }
        Ok(reply)
    }
}

#[async_trait]
impl Provider for Anthropic {
    async fn chat(&self, req: ChatRequest) -> Result<ChatReply, ProviderError> {
        Anthropic::chat(self, req).await
    }
}

pub(crate) fn error_respuesta_cortada(max_tokens: i64) -> String {
    format!(
        "la respuesta se cortó al llegar al límite de {max_tokens} tokens; pide una respuesta más corta o continúa en otro mensaje"
    )
}

fn convertir_mensajes_anthropic(msgs: &[Message]) -> (String, Vec<AnthMessage>) {
    let mut sistema = String::new();
    let mut out: Vec<AnthMessage> = Vec::new();
    let mut i = 0;
    while i < msgs.len() {
        let m = &msgs[i];
        match m.role.as_str() {
            "system" => {
                let texto = m.content.trim();
                if !texto.is_empty() {
                    if !sistema.is_empty() {
                        sistema.push_str("\n\n");
                    }
                    sistema.push_str(texto);
                }
                i += 1;
            }
            "tool" => {
                // La API exige alternar roles: todas las respuestas de
                // herramientas de esta vuelta van en un único mensaje user.
                let mut bloques = Vec::new();
                while i < msgs.len() && msgs[i].role == "tool" {
                    let t = &msgs[i];
                    bloques.push(AnthBlock {
                        kind: "tool_result".to_owned(),
                        tool_use_id: t.tool_call_id.clone(),
                        content: Some(Value::String(t.content.clone())),
                        ..AnthBlock::default()
                    });
                    i += 1;
                }
                // Si el mensaje anterior ya es user, los tool_result se funden
                // con él: la API exige alternancia (issue #93). Van al
                // principio del contenido, como exige la API.
                match out.last_mut() {
                    Some(ultimo) if ultimo.role == "user" => {
                        ultimo.content.splice(0..0, bloques);
                    }
                    _ => out.push(AnthMessage {
                        role: "user".to_owned(),
                        content: bloques,
                    }),
                }
            }
            "user" | "assistant" => {
                let bloques = bloques_de_mensaje(m);
                // La API exige alternar roles: dos user seguidos (un turno
                // fallido deja el user colgando) o dos assistant se funden en
                // un mensaje (issue #93).
                match out.last_mut() {
                    Some(ultimo) if ultimo.role == m.role => {
                        ultimo.content.extend(bloques);
                    }
                    _ => out.push(AnthMessage {
                        role: m.role.clone(),
                        content: bloques,
                    }),
                }
                i += 1;
            }
            _ => i += 1, // un rol que este adaptador no habla se salta
        }
    }
    // La API exige que la conversación empiece por user; tras compactar puede
    // empezar en assistant.
    if out.first().is_some_and(|m| m.role == "assistant") {
        out.insert(
            0,
            AnthMessage {
                role: "user".to_owned(),
                content: vec![AnthBlock {
                    kind: "text".to_owned(),
                    text: "(continúa la conversación)".to_owned(),
                    ..AnthBlock::default()
                }],
            },
        );
    }
    (sistema, out)
}

fn bloques_de_mensaje(m: &Message) -> Vec<AnthBlock> {
    let mut bloques = Vec::new();
    // Las imágenes van antes del texto: el modelo ve el adjunto y después lee
    // la pregunta que lo acompaña.
    for imagen in m.images.as_deref().unwrap_or_default() {
        bloques.push(AnthBlock {
            kind: "image".to_owned(),
            source: Some(AnthSource {
                kind: "base64".to_owned(),
                media_type: imagen.mime.clone(),
                data: imagen.data.clone(),
            }),
            ..AnthBlock::default()
        });
    }
    if !m.content.is_empty() {
        bloques.push(AnthBlock {
            kind: "text".to_owned(),
            text: m.content.clone(),
            ..AnthBlock::default()
        });
    }
    for llamada in m.tool_calls.as_deref().unwrap_or_default() {
        let args = if llamada.args.is_null() {
            json!({})
        } else {
            llamada.args.clone()
        };
        bloques.push(AnthBlock {
            kind: "tool_use".to_owned(),
            id: llamada.id.clone(),
            name: llamada.name.clone(),
            input: Some(args),
            ..AnthBlock::default()
        });
    }
    if bloques.is_empty() {
        // La API rechaza un mensaje sin bloques; un espacio conserva la
        // alternancia.
        bloques.push(AnthBlock {
            kind: "text".to_owned(),
            text: " ".to_owned(),
            ..AnthBlock::default()
        });
    }
    bloques
}

fn convertir_herramientas(specs: &[ToolSpec]) -> Vec<AnthTool> {
    specs
        .iter()
        .map(|spec| AnthTool {
            name: spec.name.clone(),
            description: spec.description.clone(),
            input_schema: if spec.schema.is_null() {
                json!({"type": "object", "properties": {}})
            } else {
                spec.schema.clone()
            },
        })
        .collect()
}

#[derive(Debug, Serialize)]
struct AnthRequest {
    model: String,
    max_tokens: i64,
    #[serde(skip_serializing_if = "String::is_empty")]
    system: String,
    messages: Vec<AnthMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<AnthTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<AnthChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
}

#[derive(Debug, Serialize)]
struct AnthMessage {
    role: String,
    content: Vec<AnthBlock>,
}

/// Bloque de contenido: texto, `tool_use` del historial, `tool_result` o
/// imagen.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct AnthBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    text: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    input: Option<Value>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    tool_use_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    content: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source: Option<AnthSource>,
}

/// Origen de una imagen: base64 dentro del mensaje, no una URL.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct AnthSource {
    #[serde(rename = "type")]
    kind: String,
    media_type: String,
    data: String,
}

#[derive(Debug, Serialize)]
struct AnthTool {
    name: String,
    description: String,
    input_schema: Value,
}

#[derive(Debug, Serialize)]
struct AnthChoice {
    #[serde(rename = "type")]
    kind: &'static str,
}

#[derive(Debug, Default, Deserialize)]
struct AnthRespuesta {
    #[serde(default)]
    content: Option<Vec<AnthBlock>>,
    #[serde(default)]
    stop_reason: String,
    #[serde(default)]
    usage: Option<AnthUsage>,
    #[serde(default)]
    error: Option<AnthError>,
}

#[derive(Debug, Default, Deserialize)]
struct AnthUsage {
    #[serde(default)]
    input_tokens: i64,
    #[serde(default)]
    output_tokens: i64,
}

#[derive(Debug, Default, Deserialize)]
struct AnthError {
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
    use orus_core::Image;
    use serde_json::{Value, json};

    use super::*;
    use crate::openai::MAX_CUERPO_ERR;

    const RESPUESTA_SIMPLE: &str = r#"{"content":[{"type":"text","text":"hola"}],"usage":{"input_tokens":11,"output_tokens":5}}"#;

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

    fn proveedor(base: &str) -> Anthropic {
        Anthropic::new(base, "sk-ant-test").expect("el cliente HTTP debe armarse")
    }

    fn proveedor_con_clave(base: &str, clave: &str) -> Anthropic {
        Anthropic::new(base, clave).expect("el cliente HTTP debe armarse")
    }

    fn proveedor_sin_espera(base: &str) -> Anthropic {
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
    async fn clasifica_los_errores_http_para_reintentar() {
        const CUERPO: &str =
            r#"{"type":"error","error":{"type":"rate_limit_error","message":"espera"}}"#;
        let servidor_url = servidor(|_peticion: Request| async { con_json(429, CUERPO) }).await;

        let error = proveedor_sin_espera(&servidor_url)
            .chat(peticion())
            .await
            .expect_err("un 429 debe fallar");
        assert_eq!(error.status, 429);
        assert!(
            error.is_transient(),
            "un 429 del proveedor debe clasificarse como transitorio"
        );

        // En streaming el error llega antes de leer el cuerpo: mismo contrato
        // y sin emitir deltas.
        let emitidos = Arc::new(AtomicUsize::new(0));
        let error = proveedor_sin_espera(&servidor_url)
            .chat(ChatRequest {
                on_delta: Some(Box::new({
                    let emitidos = Arc::clone(&emitidos);
                    move |_texto: &str| {
                        emitidos.fetch_add(1, Ordering::SeqCst);
                    }
                })),
                ..peticion()
            })
            .await
            .expect_err("un 429 en streaming debe fallar");
        assert_eq!(error.status, 429);
        assert!(error.is_transient());
        assert_eq!(
            emitidos.load(Ordering::SeqCst),
            0,
            "con un 429 no debía emitirse ningún delta"
        );
    }

    #[tokio::test]
    async fn envia_cabecera_ruta_y_herramientas() {
        struct Visto {
            ruta: String,
            x_api_key: String,
            authorization: String,
            version: String,
            content_type: String,
            cuerpo: Value,
        }

        let visto = Arc::new(Mutex::new(None::<Visto>));
        let servidor_url = servidor({
            let visto = Arc::clone(&visto);
            move |peticion: Request| {
                let visto = Arc::clone(&visto);
                async move {
                    let ruta = peticion.uri().path().to_owned();
                    let x_api_key = peticion
                        .headers()
                        .get("x-api-key")
                        .and_then(|valor| valor.to_str().ok())
                        .unwrap_or("")
                        .to_owned();
                    let authorization = peticion
                        .headers()
                        .get(header::AUTHORIZATION)
                        .and_then(|valor| valor.to_str().ok())
                        .unwrap_or("")
                        .to_owned();
                    let version = peticion
                        .headers()
                        .get("anthropic-version")
                        .and_then(|valor| valor.to_str().ok())
                        .unwrap_or("")
                        .to_owned();
                    let content_type = peticion
                        .headers()
                        .get(header::CONTENT_TYPE)
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
                        x_api_key,
                        authorization,
                        version,
                        content_type,
                        cuerpo,
                    });
                    con_json(200, RESPUESTA_SIMPLE)
                }
            }
        })
        .await;

        let esquema = json!({"type": "object", "properties": {"ruta": {"type": "string"}}});
        let respuesta = proveedor(&format!("{servidor_url}/"))
            .chat(ChatRequest {
                model: "modelo-x".to_owned(),
                messages: vec![
                    Message {
                        role: "system".to_owned(),
                        content: "eres concreto".to_owned(),
                        ..Message::default()
                    },
                    Message {
                        role: "user".to_owned(),
                        content: "hola".to_owned(),
                        ..Message::default()
                    },
                ],
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
        assert_eq!(visto.ruta, "/v1/messages");
        assert_eq!(visto.x_api_key, "sk-ant-test");
        assert_eq!(visto.authorization, "");
        assert_eq!(visto.version, "2023-06-01");
        assert!(visto.content_type.starts_with("application/json"));
        assert_eq!(visto.cuerpo["model"], "modelo-x");
        assert_eq!(visto.cuerpo["max_tokens"], 4096);
        assert_eq!(visto.cuerpo["system"], "eres concreto");
        assert_eq!(visto.cuerpo["tool_choice"], json!({"type": "auto"}));
        assert!(
            visto.cuerpo.get("stream").is_none(),
            "sin OnDelta no debe pedir stream"
        );
        let herramientas = visto.cuerpo["tools"].as_array().expect("tools");
        assert_eq!(herramientas.len(), 2);
        assert_eq!(herramientas[0]["name"], "leer");
        assert_eq!(herramientas[0]["input_schema"], esquema);
        assert_eq!(
            herramientas[1]["input_schema"],
            json!({"type": "object", "properties": {}})
        );
        assert_eq!(respuesta.message.content, "hola");

        // Con auth Bearer se cambia `x-api-key` por `Authorization: Bearer`.
        let captura = Arc::new(Mutex::new(None::<(String, String)>));
        let servidor_url = servidor({
            let captura = Arc::clone(&captura);
            move |peticion: Request| {
                let captura = Arc::clone(&captura);
                async move {
                    let authorization = peticion
                        .headers()
                        .get(header::AUTHORIZATION)
                        .and_then(|valor| valor.to_str().ok())
                        .unwrap_or("")
                        .to_owned();
                    let x_api_key = peticion
                        .headers()
                        .get("x-api-key")
                        .and_then(|valor| valor.to_str().ok())
                        .unwrap_or("")
                        .to_owned();
                    *captura.lock().expect("estado compartido") = Some((authorization, x_api_key));
                    con_json(200, RESPUESTA_SIMPLE)
                }
            }
        })
        .await;
        let mut bearer = proveedor(&servidor_url);
        bearer.auth_bearer = true;
        bearer.chat(peticion()).await.expect("Chat debe responder");
        let (authorization, x_api_key) = captura
            .lock()
            .expect("estado compartido")
            .take()
            .expect("el servidor recibió el pedido");
        assert_eq!(authorization, "Bearer sk-ant-test");
        assert_eq!(x_api_key, "");
    }

    #[tokio::test]
    async fn system_y_tool_results_se_traducen() {
        let cuerpo = Arc::new(Mutex::new(None::<Value>));
        let servidor_url = servidor({
            let cuerpo = Arc::clone(&cuerpo);
            move |peticion: Request| {
                let cuerpo = Arc::clone(&cuerpo);
                async move {
                    let bytes = to_bytes(peticion.into_body(), usize::MAX)
                        .await
                        .expect("cuerpo del pedido");
                    *cuerpo.lock().expect("estado compartido") =
                        Some(serde_json::from_slice(&bytes).expect("el pedido debe ser JSON"));
                    con_json(200, RESPUESTA_SIMPLE)
                }
            }
        })
        .await;
        let historial = vec![
            Message {
                role: "system".to_owned(),
                content: "eres concreto".to_owned(),
                ..Message::default()
            },
            Message {
                role: "user".to_owned(),
                content: "¿qué hay en a.txt?".to_owned(),
                ..Message::default()
            },
            Message {
                role: "assistant".to_owned(),
                content: "lo leo".to_owned(),
                tool_calls: Some(vec![ToolCall {
                    id: "toolu_1".to_owned(),
                    name: "leer".to_owned(),
                    args: json!({"ruta": "a.txt"}),
                    invalid_args: false,
                }]),
                ..Message::default()
            },
            Message {
                role: "tool".to_owned(),
                content: "contenido-del-archivo".to_owned(),
                tool_call_id: "toolu_1".to_owned(),
                ..Message::default()
            },
            Message {
                role: "tool".to_owned(),
                content: "segunda-vuelta".to_owned(),
                tool_call_id: "toolu_2".to_owned(),
                ..Message::default()
            },
            Message {
                role: "assistant".to_owned(),
                content: "listo".to_owned(),
                ..Message::default()
            },
        ];
        proveedor(&servidor_url)
            .chat(ChatRequest {
                model: "m".to_owned(),
                messages: historial,
                tools: Vec::new(),
                on_delta: None,
            })
            .await
            .expect("Chat debe responder");

        let cuerpo = cuerpo
            .lock()
            .expect("estado compartido")
            .take()
            .expect("el servidor recibió el pedido");
        let mensajes = cuerpo["messages"].as_array().expect("messages");
        // El system no viaja como mensaje; los dos tool van juntos.
        assert_eq!(mensajes.len(), 4);
        let asistente = &mensajes[1];
        assert_eq!(asistente["role"], "assistant");
        assert_eq!(asistente["content"][0]["type"], "text");
        assert_eq!(asistente["content"][1]["type"], "tool_use");
        assert_eq!(asistente["content"][1]["id"], "toolu_1");
        assert_eq!(asistente["content"][1]["name"], "leer");
        assert_eq!(asistente["content"][1]["input"], json!({"ruta": "a.txt"}));
        let herramienta = &mensajes[2];
        assert_eq!(herramienta["role"], "user");
        assert_eq!(herramienta["content"].as_array().map(Vec::len), Some(2));
        assert_eq!(herramienta["content"][0]["type"], "tool_result");
        assert_eq!(herramienta["content"][0]["tool_use_id"], "toolu_1");
        assert_eq!(
            herramienta["content"][0]["content"],
            "contenido-del-archivo"
        );
        assert_eq!(herramienta["content"][1]["tool_use_id"], "toolu_2");
        assert_eq!(mensajes[3]["role"], "assistant");
    }

    #[tokio::test]
    async fn lee_tool_use_y_usage() {
        const RESPUESTA: &str = r#"{"content":[
            {"type":"text","text":"lo reviso"},
            {"type":"tool_use","id":"toolu_1","name":"leer","input":{"ruta":"a.txt"}},
            {"type":"tool_use","id":"toolu_2","name":"hora","input":{}}],
            "usage":{"input_tokens":40,"output_tokens":7}}"#;
        let servidor_url = servidor(|_peticion: Request| async { con_json(200, RESPUESTA) }).await;

        let respuesta = proveedor(&servidor_url)
            .chat(peticion())
            .await
            .expect("Chat debe responder");

        assert_eq!(respuesta.message.content, "lo reviso");
        let llamadas = respuesta
            .message
            .tool_calls
            .as_deref()
            .expect("la respuesta trae dos llamadas");
        assert_eq!(llamadas.len(), 2);
        assert_eq!(llamadas[0].id, "toolu_1");
        assert_eq!(llamadas[0].name, "leer");
        assert_eq!(llamadas[0].args, json!({"ruta": "a.txt"}));
        assert_eq!(llamadas[1].args, json!({}));
        assert_eq!(respuesta.prompt_tokens, 40);
        assert_eq!(respuesta.completion_tokens, 7);
    }

    #[tokio::test]
    async fn no_muestra_la_clave_en_errores() {
        const CLAVE: &str = "sk-ant-secreta";
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
    async fn muestra_el_error_del_proveedor_con_respuesta_200() {
        const RESPUESTA: &str =
            r#"{"type":"error","error":{"type":"overloaded_error","message":"sobrecargado"}}"#;
        let servidor_url = servidor(|_peticion: Request| async { con_json(200, RESPUESTA) }).await;

        let error = proveedor(&servidor_url)
            .chat(peticion())
            .await
            .expect_err("un 200 con error debe fallar");
        let mensaje = error.to_string();
        assert!(
            mensaje.contains("sobrecargado"),
            "el error no trae el mensaje del proveedor: {mensaje}"
        );
        assert!(
            !mensaje.contains("ninguna respuesta"),
            "el error no usa el mensaje del proveedor: {mensaje}"
        );
    }

    #[tokio::test]
    async fn sin_bloques_en_respuesta_200() {
        let servidor_url = servidor(|_peticion: Request| async {
            con_json(
                200,
                r#"{"content":null,"usage":{"input_tokens":0,"output_tokens":0}}"#,
            )
        })
        .await;

        let error = proveedor(&servidor_url)
            .chat(peticion())
            .await
            .expect_err("un 200 sin bloques debe fallar");
        assert!(
            error.to_string().contains("no devolvió ninguna respuesta"),
            "err = {error}"
        );
    }

    #[tokio::test]
    async fn avisa_cuando_la_respuesta_se_corta() {
        let servidor_url = servidor(|_peticion: Request| async {
            con_json(
                200,
                r#"{"content":[{"type":"text","text":"a medias"}],"stop_reason":"max_tokens","usage":{"input_tokens":10,"output_tokens":4096}}"#,
            )
        })
        .await;

        let error = proveedor(&servidor_url)
            .chat(peticion())
            .await
            .expect_err("una respuesta cortada no es completa");
        assert_eq!(
            error.to_string(),
            "la respuesta se cortó al llegar al límite de 4096 tokens; pide una respuesta más corta o continúa en otro mensaje"
        );
        let parcial = error
            .partial
            .as_deref()
            .expect("la respuesta parcial viaja en el error");
        assert_eq!(parcial.message.content, "a medias");
    }

    #[tokio::test]
    async fn antecede_un_turno_si_el_historial_empieza_en_assistant() {
        let cuerpo = Arc::new(Mutex::new(None::<Value>));
        let servidor_url = servidor({
            let cuerpo = Arc::clone(&cuerpo);
            move |peticion: Request| {
                let cuerpo = Arc::clone(&cuerpo);
                async move {
                    let bytes = to_bytes(peticion.into_body(), usize::MAX)
                        .await
                        .expect("cuerpo del pedido");
                    *cuerpo.lock().expect("estado compartido") =
                        Some(serde_json::from_slice(&bytes).expect("el pedido debe ser JSON"));
                    con_json(200, RESPUESTA_SIMPLE)
                }
            }
        })
        .await;
        proveedor(&servidor_url)
            .chat(ChatRequest {
                model: "m".to_owned(),
                messages: vec![Message {
                    role: "assistant".to_owned(),
                    content: "ya respondí".to_owned(),
                    ..Message::default()
                }],
                tools: Vec::new(),
                on_delta: None,
            })
            .await
            .expect("Chat debe responder");

        let cuerpo = cuerpo
            .lock()
            .expect("estado compartido")
            .take()
            .expect("el servidor recibió el pedido");
        let mensajes = cuerpo["messages"].as_array().expect("messages");
        assert!(mensajes.len() >= 2, "faltó el turno antepuesto");
        assert_eq!(mensajes[0]["role"], "user");
        assert_eq!(
            mensajes[0]["content"][0]["text"],
            "(continúa la conversación)"
        );
        assert_eq!(mensajes[1]["role"], "assistant");
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
                    con_json(
                        400,
                        r#"{"type":"error","error":{"message":"petición inválida"}}"#,
                    )
                }
            }
        })
        .await;

        proveedor_sin_espera(&servidor_url)
            .chat(peticion())
            .await
            .expect_err("un 400 no se reintenta");
        assert_eq!(llamadas.load(Ordering::SeqCst), 1);
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
                let mut respuesta = con_json(302, "");
                *respuesta.status_mut() = StatusCode::FOUND;
                respuesta.headers_mut().insert(
                    header::LOCATION,
                    format!("{destino}/v1/messages")
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
        assert_eq!(error.status, 302);
        assert_eq!(
            destino_llamadas.load(Ordering::SeqCst),
            0,
            "la petición con la clave llegó al destino de la redirección"
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

    /// Cubre lo que el contrato fija fuera de los doce tests de la tarjeta: imagen
    /// antes del texto, mensaje sin bloques, `input` nulo y `call_{i}`.
    #[test]
    fn normaliza_imagenes_mensajes_vacios_y_entradas_nulas() {
        let (_, mensajes) = convertir_mensajes_anthropic(&[
            Message {
                role: "user".to_owned(),
                content: "¿qué ves?".to_owned(),
                images: Some(vec![Image {
                    mime: "image/png".to_owned(),
                    data: "abc".to_owned(),
                    preview: String::new(),
                }]),
                ..Message::default()
            },
            Message {
                role: "user".to_owned(),
                ..Message::default()
            },
        ]);
        // Dos user seguidos se funden en un mensaje: la API exige alternancia
        // y un turno fallido deja un user colgando (issue #93).
        assert_eq!(mensajes.len(), 1, "dos user seguidos van en un mensaje");
        assert_eq!(mensajes[0].content[0].kind, "image");
        let source = mensajes[0].content[0].source.as_ref().expect("source");
        assert_eq!(source.media_type, "image/png");
        assert_eq!(source.data, "abc");
        assert_eq!(mensajes[0].content[1].kind, "text");
        assert_eq!(mensajes[0].content[1].text, "¿qué ves?");
        assert_eq!(mensajes[0].content[2].text, " ");

        let proveedor = proveedor("http://127.0.0.1:1");
        let respuesta = proveedor
            .leer_respuesta(
                br#"{"content":[
                    {"type":"tool_use","name":"hora","input":null},
                    {"type":"tool_use","id":"","name":"leer","input":{}}]}"#,
            )
            .expect("la respuesta debe ser válida");
        let llamadas = respuesta.message.tool_calls.as_deref().expect("llamadas");
        assert_eq!(llamadas[0].id, "call_0");
        assert_eq!(llamadas[0].args, json!({}));
        assert_eq!(llamadas[1].id, "call_1");

        let solo_texto = proveedor
            .leer_respuesta(br#"{"content":[{"type":"text","text":"hola"}]}"#)
            .expect("la respuesta debe ser válida");
        assert_eq!(
            solo_texto.message.tool_calls, None,
            "sin tool_use, la lista de llamadas queda en None"
        );
    }

    /// La API exige alternancia: dos user seguidos (un turno fallido deja el
    /// user colgando) se funden en un mensaje, y lo mismo assistant/assistant
    /// y el user que sigue a un grupo de tool_result (issue #93).
    #[test]
    fn funde_roles_consecutivos() {
        let (_, mensajes) = convertir_mensajes_anthropic(&[
            Message {
                role: "user".to_owned(),
                content: "primera".to_owned(),
                ..Message::default()
            },
            Message {
                role: "user".to_owned(),
                content: "segunda".to_owned(),
                ..Message::default()
            },
            Message {
                role: "assistant".to_owned(),
                content: "respuesta".to_owned(),
                ..Message::default()
            },
            Message {
                role: "assistant".to_owned(),
                content: "sigo".to_owned(),
                ..Message::default()
            },
        ]);
        assert_eq!(
            mensajes.len(),
            2,
            "user/user y assistant/assistant se funden"
        );
        assert_eq!(mensajes[0].role, "user");
        assert_eq!(mensajes[0].content.len(), 2);
        assert_eq!(mensajes[0].content[0].text, "primera");
        assert_eq!(mensajes[0].content[1].text, "segunda");
        assert_eq!(mensajes[1].role, "assistant");
        assert_eq!(mensajes[1].content.len(), 2);
        assert_eq!(mensajes[1].content[0].text, "respuesta");
        assert_eq!(mensajes[1].content[1].text, "sigo");

        // El user que sigue a los tool_result también se funde con ellos.
        // El historial empieza en assistant: se antepone el turno de cortesía.
        let (_, mensajes) = convertir_mensajes_anthropic(&[
            Message {
                role: "assistant".to_owned(),
                tool_calls: Some(vec![ToolCall {
                    id: "toolu_1".to_owned(),
                    name: "leer".to_owned(),
                    args: json!({}),
                    invalid_args: false,
                }]),
                ..Message::default()
            },
            Message {
                role: "tool".to_owned(),
                content: "resultado".to_owned(),
                tool_call_id: "toolu_1".to_owned(),
                ..Message::default()
            },
            Message {
                role: "user".to_owned(),
                content: "gracias".to_owned(),
                ..Message::default()
            },
        ]);
        assert_eq!(mensajes.len(), 3);
        assert_eq!(mensajes[0].role, "user");
        assert_eq!(mensajes[0].content[0].text, "(continúa la conversación)");
        assert_eq!(mensajes[1].role, "assistant");
        assert_eq!(mensajes[2].role, "user");
        assert_eq!(mensajes[2].content.len(), 2);
        assert_eq!(mensajes[2].content[0].kind, "tool_result");
        assert_eq!(mensajes[2].content[1].text, "gracias");
    }

    /// Un 200 con solo bloques de tipo desconocido no puede cerrar el turno en
    /// blanco: lo que no se entendió no es una respuesta (issue #93).
    #[tokio::test]
    async fn rechaza_un_200_con_solo_bloques_desconocidos() {
        let servidor_url = servidor(|_peticion: Request| async {
            con_json(
                200,
                r#"{"content":[{"type":"thinking","thinking":"razonando"}],"usage":{"input_tokens":3,"output_tokens":1}}"#,
            )
        })
        .await;
        let error = proveedor(&servidor_url)
            .chat(peticion())
            .await
            .expect_err("un 200 sin texto ni tool_use debe fallar");
        assert!(
            error.to_string().contains("no devolvió ninguna respuesta"),
            "err = {error}"
        );
    }

    /// El plazo de 120 s del cliente es TOTAL y corta un stream sano que dura
    /// más. En streaming el plazo debe ser por lectura (issue #93): acá el
    /// cliente de prueba lleva un plazo total de 120 ms y el stream dura
    /// 250 ms; con read_timeout el stream llega entero.
    #[tokio::test]
    async fn el_stream_no_se_corta_con_el_plazo_total() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        async fn chunk(socket: &mut tokio::net::TcpStream, datos: &[u8]) {
            socket
                .write_all(format!("{:x}\r\n", datos.len()).as_bytes())
                .await
                .expect("tamaño del trozo");
            socket.write_all(datos).await.expect("datos del trozo");
            socket.write_all(b"\r\n").await.expect("fin del trozo");
            socket.flush().await.expect("enviar el trozo");
        }

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
            chunk(
                &mut socket,
                b"data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":1}}}\n\n",
            )
            .await;
            chunk(
                &mut socket,
                b"data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\"}}\n\n",
            )
            .await;
            chunk(
                &mut socket,
                b"data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"uno\"}}\n\n",
            )
            .await;
            tokio::time::sleep(Duration::from_millis(250)).await;
            chunk(
                &mut socket,
                b"data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\" dos\"}}\n\n",
            )
            .await;
            chunk(&mut socket, b"data: {\"type\":\"message_stop\"}\n\n").await;
            socket
                .write_all(b"0\r\n\r\n")
                .await
                .expect("fin del cuerpo");
            socket.shutdown().await.expect("cerrar");
        });

        let cliente = Anthropic {
            base_url: format!("http://{addr}"),
            api_key: "sk-ant-test".to_owned(),
            client: reqwest::Client::builder()
                .timeout(Duration::from_millis(120))
                .build()
                .expect("cliente de prueba"),
            retry_delay: None,
            max_tokens: 0,
            auth_bearer: false,
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
}
