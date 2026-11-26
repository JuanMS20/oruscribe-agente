//! Tests de contrato de los mensajes de la organización.
//!
//! El fake implementa [`ToolCaller`] en vez de levantar un servidor MCP: lo que
//! se prueba es el nombre de la herramienta y los argumentos que arma el port.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use orus_org::mensajes::{self, ToolCaller};
use orus_org::{JsonObject, OrgError, Result};
use serde_json::{Value, json};

struct Fake {
    answers: Arc<Mutex<VecDeque<String>>>,
    calls: Arc<Mutex<Vec<(String, JsonObject)>>>,
}

impl Fake {
    fn new(answers: &[&str]) -> Self {
        Self {
            answers: Arc::new(Mutex::new(
                answers.iter().map(|answer| (*answer).to_owned()).collect(),
            )),
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn calls(&self) -> Vec<(String, JsonObject)> {
        self.calls.lock().expect("mutex de llamadas").clone()
    }
}

impl ToolCaller for Fake {
    async fn call(&self, name: &str, args: JsonObject) -> Result<String> {
        let name = name.to_owned();
        self.calls
            .lock()
            .expect("mutex de llamadas")
            .push((name.clone(), args));
        self.answers
            .lock()
            .expect("mutex de respuestas")
            .pop_front()
            .ok_or_else(|| OrgError::Tool {
                tool: name,
                message: "sin respuesta guionada".to_owned(),
            })
    }
}

fn assert_call(calls: &[(String, JsonObject)], index: usize, tool: &str, expected: Value) {
    let (name, args) = &calls[index];
    assert_eq!(name, tool, "herramienta de la llamada {index}");
    assert_eq!(
        Value::Object(args.clone()),
        expected,
        "argumentos de la llamada {index}"
    );
}

#[tokio::test]
async fn me_pide_get_me_sin_argumentos() {
    let fake = Fake::new(&[
        r#"{"user_id": "u1", "name": "Ana", "organization": {"slug": "acme"}, "capabilities": {"read_tasks": "all"}}"#,
    ]);

    let me = mensajes::me(&fake).await.unwrap();

    assert_eq!(me.user_id, "u1");
    assert_eq!(me.organization.slug, "acme");
    let calls = fake.calls();
    assert_eq!(calls.len(), 1);
    assert_call(&calls, 0, "get_me", json!({}));
}

#[tokio::test]
async fn tasks_page_manda_el_offset_al_servidor() {
    let fake = Fake::new(&[r#"{"tasks": [{"id": "t1"}], "has_more": true, "next_offset": 50}"#]);

    let page = mensajes::tasks_page(&fake, 50).await.unwrap();

    assert!(page.has_more);
    assert_eq!(page.next_offset, 50);
    let calls = fake.calls();
    assert_eq!(calls.len(), 1);
    assert_call(&calls, 0, "get_my_tasks", json!({"offset": 50}));
}

#[tokio::test]
async fn tasks_pide_get_my_tasks_sin_argumentos() {
    let fake = Fake::new(&[r#"{"tasks": [{"id": "t1"}, {"id": "t2"}]}"#]);

    let tasks = mensajes::tasks(&fake).await.unwrap();

    assert_eq!(
        tasks
            .items
            .iter()
            .map(|task| task.id.as_str())
            .collect::<Vec<_>>(),
        ["t1", "t2"]
    );
    let calls = fake.calls();
    assert_eq!(calls.len(), 1);
    assert_call(&calls, 0, "get_my_tasks", json!({}));
}

#[tokio::test]
async fn rooms_pide_list_my_chat_rooms_con_limit_50() {
    let fake = Fake::new(&[r#"{"rooms": [{"room_id": "r1"}]}"#]);

    let rooms = mensajes::rooms(&fake).await.unwrap();

    assert_eq!(rooms.items.len(), 1);
    assert_eq!(rooms.items[0].room_id, "r1");
    let calls = fake.calls();
    assert_eq!(calls.len(), 1);
    assert_call(&calls, 0, "list_my_chat_rooms", json!({"limit": 50}));
}

#[tokio::test]
async fn messages_pide_la_pagina_que_sigue() {
    // El servidor manda la más nueva primero; cada página está invertida y la
    // última no trae next_offset.
    let fake = Fake::new(&[
        r#"{"messages": [{"id": "m5", "created_at": "2026-11-05T12:04:00Z"}, {"id": "m4", "created_at": "2026-11-05T12:03:00Z"}], "has_more": true, "next_offset": 2}"#,
        r#"{"messages": [{"id": "m3", "created_at": "2026-11-05T12:02:00Z"}, {"id": "m2", "created_at": "2026-11-05T12:01:00Z"}], "has_more": true, "next_offset": 4}"#,
        r#"{"messages": [{"id": "m1", "created_at": "2026-11-05T12:00:00Z"}], "has_more": false}"#,
    ]);

    let primera = mensajes::messages(&fake, "r1", 2, 0).await.unwrap();
    assert_eq!(
        primera
            .messages
            .iter()
            .map(|message| message.id.as_str())
            .collect::<Vec<_>>(),
        ["m4", "m5"],
        "primera página en orden de lectura"
    );
    assert!(primera.has_more);
    assert_eq!(primera.next_offset, 2);

    let segunda = mensajes::messages(&fake, "r1", 2, primera.next_offset)
        .await
        .unwrap();
    assert_eq!(
        segunda
            .messages
            .iter()
            .map(|message| message.id.as_str())
            .collect::<Vec<_>>(),
        ["m2", "m3"]
    );
    assert!(segunda.has_more);
    assert_eq!(segunda.next_offset, 4);

    let ultima = mensajes::messages(&fake, "r1", 2, segunda.next_offset)
        .await
        .unwrap();
    assert_eq!(
        ultima
            .messages
            .iter()
            .map(|message| message.id.as_str())
            .collect::<Vec<_>>(),
        ["m1"]
    );
    assert!(!ultima.has_more);
    assert_eq!(ultima.next_offset, 0, "sin next_offset queda en cero");

    let calls = fake.calls();
    assert_eq!(calls.len(), 3);
    assert_call(
        &calls,
        0,
        "get_chat_messages",
        json!({"room_id": "r1", "limit": 2}),
    );
    assert_call(
        &calls,
        1,
        "get_chat_messages",
        json!({"room_id": "r1", "limit": 2, "offset": 2}),
    );
    assert_call(
        &calls,
        2,
        "get_chat_messages",
        json!({"room_id": "r1", "limit": 2, "offset": 4}),
    );
}

#[tokio::test]
async fn messages_limit_fuera_de_rango_usa_50() {
    let fake = Fake::new(&[
        r#"{"messages": [], "has_more": false}"#,
        r#"{"messages": [], "has_more": false}"#,
    ]);

    let cero = mensajes::messages(&fake, "r1", 0, 0).await.unwrap();
    let cincuenta_y_uno = mensajes::messages(&fake, "r1", 51, 0).await.unwrap();

    assert!(cero.messages.is_empty());
    assert!(cincuenta_y_uno.messages.is_empty());
    let calls = fake.calls();
    assert_eq!(calls.len(), 2);
    assert_call(
        &calls,
        0,
        "get_chat_messages",
        json!({"room_id": "r1", "limit": 50}),
    );
    assert_call(
        &calls,
        1,
        "get_chat_messages",
        json!({"room_id": "r1", "limit": 50}),
    );
}

#[tokio::test]
async fn send_sin_reply_no_manda_reply_to_id() {
    let fake = Fake::new(&["ok"]);

    mensajes::send(&fake, "r1", "hola", "").await.unwrap();

    let calls = fake.calls();
    assert_eq!(calls.len(), 1);
    assert_call(
        &calls,
        0,
        "send_chat_message",
        json!({"room_id": "r1", "body": "hola"}),
    );
}

#[tokio::test]
async fn send_con_reply_manda_reply_to_id() {
    let fake = Fake::new(&["ok"]);

    mensajes::send(&fake, "r1", "hola", "m9").await.unwrap();

    let calls = fake.calls();
    assert_eq!(calls.len(), 1);
    assert_call(
        &calls,
        0,
        "send_chat_message",
        json!({"room_id": "r1", "body": "hola", "reply_to_id": "m9"}),
    );
}

#[tokio::test]
async fn mark_room_read_manda_room_id() {
    let fake = Fake::new(&["ok"]);

    mensajes::mark_room_read(&fake, "r1").await.unwrap();

    let calls = fake.calls();
    assert_eq!(calls.len(), 1);
    assert_call(&calls, 0, "mark_chat_read", json!({"room_id": "r1"}));
}

#[tokio::test]
async fn mark_mentions_read_vacio_no_llama() {
    let fake = Fake::new(&[]);

    mensajes::mark_mentions_read(&fake, &[]).await.unwrap();

    assert!(fake.calls().is_empty(), "sin ids no se llama al servidor");
}

#[tokio::test]
async fn mark_mentions_read_con_ids_manda_message_ids() {
    let fake = Fake::new(&["ok"]);
    let ids = ["m1".to_owned(), "m2".to_owned()];

    mensajes::mark_mentions_read(&fake, &ids).await.unwrap();

    let calls = fake.calls();
    assert_eq!(calls.len(), 1);
    assert_call(
        &calls,
        0,
        "mark_chat_read",
        json!({"message_ids": ["m1", "m2"]}),
    );
}

#[tokio::test]
async fn send_direct_sin_reply_manda_to_user_id() {
    let fake = Fake::new(&["ok"]);

    mensajes::send_direct(&fake, "u2", "hola", "")
        .await
        .unwrap();

    let calls = fake.calls();
    assert_eq!(calls.len(), 1);
    assert_call(
        &calls,
        0,
        "send_chat_message",
        json!({"to_user_id": "u2", "body": "hola"}),
    );
}

#[tokio::test]
async fn send_direct_con_reply_manda_reply_to_id() {
    let fake = Fake::new(&["ok"]);

    mensajes::send_direct(&fake, "u2", "hola", "m9")
        .await
        .unwrap();

    let calls = fake.calls();
    assert_eq!(calls.len(), 1);
    assert_call(
        &calls,
        0,
        "send_chat_message",
        json!({"to_user_id": "u2", "body": "hola", "reply_to_id": "m9"}),
    );
}

#[tokio::test]
async fn contadores_null_son_cero() {
    let fake = Fake::new(&[r#"{
        "messages": [{
            "id": "m1",
            "created_at": "2026-11-05T12:00:00Z",
            "reactions": [{"emoji": "👍", "count": null, "mine": false}],
            "attachments": [{"id": "a1", "file_name": "f.pdf", "size_bytes": null, "download_url": "u"}]
        }],
        "has_more": true,
        "next_offset": null
    }"#]);

    let pagina = mensajes::messages(&fake, "r1", 50, 0).await.unwrap();

    assert_eq!(pagina.next_offset, 0, "next_offset null es cero");
    let mensaje = &pagina.messages[0];
    assert_eq!(mensaje.reactions.as_ref().unwrap()[0].count, 0);
    assert_eq!(mensaje.attachments.as_ref().unwrap()[0].size_bytes, 0);
}

#[tokio::test]
async fn tasks_conserva_la_paginacion_del_servidor() {
    let fake = Fake::new(&[r#"{
        "tasks": [{"id": "t1"}],
        "count": 1, "total": 30, "has_more": true, "truncated": false, "next_offset": 30
    }"#]);

    let page = mensajes::tasks(&fake).await.unwrap();
    let valor = serde_json::to_value(&page).unwrap();

    assert_eq!(valor["has_more"], json!(true), "faltan tareas asignadas");
    assert_eq!(valor["next_offset"], json!(30));
}

#[tokio::test]
async fn rooms_conserva_la_paginacion_del_servidor() {
    let fake = Fake::new(&[r#"{
        "rooms": [{"room_id": "r1"}],
        "count": 1, "total": 80, "has_more": true, "truncated": true, "next_offset": 50
    }"#]);

    let page = mensajes::rooms(&fake).await.unwrap();
    let valor = serde_json::to_value(&page).unwrap();

    assert_eq!(valor["has_more"], json!(true), "faltan salas");
    assert_eq!(valor["truncated"], json!(true));
    assert_eq!(valor["next_offset"], json!(50));
}

#[tokio::test]
async fn tareas_y_salas_aceptan_contadores_nulos() {
    let fake = Fake::new(&[
        r#"{"tasks": [], "count": null, "total": null, "next_offset": null}"#,
        r#"{"rooms": []}"#,
    ]);

    let tareas = mensajes::tasks(&fake).await.unwrap();
    let salas = mensajes::rooms(&fake).await.unwrap();

    assert_eq!(tareas.count, 0);
    assert_eq!(tareas.total, 0);
    assert_eq!(tareas.next_offset, 0);
    assert!(!salas.has_more);
    assert_eq!(salas.total, 0);
}
