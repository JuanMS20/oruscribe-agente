//! Lectura de la respuesta en streaming (SSE) de un proveedor compatible con
//! OpenAI.
//!
//! El
//! reintento HTTP ya ocurrió antes de entrar acá: una vez que el cuerpo empezó
//! a leerse no se repite la petición, para no duplicar texto en la ventana.

use orus_core::{Message, ToolCall};
use serde::Deserialize;

use crate::error::{CORTE_STREAM, ProviderError, SIN_RESPUESTA, respuesta_vacia};
use crate::openai::{MAX_RESPUESTA, OpenAICompat, error_respuesta_cortada};
use crate::runner_tools::MAX_LLAMADAS;
use crate::sse::leer_lineas;
use crate::types::ChatReply;

/// Lee los eventos del servidor y llama a `on_delta` con cada fragmento.
///
/// Un evento partido a la mitad de una letra no rompe: se acumulan las líneas
/// `data:` hasta la línea vacía y se decodifica el JSON completo.
pub(crate) async fn leer_stream(
    respuesta: reqwest::Response,
    proveedor: &OpenAICompat,
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
    let cortada = estado.cortada;
    let finalizada = estado.finalizada;
    let respuesta = estado.terminar();
    if !terminado && !finalizada {
        // Sin [DONE] ni un `finish_reason` final la respuesta no está completa:
        // la parcial que la persona ya vio viaja en el error para que el
        // runner la guarde (issue #93). Varios servidores compatibles cierran
        // el stream tras `finish_reason` sin mandar [DONE].
        return Err(ProviderError::local(CORTE_STREAM).con_parcial(respuesta));
    }
    if cortada {
        // OpenAI no incluye max_tokens en el pedido, así que el límite es el
        // del proveedor; el texto que llegó es parcial y no se puede presentar
        // como respuesta completa.
        return Err(ProviderError::local(error_respuesta_cortada()).con_parcial(respuesta));
    }
    if respuesta_vacia(&respuesta.message) {
        // Un [DONE] sin texto ni llamadas no cierra el turno en blanco
        // (issue #126): extiende al stream el guard del 200 vacío de #93.
        return Err(ProviderError::local(SIN_RESPUESTA));
    }
    Ok(respuesta)
}

fn linea_demasiado_larga() -> ProviderError {
    ProviderError::local(
        "se cortó la respuesta del proveedor: la línea del evento supera los 4 MiB",
    )
}

#[derive(Debug, Default)]
struct Estado {
    texto: String,
    llamadas: Vec<LlamadaEnCurso>,
    prompt_tokens: i64,
    completion_tokens: i64,
    cortada: bool,
    finalizada: bool,
}

#[derive(Debug)]
struct LlamadaEnCurso {
    index: i64,
    id: String,
    name: String,
    argumentos: String,
}

impl Estado {
    /// Procesa una línea del stream. Devuelve `true` cuando llegó `[DONE]`.
    fn procesar(
        &mut self,
        linea: &[u8],
        proveedor: &OpenAICompat,
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
        // `[DONE]` se mira antes de parsear: no es JSON.
        if dato == "[DONE]" {
            return Ok(true);
        }

        let evento: Evento = serde_json::from_str(dato).map_err(|error| {
            ProviderError::local(format!("evento inválido del proveedor: {error}"))
        })?;

        if let Some(error) = &evento.error
            && let Some(mensaje) = &error.message
            && !mensaje.is_empty()
        {
            return Err(ProviderError::local(format!(
                "el proveedor devolvió un error: {}",
                proveedor.resumen(mensaje.as_bytes())
            )));
        }

        // Solo se toma el uso si el evento lo trae: los eventos siguientes sin
        // usage no borran lo que el proveedor ya había reportado.
        if let Some(uso) = &evento.usage {
            self.prompt_tokens = uso.prompt_tokens;
            self.completion_tokens = uso.completion_tokens;
        }

        for eleccion in &evento.choices {
            match eleccion.finish_reason.as_deref() {
                Some("length") => self.cortada = true,
                Some("stop" | "tool_calls" | "function_call") => self.finalizada = true,
                _ => {}
            }
            if let Some(contenido) = &eleccion.delta.content
                && !contenido.is_empty()
            {
                self.texto.push_str(contenido);
                on_delta(contenido);
            }
            for llamada in &eleccion.delta.tool_calls {
                // `index` es obligatorio en el spec de streaming: sin él no se
                // sabe a qué llamada pertenece el delta y no se funde en
                // silencio con la anterior (issue #93).
                let Some(indice) = llamada.index else {
                    return Err(ProviderError::local(
                        "evento inválido del proveedor: falta el index del tool_call",
                    ));
                };
                let existente = self
                    .llamadas
                    .iter()
                    .position(|acumulada| acumulada.index == indice);
                let posicion = match existente {
                    Some(posicion) => posicion,
                    None => {
                        if self.llamadas.len() >= MAX_LLAMADAS {
                            return Err(Self::presupuesto_excedido());
                        }
                        self.llamadas.push(LlamadaEnCurso {
                            index: indice,
                            id: String::new(),
                            name: String::new(),
                            argumentos: String::new(),
                        });
                        self.llamadas.len() - 1
                    }
                };
                let acumulada = &mut self.llamadas[posicion];
                if let Some(id) = &llamada.id
                    && !id.is_empty()
                {
                    // El id se queda con el último no vacío.
                    acumulada.id = id.clone();
                }
                if let Some(name) = &llamada.function.name
                    && !name.is_empty()
                {
                    // El nombre llega una vez por índice: se asigna, no se
                    // concatena; un proveedor que lo repite no produce
                    // "leerleer" (issue #93).
                    acumulada.name = name.clone();
                }
                if let Some(argumentos) = &llamada.function.arguments {
                    acumulada.argumentos.push_str(argumentos);
                }
            }
        }

        if self.excede_presupuesto() {
            return Err(Self::presupuesto_excedido());
        }

        Ok(false)
    }

    fn excede_presupuesto(&self) -> bool {
        self.llamadas.len() > MAX_LLAMADAS
            || self
                .llamadas
                .iter()
                .fold(self.texto.len(), |total, llamada| {
                    total
                        .saturating_add(llamada.id.len())
                        .saturating_add(llamada.name.len())
                        .saturating_add(llamada.argumentos.len())
                })
                > MAX_RESPUESTA
    }

    fn presupuesto_excedido() -> ProviderError {
        ProviderError::local("la respuesta del proveedor supera el límite permitido")
    }

    fn terminar(self) -> ChatReply {
        let mut llamadas = Vec::with_capacity(self.llamadas.len());
        for llamada in &self.llamadas {
            // El contrato guarda los argumentos crudos y el bucle marca `invalidArgs`;
            // acá `Args` ya es un `Value`, así que la marca se pone al entrar.
            // El id de respaldo sale del `index` del proveedor, no de la
            // posición del vector (issue #126).
            llamadas.push(ToolCall::con_argumentos(
                &llamada.id,
                &llamada.name,
                &llamada.argumentos,
                llamada.index as usize,
            ));
        }
        ChatReply {
            message: Message {
                role: "assistant".to_owned(),
                content: self.texto,
                images: None,
                // El contrato crea `calls` con `make`, así que una respuesta sin
                // llamadas queda `[]` y no `null`.
                tool_calls: Some(llamadas),
                tool_call_id: String::new(),
            },
            prompt_tokens: self.prompt_tokens,
            completion_tokens: self.completion_tokens,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct Evento {
    #[serde(default)]
    choices: Vec<Eleccion>,
    #[serde(default)]
    usage: Option<Uso>,
    #[serde(default)]
    error: Option<ErrorEvento>,
}

#[derive(Debug, Default, Deserialize)]
struct Eleccion {
    #[serde(default)]
    delta: Delta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct Delta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<LlamadaDelta>,
}

#[derive(Debug, Default, Deserialize)]
struct LlamadaDelta {
    /// Obligatorio en el spec de streaming; ausente es un evento inválido.
    #[serde(default)]
    index: Option<i64>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: FuncionDelta,
}

#[derive(Debug, Default, Deserialize)]
struct FuncionDelta {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct Uso {
    #[serde(default)]
    prompt_tokens: i64,
    #[serde(default)]
    completion_tokens: i64,
}

#[derive(Debug, Default, Deserialize)]
struct ErrorEvento {
    #[serde(default)]
    message: Option<String>,
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use axum::body::to_bytes;
    use axum::extract::Request;
    use axum::http::{StatusCode, header};
    use axum::response::{IntoResponse, Response};
    use serde_json::{Value, json};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;
    use crate::types::ChatRequest;

    /// Servidor loopback con un handler para cualquier ruta.
    async fn servidor<H, Fut>(handler: H) -> String
    where
        H: Fn(Request) -> Fut + Clone + Send + Sync + 'static,
        Fut: std::future::Future<Output = Response> + Send + 'static,
    {
        let app = axum::Router::new().fallback(handler);
        let listener = TcpListener::bind("127.0.0.1:0")
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

    fn eventos(lineas: &[&str]) -> String {
        let mut cuerpo = String::new();
        for linea in lineas {
            cuerpo.push_str(linea);
            cuerpo.push_str("\n\n");
        }
        cuerpo
    }

    fn proveedor(base: &str) -> OpenAICompat {
        OpenAICompat::new(base, "sk-test").expect("el cliente HTTP debe armarse")
    }

    fn proveedor_con_clave(base: &str, clave: &str) -> OpenAICompat {
        OpenAICompat::new(base, clave).expect("el cliente HTTP debe armarse")
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

    fn fragmentos() -> Arc<Mutex<Vec<String>>> {
        Arc::new(Mutex::new(Vec::new()))
    }

    fn peticion_con_delta(fragmentos: Arc<Mutex<Vec<String>>>) -> ChatRequest {
        ChatRequest {
            on_delta: Some(Box::new(move |texto: &str| {
                fragmentos
                    .lock()
                    .expect("los fragmentos se comparten")
                    .push(texto.to_owned());
            })),
            ..peticion()
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

    async fn esperar_bandera(bandera: &AtomicBool) {
        for _ in 0..300 {
            if bandera.load(Ordering::SeqCst) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("la bandera no se encendió");
    }

    #[tokio::test]
    async fn stream_acumula_el_texto_y_emite_cada_fragmento() {
        let servidor_url = servidor(|_peticion: Request| async {
            sse(concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"Hola\"}}]}\r\n\r\n",
                "data: {\"choices\":[{\"delta\":\r\n",
                "data: {\"content\":\" mundo\"}}]}\r\n\r\n",
                "data: [DONE]\r\r",
            ))
        })
        .await;
        let visto = fragmentos();

        let respuesta = proveedor(&servidor_url)
            .chat(peticion_con_delta(Arc::clone(&visto)))
            .await
            .expect("Chat debe responder");

        assert_eq!(respuesta.message.content, "Hola mundo");
        assert_eq!(
            *visto.lock().expect("fragmentos"),
            ["Hola".to_owned(), " mundo".to_owned()]
        );
        assert_eq!(
            respuesta.message.tool_calls,
            Some(Vec::new()),
            "queda una lista vacía, no null"
        );
    }

    #[tokio::test]
    async fn stream_pide_stream_verdadero() {
        let visto = Arc::new(Mutex::new(None::<Value>));
        let servidor_url = servidor({
            let visto = Arc::clone(&visto);
            move |peticion: Request| {
                let visto = Arc::clone(&visto);
                async move {
                    let bytes = to_bytes(peticion.into_body(), usize::MAX)
                        .await
                        .expect("cuerpo del pedido");
                    let cuerpo =
                        serde_json::from_slice(&bytes).expect("el pedido debe ser JSON válido");
                    *visto.lock().expect("estado compartido") = Some(cuerpo);
                    sse("data: {\"choices\":[{\"delta\":{\"content\":\"hola\"}}]}\n\ndata: [DONE]\n\n")
                }
            }
        })
        .await;

        proveedor(&servidor_url)
            .chat(peticion_con_delta(fragmentos()))
            .await
            .expect("Chat debe responder");

        let visto = visto
            .lock()
            .expect("estado compartido")
            .take()
            .expect("el servidor recibió el pedido");
        assert_eq!(visto["stream"], json!(true));
    }

    /// El servidor corta la ñ justo entre sus dos
    /// bytes, como haría la red.
    #[tokio::test]
    async fn stream_junta_una_letra_partida_entre_dos_eventos() {
        let linea = "data: {\"choices\":[{\"delta\":{\"content\":\"ñ\"}}]}\n\n".as_bytes();
        let corte = linea
            .iter()
            .position(|byte| *byte == 0xC3)
            .expect("la ñ empieza con C3")
            + 1;
        let listener = TcpListener::bind("127.0.0.1:0")
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
            escribir_chunk(&mut socket, &linea[..corte]).await;
            escribir_chunk(&mut socket, &linea[corte..]).await;
            escribir_chunk(&mut socket, b"data: [DONE]\n\n").await;
            socket
                .write_all(b"0\r\n\r\n")
                .await
                .expect("fin del cuerpo");
            socket.shutdown().await.expect("cerrar");
        });

        let respuesta = proveedor(&format!("http://{addr}"))
            .chat(peticion_con_delta(fragmentos()))
            .await
            .expect("Chat debe responder");

        assert_eq!(respuesta.message.content, "ñ");
        tarea.await.expect("el servidor termina");
    }

    #[tokio::test]
    async fn stream_junta_llamadas_partidas_en_varios_eventos() {
        let servidor_url = servidor(|_peticion: Request| async {
            sse(eventos(&[
                r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"get_my_tasks","arguments":"{\"lim"}}]}}]}"#,
                r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"it\":5}"}}]}}]}"#,
                "data: [DONE]",
            ]))
        })
        .await;

        let respuesta = proveedor(&servidor_url)
            .chat(peticion_con_delta(fragmentos()))
            .await
            .expect("Chat debe responder");

        let llamadas = respuesta
            .message
            .tool_calls
            .as_deref()
            .expect("la respuesta trae llamadas");
        assert_eq!(llamadas.len(), 1);
        assert_eq!(llamadas[0].id, "call_1");
        assert_eq!(llamadas[0].name, "get_my_tasks");
        assert_eq!(llamadas[0].args, json!({"limit": 5}));
        assert!(!llamadas[0].invalid_args);
    }

    #[tokio::test]
    async fn stream_avisa_cuando_la_respuesta_se_corta() {
        let servidor_url = servidor(|_peticion: Request| async {
            sse("data: {\"choices\":[{\"delta\":{\"content\":\"a medias\"}}]}\n\n")
        })
        .await;
        let visto = fragmentos();

        let error = proveedor(&servidor_url)
            .chat(peticion_con_delta(Arc::clone(&visto)))
            .await
            .expect_err("un corte sin [DONE] no es una respuesta completa");

        assert_eq!(
            error.to_string(),
            "la respuesta del proveedor se cortó a mitad"
        );
        assert_eq!(*visto.lock().expect("fragmentos"), ["a medias".to_owned()]);

        let servidor_url = servidor(|_peticion: Request| async {
            sse("data: {\"choices\":[{\"delta\":{\"content\":\"a medias\"}}]}\n\ndata: [DONE]\n")
        })
        .await;
        let error = proveedor(&servidor_url)
            .chat(peticion_con_delta(fragmentos()))
            .await
            .expect_err("un [DONE] sin línea vacía no cierra el stream");
        assert_eq!(
            error.to_string(),
            "la respuesta del proveedor se cortó a mitad"
        );
    }

    #[tokio::test]
    async fn un_final_con_finish_reason_sin_done_es_una_respuesta_completa() {
        let servidor_url = servidor(|_peticion: Request| async {
            sse("data: {\"choices\":[{\"delta\":{\"content\":\"hola\"},\"finish_reason\":\"stop\"}]}\n\n")
        })
        .await;

        let respuesta = proveedor(&servidor_url)
            .chat(peticion_con_delta(fragmentos()))
            .await
            .expect("un stream que ya dijo stop no se pierde por faltar [DONE]");

        assert_eq!(respuesta.message.content, "hola");
    }

    /// En
    /// Rust la cancelación es dejar caer el futuro: se aborta la tarea y el
    /// servidor ve cerrarse la conexión.
    #[tokio::test]
    async fn stream_se_corta_al_cancelar() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("el servidor de prueba debe escuchar");
        let addr = listener.local_addr().expect("dirección del servidor");
        let tarea_servidor = tokio::spawn(async move {
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
            // El cliente debe cortar al abortar la tarea.
            loop {
                match socket.read(&mut buffer).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        });

        let llego = Arc::new(AtomicBool::new(false));
        let peticion = ChatRequest {
            on_delta: Some(Box::new({
                let llego = Arc::clone(&llego);
                move |_texto: &str| llego.store(true, Ordering::SeqCst)
            })),
            ..peticion()
        };
        let cliente = proveedor(&format!("http://{addr}"));
        let tarea = tokio::spawn(async move { cliente.chat(peticion).await });

        esperar_bandera(&llego).await;
        tarea.abort();
        let cancelada = tarea.await.expect_err("la tarea debe quedar cancelada");
        assert!(cancelada.is_cancelled());

        tokio::time::timeout(Duration::from_secs(3), tarea_servidor)
            .await
            .expect("el servidor debe ver la conexión cerrada")
            .expect("la tarea del servidor termina");
    }

    #[tokio::test]
    async fn stream_no_muestra_la_clave_en_el_error() {
        const CLAVE: &str = "sk-secreta-123456";
        let servidor_url = servidor(|_peticion: Request| async {
            (
                StatusCode::UNAUTHORIZED,
                [(header::CONTENT_TYPE, "application/json")],
                format!(r#"{{"error":{{"message":"clave {CLAVE} rechazada"}}}}"#),
            )
                .into_response()
        })
        .await;

        let error = proveedor_con_clave(&servidor_url, CLAVE)
            .chat(peticion_con_delta(fragmentos()))
            .await
            .expect_err("un 401 no es una respuesta");

        assert!(
            !error.to_string().contains(CLAVE),
            "el error muestra la clave: {error}"
        );
        assert_eq!(error.status, 401);
    }

    /// El 429 se reintenta antes de leer el cuerpo.
    #[tokio::test]
    async fn reintenta_el_stream_sin_duplicar_texto() {
        let llamadas = Arc::new(AtomicUsize::new(0));
        let servidor_url = servidor({
            let llamadas = Arc::clone(&llamadas);
            move |_peticion: Request| {
                let llamadas = Arc::clone(&llamadas);
                async move {
                    if llamadas.fetch_add(1, Ordering::SeqCst) == 0 {
                        return StatusCode::TOO_MANY_REQUESTS.into_response();
                    }
                    sse("data: {\"choices\":[{\"delta\":{\"content\":\"hola\"}}]}\n\ndata: [DONE]\n\n")
                }
            }
        })
        .await;
        let mut cliente = proveedor(&servidor_url);
        cliente.retry_delay = Some(Box::new(|_| Duration::ZERO));
        let visto = fragmentos();

        let respuesta = cliente
            .chat(peticion_con_delta(Arc::clone(&visto)))
            .await
            .expect("el segundo intento responde");

        assert_eq!(respuesta.message.content, "hola");
        assert_eq!(*visto.lock().expect("fragmentos"), ["hola".to_owned()]);
        assert_eq!(llamadas.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn no_presenta_como_completo_un_stream_cortado() {
        let servidor_url = servidor(|_peticion: Request| async {
            sse("data: {\"choices\":[{\"delta\":{\"content\":\"a medias\"},\"finish_reason\":\"length\"}]}\n\ndata: [DONE]\n\n")
        })
        .await;

        let error = proveedor(&servidor_url)
            .chat(peticion_con_delta(fragmentos()))
            .await
            .expect_err("un stream cortado por length no es completo");

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

    #[tokio::test]
    async fn pide_el_uso_en_streaming() {
        let visto = Arc::new(Mutex::new(None::<Value>));
        let servidor_url = servidor({
            let visto = Arc::clone(&visto);
            move |peticion: Request| {
                let visto = Arc::clone(&visto);
                async move {
                    let bytes = to_bytes(peticion.into_body(), usize::MAX)
                        .await
                        .expect("cuerpo del pedido");
                    let cuerpo =
                        serde_json::from_slice(&bytes).expect("el pedido debe ser JSON válido");
                    *visto.lock().expect("estado compartido") = Some(cuerpo);
                    sse(eventos(&[
                        r#"data: {"choices":[{"delta":{"content":"hola"}}]}"#,
                        r#"data: {"choices":[],"usage":{"prompt_tokens":120,"completion_tokens":7}}"#,
                        "data: [DONE]",
                    ]))
                }
            }
        })
        .await;

        let respuesta = proveedor(&servidor_url)
            .chat(peticion_con_delta(fragmentos()))
            .await
            .expect("Chat debe responder");

        let visto = visto
            .lock()
            .expect("estado compartido")
            .take()
            .expect("el servidor recibió el pedido");
        assert_eq!(visto["stream"], json!(true));
        assert_eq!(visto["stream_options"]["include_usage"], json!(true));
        assert_eq!(respuesta.prompt_tokens, 120);
        assert_eq!(respuesta.completion_tokens, 7);
    }

    /// Un evento sin `usage` después del bloque final no borra el uso recibido
    #[tokio::test]
    async fn stream_conserva_el_uso_si_un_evento_posterior_no_lo_trae() {
        let servidor_url = servidor(|_peticion: Request| async {
            sse(eventos(&[
                r#"data: {"choices":[{"delta":{"content":"a"}}]}"#,
                r#"data: {"choices":[],"usage":{"prompt_tokens":5,"completion_tokens":2}}"#,
                r#"data: {"choices":[{"delta":{"content":"b"}}]}"#,
                "data: [DONE]",
            ]))
        })
        .await;

        let respuesta = proveedor(&servidor_url)
            .chat(peticion_con_delta(fragmentos()))
            .await
            .expect("Chat debe responder");

        assert_eq!(respuesta.message.content, "ab");
        assert_eq!(respuesta.prompt_tokens, 5);
        assert_eq!(respuesta.completion_tokens, 2);
    }

    /// Un evento con `error.message` se muestra como error del proveedor, con
    /// la clave redactada y el resumen de 300 runas.
    #[tokio::test]
    async fn stream_redacta_el_error_del_evento() {
        const CLAVE: &str = "sk-secreta-123456";
        let servidor_url = servidor(|_peticion: Request| async {
            let mensaje = format!("la clave {CLAVE} no sirve {}", "z".repeat(500));
            let evento = json!({"error": {"message": mensaje}});
            sse(format!("data: {evento}\n\ndata: [DONE]\n\n"))
        })
        .await;

        let error = proveedor_con_clave(&servidor_url, CLAVE)
            .chat(peticion_con_delta(fragmentos()))
            .await
            .expect_err("el evento de error debe fallar");

        let mensaje = error.to_string();
        assert!(
            !mensaje.contains(CLAVE),
            "el error muestra la clave: {mensaje}"
        );
        let resumen = mensaje
            .strip_prefix("el proveedor devolvió un error: ")
            .expect("falta el prefijo del error del proveedor");
        assert_eq!(resumen.chars().count(), 300);
    }

    /// El corte sin [DONE] conserva la parcial que la persona ya vio: viaja
    /// en el error para que el runner la guarde (issue #93).
    #[tokio::test]
    async fn stream_cortado_conserva_la_parcial() {
        let servidor_url = servidor(|_peticion: Request| async {
            sse("data: {\"choices\":[{\"delta\":{\"content\":\"a medias\"}}]}\n\n")
        })
        .await;
        let error = proveedor(&servidor_url)
            .chat(peticion_con_delta(fragmentos()))
            .await
            .expect_err("un corte sin [DONE] no es una respuesta completa");
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

    /// Un corte de red a mitad del stream también conserva la parcial vista.
    #[tokio::test]
    async fn corte_de_red_conserva_la_parcial() {
        let listener = TcpListener::bind("127.0.0.1:0")
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
                b"data: {\"choices\":[{\"delta\":{\"content\":\"a medias\"}}]}\n\n",
            )
            .await;
            // Cierra sin el chunk final: la red se corta a mitad del stream.
            socket.shutdown().await.expect("cerrar");
        });
        let error = proveedor(&format!("http://{addr}"))
            .chat(peticion_con_delta(fragmentos()))
            .await
            .expect_err("la red se cortó a mitad del stream");
        let parcial = error
            .partial
            .as_deref()
            .expect("la parcial vista sobrevive al corte de red");
        assert_eq!(parcial.message.content, "a medias");
        tarea.await.expect("el servidor termina");
    }

    /// El spec manda el nombre una vez por índice; un proveedor que lo repite
    /// en cada delta no puede producir "leerleer" (issue #93).
    #[tokio::test]
    async fn el_nombre_de_la_herramienta_se_asigna_una_vez() {
        let servidor_url = servidor(|_peticion: Request| async {
            sse(eventos(&[
                r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"leer","arguments":"{\"ru"}}]}}]}"#,
                r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"leer","arguments":"ta\":\"a.txt\"}"}}]}}]}"#,
                "data: [DONE]",
            ]))
        })
        .await;
        let respuesta = proveedor(&servidor_url)
            .chat(peticion_con_delta(fragmentos()))
            .await
            .expect("Chat debe responder");
        let llamadas = respuesta
            .message
            .tool_calls
            .as_deref()
            .expect("la respuesta trae una llamada");
        assert_eq!(llamadas.len(), 1);
        assert_eq!(
            llamadas[0].name, "leer",
            "el nombre se asigna una vez, no se concatena"
        );
        assert_eq!(llamadas[0].args, json!({"ruta": "a.txt"}));
    }

    /// `index` es obligatorio en el spec de streaming: sin él, la llamada no
    /// puede fundirse con la anterior en silencio (issue #93).
    #[tokio::test]
    async fn un_tool_call_sin_index_falla() {
        let servidor_url = servidor(|_peticion: Request| async {
            sse(eventos(&[
                r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"leer","arguments":"{}"}}]}}]}"#,
                r#"data: {"choices":[{"delta":{"tool_calls":[{"id":"call_2","function":{"name":"hora","arguments":"{}"}}]}}]}"#,
                "data: [DONE]",
            ]))
        })
        .await;
        let error = proveedor(&servidor_url)
            .chat(peticion_con_delta(fragmentos()))
            .await
            .expect_err("un tool_call sin index debe fallar");
        assert!(error.to_string().contains("index"), "err = {error}");
    }

    /// El id de respaldo sale del `index` del proveedor, no de la posición
    /// del vector: dos llamadas con índices 1 y 3 no colisionan (issue #126).
    #[tokio::test]
    async fn stream_usa_el_indice_del_proveador_en_las_llamadas() {
        let servidor_url = servidor(|_peticion: Request| async {
            sse(eventos(&[
                r#"data: {"choices":[{"delta":{"tool_calls":[{"index":1,"function":{"name":"primera","arguments":"{}"}}]}}]}"#,
                r#"data: {"choices":[{"delta":{"tool_calls":[{"index":3,"function":{"name":"segunda","arguments":"{}"}}]}}]}"#,
                "data: [DONE]",
            ]))
        })
        .await;

        let respuesta = proveedor(&servidor_url)
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

    /// Un stream que termina sin texto y sin llamadas no cierra el turno
    /// (issue #126).
    #[tokio::test]
    async fn stream_vacio_no_es_respuesta() {
        let servidor_url = servidor(|_peticion: Request| async { sse("data: [DONE]\n\n") }).await;

        let error = proveedor(&servidor_url)
            .chat(peticion_con_delta(fragmentos()))
            .await
            .expect_err("un [DONE] sin contenido no es una respuesta");

        assert_eq!(
            error.to_string(),
            "el proveedor no devolvió ninguna respuesta"
        );
    }

    #[tokio::test]
    async fn stream_no_acepta_llamadas_sin_presupuesto() {
        let servidor_url = servidor(|_peticion: Request| async {
            let mut cuerpo = String::new();
            for indice in 0..=MAX_LLAMADAS {
                let evento = json!({
                    "choices": [{
                        "delta": {
                            "tool_calls": [{
                                "index": indice,
                                "id": format!("c{indice}"),
                                "function": {"name": "leer", "arguments": ""}
                            }]
                        }
                    }]
                });
                cuerpo.push_str(&format!("data: {evento}\n\n"));
            }
            sse(cuerpo)
        })
        .await;

        let error = proveedor(&servidor_url)
            .chat(peticion_con_delta(fragmentos()))
            .await
            .expect_err("los índices únicos no pueden crecer sin límite");
        assert!(
            error.to_string().contains("supera el límite permitido"),
            "error = {error}"
        );
    }
}
