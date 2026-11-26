//! Timbre de Oruscribe: suscripción viva a un topic de Supabase Realtime.
//!
//! `Config`, `Event`, la unión
//! al canal con `phx_join`, el latido cada 25 s, el bucle de lectura y el cierre
//! idempotente.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use thiserror::Error;
use tokio::net::TcpStream;
use tokio::sync::{Mutex, watch};
use tokio::time::MissedTickBehavior;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::ClientRequestBuilder;
use tokio_tungstenite::tungstenite::http::Uri;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

/// El socket partido: el sink va detrás del mutex y el stream en la lectura.
type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;
type SocketSink = SplitSink<Socket, Message>;
type SocketStream = SplitStream<Socket>;

/// El `ref` del join; el primer latido usa "2".
const JOIN_REF: &str = "1";

/// El handshake y el join esperan como máximo 15 s.
const JOIN_TIMEOUT: Duration = Duration::from_secs(15);

/// Cada cuánto late el timbre.
const HEARTBEAT_EVERY: Duration = Duration::from_secs(25);

/// Sin tráfico ni respuesta al latido, el socket quedó a medias. Se corta para
/// que el `Watch` vuelva a conectar con su backoff en vez de quedarse mudo.
const READ_TIMEOUT: Duration = Duration::from_secs(90);

/// Lo que responde la herramienta `get_my_doorbell`.
///
/// Un campo ausente queda en
/// su cero, como el `encoding/json` del contrato.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct Config {
    pub topic: String,
    pub supabase_url: String,
    pub anon_key: String,
    pub private: bool,
    /// JWT del usuario. Vacío en un canal público, y entonces no viaja en el
    /// join.
    pub access_token: String,
}

/// El contenido de un aviso.
///
/// El payload es un [`Value`], así que se conserva la estructura
/// pero no el orden de las claves.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub kind: String,
    pub name: String,
    pub raw: Value,
}

/// El aviso que corre en la tarea de lectura; no debe bloquear
pub type OnEvent = Box<dyn FnMut(Event) + Send + 'static>;

/// Lo que comparten el cierre, el latido y la tarea de lectura.
struct Shared {
    sink: Mutex<SocketSink>,
    refs: AtomicU64,
    closed: AtomicBool,
    done: watch::Sender<bool>,
}

/// Una conexión viva a un topic.
///
/// El latido y la lectura corren en sus propias tareas. El llamador cierra con
/// [`Doorbell::close`] o espera el cierre remoto con [`Doorbell::done`].
pub struct Doorbell {
    shared: Arc<Shared>,
}

/// Error del timbre.
///
/// Los textos son los del contrato: los lee una persona y algunos los comparan los
/// tests.
#[derive(Debug, Error)]
pub enum DoorbellError {
    #[error("el timbre llegó incompleto")]
    Incomplete,

    #[error("el timbre llegó sin llave")]
    MissingKey,

    #[error("no pudimos abrir el timbre: {0}")]
    Connect(String),

    #[error("el servidor no abrió el canal: {0}")]
    JoinRejected(String),

    #[error("el timbre no contestó: {0}")]
    NoReply(String),

    /// Un fallo de `write`: serializar o escribir el frame. El contrato devuelve el
    /// error sin envolver.
    #[error("{0}")]
    Write(String),
}

/// Se une al topic y deja corriendo el latido y la lectura.
///
/// El callback corre en la tarea
/// de lectura y no debe bloquear.
pub async fn dial(
    cfg: Config,
    on_event: Option<OnEvent>,
) -> std::result::Result<Doorbell, DoorbellError> {
    dial_with_heartbeat(cfg, on_event, HEARTBEAT_EVERY, READ_TIMEOUT, JOIN_TIMEOUT).await
}

/// El test acorta el latido, el plazo de lectura y el del handshake sin
/// esperar 25 s; el contrato sigue en [`HEARTBEAT_EVERY`], [`READ_TIMEOUT`] y
/// [`JOIN_TIMEOUT`]. No son opciones de [`Config`].
async fn dial_with_heartbeat(
    cfg: Config,
    on_event: Option<OnEvent>,
    heartbeat_every: Duration,
    read_timeout: Duration,
    connect_timeout: Duration,
) -> std::result::Result<Doorbell, DoorbellError> {
    if cfg.topic.is_empty() || cfg.supabase_url.is_empty() {
        return Err(DoorbellError::Incomplete);
    }
    if cfg.anon_key.is_empty() {
        return Err(DoorbellError::MissingKey);
    }
    let base = reqwest::Url::parse(&cfg.supabase_url).map_err(|_| {
        DoorbellError::Connect("la base del timbre no es una dirección válida".to_owned())
    })?;
    if !crate::url_segura(&base) {
        return Err(DoorbellError::Connect(
            "la base del timbre debe usar https salvo localhost".to_owned(),
        ));
    }

    let url = websocket_url(&cfg.supabase_url, &cfg.anon_key);
    let uri = url
        .parse::<Uri>()
        .map_err(|error| DoorbellError::Connect(error.to_string()))?;
    // El User-Agent es el del contrato.
    let request = ClientRequestBuilder::new(uri).with_header("User-Agent", "Oruscribe Agente");
    // El handshake (TCP + TLS + upgrade HTTP) también tiene plazo: un servidor
    // que acepta y calla no puede dejar la app en «conectando» para siempre.
    let (mut conn, _) = tokio::time::timeout(connect_timeout, connect_async(request))
        .await
        .map_err(|_| DoorbellError::Connect("context deadline exceeded".to_owned()))?
        .map_err(|error| DoorbellError::Connect(error.to_string()))?;

    let topic = format!("realtime:{}", cfg.topic);
    if let Err(error) = join(&mut conn, &topic, &cfg.access_token, cfg.private).await {
        // El contrato cierra con StatusInternalError y "sin canal" si el join falla
        let _ = conn
            .close(Some(CloseFrame {
                code: CloseCode::Error,
                reason: "sin canal".into(),
            }))
            .await;
        return Err(error);
    }

    // El sink queda detrás del mutex y el stream en la tarea de lectura
    let (sink, stream) = conn.split();
    let (done, _) = watch::channel(false);
    let shared = Arc::new(Shared {
        sink: Mutex::new(sink),
        refs: AtomicU64::new(0),
        closed: AtomicBool::new(false),
        done,
    });
    tokio::spawn(heartbeat(Arc::clone(&shared), heartbeat_every));
    tokio::spawn(read_loop(
        Arc::clone(&shared),
        stream,
        on_event,
        read_timeout,
    ));
    Ok(Doorbell { shared })
}

/// Arma la URL del websocket
fn websocket_url(supabase: &str, anon_key: &str) -> String {
    let url = supabase.replacen("https://", "wss://", 1);
    let url = url.replacen("http://", "ws://", 1);
    format!(
        "{}/realtime/v1/websocket?apikey={anon_key}&vsn=1.0.0",
        url.trim_end_matches('/')
    )
}

impl Doorbell {
    /// Espera a que el canal se cierre, por la app o por la red.
    ///
    /// Mira el valor actual, así que un
    /// cierre anterior no se pierde.
    pub async fn done(&self) {
        let mut done = self.shared.done.subscribe();
        let _ = done.wait_for(|closed| *closed).await;
    }

    /// Cierra el canal. Es seguro llamarlo más de una vez.
    ///
    /// La segunda llamada devuelve
    /// `Ok`, cierra `Done` una sola vez y manda el cierre normal "adios".
    pub async fn close(&self) -> std::result::Result<(), DoorbellError> {
        self.shared.close().await
    }
}

impl Shared {
    /// Serializa, comprueba el cierre y manda un frame de texto
    async fn write(&self, value: &Value) -> std::result::Result<(), DoorbellError> {
        let data = serde_json::to_string(value)
            .map_err(|error| DoorbellError::Write(error.to_string()))?;
        let mut sink = self.sink.lock().await;
        if self.closed.load(Ordering::SeqCst) {
            return Err(DoorbellError::Write("el timbre ya está cerrado".to_owned()));
        }
        sink.send(Message::Text(data.into()))
            .await
            .map_err(|error| DoorbellError::Write(error.to_string()))
    }

    /// El contrato incrementa y devuelve uno más: el join usa
    /// "1" y el primer latido sale "2".
    fn next_ref(&self) -> String {
        (self.refs.fetch_add(1, Ordering::Relaxed) + 2).to_string()
    }

    /// Marca el cierre y despierta a `done()` sin esperar al socket. Devuelve
    /// `true` si ya estaba cerrado.
    fn mark_closed(&self) -> bool {
        if self.closed.swap(true, Ordering::SeqCst) {
            return true;
        }
        self.done.send_replace(true);
        false
    }

    /// El cierre del contrato, idempotente.
    async fn close(&self) -> std::result::Result<(), DoorbellError> {
        if self.mark_closed() {
            return Ok(());
        }
        let mut sink = self.sink.lock().await;
        sink.send(Message::Close(Some(CloseFrame {
            code: CloseCode::Normal,
            reason: "adios".into(),
        })))
        .await
        .map_err(|error| DoorbellError::Write(error.to_string()))
    }
}

/// Espera el cierre sin retener el guardián del `watch` dentro de un
/// `select!`, que lo haría no `Send`.
async fn wait_closed(done: &mut watch::Receiver<bool>) {
    let _ = done.wait_for(|closed| *closed).await;
}

/// Cada 25 s manda el latido de Phoenix.
async fn heartbeat(shared: Arc<Shared>, every: Duration) {
    let mut ticker = tokio::time::interval_at(tokio::time::Instant::now() + every, every);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut done = shared.done.subscribe();
    loop {
        tokio::select! {
            _ = wait_closed(&mut done) => return,
            _ = ticker.tick() => {
                let message = json!({
                    "topic": "phoenix",
                    "event": "heartbeat",
                    "payload": {},
                    "ref": shared.next_ref(),
                });
                if shared.write(&message).await.is_err() {
                    // El socket ya no acepta escrituras: no esperar al cierre
                    // normal, que también escribe. Se despierta `done()` para
                    // que el `Watch` reconecte.
                    shared.mark_closed();
                    return;
                }
            }
        }
    }
}

/// El bucle de lectura. Al salir cierra, como el
/// `defer d.Close` del contrato.
async fn read_loop(
    shared: Arc<Shared>,
    mut stream: SocketStream,
    mut on_event: Option<OnEvent>,
    read_timeout: Duration,
) {
    let mut done = shared.done.subscribe();
    loop {
        tokio::select! {
            _ = wait_closed(&mut done) => break,
            message = tokio::time::timeout(read_timeout, stream.next()) => {
                let Ok(message) = message else {
                    // Ni un frame ni respuesta al latido: el socket quedó a
                    // medias. Se marca cerrado para que `done()` despierte.
                    shared.mark_closed();
                    break;
                };
                let Some(message) = message else { break };
                let Ok(message) = message else { break };
                let data = match &message {
                    Message::Text(text) => text.as_str().as_bytes(),
                    Message::Binary(data) => &data[..],
                    _ => continue,
                };
                let Ok(value) = serde_json::from_slice::<Value>(data) else {
                    continue;
                };
                let event = value.get("event").and_then(Value::as_str).unwrap_or_default();
                match event {
                    "broadcast" => {
                        // El contrato distingue el payload ausente (nil, no parsea) del
                        // `null` explícito (cero valores y sí avisa)
                        let Some(payload) = value.get("payload") else {
                            continue;
                        };
                        let broadcast = match payload {
                            Value::Null => BroadcastFrame::default(),
                            payload => match serde_json::from_value::<BroadcastFrame>(payload.clone()) {
                                Ok(broadcast) => broadcast,
                                Err(_) => continue,
                            },
                        };
                        if let Some(on_event) = on_event.as_mut() {
                            on_event(Event {
                                kind: kind_of(&broadcast.event, &broadcast.payload),
                                name: broadcast.event,
                                raw: broadcast.payload,
                            });
                        }
                    }
                    "phx_error" | "phx_close" => break,
                    _ => {}
                }
            }
        }
    }
    let _ = shared.close().await;
}

/// Prefiere `payload.kind` y cae al nombre del evento.
fn kind_of(name: &str, payload: &Value) -> String {
    payload
        .get("kind")
        .and_then(Value::as_str)
        .filter(|kind| !kind.is_empty())
        .unwrap_or(name)
        .to_owned()
}

/// El `broadcast` de Phoenix.
#[derive(Default, Deserialize)]
struct BroadcastFrame {
    #[serde(default)]
    event: String,
    #[serde(default)]
    payload: Value,
}

/// Manda el join y espera el `phx_reply` con `ref` "1".
///
/// Los frames que no son JSON o no son esa respuesta se ignoran
async fn join(
    conn: &mut Socket,
    topic: &str,
    token: &str,
    private: bool,
) -> std::result::Result<(), DoorbellError> {
    let message = join_message(topic, token, private)?;
    let joined = async {
        conn.send(Message::Text(message.into()))
            .await
            .map_err(|error| DoorbellError::Write(error.to_string()))?;
        loop {
            let Some(message) = conn.next().await else {
                // El contrato devuelve el `io.EOF` del `Read`.
                return Err(DoorbellError::NoReply("EOF".to_owned()));
            };
            let message = message.map_err(|error| DoorbellError::NoReply(error.to_string()))?;
            let data = match &message {
                Message::Text(text) => text.as_str().as_bytes(),
                Message::Binary(data) => &data[..],
                _ => continue,
            };
            let Ok(frame) = serde_json::from_slice::<ReplyFrame>(data) else {
                continue;
            };
            if frame.event != "phx_reply" || frame.ref_id != JOIN_REF {
                continue;
            }
            if frame.payload.status != "ok" {
                return Err(DoorbellError::JoinRejected(reply_text(
                    &frame.payload.response,
                )));
            }
            return Ok(());
        }
    };

    // El plazo cubre la escritura y la lectura, como el contexto del contrato.
    tokio::time::timeout(JOIN_TIMEOUT, joined)
        .await
        .map_err(|_| DoorbellError::NoReply("context deadline exceeded".to_owned()))?
}

/// Serializa el mensaje de `phx_join`.
fn join_message(
    topic: &str,
    token: &str,
    private: bool,
) -> std::result::Result<String, DoorbellError> {
    let mut payload = json!({
        "config": {
            "broadcast": { "self": false },
            "presence": { "key": "" },
            "private": private,
        }
    });
    // Un token vacío sería un JWT malformado para el servidor; se omite.
    if !token.is_empty()
        && let Value::Object(map) = &mut payload
    {
        map.insert("access_token".to_owned(), Value::String(token.to_owned()));
    }

    let message = json!({
        "topic": topic,
        "event": "phx_join",
        "payload": payload,
        "ref": JOIN_REF,
        "join_ref": JOIN_REF,
    });
    serde_json::to_string(&message).map_err(|error| DoorbellError::Write(error.to_string()))
}

/// La parte del `phx_reply` que lee el join.
#[derive(Deserialize)]
struct ReplyFrame {
    #[serde(default)]
    event: String,
    #[serde(default, rename = "ref")]
    ref_id: String,
    #[serde(default)]
    payload: ReplyPayload,
}

#[derive(Default, Deserialize)]
struct ReplyPayload {
    #[serde(default)]
    status: String,
    /// Ausente y `null` no son lo mismo.
    #[serde(default)]
    response: Option<Value>,
}

/// El `response` como lo imprime el contrato con `strings.TrimSpace`.
fn reply_text(response: &Option<Value>) -> String {
    match response {
        None => String::new(),
        Some(Value::Null) => "null".to_owned(),
        Some(other) => other.to_string().trim().to_owned(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::time::Duration;

    use futures_util::{SinkExt, StreamExt};
    use serde::Deserialize;
    use serde_json::json;
    use tokio::net::TcpListener;
    use tokio::sync::{mpsc, oneshot};
    use tokio_tungstenite::accept_async;
    use tokio_tungstenite::tungstenite::Message;

    use super::*;

    const TIMEOUT: Duration = Duration::from_secs(3);

    /// Los campos del join que el servidor de prueba registra.
    #[derive(Debug, Deserialize)]
    struct JoinFrame {
        topic: String,
        event: String,
        #[serde(rename = "ref")]
        ref_id: String,
        join_ref: String,
        payload: JoinPayload,
    }

    #[derive(Debug, Deserialize)]
    struct JoinPayload {
        config: JoinConfig,
        #[serde(default)]
        access_token: Option<String>,
    }

    #[derive(Debug, Deserialize)]
    struct JoinConfig {
        broadcast: BroadcastConfig,
        presence: PresenceConfig,
        private: bool,
    }

    #[derive(Debug, Deserialize)]
    struct BroadcastConfig {
        #[serde(rename = "self")]
        itself: bool,
    }

    #[derive(Debug, Deserialize)]
    struct PresenceConfig {
        key: String,
    }

    /// Habla lo mínimo del protocolo Phoenix para probar el join.
    async fn fake_supabase(join_status: &'static str) -> (String, oneshot::Receiver<JoinFrame>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let (joined, received) = oneshot::channel();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            let Some(Ok(message)) = socket.next().await else {
                return;
            };
            let join: JoinFrame = serde_json::from_str(message.to_text().unwrap()).unwrap();
            let reply = json!({
                "topic": join.topic.as_str(),
                "ref": join.ref_id.as_str(),
                "event": "phx_reply",
                "payload": { "status": join_status, "response": {} },
            });
            let _ = joined.send(join);
            socket
                .send(Message::Text(reply.to_string().into()))
                .await
                .unwrap();
            // Espera el cierre del cliente para no dejar el socket a medias.
            let _ = socket.next().await;
        });
        (format!("http://{address}"), received)
    }

    /// Un servidor que contesta el join, manda los frames pedidos y registra lo
    /// que el cliente escribe después.
    struct Session {
        url: String,
        frames: mpsc::UnboundedReceiver<Value>,
        closes: oneshot::Receiver<usize>,
    }

    async fn fake_supabase_session(after_join: Vec<Value>) -> Session {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let (frames_tx, frames) = mpsc::unbounded_channel();
        let (closes_tx, closes) = oneshot::channel();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            let Some(Ok(message)) = socket.next().await else {
                return;
            };
            let join: JoinFrame = serde_json::from_str(message.to_text().unwrap()).unwrap();
            let reply = json!({
                "topic": join.topic.as_str(),
                "ref": join.ref_id.as_str(),
                "event": "phx_reply",
                "payload": { "status": "ok", "response": {} },
            });
            socket
                .send(Message::Text(reply.to_string().into()))
                .await
                .unwrap();
            for frame in after_join {
                socket
                    .send(Message::Text(frame.to_string().into()))
                    .await
                    .unwrap();
            }
            let mut closes = 0;
            while let Some(Ok(message)) = socket.next().await {
                match message {
                    Message::Text(text) => {
                        if let Ok(frame) = serde_json::from_str::<Value>(&text) {
                            let _ = frames_tx.send(frame);
                        }
                    }
                    Message::Close(_) => {
                        closes += 1;
                        break;
                    }
                    _ => {}
                }
            }
            let _ = closes_tx.send(closes);
        });
        Session {
            url: format!("http://{address}"),
            frames,
            closes,
        }
    }

    fn config(url: &str) -> Config {
        Config {
            topic: "doorbell:abc".to_owned(),
            supabase_url: url.to_owned(),
            anon_key: "llave".to_owned(),
            ..Config::default()
        }
    }

    #[tokio::test]
    async fn dial_joins_given_topic() {
        let (url, joined) = fake_supabase("ok").await;
        let _doorbell = dial(config(&url), None).await.unwrap();

        let join = tokio::time::timeout(TIMEOUT, joined)
            .await
            .expect("el servidor nunca recibió el join")
            .unwrap();
        assert_eq!(join.topic, "realtime:doorbell:abc");
        assert_eq!(join.event, "phx_join");
        assert_eq!(join.ref_id, JOIN_REF);
        assert_eq!(join.join_ref, JOIN_REF);
        assert!(!join.payload.config.private);
        assert!(!join.payload.config.broadcast.itself);
        assert_eq!(join.payload.config.presence.key, "");
        assert_eq!(join.payload.access_token, None);
    }

    #[tokio::test]
    async fn dial_joins_private_channel_as_private() {
        let (url, joined) = fake_supabase("ok").await;
        let mut cfg = config(&url);
        cfg.private = true;
        let _doorbell = dial(cfg, None).await.unwrap();

        let join = tokio::time::timeout(TIMEOUT, joined)
            .await
            .expect("el servidor nunca recibió el join")
            .unwrap();
        assert!(join.payload.config.private);
        assert_eq!(join.topic, "realtime:doorbell:abc");
    }

    #[tokio::test]
    async fn dial_sends_access_token_when_set() {
        let (url, joined) = fake_supabase("ok").await;
        let mut cfg = config(&url);
        cfg.access_token = "jwt".to_owned();
        let _doorbell = dial(cfg, None).await.unwrap();

        let join = tokio::time::timeout(TIMEOUT, joined)
            .await
            .expect("el servidor nunca recibió el join")
            .unwrap();
        assert_eq!(join.payload.access_token.as_deref(), Some("jwt"));
    }

    #[tokio::test]
    async fn dial_rejects_closed_channel() {
        let (url, _joined) = fake_supabase("error").await;
        let error = match dial(config(&url), None).await {
            Ok(_) => panic!("un canal rechazado no puede pasar por bueno"),
            Err(error) => error,
        };
        assert!(matches!(error, DoorbellError::JoinRejected(_)));
        assert_eq!(
            error.to_string(),
            "el servidor no abrió el canal: {}",
            "el response del servidor viaja recortado"
        );
    }

    #[tokio::test]
    async fn dial_rejects_incomplete_config() {
        let error = match dial(
            Config {
                topic: "x".to_owned(),
                ..Config::default()
            },
            None,
        )
        .await
        {
            Ok(_) => panic!("sin servidor no se puede abrir"),
            Err(error) => error,
        };
        assert_eq!(error.to_string(), "el timbre llegó incompleto");

        let error = match dial(
            Config {
                topic: "x".to_owned(),
                supabase_url: "http://127.0.0.1:1".to_owned(),
                ..Config::default()
            },
            None,
        )
        .await
        {
            Ok(_) => panic!("sin llave no se puede abrir"),
            Err(error) => error,
        };
        assert_eq!(error.to_string(), "el timbre llegó sin llave");
    }

    #[tokio::test]
    async fn ping_arrives_with_kind() {
        let Session { url, .. } = fake_supabase_session(vec![json!({
            "event": "broadcast",
            "payload": {
                "type": "broadcast",
                "event": "ping",
                "payload": { "kind": "chat_mention" },
            },
        })])
        .await;
        let (events_tx, mut events_rx) = mpsc::unbounded_channel();
        let doorbell = dial(
            config(&url),
            Some(Box::new(move |event| {
                let _ = events_tx.send(event);
            })),
        )
        .await
        .unwrap();

        let event = tokio::time::timeout(TIMEOUT, events_rx.recv())
            .await
            .expect("el ping nunca llegó")
            .unwrap();
        assert_eq!(event.kind, "chat_mention");
        assert_eq!(event.name, "ping");
        assert_eq!(event.raw, json!({ "kind": "chat_mention" }));

        doorbell.close().await.unwrap();
    }

    #[tokio::test]
    async fn phx_close_closes_done() {
        let Session { url, .. } =
            fake_supabase_session(vec![json!({ "event": "phx_close" })]).await;
        let doorbell = dial(config(&url), None).await.unwrap();

        tokio::time::timeout(TIMEOUT, doorbell.done())
            .await
            .expect("el phx_close no cerró Done");
    }

    #[tokio::test]
    async fn close_is_idempotent() {
        let Session { url, closes, .. } = fake_supabase_session(Vec::new()).await;
        let doorbell = dial(config(&url), None).await.unwrap();

        doorbell.close().await.unwrap();
        doorbell.close().await.unwrap();

        let closes = tokio::time::timeout(TIMEOUT, closes)
            .await
            .expect("el servidor no vio el cierre")
            .unwrap();
        assert_eq!(closes, 1);
    }

    async fn next_heartbeat(frames: &mut mpsc::UnboundedReceiver<Value>) -> Value {
        loop {
            let frame = frames.recv().await.unwrap();
            if frame["event"] == "heartbeat" {
                return frame;
            }
        }
    }

    #[tokio::test]
    async fn heartbeat_uses_incrementing_ref() {
        let mut session = fake_supabase_session(Vec::new()).await;
        let doorbell = dial_with_heartbeat(
            config(&session.url),
            None,
            Duration::from_millis(30),
            Duration::from_secs(5),
            JOIN_TIMEOUT,
        )
        .await
        .unwrap();

        let first = tokio::time::timeout(TIMEOUT, next_heartbeat(&mut session.frames))
            .await
            .expect("el primer latido nunca llegó");
        let second = tokio::time::timeout(TIMEOUT, next_heartbeat(&mut session.frames))
            .await
            .expect("el segundo latido nunca llegó");

        assert_eq!(first["topic"], "phoenix");
        assert_eq!(first["event"], "heartbeat");
        assert_eq!(first["payload"], json!({}));
        assert_eq!(first["ref"], "2");
        assert_eq!(second["ref"], "3");

        doorbell.close().await.unwrap();
    }

    // El fallo que esto cubre: un socket que acepta el join y después no manda
    // nada dejaba el `Watch` esperando para siempre y la app decía "Conectado".
    #[tokio::test]
    async fn un_timbre_mudo_se_cierra_por_plazo() {
        let session = fake_supabase_session(Vec::new()).await;
        let doorbell = dial_with_heartbeat(
            config(&session.url),
            None,
            Duration::from_millis(30),
            Duration::from_millis(100),
            JOIN_TIMEOUT,
        )
        .await
        .unwrap();

        tokio::time::timeout(TIMEOUT, doorbell.done())
            .await
            .expect("el timbre mudo no se cerró");
    }

    /// El fallo que esto cubre: `connect_async` (TCP + TLS + upgrade HTTP) no
    /// tenía plazo; un servidor que acepta y calla antes del upgrade dejaba
    /// `dial` colgado para siempre y la app en «conectando».
    #[tokio::test]
    async fn handshake_con_servidor_mudo_falla_en_plazo() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            // Acepta y calla: nunca responde el upgrade HTTP.
            let (_stream, _) = listener.accept().await.unwrap();
            tokio::time::sleep(Duration::from_secs(30)).await;
        });
        let url = format!("http://{address}");

        // El plazo de producción es JOIN_TIMEOUT (15 s); el test lo acorta
        // por la misma costura que el latido y la lectura.
        let resultado = tokio::time::timeout(
            TIMEOUT,
            dial_with_heartbeat(
                config(&url),
                None,
                HEARTBEAT_EVERY,
                READ_TIMEOUT,
                Duration::from_millis(100),
            ),
        )
        .await
        .expect("el handshake no tiene plazo");
        let error = match resultado {
            Ok(_) => panic!("un servidor mudo no puede abrir el timbre"),
            Err(error) => error,
        };
        assert!(matches!(error, DoorbellError::Connect(_)));
        assert_eq!(
            error.to_string(),
            "no pudimos abrir el timbre: context deadline exceeded"
        );
    }

    #[test]
    fn url_uses_wss_and_apikey() {
        assert_eq!(
            websocket_url("https://abc.supabase.co/", "llave"),
            "wss://abc.supabase.co/realtime/v1/websocket?apikey=llave&vsn=1.0.0"
        );
        assert_eq!(
            websocket_url("http://127.0.0.1:54321", "llave"),
            "ws://127.0.0.1:54321/realtime/v1/websocket?apikey=llave&vsn=1.0.0"
        );
    }

    #[test]
    fn reply_text_distinguishes_absent_from_null() {
        assert_eq!(reply_text(&None), "");
        assert_eq!(reply_text(&Some(Value::Null)), "null");
        assert_eq!(
            reply_text(&Some(json!({"reason": "x"}))),
            r#"{"reason":"x"}"#
        );
    }
}
