//! Mensajes de la organización: leer salas y mensajes, enviar y marcar leído.
//!
//! Las funciones públicas
//! reciben un [`ToolCaller`]; [`Client`] lo implementa sobre MCP y los tests lo
//! implementan con respuestas guionadas.

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::client::{Client, JsonObject, decode_json};
use crate::error::Result;
use crate::types::{Me, Message, Pagina, Paginacion, Room, Task, nulo_como_cero};

/// Una página de mensajes de una sala, del más viejo al más nuevo, más lo que
/// necesita la petición siguiente. `next_offset` se pasa tal cual como offset.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MessagesPage {
    pub messages: Vec<Message>,
    pub has_more: bool,
    pub next_offset: i64,
}

/// Lo mínimo que necesitan los métodos de mensajes para hablar con el servidor.
///
/// `Client` lo implementa delegando en MCP. Es una costura para probar el
/// nombre de la herramienta y los argumentos sin levantar un servidor. El trait
/// no promete `Send` en el future; los métodos concretos de `Client` sí lo son.
#[allow(async_fn_in_trait)]
pub trait ToolCaller: Sync {
    /// Ejecuta una herramienta y devuelve su texto.
    async fn call(&self, name: &str, args: JsonObject) -> Result<String>;

    /// Ejecuta una herramienta y decodifica su texto como JSON.
    async fn call_json<T: DeserializeOwned>(&self, name: &str, args: JsonObject) -> Result<T> {
        let text = self.call(name, args).await?;
        decode_json(name, &text)
    }
}

impl ToolCaller for Client {
    async fn call(&self, name: &str, args: JsonObject) -> Result<String> {
        Client::call(self, name, args).await
    }
}

#[derive(Debug, Deserialize)]
struct TasksResponse {
    #[serde(default)]
    tasks: Vec<Task>,
    #[serde(flatten)]
    pagina: Paginacion,
}

#[derive(Debug, Deserialize)]
struct RoomsResponse {
    #[serde(default)]
    rooms: Vec<Room>,
    #[serde(flatten)]
    pagina: Paginacion,
}

#[derive(Debug, Deserialize)]
struct MessagesResponse {
    #[serde(default)]
    messages: Vec<Message>,
    #[serde(default)]
    has_more: bool,
    #[serde(default, deserialize_with = "nulo_como_cero")]
    next_offset: i64,
}

/// La persona de la sesión, con lo que puede hacer.
pub async fn me<C: ToolCaller + ?Sized>(caller: &C) -> Result<Me> {
    caller.call_json("get_me", JsonObject::new()).await
}

/// Las tareas asignadas a la persona de la sesión.
pub async fn tasks<C: ToolCaller + ?Sized>(caller: &C) -> Result<Pagina<Task>> {
    tasks_page(caller, 0).await
}

pub async fn tasks_page<C: ToolCaller + ?Sized>(caller: &C, offset: i64) -> Result<Pagina<Task>> {
    let mut args = JsonObject::new();
    if offset > 0 {
        args.insert("offset".to_owned(), Value::from(offset));
    }
    let out: TasksResponse = caller.call_json("get_my_tasks", args).await?;
    Ok(out.pagina.con_items(out.tasks))
}

/// Las salas de chat de la persona, con un tope fijo de 50.
pub async fn rooms<C: ToolCaller + ?Sized>(caller: &C) -> Result<Pagina<Room>> {
    rooms_page(caller, 0).await
}

pub async fn rooms_page<C: ToolCaller + ?Sized>(caller: &C, offset: i64) -> Result<Pagina<Room>> {
    let mut args = JsonObject::new();
    args.insert("limit".to_owned(), Value::from(50));
    if offset > 0 {
        args.insert("offset".to_owned(), Value::from(offset));
    }
    let out: RoomsResponse = caller.call_json("list_my_chat_rooms", args).await?;
    Ok(out.pagina.con_items(out.rooms))
}

/// Una página de mensajes, del más viejo al más nuevo.
///
/// El servidor manda el más nuevo primero; el orden se invierte con un sort
/// estable.
pub async fn messages<C: ToolCaller + ?Sized>(
    caller: &C,
    room_id: &str,
    limit: i64,
    offset: i64,
) -> Result<MessagesPage> {
    let limit = if limit <= 0 || limit > 50 { 50 } else { limit };
    let mut args = JsonObject::new();
    args.insert("room_id".to_owned(), Value::from(room_id));
    args.insert("limit".to_owned(), Value::from(limit));
    if offset > 0 {
        args.insert("offset".to_owned(), Value::from(offset));
    }
    let out: MessagesResponse = caller.call_json("get_chat_messages", args).await?;
    let mut messages = out.messages;
    messages.sort_by_key(|message| message.created_at);
    Ok(MessagesPage {
        messages,
        has_more: out.has_more,
        next_offset: out.next_offset,
    })
}

/// Publica un mensaje en una sala. `reply_to` es opcional.
pub async fn send<C: ToolCaller + ?Sized>(
    caller: &C,
    room_id: &str,
    body: &str,
    reply_to: &str,
) -> Result<()> {
    let mut args = JsonObject::new();
    args.insert("room_id".to_owned(), Value::from(room_id));
    args.insert("body".to_owned(), Value::from(body));
    if !reply_to.is_empty() {
        args.insert("reply_to_id".to_owned(), Value::from(reply_to));
    }
    caller.call("send_chat_message", args).await.map(|_| ())
}

/// Marca leída una sala completa.
pub async fn mark_room_read<C: ToolCaller + ?Sized>(caller: &C, room_id: &str) -> Result<()> {
    let mut args = JsonObject::new();
    args.insert("room_id".to_owned(), Value::from(room_id));
    caller.call("mark_chat_read", args).await.map(|_| ())
}

/// Marca leídas menciones concretas. Una mención es un mensaje, así que el
/// servidor recibe ids de mensaje y no una sala. Sin ids no se llama.
pub async fn mark_mentions_read<C: ToolCaller + ?Sized>(caller: &C, ids: &[String]) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let mut args = JsonObject::new();
    args.insert("message_ids".to_owned(), json!(ids));
    caller.call("mark_chat_read", args).await.map(|_| ())
}

/// Abre (o reutiliza) el chat directo con un miembro y publica ahí.
pub async fn send_direct<C: ToolCaller + ?Sized>(
    caller: &C,
    to_user_id: &str,
    body: &str,
    reply_to: &str,
) -> Result<()> {
    let mut args = JsonObject::new();
    args.insert("to_user_id".to_owned(), Value::from(to_user_id));
    args.insert("body".to_owned(), Value::from(body));
    if !reply_to.is_empty() {
        args.insert("reply_to_id".to_owned(), Value::from(reply_to));
    }
    caller.call("send_chat_message", args).await.map(|_| ())
}

impl Client {
    /// La persona de la sesión.
    pub async fn me(&self) -> Result<Me> {
        me(self).await
    }

    /// Las tareas asignadas a la persona de la sesión.
    pub async fn tasks(&self) -> Result<Pagina<Task>> {
        tasks(self).await
    }

    pub async fn tasks_page(&self, offset: i64) -> Result<Pagina<Task>> {
        tasks_page(self, offset).await
    }

    /// Las salas de chat de la persona.
    pub async fn rooms(&self) -> Result<Pagina<Room>> {
        rooms(self).await
    }

    pub async fn rooms_page(&self, offset: i64) -> Result<Pagina<Room>> {
        rooms_page(self, offset).await
    }

    /// Una página de mensajes, del más viejo al más nuevo.
    pub async fn messages(&self, room_id: &str, limit: i64, offset: i64) -> Result<MessagesPage> {
        messages(self, room_id, limit, offset).await
    }

    /// Publica un mensaje en una sala.
    pub async fn send(&self, room_id: &str, body: &str, reply_to: &str) -> Result<()> {
        send(self, room_id, body, reply_to).await
    }

    /// Marca leída una sala completa.
    pub async fn mark_room_read(&self, room_id: &str) -> Result<()> {
        mark_room_read(self, room_id).await
    }

    /// Marca leídas menciones concretas.
    pub async fn mark_mentions_read(&self, ids: &[String]) -> Result<()> {
        mark_mentions_read(self, ids).await
    }

    /// Publica en el chat directo con un miembro.
    pub async fn send_direct(&self, to_user_id: &str, body: &str, reply_to: &str) -> Result<()> {
        send_direct(self, to_user_id, body, reply_to).await
    }
}
