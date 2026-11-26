//! Lectura de la respuesta en streaming (SSE) de la Messages API de Anthropic.
//!
//! El
//! reintento HTTP ya ocurrió antes de entrar acá: una vez que el cuerpo empezó
//! a leerse no se repite la petición, para no duplicar texto en la ventana.

use orus_core::{Message, ToolCall};
use serde::Deserialize;

use crate::anthropic::{Anthropic, error_respuesta_cortada};
use crate::error::{CORTE_STREAM, ProviderError, SIN_RESPUESTA, respuesta_vacia};
use crate::openai::{MAX_RESPUESTA, resumen};
use crate::runner_tools::MAX_LLAMADAS;
use crate::sse::leer_lineas;
use crate::types::ChatReply;

/// Lee los eventos del servidor y llama a `on_delta` con cada fragmento.
///
/// Un evento partido a la mitad de una letra no rompe: se acumulan las líneas
/// `data:` hasta la línea vacía y se decodifica el JSON completo.
pub(crate) async fn leer_stream(
    respuesta: reqwest::Response,
    proveedor: &Anthropic,
    on_delta: &(dyn Fn(&str) + Send + Sync),
) -> Result<ChatReply, ProviderError> {
    let mut estado = Estado::default();
    let resultado = leer_lineas(respuesta, linea_demasiado_larga, |linea| {
        estado.procesar(linea, proveedor, on_delta)
    })
    .await;
    let terminado = match resultado {
        Ok(terminado) => terminado,
        // El corte de red también conserva lo que la persona ya vio (#93).
        Err(error) => return Err(error.con_parcial(estado.terminar())),
    };
    let con_stop = estado.terminado;
    let cortada = estado.cortada;
    let respuesta = estado.terminar();
    if !terminado && !con_stop {
        // Sin message_stop la respuesta no está completa: la parcial que la
        // persona ya vio viaja en el error para que el runner la guarde (#93).
        return Err(ProviderError::local(CORTE_STREAM).con_parcial(respuesta));
    }
    if cortada {
        // El texto llegó incompleto: devolverlo como respuesta entera haría
        // creer que el modelo terminó. D24: la parcial viaja en el error.
        return Err(
            ProviderError::local(error_respuesta_cortada(proveedor.limite()))
                .con_parcial(respuesta),
        );
    }
    if respuesta_vacia(&respuesta.message) {
        // Un message_stop sin texto ni herramientas no cierra el turno en
        // blanco (issue #126).
        return Err(ProviderError::local(SIN_RESPUESTA));
    }
    Ok(respuesta)
}

/// El texto del error de línea demasiado larga.
fn linea_demasiado_larga() -> ProviderError {
    ProviderError::local("se cortó la respuesta del proveedor: bufio.Scanner: token too long")
}

fn evento_invalido(error: serde_json::Error) -> ProviderError {
    ProviderError::local(format!("evento inválido del proveedor: {error}"))
}

#[derive(Debug, Default)]
struct Estado {
    bloques: Vec<BloqueStream>,
    prompt_tokens: i64,
    completion_tokens: i64,
    cortada: bool,
    terminado: bool,
}

#[derive(Debug, Default)]
struct BloqueStream {
    indice: i64,
    tipo: String,
    texto: String,
    id: String,
    name: String,
    args: String,
}

impl Estado {
    /// Anuncia el bloque de `indice` al recibir su `content_block_start`.
    /// El orden del stream es el orden de primera aparición.
    fn crear_bloque(&mut self, indice: i64) -> Result<usize, ProviderError> {
        if let Some(posicion) = self.posicion(indice) {
            return Ok(posicion);
        }
        if self.bloques.len() >= MAX_LLAMADAS {
            return Err(Self::presupuesto_excedido());
        }
        self.bloques.push(BloqueStream {
            indice,
            ..BloqueStream::default()
        });
        Ok(self.bloques.len() - 1)
    }

    fn posicion(&self, indice: i64) -> Option<usize> {
        self.bloques
            .iter()
            .position(|bloque| bloque.indice == indice)
    }

    /// Procesa una línea del stream. Devuelve `true` cuando llegó `[DONE]`.
    fn procesar(
        &mut self,
        linea: &[u8],
        proveedor: &Anthropic,
        on_delta: &(dyn Fn(&str) + Send + Sync),
    ) -> Result<bool, ProviderError> {
        let linea = String::from_utf8_lossy(linea);
        let linea = linea.trim();
        let Some(dato) = linea.strip_prefix("data:") else {
            return Ok(false);
        };
        let dato = dato.trim();
        if dato.is_empty() {
            return Ok(false);
        }
        // `[DONE]` no es el final del protocolo de Anthropic: solo cierra el
        // stream cuando `message_stop` ya cerró el mensaje. Si llega antes,
        // el turno se corta y la parcial viaja en el error (issue #126).
        if dato == "[DONE]" {
            return if self.terminado {
                Ok(true)
            } else {
                Err(ProviderError::local(CORTE_STREAM))
            };
        }

        let base: EventoBase = serde_json::from_str(dato).map_err(evento_invalido)?;
        match base.kind.as_str() {
            "message_start" => {
                // En este evento el error de JSON se ignora.
                if let Ok(evento) = serde_json::from_str::<EventoInicio>(dato)
                    && evento.message.usage.input_tokens > 0
                {
                    self.prompt_tokens = evento.message.usage.input_tokens;
                }
            }
            "content_block_start" => {
                let evento: EventoBloqueInicio =
                    serde_json::from_str(dato).map_err(evento_invalido)?;
                let posicion = self.crear_bloque(evento.index)?;
                {
                    let bloque = &mut self.bloques[posicion];
                    bloque.tipo = evento.content_block.kind;
                    bloque.id = evento.content_block.id;
                    bloque.name = evento.content_block.name;
                }
                if self.excede_presupuesto() {
                    return Err(Self::presupuesto_excedido());
                }
            }
            "content_block_delta" => {
                let evento: EventoDelta = serde_json::from_str(dato).map_err(evento_invalido)?;
                let Some(posicion) = self.posicion(evento.index) else {
                    return Err(ProviderError::local(
                        "evento inválido del proveedor: delta sin content_block_start",
                    ));
                };
                match evento.delta.kind.as_str() {
                    "text_delta" => {
                        let bloque = &mut self.bloques[posicion];
                        bloque.texto.push_str(&evento.delta.text);
                        on_delta(&evento.delta.text);
                    }
                    "input_json_delta" => {
                        self.bloques[posicion]
                            .args
                            .push_str(&evento.delta.partial_json);
                    }
                    _ => {}
                }
                if self.excede_presupuesto() {
                    return Err(Self::presupuesto_excedido());
                }
            }
            "message_delta" => {
                let evento: EventoFin = serde_json::from_str(dato).map_err(evento_invalido)?;
                if evento.delta.stop_reason == "max_tokens" {
                    self.cortada = true;
                }
                if let Some(uso) = evento.usage {
                    self.completion_tokens = uso.output_tokens;
                }
            }
            "message_stop" => self.terminado = true,
            "error" => {
                let mensaje = match serde_json::from_str::<EventoError>(dato) {
                    Ok(evento) if !evento.error.message.is_empty() => {
                        resumen(evento.error.message.as_bytes(), &proveedor.api_key)
                    }
                    _ => "sin detalle".to_owned(),
                };
                return Err(ProviderError::local(format!(
                    "el proveedor devolvió un error: {mensaje}"
                )));
            }
            // ping, content_block_stop y message_limit no acumulan nada; un
            // tipo que este adaptador no habla también se ignora, como el
            // switch sin default del contrato.
            _ => {}
        }
        Ok(false)
    }

    fn excede_presupuesto(&self) -> bool {
        self.bloques.len() > MAX_LLAMADAS
            || self.bloques.iter().fold(0usize, |total, bloque| {
                total
                    .saturating_add(bloque.tipo.len())
                    .saturating_add(bloque.id.len())
                    .saturating_add(bloque.name.len())
                    .saturating_add(bloque.texto.len())
                    .saturating_add(bloque.args.len())
            }) > MAX_RESPUESTA
    }

    fn presupuesto_excedido() -> ProviderError {
        ProviderError::local("la respuesta del proveedor supera el límite permitido")
    }

    fn terminar(self) -> ChatReply {
        let mut texto = String::new();
        let mut llamadas = Vec::new();
        for bloque in &self.bloques {
            match bloque.tipo.as_str() {
                "text" => texto.push_str(&bloque.texto),
                "tool_use" => {
                    // Un `input_json_delta` vacío es la herramienta sin
                    // argumentos; uno roto se marca para no ejecutarla con
                    // datos inventados. El id de
                    // respaldo sale del índice del proveedor, no de la
                    // posición del vector (issue #126).
                    llamadas.push(ToolCall::con_argumentos(
                        &bloque.id,
                        &bloque.name,
                        &bloque.args,
                        bloque.indice as usize,
                    ));
                }
                _ => {}
            }
        }
        ChatReply {
            message: Message {
                role: "assistant".to_owned(),
                content: texto,
                images: None,
                // El contrato crea las llamadas con `make`, así que una respuesta sin
                // llamadas queda `[]` y no `null` (D28).
                tool_calls: Some(llamadas),
                tool_call_id: String::new(),
            },
            prompt_tokens: self.prompt_tokens,
            completion_tokens: self.completion_tokens,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct EventoBase {
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct EventoInicio {
    message: MensajeInicio,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct MensajeInicio {
    usage: Uso,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Uso {
    input_tokens: i64,
    output_tokens: i64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct EventoBloqueInicio {
    index: i64,
    content_block: BloqueInicio,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct BloqueInicio {
    #[serde(rename = "type")]
    kind: String,
    id: String,
    name: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct EventoDelta {
    index: i64,
    delta: Delta,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Delta {
    #[serde(rename = "type")]
    kind: String,
    text: String,
    partial_json: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct EventoFin {
    delta: DeltaFin,
    usage: Option<Uso>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct DeltaFin {
    stop_reason: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct EventoError {
    error: DetalleError,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct DetalleError {
    message: String,
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::body::to_bytes;
    use axum::extract::Request;
    use axum::http::header;
    use axum::response::{IntoResponse, Response};
    use serde_json::{Value, json};

    use super::*;
    use crate::types::ChatRequest;

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

    fn sse(cuerpo: impl Into<String>) -> Response {
        ([(header::CONTENT_TYPE, "text/event-stream")], cuerpo.into()).into_response()
    }

    fn fragmentos() -> Arc<Mutex<Vec<String>>> {
        Arc::new(Mutex::new(Vec::new()))
    }

    fn peticion_con_delta(fragmentos: Arc<Mutex<Vec<String>>>) -> ChatRequest {
        ChatRequest {
            model: "m".to_owned(),
            messages: vec![Message {
                role: "user".to_owned(),
                content: "hola".to_owned(),
                ..Message::default()
            }],
            tools: Vec::new(),
            on_delta: Some(Box::new(move |texto: &str| {
                fragmentos
                    .lock()
                    .expect("los fragmentos se comparten")
                    .push(texto.to_owned());
            })),
        }
    }

    #[tokio::test]
    async fn stream_con_texto_y_herramienta() {
        const STREAM: &str = r#"event: message_start
data: {"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","content":[],"usage":{"input_tokens":25,"output_tokens":1}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hola "}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"mundo"}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: content_block_start
data: {"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_9","name":"leer"}}

event: content_block_delta
data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"ru"}}

event: content_block_delta
data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"ta\":\"a.txt\"}"}}

event: content_block_stop
data: {"type":"content_block_stop","index":1}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},"usage":{"output_tokens":15}}

event: message_stop
data: {"type":"message_stop"}

"#;
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
                    sse(STREAM)
                }
            }
        })
        .await;
        let visto = fragmentos();

        let respuesta = Anthropic::new(&servidor_url, "sk-ant-test")
            .expect("el cliente HTTP debe armarse")
            .chat(peticion_con_delta(Arc::clone(&visto)))
            .await
            .expect("Chat debe responder");

        assert_eq!(
            *visto.lock().expect("fragmentos"),
            ["Hola ".to_owned(), "mundo".to_owned()]
        );
        assert_eq!(respuesta.message.content, "Hola mundo");
        let llamada = &respuesta
            .message
            .tool_calls
            .as_deref()
            .expect("la respuesta trae una llamada")[0];
        assert_eq!(llamada.id, "toolu_9");
        assert_eq!(llamada.name, "leer");
        assert_eq!(llamada.args, json!({"ruta": "a.txt"}));
        assert_eq!(
            (respuesta.prompt_tokens, respuesta.completion_tokens),
            (25, 15)
        );

        let pedido = cuerpo
            .lock()
            .expect("estado compartido")
            .take()
            .expect("el servidor recibió el pedido");
        assert_eq!(pedido["stream"], json!(true));
    }

    #[tokio::test]
    async fn stream_cortado() {
        const STREAM: &str = "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":10,\"output_tokens\":1}}}\n\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\"}}\n\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hola\"}}\n\n";
        let servidor_url = servidor(|_peticion: Request| async { sse(STREAM) }).await;

        let error = Anthropic::new(&servidor_url, "sk-ant-test")
            .expect("el cliente HTTP debe armarse")
            .chat(peticion_con_delta(fragmentos()))
            .await
            .expect_err("un stream sin message_stop no es una respuesta completa");

        assert_eq!(
            error.to_string(),
            "la respuesta del proveedor se cortó a mitad"
        );
    }

    #[tokio::test]
    async fn acepta_done_al_final_del_stream() {
        const STREAM: &str = "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":3}}}\n\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\"}}\n\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hola\"}}\n\ndata: {\"type\":\"message_stop\"}\n\ndata: [DONE]\n\n";
        let servidor_url = servidor(|_peticion: Request| async { sse(STREAM) }).await;

        let respuesta = Anthropic::new(&servidor_url, "sk-ant-test")
            .expect("el cliente HTTP debe armarse")
            .chat(peticion_con_delta(fragmentos()))
            .await
            .expect("Chat debe aceptar [DONE] al final");

        assert_eq!(respuesta.message.content, "hola");
        assert_eq!(
            respuesta.message.tool_calls,
            Some(Vec::new()),
            "queda una lista vacía, no null (D28)"
        );
    }

    /// Un corte por max_tokens no se presenta
    /// como respuesta completa y la parcial viaja en el error (D24).
    #[tokio::test]
    async fn avisa_cuando_la_respuesta_se_corta() {
        const STREAM: &str = "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":10}}}\n\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\"}}\n\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"a medias\"}}\n\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"max_tokens\"},\"usage\":{\"output_tokens\":4096}}\n\ndata: {\"type\":\"message_stop\"}\n\n";
        let servidor_url = servidor(|_peticion: Request| async { sse(STREAM) }).await;

        let error = Anthropic::new(&servidor_url, "sk-ant-test")
            .expect("el cliente HTTP debe armarse")
            .chat(peticion_con_delta(fragmentos()))
            .await
            .expect_err("un stream cortado por max_tokens no es completo");

        assert_eq!(
            error.to_string(),
            "la respuesta se cortó al llegar al límite de 4096 tokens; pide una respuesta más corta o continúa en otro mensaje"
        );
        let parcial = error
            .partial
            .as_deref()
            .expect("la respuesta parcial viaja en el error");
        assert_eq!(parcial.message.content, "a medias");
        assert_eq!(parcial.completion_tokens, 4096);
    }

    /// El corte sin message_stop conserva la parcial que la persona ya vio:
    /// viaja en el error para que el runner la guarde (issue #93).
    #[tokio::test]
    async fn stream_cortado_conserva_la_parcial() {
        const STREAM: &str = "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\"}}\n\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"a medias\"}}\n\n";
        let servidor_url = servidor(|_peticion: Request| async { sse(STREAM) }).await;
        let error = Anthropic::new(&servidor_url, "sk-ant-test")
            .expect("el cliente HTTP debe armarse")
            .chat(peticion_con_delta(fragmentos()))
            .await
            .expect_err("un stream sin message_stop no es una respuesta completa");
        assert_eq!(
            error.to_string(),
            "la respuesta del proveedor se cortó a mitad"
        );
        let parcial = error
            .partial
            .as_deref()
            .expect("la parcial que la persona vio viaja en el error");
        assert_eq!(parcial.message.content, "a medias");
    }

    /// El id de respaldo sale del `index` del bloque, no de su posición en el
    /// vector: dos bloques con índices 1 y 3 no colisionan (issue #126).
    #[tokio::test]
    async fn stream_usa_el_indice_del_proveador_en_los_bloques() {
        const STREAM: &str = r#"data: {"type":"message_start","message":{"usage":{"input_tokens":3}}}

data: {"type":"content_block_start","index":1,"content_block":{"type":"tool_use","name":"primera"}}

data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{}"}}

data: {"type":"content_block_start","index":3,"content_block":{"type":"tool_use","name":"segunda"}}

data: {"type":"content_block_delta","index":3,"delta":{"type":"input_json_delta","partial_json":"{}"}}

data: {"type":"message_stop"}

"#;
        let servidor_url = servidor(|_peticion: Request| async { sse(STREAM) }).await;

        let respuesta = Anthropic::new(&servidor_url, "sk-ant-test")
            .expect("el cliente HTTP debe armarse")
            .chat(peticion_con_delta(fragmentos()))
            .await
            .expect("Chat debe responder");
        let llamadas = respuesta
            .message
            .tool_calls
            .as_deref()
            .expect("la respuesta trae llamadas");
        let ids: Vec<_> = llamadas.iter().map(|llamada| llamada.id.as_str()).collect();
        assert_eq!(ids, ["call_1", "call_3"]);
    }

    /// `[DONE]` sin `message_stop` es un corte: el texto visto viaja en el
    /// error y no se presenta como respuesta completa (issue #126).
    #[tokio::test]
    async fn done_sin_message_stop_es_un_corte() {
        const STREAM: &str = r#"data: {"type":"message_start","message":{"usage":{"input_tokens":3}}}

data: {"type":"content_block_start","index":0,"content_block":{"type":"text"}}

data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hola"}}

data: [DONE]
"#;
        let servidor_url = servidor(|_peticion: Request| async { sse(STREAM) }).await;

        let error = Anthropic::new(&servidor_url, "sk-ant-test")
            .expect("el cliente HTTP debe armarse")
            .chat(peticion_con_delta(fragmentos()))
            .await
            .expect_err("un [DONE] sin message_stop no es una respuesta completa");

        assert_eq!(
            error.to_string(),
            "la respuesta del proveedor se cortó a mitad"
        );
        let parcial = error
            .partial
            .as_deref()
            .expect("la parcial vista viaja en el error");
        assert_eq!(parcial.message.content, "hola");
    }

    /// Un `message_stop` sin texto ni herramientas tampoco es una respuesta
    /// (issue #126).
    #[tokio::test]
    async fn message_stop_sin_contenido_no_es_respuesta() {
        const STREAM: &str = r#"data: {"type":"message_start","message":{"usage":{"input_tokens":3}}}

data: {"type":"message_stop"}

"#;
        let servidor_url = servidor(|_peticion: Request| async { sse(STREAM) }).await;

        let error = Anthropic::new(&servidor_url, "sk-ant-test")
            .expect("el cliente HTTP debe armarse")
            .chat(peticion_con_delta(fragmentos()))
            .await
            .expect_err("un stream sin contenido no es una respuesta");

        assert_eq!(
            error.to_string(),
            "el proveedor no devolvió ninguna respuesta"
        );
    }

    #[tokio::test]
    async fn eventos_invalidos_y_bloques_sin_presupuesto_no_cierran_el_turno() {
        const SIN_START: &str = r#"data: {"type":"message_start","message":{"usage":{"input_tokens":3}}}

data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hola"}}

data: {"type":"message_stop"}

"#;
        let servidor_url = servidor(|_peticion: Request| async { sse(SIN_START) }).await;
        let visto = fragmentos();
        let error = Anthropic::new(&servidor_url, "sk-ant-test")
            .expect("el cliente HTTP debe armarse")
            .chat(peticion_con_delta(Arc::clone(&visto)))
            .await
            .expect_err("un delta sin content_block_start es inválido");
        assert!(error.to_string().contains("evento inválido"));
        assert!(
            visto.lock().expect("fragmentos").is_empty(),
            "el texto fantasma no debe llegar a la interfaz"
        );

        const DELTA_INVALIDO: &str = r#"data: {"type":"message_start","message":{"usage":{"input_tokens":3}}}

data: {"type":"content_block_start","index":0,"content_block":{"type":"text"}}

data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"a medias"}}

data: {"type":"message_delta","delta":{"stop_reason":"max_tokens"},"usage":{"output_tokens":"4096"}}

data: {"type":"message_stop"}

"#;
        let servidor_url = servidor(|_peticion: Request| async { sse(DELTA_INVALIDO) }).await;
        let error = Anthropic::new(&servidor_url, "sk-ant-test")
            .expect("el cliente HTTP debe armarse")
            .chat(peticion_con_delta(fragmentos()))
            .await
            .expect_err("un message_delta roto no puede presentarse como completo");
        assert!(error.to_string().contains("evento inválido"));
        assert_eq!(
            error
                .partial
                .as_deref()
                .expect("la parcial que la persona vio viaja en el error")
                .message
                .content,
            "a medias"
        );

        let servidor_url = servidor(|_peticion: Request| async {
            let mut cuerpo = String::new();
            for indice in 0..=MAX_LLAMADAS {
                cuerpo.push_str(&format!(
                    "data: {{\"type\":\"content_block_start\",\"index\":{indice},\"content_block\":{{\"type\":\"text\"}}}}\n\n"
                ));
            }
            sse(cuerpo)
        })
        .await;
        let error = Anthropic::new(&servidor_url, "sk-ant-test")
            .expect("el cliente HTTP debe armarse")
            .chat(peticion_con_delta(fragmentos()))
            .await
            .expect_err("los bloques sin deltas también agotan el presupuesto");
        assert!(
            error.to_string().contains("supera el límite permitido"),
            "error = {error}"
        );
    }
}
