//! Tests de contrato de subidas, cuota, alertas y estado de tareas.
//!
//! El fake implementa [`ToolCaller`] para probar nombres y argumentos, y un
//! servidor TCP loopback mínimo recibe la subida HTTP real. Nunca se toca la
//! red de verdad.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

use orus_org::subidas::{self};
use orus_org::{JsonObject, OrgError, Progress, Result, Status, ToolCaller};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Debug)]
enum Answer {
    Text(String),
    Error(String),
}

struct Fake {
    answers: Mutex<VecDeque<Answer>>,
    calls: Mutex<Vec<(String, JsonObject)>>,
}

impl Fake {
    fn with(answers: Vec<Answer>) -> Self {
        Self {
            answers: Mutex::new(answers.into()),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn texto(respuestas: &[&str]) -> Self {
        Self::with(
            respuestas
                .iter()
                .map(|respuesta| Answer::Text((*respuesta).to_owned()))
                .collect(),
        )
    }

    fn errores(textos: &[&str]) -> Self {
        Self::with(
            textos
                .iter()
                .map(|texto| Answer::Error((*texto).to_owned()))
                .collect(),
        )
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
        match self
            .answers
            .lock()
            .expect("mutex de respuestas")
            .pop_front()
        {
            Some(Answer::Text(texto)) => Ok(texto),
            Some(Answer::Error(message)) => Err(OrgError::Tool {
                tool: name,
                message,
            }),
            None => Err(OrgError::Tool {
                tool: name,
                message: "sin respuesta guionada".to_owned(),
            }),
        }
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

fn respuesta_http(status: u16, body: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

/// Levanta un servidor loopback que atiende una sola subida y devuelve la
/// cabecera de la petición y su cuerpo.
async fn servidor_de_subida(
    respuesta: Vec<u8>,
) -> (String, tokio::task::JoinHandle<(String, Vec<u8>)>) {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("no se pudo abrir el puerto de prueba");
    let addr = listener.local_addr().expect("sin dirección local");
    let url = format!("http://{addr}/up");
    let handle = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("sin conexión");
        let mut data = Vec::new();
        let mut buffer = [0_u8; 1024];
        let header_end = loop {
            let read = socket.read(&mut buffer).await.expect("lectura falló");
            assert!(read > 0, "el cliente cerró antes de mandar los headers");
            data.extend_from_slice(&buffer[..read]);
            if let Some(position) = data.windows(4).position(|window| window == b"\r\n\r\n") {
                break position;
            }
        };
        let head = String::from_utf8_lossy(&data[..header_end]).into_owned();
        let content_length = head
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                if !name.eq_ignore_ascii_case("content-length") {
                    return None;
                }
                value.trim().parse::<usize>().ok()
            })
            .unwrap_or(0);
        let body_start = header_end + 4;
        while data.len() < body_start + content_length {
            let read = socket.read(&mut buffer).await.expect("lectura falló");
            if read == 0 {
                break;
            }
            data.extend_from_slice(&buffer[..read]);
        }
        let body = data[body_start..body_start + content_length].to_vec();
        socket.write_all(&respuesta).await.expect("escritura falló");
        socket.flush().await.expect("flush falló");
        (head, body)
    });
    (url, handle)
}

#[tokio::test]
async fn las_subidas_traducen_la_cuota() {
    let casos = [
        ("storage quota exceeded", true),
        ("no space left for this organization", true),
        ("la organización se quedó sin espacio", true),
        ("invalid room_id", false),
    ];

    for (texto, cuota) in casos {
        let fake = Fake::errores(&[texto, texto]);

        let error_chat = subidas::upload_chat_file(&fake, "r1", "a.txt", "text/plain", b"hola", "")
            .await
            .expect_err("quería error en el chat");
        assert_eq!(
            matches!(&error_chat, OrgError::SinEspacio { .. }),
            cuota,
            "chat: cuota={cuota} para {texto:?}: {error_chat}"
        );
        if cuota {
            assert_eq!(
                error_chat.to_string(),
                format!(
                    "la organización se quedó sin espacio para archivos: upload_chat_file: {texto}"
                )
            );
        } else {
            assert!(matches!(&error_chat, OrgError::Tool { .. }));
        }

        let error_avance = subidas::upload_advance(&fake, "t1", "a.txt", "text/plain", b"hola", "")
            .await
            .expect_err("quería error en el avance");
        assert_eq!(
            matches!(&error_avance, OrgError::SinEspacio { .. }),
            cuota,
            "avance: cuota={cuota} para {texto:?}: {error_avance}"
        );

        let calls = fake.calls();
        assert_eq!(calls.len(), 2, "una llamada por subida, sin tocar el HTTP");
        assert_eq!(calls[0].0, "upload_chat_file");
        assert_eq!(calls[1].0, "upload_task_file");
    }
}

#[tokio::test]
async fn la_subida_rechaza_direcciones_sin_https() {
    let casos = [
        (
            "http://0.0.0.0:1/up",
            "upload_chat_file: la dirección de subida debe usar https; http solo se permite en localhost",
        ),
        (
            "file:///etc/passwd",
            "upload_chat_file: la dirección de subida no es válida",
        ),
        (
            "sin-direccion",
            "upload_chat_file: la dirección de subida no es válida",
        ),
    ];

    for (destino, esperado) in casos {
        let ticket = json!({
            "attachment_id": "a1",
            "upload_url": destino,
        })
        .to_string();
        let fake = Fake::texto(&[ticket.as_str()]);

        let error = subidas::upload_chat_file(&fake, "r1", "a.txt", "text/plain", b"hola", "")
            .await
            .expect_err("quería el rechazo de la dirección");

        assert_eq!(error.to_string(), esperado);
        assert!(error.to_string().contains("subida"));
        assert!(!error.to_string().contains("subir archivo"));
        assert_eq!(fake.calls().len(), 1, "no siguió con send_chat_message");
    }
}

#[tokio::test]
async fn la_subida_acepta_loopback() {
    let (url, servidor) = servidor_de_subida(respuesta_http(200, "")).await;
    let ticket = json!({
        "attachment_id": "a9",
        "upload_url": url,
        "method": "PUT",
    })
    .to_string();
    let fake = Fake::texto(&[ticket.as_str(), "ok"]);

    subidas::upload_chat_file(&fake, "r1", "a.txt", "text/plain", b"hola", "")
        .await
        .unwrap();

    let (head, body) = servidor.await.expect("el servidor no terminó");
    assert_eq!(body.as_slice(), b"hola");
    assert!(head.starts_with("PUT /up "), "método: {head}");
    assert!(
        head.to_ascii_lowercase()
            .contains("content-type: text/plain"),
        "headers: {head}"
    );

    let calls = fake.calls();
    assert_eq!(calls.len(), 2);
    assert_call(
        &calls,
        0,
        "upload_chat_file",
        json!({
            "room_id": "r1",
            "file_name": "a.txt",
            "size_bytes": 4,
            "mime_type": "text/plain",
        }),
    );
    assert_call(
        &calls,
        1,
        "send_chat_message",
        json!({"room_id": "r1", "attachment_ids": ["a9"]}),
    );
}

#[tokio::test]
async fn la_subida_usa_el_content_type_del_boleto_y_put_por_defecto() {
    let (url, servidor) = servidor_de_subida(respuesta_http(200, "")).await;
    let ticket = json!({
        "attachment_id": "a1",
        "upload_url": url,
        "headers": {"Content-Type": "application/pdf"},
    })
    .to_string();
    let fake = Fake::texto(&[ticket.as_str(), "ok"]);

    subidas::upload_chat_file(&fake, "r1", "a.pdf", "text/plain", b"pdf", "")
        .await
        .unwrap();

    let (head, body) = servidor.await.expect("el servidor no terminó");
    assert_eq!(body.as_slice(), b"pdf");
    assert!(head.starts_with("PUT /up "), "método por defecto: {head}");
    assert!(
        head.to_ascii_lowercase()
            .contains("content-type: application/pdf"),
        "el boleto manda: {head}"
    );
}

#[tokio::test]
async fn la_subida_reporta_el_estado_http_y_el_cuerpo() {
    let (url, servidor) = servidor_de_subida(respuesta_http(500, "boom")).await;
    let ticket = json!({"attachment_id": "a1", "upload_url": url}).to_string();
    let fake = Fake::texto(&[ticket.as_str()]);

    let error = subidas::upload_chat_file(&fake, "r1", "a.txt", "text/plain", b"hola", "")
        .await
        .expect_err("quería el estado 500");
    servidor.await.expect("el servidor no terminó");

    assert_eq!(error.to_string(), "subir archivo: 500 boom");
    assert!(matches!(&error, OrgError::UploadStatus { status: 500, .. }));
    assert_eq!(fake.calls().len(), 1, "no siguió con send_chat_message");
}

#[tokio::test]
async fn el_error_de_subida_no_expone_la_url_firmada() {
    let puerto = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("puerto libre");
        listener.local_addr().expect("dirección").port()
    };
    let url = format!("http://127.0.0.1:{puerto}/up?token=SECRETO");
    let ticket = json!({"attachment_id": "a1", "upload_url": url}).to_string();
    let fake = Fake::texto(&[ticket.as_str()]);

    let error = subidas::upload_chat_file(&fake, "r1", "a.txt", "text/plain", b"hola", "")
        .await
        .expect_err("quería el fallo de transporte");

    assert!(error.to_string().starts_with("subir archivo:"), "{error}");
    assert!(!error.to_string().contains("SECRETO"), "{error}");
    assert!(!error.to_string().contains("token="), "{error}");
}

#[tokio::test]
async fn la_subida_lee_como_mucho_4096_bytes_del_error() {
    let cuerpo = "x".repeat(5000);
    let (url, servidor) = servidor_de_subida(respuesta_http(500, &cuerpo)).await;
    let ticket = json!({"attachment_id": "a1", "upload_url": url}).to_string();
    let fake = Fake::texto(&[ticket.as_str()]);

    let error = subidas::upload_chat_file(&fake, "r1", "a.txt", "text/plain", b"hola", "")
        .await
        .expect_err("quería el estado 500");
    servidor.await.expect("el servidor no terminó");

    assert_eq!(
        error.to_string(),
        format!("subir archivo: 500 {}", "x".repeat(4096))
    );
}

#[tokio::test]
async fn la_subida_traduce_el_507_y_el_texto_de_cuota() {
    let casos = [(507_u16, "lleno"), (500, "storage quota exceeded")];

    for (status, cuerpo) in casos {
        let (url, servidor) = servidor_de_subida(respuesta_http(status, cuerpo)).await;
        let ticket = json!({"attachment_id": "a1", "upload_url": url}).to_string();
        let fake = Fake::texto(&[ticket.as_str()]);

        let error = subidas::upload_chat_file(&fake, "r1", "a.txt", "text/plain", b"hola", "")
            .await
            .expect_err("quería la cuota");
        servidor.await.expect("el servidor no terminó");

        assert!(
            matches!(&error, OrgError::SinEspacio { .. }),
            "{status}: {error}"
        );
        assert_eq!(
            error.to_string(),
            format!(
                "la organización se quedó sin espacio para archivos: subir archivo: {status} {cuerpo}"
            )
        );
    }
}

#[tokio::test]
async fn la_subida_exige_attachment_y_upload_url() {
    let roto = Fake::texto(&["no es json"]);
    let error = subidas::upload_chat_file(&roto, "r1", "a.txt", "text/plain", b"hola", "")
        .await
        .expect_err("quería el JSON roto");
    assert!(
        error
            .to_string()
            .starts_with("upload_chat_file: respuesta no es JSON: "),
        "{error}"
    );

    let faltan = Fake::texto(&[r#"{"attachment_id": "a1"}"#]);
    let error = subidas::upload_chat_file(&faltan, "r1", "a.txt", "text/plain", b"hola", "")
        .await
        .expect_err("quería los campos faltantes");
    assert_eq!(
        error.to_string(),
        "upload_chat_file: faltan campos (attachment_id/upload_url)"
    );
}

#[tokio::test]
async fn upload_chat_file_manda_sus_argumentos_y_recorta_el_body() {
    let (url, servidor) = servidor_de_subida(respuesta_http(200, "")).await;
    let ticket = json!({"attachment_id": "a1", "upload_url": url}).to_string();
    let fake = Fake::texto(&[ticket.as_str(), "ok"]);

    subidas::upload_chat_file(&fake, "r1", "a.txt", "", b"hola", "  mira  ")
        .await
        .unwrap();
    servidor.await.expect("el servidor no terminó");

    let calls = fake.calls();
    assert_eq!(calls.len(), 2);
    assert_call(
        &calls,
        0,
        "upload_chat_file",
        json!({
            "room_id": "r1",
            "file_name": "a.txt",
            "size_bytes": 4,
            "mime_type": "application/octet-stream",
        }),
    );
    assert_call(
        &calls,
        1,
        "send_chat_message",
        json!({"room_id": "r1", "attachment_ids": ["a1"], "body": "mira"}),
    );
}

#[tokio::test]
async fn upload_chat_file_sin_body_no_manda_body() {
    let (url, servidor) = servidor_de_subida(respuesta_http(200, "")).await;
    let ticket = json!({"attachment_id": "a1", "upload_url": url}).to_string();
    let fake = Fake::texto(&[ticket.as_str(), "ok"]);

    subidas::upload_chat_file(&fake, "r1", "a.txt", "text/plain", b"hola", "   ")
        .await
        .unwrap();
    servidor.await.expect("el servidor no terminó");

    let calls = fake.calls();
    assert_call(
        &calls,
        1,
        "send_chat_message",
        json!({"room_id": "r1", "attachment_ids": ["a1"]}),
    );
}

#[tokio::test]
async fn upload_advance_encadena_attach_y_devuelve_el_update() {
    let (url, servidor) = servidor_de_subida(respuesta_http(200, "")).await;
    let ticket = json!({"attachment_id": "a1", "upload_url": url}).to_string();
    let fake = Fake::texto(&[ticket.as_str(), r#"{"update_id": "up7"}"#]);

    let update = subidas::upload_advance(&fake, "t1", "a.txt", "text/plain", b"hola", "nota")
        .await
        .unwrap();
    servidor.await.expect("el servidor no terminó");

    assert_eq!(update, "up7");
    let calls = fake.calls();
    assert_eq!(calls.len(), 2);
    assert_call(
        &calls,
        0,
        "upload_task_file",
        json!({
            "task_id": "t1",
            "file_name": "a.txt",
            "size_bytes": 4,
            "mime_type": "text/plain",
        }),
    );
    assert_call(
        &calls,
        1,
        "attach_task_files",
        json!({"task_id": "t1", "attachment_ids": ["a1"], "note": "nota"}),
    );
}

#[tokio::test]
async fn upload_advance_sin_update_id_falla() {
    let (url, servidor) = servidor_de_subida(respuesta_http(200, "")).await;
    let ticket = json!({"attachment_id": "a1", "upload_url": url}).to_string();
    let fake = Fake::texto(&[ticket.as_str(), "{}"]);

    let error = subidas::upload_advance(&fake, "t1", "a.txt", "text/plain", b"hola", "")
        .await
        .expect_err("quería el avance sin update_id");
    servidor.await.expect("el servidor no terminó");

    assert_eq!(error.to_string(), "attach_task_files: sin update_id");
}

#[tokio::test]
async fn alerts_pide_get_my_alerts_con_limit_50() {
    let fake = Fake::texto(&[
        r#"{"alerts": [{"id": "al1", "type": "mention", "message": "hola", "read": false, "created_at": "2026-01-01T00:00:00Z"}]}"#,
    ]);

    let alerts = subidas::alerts(&fake).await.unwrap();

    assert_eq!(alerts.items.len(), 1);
    assert_eq!(alerts.items[0].id, "al1");
    assert_eq!(alerts.items[0].kind, "mention");
    let calls = fake.calls();
    assert_eq!(calls.len(), 1);
    assert_call(&calls, 0, "get_my_alerts", json!({"limit": 50}));
}

#[tokio::test]
async fn mark_alerts_read_vacio_no_llama() {
    let fake = Fake::texto(&[]);

    subidas::mark_alerts_read(&fake, &[]).await.unwrap();

    assert!(fake.calls().is_empty(), "sin ids no se llama al servidor");
}

#[tokio::test]
async fn mark_alerts_read_con_ids_manda_alert_ids() {
    let fake = Fake::texto(&["ok"]);
    let ids = ["al1".to_owned(), "al2".to_owned()];

    subidas::mark_alerts_read(&fake, &ids).await.unwrap();

    let calls = fake.calls();
    assert_eq!(calls.len(), 1);
    assert_call(
        &calls,
        0,
        "mark_alerts_read",
        json!({"alert_ids": ["al1", "al2"]}),
    );
}

#[tokio::test]
async fn set_task_status_sin_nota_lee_el_progress() {
    let fake = Fake::texto(&[r#"{"progress_reminder": "recuerda", "goal_pct": 12.5}"#]);

    let progress = subidas::set_task_status(&fake, "t1", Status::InProgress, "   ")
        .await
        .unwrap();

    assert_eq!(progress.reminder, "recuerda");
    assert_eq!(progress.goal_pct, 12.5);
    let calls = fake.calls();
    assert_eq!(calls.len(), 1);
    assert_call(
        &calls,
        0,
        "update_task_status",
        json!({"task_id": "t1", "status": "in_progress"}),
    );
}

#[tokio::test]
async fn set_task_status_manda_la_nota_sin_recortar() {
    let fake = Fake::texto(&["{}"]);

    subidas::set_task_status(&fake, "t1", Status::Blocked, "  espero revisión  ")
        .await
        .unwrap();

    let calls = fake.calls();
    assert_call(
        &calls,
        0,
        "update_task_status",
        json!({"task_id": "t1", "status": "blocked", "note": "  espero revisión  "}),
    );
}

#[tokio::test]
async fn set_task_status_con_json_raro_no_falla() {
    let fake = Fake::texto(&["no es json"]);

    let progress = subidas::set_task_status(&fake, "t1", Status::Done, "")
        .await
        .unwrap();

    assert_eq!(progress, Progress::default());
}

#[tokio::test]
async fn set_task_status_rechaza_un_estado_desconocido() {
    let fake = Fake::texto(&[]);

    let error = subidas::set_task_status(&fake, "t1", Status::Unknown("archived".to_owned()), "")
        .await
        .expect_err("quería el estado desconocido");

    assert!(matches!(&error, OrgError::UnknownStatus(value) if value == "archived"));
    assert_eq!(error.to_string(), "estado desconocido: \"archived\"");
    assert!(
        fake.calls().is_empty(),
        "no se llama con un estado inválido"
    );
}

#[tokio::test]
async fn task_files_pide_get_task_files() {
    let fake = Fake::texto(&[
        r#"{"files": [{"id": "f1", "file_name": "x.txt"}], "update_comments": {"up1": []}}"#,
    ]);

    let files = subidas::task_files(&fake, "t1").await.unwrap();

    assert_eq!(
        files
            .files
            .as_ref()
            .and_then(|files| files.first())
            .map(|file| file.id.as_str()),
        Some("f1")
    );
    assert!(files.update_comments.contains_key("up1"));
    let calls = fake.calls();
    assert_eq!(calls.len(), 1);
    assert_call(&calls, 0, "get_task_files", json!({"task_id": "t1"}));
}

#[tokio::test]
async fn comment_update_manda_task_update_y_body() {
    let fake = Fake::texto(&["ok"]);

    subidas::comment_update(&fake, "t1", "up1", "gracias")
        .await
        .unwrap();

    let calls = fake.calls();
    assert_eq!(calls.len(), 1);
    assert_call(
        &calls,
        0,
        "comment_task_update",
        json!({"task_id": "t1", "update_id": "up1", "body": "gracias"}),
    );
}

#[tokio::test]
async fn task_files_conserva_la_paginacion_del_servidor() {
    // Forma real de get_task_files (docs/contrato-avances.md §4): el servidor
    // manda count/total/has_more/truncated/next_offset. El tipo los descartaba
    // y una tarea con muchos avances mostraba una lista parcial sin aviso
    // (issue #97).
    let fake = Fake::texto(&[
        r#"{"files": [{"id": "f1", "file_name": "x.txt"}], "update_comments": {}, "count": 1, "total": 3, "has_more": true, "truncated": true, "next_offset": 1}"#,
    ]);

    let files = subidas::task_files(&fake, "t1").await.unwrap();

    let valor = serde_json::to_value(&files).unwrap();
    assert_eq!(valor["has_more"], json!(true), "has_more se conserva");
    assert_eq!(valor["truncated"], json!(true), "truncated se conserva");
    assert_eq!(valor["next_offset"], json!(1), "next_offset se conserva");
    assert_eq!(valor["count"], json!(1), "count se conserva");
    assert_eq!(valor["total"], json!(3), "total se conserva");
}

#[tokio::test]
async fn task_files_con_next_offset_nulo_lo_lee_como_cero() {
    // El servidor manda next_offset: null cuando no hay página siguiente
    // (docs/contrato-avances.md §4), igual que get_chat_messages.
    let fake = Fake::texto(&[
        r#"{"files": [], "update_comments": {}, "has_more": false, "truncated": false, "next_offset": null}"#,
    ]);

    let files = subidas::task_files(&fake, "t1").await.unwrap();

    let valor = serde_json::to_value(&files).unwrap();
    assert_eq!(
        valor["next_offset"],
        json!(0),
        "el null del servidor se lee como cero"
    );
    assert_eq!(valor["has_more"], json!(false));
}

#[tokio::test]
async fn el_mensaje_fallido_se_reintenta_para_no_dejar_el_adjunto_huerfano() {
    // Los bytes ya están en storage cuando falla send_chat_message: sin
    // recuperación el adjunto queda huérfano y gasta cuota (issue #97, #84).
    let (url, servidor) = servidor_de_subida(respuesta_http(200, "")).await;
    let ticket = json!({"attachment_id": "a1", "upload_url": url}).to_string();
    let fake = Fake::with(vec![
        Answer::Text(ticket),
        Answer::Error("se cayó la red".to_owned()),
        Answer::Text(r#"{"messages": [], "has_more": false}"#.to_owned()),
        Answer::Text("ok".to_owned()),
    ]);

    subidas::upload_chat_file(&fake, "r1", "a.txt", "text/plain", b"hola", "mira")
        .await
        .unwrap();
    servidor.await.expect("el servidor no terminó");

    let calls = fake.calls();
    assert_eq!(calls.len(), 4, "se consulta y se reintenta una vez");
    assert_eq!(calls[1].0, "send_chat_message");
    assert_eq!(calls[2].0, "get_chat_messages");
    assert_eq!(calls[3].0, "send_chat_message");
    assert_eq!(
        Value::Object(calls[1].1.clone()),
        Value::Object(calls[3].1.clone()),
        "el reintento publica el mismo adjunto"
    );

    let (url, servidor) = servidor_de_subida(respuesta_http(200, "")).await;
    let ticket = json!({"attachment_id": "a1", "upload_url": url}).to_string();
    let ya_publicado = Fake::with(vec![
        Answer::Text(ticket),
        Answer::Error("se cortó la respuesta".to_owned()),
        Answer::Text(
            r#"{"messages": [{"id": "m1", "attachments": [{"id": "a1"}]}], "has_more": false}"#
                .to_owned(),
        ),
    ]);
    subidas::upload_chat_file(&ya_publicado, "r1", "a.txt", "text/plain", b"hola", "mira")
        .await
        .unwrap();
    servidor.await.expect("el servidor no terminó");
    let calls = ya_publicado.calls();
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.0 == "send_chat_message")
            .count(),
        1,
        "un mensaje ya guardado no se publica dos veces"
    );
}

#[tokio::test]
async fn si_el_reintento_del_mensaje_tambien_falla_el_error_sube() {
    let (url, servidor) = servidor_de_subida(respuesta_http(200, "")).await;
    let ticket = json!({"attachment_id": "a1", "upload_url": url}).to_string();
    let fake = Fake::with(vec![
        Answer::Text(ticket),
        Answer::Error("se cayó la red".to_owned()),
        Answer::Text(r#"{"messages": [], "has_more": false}"#.to_owned()),
        Answer::Error("la red sigue caída".to_owned()),
    ]);

    let error = subidas::upload_chat_file(&fake, "r1", "a.txt", "text/plain", b"hola", "")
        .await
        .expect_err("quería el error del reintento");
    servidor.await.expect("el servidor no terminó");

    assert!(error.to_string().contains("la red sigue caída"), "{error}");
    assert_eq!(fake.calls().len(), 4, "se intentó el mensaje dos veces");
}

#[tokio::test]
async fn el_avance_fallido_se_reintenta_para_no_dejar_el_adjunto_huerfano() {
    let (url, servidor) = servidor_de_subida(respuesta_http(200, "")).await;
    let ticket = json!({"attachment_id": "a1", "upload_url": url}).to_string();
    let fake = Fake::with(vec![
        Answer::Text(ticket),
        Answer::Error("se cayó la red".to_owned()),
        Answer::Text(r#"{"files": [], "update_comments": {}}"#.to_owned()),
        Answer::Text(r#"{"update_id": "up9"}"#.to_owned()),
    ]);

    let update = subidas::upload_advance(&fake, "t1", "a.txt", "text/plain", b"hola", "nota")
        .await
        .unwrap();
    servidor.await.expect("el servidor no terminó");

    assert_eq!(update, "up9");
    let calls = fake.calls();
    assert_eq!(
        calls.len(),
        4,
        "se comprueba el storage y se reintenta una vez"
    );
    assert_eq!(calls[1].0, "attach_task_files");
    assert_eq!(calls[2].0, "get_task_files");
    assert_eq!(calls[3].0, "attach_task_files");
    assert_eq!(
        Value::Object(calls[1].1.clone()),
        Value::Object(calls[3].1.clone()),
        "el reintento enlaza el mismo adjunto"
    );
}

#[tokio::test]
async fn un_attach_sin_update_id_no_se_reintenta() {
    // Si attach_task_files respondió, el servidor ya creó el avance aunque
    // falte el id: reintentar duplicaría el avance, no cura un huérfano.
    let (url, servidor) = servidor_de_subida(respuesta_http(200, "")).await;
    let ticket = json!({"attachment_id": "a1", "upload_url": url}).to_string();
    let fake = Fake::texto(&[ticket.as_str(), "{}"]);

    let error = subidas::upload_advance(&fake, "t1", "a.txt", "text/plain", b"hola", "")
        .await
        .expect_err("quería el avance sin update_id");
    servidor.await.expect("el servidor no terminó");

    assert_eq!(error.to_string(), "attach_task_files: sin update_id");
    assert_eq!(
        fake.calls().len(),
        2,
        "no se reintenta una respuesta ya recibida"
    );
}

/// Servidor que lee el cuerpo en trozos de 1 MiB con 5 s de pausa entre trozos:
/// 25 MiB tardan ~125 s, más que los 120 s fijos del defecto #84. Si el cliente
/// corta antes (plazo vencido), devuelve lo que alcanzó a llegar.
async fn servidor_lento(respuesta: Vec<u8>) -> (String, tokio::task::JoinHandle<usize>) {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("no se pudo abrir el puerto de prueba");
    let addr = listener.local_addr().expect("sin dirección local");
    let url = format!("http://{addr}/up");
    let handle = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("sin conexión");
        let mut data = Vec::new();
        let mut buffer = [0_u8; 64 * 1024];
        let header_end = loop {
            let read = socket.read(&mut buffer).await.expect("lectura falló");
            assert!(read > 0, "el cliente cerró antes de mandar los headers");
            data.extend_from_slice(&buffer[..read]);
            if let Some(position) = data.windows(4).position(|window| window == b"\r\n\r\n") {
                break position;
            }
        };
        let head = String::from_utf8_lossy(&data[..header_end]).into_owned();
        let content_length = head
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                if !name.eq_ignore_ascii_case("content-length") {
                    return None;
                }
                value.trim().parse::<usize>().ok()
            })
            .unwrap_or(0);
        let body_start = header_end + 4;
        let mut leidos = 0;
        while leidos < content_length {
            let objetivo = (leidos + 1024 * 1024).min(content_length);
            while data.len() < body_start + objetivo {
                let read = socket.read(&mut buffer).await.expect("lectura falló");
                if read == 0 {
                    break;
                }
                data.extend_from_slice(&buffer[..read]);
            }
            if data.len() < body_start + objetivo {
                break; // el cliente cortó la subida
            }
            leidos = objetivo;
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
        let _ = socket.write_all(&respuesta).await;
        let _ = socket.flush().await;
        data.len().saturating_sub(body_start)
    });
    (url, handle)
}

#[tokio::test]
#[ignore = "tarda ~125 s reales; antes de un release: cargo test -p orus-org --test subidas -- --ignored"]
async fn la_subida_de_25_mib_en_conexion_lenta_no_se_corta() {
    // #84: el plazo total fijo de 120 s no alcanza para el archivo máximo del
    // contrato (25 MiB, docs/contrato-avances.md §1) en una conexión lenta.
    // El plazo debe ser proporcional al tamaño. Tarda ~125 s a propósito.
    let datos = vec![7_u8; 25 * 1024 * 1024];
    let (url, servidor) = servidor_lento(respuesta_http(200, "")).await;
    let ticket = json!({"attachment_id": "a1", "upload_url": url}).to_string();
    let fake = Fake::texto(&[ticket.as_str(), "ok"]);

    subidas::upload_chat_file(
        &fake,
        "r1",
        "grande.bin",
        "application/octet-stream",
        &datos,
        "",
    )
    .await
    .expect("los 25 MiB en conexión lenta deben caber en el plazo");

    let recibidos = servidor.await.expect("el servidor no terminó");
    assert_eq!(recibidos, datos.len(), "el cuerpo llegó completo");
}

#[test]
fn el_plazo_de_subida_es_proporcional_al_tamano() {
    use orus_org::subidas::plazo_de_subida;

    assert_eq!(
        plazo_de_subida(0),
        Duration::from_secs(120),
        "los archivos chicos conservan el plazo base"
    );
    let grande = plazo_de_subida(25 * 1024 * 1024);
    assert!(
        grande > Duration::from_secs(120),
        "25 MiB en conexión lenta no caben en 120 s: {grande:?}"
    );
    assert!(
        grande >= Duration::from_secs(256),
        "presupuesto de al menos 100 KiB/s para 25 MiB: {grande:?}"
    );
    assert!(
        plazo_de_subida(1024 * 1024) <= grande,
        "el plazo no decrece con el tamaño"
    );
}

#[tokio::test]
async fn un_attach_fallido_no_duplica_el_avance_si_ya_esta_en_storage() {
    // Error de transporte después de que el servidor grabó el avance: consultar
    // get_task_files evita crear un segundo avance con el mismo adjunto (#130).
    let (url, servidor) = servidor_de_subida(respuesta_http(200, "")).await;
    let ticket = json!({"attachment_id": "a1", "upload_url": url}).to_string();
    let fake = Fake::with(vec![
        Answer::Text(ticket),
        Answer::Error("se cortó la respuesta".to_owned()),
        Answer::Text(
            r#"{"files": [], "update_comments": {}, "has_more": true, "next_offset": 1}"#
                .to_owned(),
        ),
        Answer::Text(
            r#"{"files": [{"id": "a1", "update_id": "up7"}], "update_comments": {}}"#.to_owned(),
        ),
        Answer::Text(r#"{"update_id": "up-NUEVO"}"#.to_owned()),
    ]);

    let update = subidas::upload_advance(&fake, "t1", "a.txt", "text/plain", b"hola", "nota")
        .await
        .unwrap();
    servidor.await.expect("el servidor no terminó");

    assert_eq!(
        update, "up7",
        "se devuelve el avance ya creado de la segunda página"
    );
    let calls = fake.calls();
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.0 == "attach_task_files")
            .count(),
        1,
        "no se reintenta un adjunto que ya está enlazado"
    );
    assert_eq!(calls[2].0, "get_task_files");
    assert_eq!(calls[3].0, "get_task_files");
    assert_call(
        &calls,
        3,
        "get_task_files",
        json!({"task_id": "t1", "offset": 1}),
    );

    let (url, servidor) = servidor_de_subida(respuesta_http(200, "")).await;
    let ticket = json!({"attachment_id": "a2", "upload_url": url}).to_string();
    let incompleto = Fake::with(vec![
        Answer::Text(ticket),
        Answer::Error("se cortó la respuesta".to_owned()),
        Answer::Text(
            r#"{"files": [], "update_comments": {}, "has_more": true, "next_offset": 0}"#
                .to_owned(),
        ),
    ]);
    let error = subidas::upload_advance(&incompleto, "t1", "a.txt", "text/plain", b"hola", "nota")
        .await
        .expect_err("la búsqueda incompleta no puede dar por bueno el reintento");
    servidor.await.expect("el servidor no terminó");
    assert!(matches!(error, OrgError::ResultadoIncierto(_)), "{error}");
    assert_eq!(
        incompleto
            .calls()
            .iter()
            .filter(|call| call.0 == "attach_task_files")
            .count(),
        1,
        "sin certeza no se reintenta el attach"
    );
}

#[tokio::test]
async fn alerts_devuelve_la_paginacion_del_servidor() {
    let fake = Fake::texto(&[r#"{
        "alerts": [{"id": "a1", "type": "info", "message": "hola"}],
        "count": 1, "total": 60, "has_more": true, "truncated": true, "next_offset": 50
    }"#]);

    let page = subidas::alerts(&fake).await.unwrap();
    let valor = serde_json::to_value(&page).unwrap();

    assert_eq!(valor["has_more"], json!(true), "faltan avisos");
    assert_eq!(valor["truncated"], json!(true));
    assert_eq!(valor["next_offset"], json!(50));
    assert_eq!(valor["total"], json!(60));
}
