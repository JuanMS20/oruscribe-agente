//! Tests del cliente MCP con un fake Streamable HTTP local.
//!
//! El fake responde `initialize`, `notifications/initialized`, `tools/list` y
//! `tools/call`; no toca la red real ni el llavero.

use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use orus_org::{Client, JsonObject, OrgError, ToolSpec};
use serde_json::{Value, json};
use tokio::net::TcpListener;

struct Fake {
    pages: Vec<Vec<Value>>,
    calls: Mutex<Vec<String>>,
    repetir: bool,
}

async fn mcp(State(fake): State<Arc<Fake>>, Json(request): Json<Value>) -> Response {
    let Some(id) = request.get("id").cloned() else {
        // notifications/initialized no espera respuesta.
        return StatusCode::ACCEPTED.into_response();
    };
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let result = match method {
        "initialize" => json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "fake", "version": "0.0.0"}
        }),
        "tools/list" => list_page(
            &fake,
            request.pointer("/params/cursor").and_then(Value::as_str),
        ),
        "tools/call" => {
            let name = request
                .pointer("/params/name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            fake.calls
                .lock()
                .expect("mutex de llamadas")
                .push(name.to_owned());
            json!({"content": [{"type": "text", "text": "{\"answer\": 42}"}]})
        }
        _ => json!({}),
    };
    Json(json!({"jsonrpc": "2.0", "id": id, "result": result})).into_response()
}

fn list_page(fake: &Fake, cursor: Option<&str>) -> Value {
    let index = cursor
        .and_then(|cursor| cursor.strip_prefix("pagina-"))
        .and_then(|number| number.parse::<usize>().ok())
        .unwrap_or(0);
    let tools = fake.pages.get(index).cloned().unwrap_or_default();
    let mut result = json!({"tools": tools});
    if fake.repetir {
        result["nextCursor"] = json!(cursor.unwrap_or("pagina-0"));
    } else if index + 1 < fake.pages.len() {
        result["nextCursor"] = json!(format!("pagina-{}", index + 1));
    }
    result
}

fn tool(name: &str, description: Option<&str>, schema: Value) -> Value {
    let mut value = json!({"name": name, "inputSchema": schema});
    if let Some(description) = description {
        value["description"] = json!(description);
    }
    value
}

async fn start(pages: Vec<Vec<Value>>) -> (String, Arc<Fake>) {
    start_con(pages, false).await
}

async fn start_con(pages: Vec<Vec<Value>>, repetir: bool) -> (String, Arc<Fake>) {
    let fake = Arc::new(Fake {
        pages,
        calls: Mutex::new(Vec::new()),
        repetir,
    });
    let app = Router::new()
        .route("/mcp", post(mcp))
        .with_state(Arc::clone(&fake));
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener local");
    let address = listener.local_addr().expect("dirección local");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("servidor fake");
    });
    (format!("http://{address}/mcp"), fake)
}

#[tokio::test]
async fn tools_on_closed_client_is_not_connected() {
    let (endpoint, _) = start(vec![vec![tool(
        "get_me",
        Some("La persona"),
        json!({"type": "object"}),
    )]])
    .await;
    let mut client = Client::open(&endpoint, "token", "acme")
        .await
        .expect("cliente abierto");
    client.close().await.expect("cierre");

    assert!(matches!(
        client.tool_specs().await,
        Err(OrgError::NotConnected)
    ));
}

#[tokio::test]
async fn tool_specs_maps_name_description_schema() {
    let schema = json!({
        "type": "object",
        "properties": {"id": {"type": "string"}},
        "required": ["id"]
    });
    let (endpoint, _) = start(vec![vec![
        tool("get_me", Some("La persona de la sesión"), schema.clone()),
        tool("ping", None, json!({"type": "object"})),
    ]])
    .await;
    let client = Client::open(&endpoint, "token", "acme")
        .await
        .expect("cliente abierto");

    let specs = client.tool_specs().await.expect("lista de herramientas");

    // El schema se compara como Value: serde_json no conserva el orden de las
    // claves, a diferencia del json.RawMessage del Go.
    assert_eq!(
        specs,
        vec![
            ToolSpec {
                name: "get_me".to_owned(),
                description: "La persona de la sesión".to_owned(),
                schema,
            },
            ToolSpec {
                name: "ping".to_owned(),
                description: String::new(),
                schema: json!({"type": "object"}),
            },
        ]
    );
}

#[tokio::test]
async fn tool_specs_follows_two_pages() {
    let (endpoint, _) = start(vec![
        vec![tool("get_me", None, json!({"type": "object"}))],
        vec![tool("create_task", None, json!({"type": "object"}))],
    ])
    .await;
    let client = Client::open(&endpoint, "token", "acme")
        .await
        .expect("cliente abierto");

    let names: Vec<String> = client
        .tool_specs()
        .await
        .expect("lista de herramientas")
        .into_iter()
        .map(|spec| spec.name)
        .collect();

    assert_eq!(names, ["get_me", "create_task"]);

    let (endpoint, _) = start_con(
        vec![vec![tool("get_me", None, json!({"type": "object"}))]],
        true,
    )
    .await;
    let client = Client::open(&endpoint, "token", "acme")
        .await
        .expect("cliente abierto");
    let error = client
        .tool_specs()
        .await
        .expect_err("un cursor repetido debe cortar");
    assert!(matches!(error, OrgError::LimitExceeded(_)), "{error}");
}

#[tokio::test]
async fn call_json_decodes_text() {
    let (endpoint, fake) = start(vec![vec![]]).await;
    let client = Client::open(&endpoint, "token", "acme")
        .await
        .expect("cliente abierto");

    let value: Value = client
        .call_json("get_me", JsonObject::new())
        .await
        .expect("JSON decodificado");

    assert_eq!(value, json!({"answer": 42}));
    assert_eq!(
        fake.calls.lock().expect("mutex de llamadas").as_slice(),
        ["get_me"]
    );
}
