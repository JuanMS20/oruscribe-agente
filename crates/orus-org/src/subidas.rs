//! Subidas de archivos, cuota, alertas y estado de tareas.
//!
//! Port de `internal/oruscribe/oruscribe.go:509-707`. La subida tiene tres
//! pasos: pedir la URL firmada a una herramienta MCP, subir los bytes a esa URL
//! y, según el caso, crear el mensaje o el avance. La cuota agotada se reconoce
//! por el texto del servidor y por el estado 507.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::client::{Client, JsonObject, decode_json};
use crate::error::{OrgError, Result};
use crate::mensajes::ToolCaller;
use crate::types::{
    Alert, Message, Pagina, Paginacion, Progress, Status, TaskFiles, nulo_como_cero,
};
use crate::url_segura;

/// Plazo base de la subida a la URL firmada: los archivos chicos lo conservan.
const PLAZO_BASE_SUBIDA: Duration = Duration::from_secs(120);

/// Velocidad mínima que se presupuesta para la subida (100 KiB/s ~ 0,8 Mbit/s).
/// El plazo total crece con el tamaño: 120 s fijos no alcanzan para los 25 MiB
/// del contrato en una conexión lenta (#84).
const BYTES_POR_SEGUNDO_SUBIDA: u64 = 100 * 1024;

/// Margen sobre la transferencia presupuestada (conexión, TLS, respuesta).
const MARGEN_SUBIDA: Duration = Duration::from_secs(60);

/// Plazo total de la subida, proporcional al tamaño del archivo.
pub fn plazo_de_subida(size_bytes: usize) -> Duration {
    let transferencia = (size_bytes as u64).div_ceil(BYTES_POR_SEGUNDO_SUBIDA);
    PLAZO_BASE_SUBIDA.max(Duration::from_secs(transferencia) + MARGEN_SUBIDA)
}

/// Cuánto cuerpo de una respuesta fallida se conserva en el error.
const LIMITE_CUERPO_DE_ERROR: usize = 4096;

const MAX_PAGAS_CONSULTA: usize = 10;

/// Reconoce el error de almacenamiento sin depender de una sola frase: el
/// texto cambia entre el MCP y el servicio de archivos.
pub fn parece_cuota(texto: &str) -> bool {
    let texto = texto.to_lowercase();
    [
        "quota",
        "storage quota",
        "out of space",
        "no space left",
        "sin espacio",
    ]
    .iter()
    .any(|marca| texto.contains(marca))
}

/// Exige https para el archivo. La dirección la elige el servidor, así que una
/// respuesta rota no puede hacer que viaje en claro; http solo se acepta en
/// loopback, igual que al configurar un proveedor.
pub fn validar_url_de_subida(raw: &str) -> Result<()> {
    let url = reqwest::Url::parse(raw).map_err(|_| OrgError::InvalidUploadUrl)?;
    if url.host_str().is_none_or(str::is_empty) {
        return Err(OrgError::InvalidUploadUrl);
    }
    if url_segura(&url) {
        Ok(())
    } else {
        Err(OrgError::InsecureUploadUrl)
    }
}

/// Lo que responde la herramienta de subida con la dirección firmada.
#[derive(Debug, Deserialize)]
struct UploadTicket {
    #[serde(default)]
    attachment_id: String,
    #[serde(default)]
    upload_url: String,
    #[serde(default)]
    method: String,
    #[serde(default)]
    headers: BTreeMap<String, String>,
}

/// Pide la URL firmada, sube los bytes y devuelve el id del adjunto.
///
/// `args` lleva lo propio de la herramienta (`room_id` o `task_id`); acá se le
/// agregan `file_name`, `size_bytes` y `mime_type`.
pub async fn subir_archivo<C: ToolCaller + ?Sized>(
    caller: &C,
    herramienta: &str,
    mut args: JsonObject,
    name: &str,
    mime: &str,
    data: &[u8],
) -> Result<String> {
    let mime = if mime.is_empty() {
        "application/octet-stream"
    } else {
        mime
    };
    args.insert("file_name".to_owned(), Value::from(name));
    args.insert("size_bytes".to_owned(), Value::from(data.len() as u64));
    args.insert("mime_type".to_owned(), Value::from(mime));

    let texto = caller.call(herramienta, args).await.map_err(|error| {
        if parece_cuota(&error.to_string()) {
            OrgError::SinEspacio {
                detalle: error.to_string(),
            }
        } else {
            error
        }
    })?;

    let ticket: UploadTicket = decode_json(herramienta, &texto)?;
    if ticket.attachment_id.is_empty() || ticket.upload_url.is_empty() {
        return Err(OrgError::MissingUploadFields {
            tool: herramienta.to_owned(),
        });
    }
    validar_url_de_subida(&ticket.upload_url).map_err(|source| OrgError::ToolContext {
        tool: herramienta.to_owned(),
        source: Box::new(source),
    })?;

    let metodo = if ticket.method.is_empty() {
        "PUT"
    } else {
        ticket.method.as_str()
    };
    let metodo = reqwest::Method::from_bytes(metodo.as_bytes())
        .map_err(|_| OrgError::InvalidUploadMethod(metodo.to_owned()))?;

    let content_type = ticket
        .headers
        .get("Content-Type")
        .filter(|value| !value.is_empty())
        .map(String::as_str)
        .unwrap_or(mime);

    // Cliente normal, no la sesión MCP: la URL firmada no lleva el token.
    let http = reqwest::Client::builder()
        .timeout(plazo_de_subida(data.len()))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(OrgError::HttpClient)?;

    let respuesta = http
        .request(metodo, &ticket.upload_url)
        .header(reqwest::header::CONTENT_TYPE, content_type)
        .body(data.to_vec())
        .send()
        .await
        .map_err(|error| OrgError::UploadHttp(error.without_url()))?;

    let estado = respuesta.status();
    if !estado.is_success() {
        let cuerpo = leer_cuerpo_de_error(respuesta).await;
        let cuerpo = String::from_utf8_lossy(&cuerpo).trim().to_owned();
        let error = OrgError::UploadStatus {
            status: estado.as_u16(),
            body: cuerpo,
        };
        if estado == reqwest::StatusCode::INSUFFICIENT_STORAGE || parece_cuota(&error.to_string()) {
            return Err(OrgError::SinEspacio {
                detalle: error.to_string(),
            });
        }
        return Err(error);
    }

    Ok(ticket.attachment_id)
}

/// Lee como mucho [`LIMITE_CUERPO_DE_ERROR`] bytes. Un fallo de lectura deja lo
/// que ya llegó: el cuerpo es contexto, no la causa.
async fn leer_cuerpo_de_error(mut respuesta: reqwest::Response) -> Vec<u8> {
    let mut cuerpo = Vec::new();
    while cuerpo.len() < LIMITE_CUERPO_DE_ERROR {
        match respuesta.chunk().await {
            Ok(Some(trozo)) => {
                let falta = LIMITE_CUERPO_DE_ERROR - cuerpo.len();
                cuerpo.extend_from_slice(&trozo[..trozo.len().min(falta)]);
            }
            Ok(None) | Err(_) => break,
        }
    }
    cuerpo
}

/// Sube un archivo al chat y, si hay texto, lo publica con el adjunto.
pub async fn upload_chat_file<C: ToolCaller + ?Sized>(
    caller: &C,
    room_id: &str,
    name: &str,
    mime: &str,
    data: &[u8],
    body: &str,
) -> Result<()> {
    let mut args = JsonObject::new();
    args.insert("room_id".to_owned(), Value::from(room_id));
    let adjunto = subir_archivo(caller, "upload_chat_file", args, name, mime, data).await?;

    let mut args = JsonObject::new();
    args.insert("room_id".to_owned(), Value::from(room_id));
    args.insert("attachment_ids".to_owned(), json!([adjunto]));
    let body = body.trim();
    if !body.is_empty() {
        args.insert("body".to_owned(), Value::from(body));
    }
    // Los bytes ya están en storage: antes de reintentar se consulta si el
    // mensaje ya se guardó para no duplicarlo (#84, P1).
    match caller.call("send_chat_message", args.clone()).await {
        Ok(_) => Ok(()),
        Err(error) => match mensaje_ya_publicado(caller, room_id, &adjunto).await {
            Ok(Some(())) => Ok(()),
            Ok(None) => caller.call("send_chat_message", args).await.map(|_| ()),
            Err(incierto) => Err(OrgError::ResultadoIncierto(format!("{error}; {incierto}"))),
        },
    }
}

#[derive(Debug, Deserialize)]
struct RespuestaMensajes {
    #[serde(default)]
    messages: Vec<Message>,
    #[serde(default)]
    has_more: bool,
    #[serde(default, deserialize_with = "nulo_como_cero")]
    next_offset: i64,
}

async fn mensaje_ya_publicado<C: ToolCaller + ?Sized>(
    caller: &C,
    room_id: &str,
    adjunto: &str,
) -> Result<Option<()>> {
    let mut offset = 0_i64;
    let mut vistos = std::collections::BTreeSet::new();
    for _ in 0..MAX_PAGAS_CONSULTA {
        let mut args = JsonObject::new();
        args.insert("room_id".to_owned(), Value::from(room_id));
        args.insert("limit".to_owned(), Value::from(50));
        if offset > 0 {
            args.insert("offset".to_owned(), Value::from(offset));
        }
        let pagina: RespuestaMensajes = caller.call_json("get_chat_messages", args).await?;
        if pagina.messages.iter().any(|mensaje| {
            mensaje
                .attachments
                .as_ref()
                .is_some_and(|adjuntos| adjuntos.iter().any(|item| item.id == adjunto))
        }) {
            return Ok(Some(()));
        }
        if !pagina.has_more {
            return Ok(None);
        }
        let siguiente = pagina.next_offset;
        if siguiente <= offset || !vistos.insert(siguiente) {
            return Err(OrgError::LimitExceeded(
                "get_chat_messages repitió el cursor".to_owned(),
            ));
        }
        offset = siguiente;
    }
    Err(OrgError::LimitExceeded(
        "get_chat_messages superó el tope de páginas".to_owned(),
    ))
}

/// Sube un archivo y crea un avance con una nota. Devuelve el `update_id`.
pub async fn upload_advance<C: ToolCaller + ?Sized>(
    caller: &C,
    task_id: &str,
    name: &str,
    mime: &str,
    data: &[u8],
    note: &str,
) -> Result<String> {
    let mut args = JsonObject::new();
    args.insert("task_id".to_owned(), Value::from(task_id));
    let adjunto = subir_archivo(caller, "upload_task_file", args, name, mime, data).await?;

    let mut args = JsonObject::new();
    args.insert("task_id".to_owned(), Value::from(task_id));
    args.insert("attachment_ids".to_owned(), json!([adjunto]));
    args.insert("note".to_owned(), Value::from(note));
    // El adjunto ya está en storage, pero un error de transporte no dice si el
    // servidor grabó el avance. Antes de reintentar se busca el adjunto: si ya
    // está enlazado, reintentar crearía un segundo avance (#130).
    let texto = match caller.call("attach_task_files", args.clone()).await {
        Ok(texto) => texto,
        Err(error) => match avance_ya_creado(caller, task_id, &adjunto).await {
            Ok(Some(update_id)) => {
                if update_id.is_empty() {
                    return Err(OrgError::MissingUpdateId);
                }
                return Ok(update_id);
            }
            Ok(None) => caller.call("attach_task_files", args).await?,
            Err(incierto) => {
                return Err(OrgError::ResultadoIncierto(format!("{error}; {incierto}")));
            }
        },
    };

    #[derive(Debug, Deserialize)]
    struct AttachResponse {
        #[serde(default)]
        update_id: String,
    }

    let respuesta: AttachResponse = decode_json("attach_task_files", &texto)?;
    if respuesta.update_id.is_empty() {
        return Err(OrgError::MissingUpdateId);
    }
    Ok(respuesta.update_id)
}

/// Busca el adjunto en los avances de la tarea. Devuelve su `update_id` si el
/// servidor ya lo enlazó antes de que se cayera la respuesta.
async fn avance_ya_creado<C: ToolCaller + ?Sized>(
    caller: &C,
    task_id: &str,
    adjunto: &str,
) -> Result<Option<String>> {
    let mut offset = 0_i64;
    let mut vistos = std::collections::BTreeSet::new();
    for _ in 0..MAX_PAGAS_CONSULTA {
        let mut args = JsonObject::new();
        args.insert("task_id".to_owned(), Value::from(task_id));
        if offset > 0 {
            args.insert("offset".to_owned(), Value::from(offset));
        }
        let archivos: TaskFiles = caller.call_json("get_task_files", args).await?;
        if let Some(encontrado) = archivos
            .files
            .unwrap_or_default()
            .into_iter()
            .find(|archivo| archivo.id == adjunto)
        {
            return Ok(Some(encontrado.update_id));
        }
        if !archivos.has_more && !archivos.truncated {
            return Ok(None);
        }
        let siguiente = archivos.next_offset;
        if siguiente <= offset || !vistos.insert(siguiente) {
            return Err(OrgError::LimitExceeded(
                "get_task_files repitió el cursor".to_owned(),
            ));
        }
        offset = siguiente;
    }
    Err(OrgError::LimitExceeded(
        "get_task_files superó el tope de páginas".to_owned(),
    ))
}

/// Las alertas de la persona, con un tope fijo de 50.
pub async fn alerts<C: ToolCaller + ?Sized>(caller: &C) -> Result<Pagina<Alert>> {
    #[derive(Debug, Deserialize)]
    struct AlertsResponse {
        #[serde(default)]
        alerts: Vec<Alert>,
        #[serde(flatten)]
        pagina: Paginacion,
    }

    let mut args = JsonObject::new();
    args.insert("limit".to_owned(), Value::from(50));
    let respuesta: AlertsResponse = caller.call_json("get_my_alerts", args).await?;
    Ok(respuesta.pagina.con_items(respuesta.alerts))
}

/// Marca leídas alertas concretas. Sin ids no se llama al servidor.
pub async fn mark_alerts_read<C: ToolCaller + ?Sized>(caller: &C, ids: &[String]) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let mut args = JsonObject::new();
    args.insert("alert_ids".to_owned(), json!(ids));
    caller.call("mark_alerts_read", args).await.map(|_| ())
}

/// Cambia el estado de una tarea y devuelve lo que el servidor agregó.
///
/// La nota solo viaja si tiene algo además de espacios, pero se manda tal
/// cual: el servidor la pide en `blocked`, no es obligatoria.
pub async fn set_task_status<C: ToolCaller + ?Sized>(
    caller: &C,
    task_id: &str,
    status: Status,
    note: &str,
) -> Result<Progress> {
    if !status.valid() {
        return Err(OrgError::UnknownStatus(status.as_str().to_owned()));
    }
    let mut args = JsonObject::new();
    args.insert("task_id".to_owned(), Value::from(task_id));
    args.insert("status".to_owned(), Value::from(status.as_str()));
    if !note.trim().is_empty() {
        args.insert("note".to_owned(), Value::from(note));
    }
    let texto = caller.call("update_task_status", args).await?;
    // El recordatorio es best effort: un cambio que funcionó no se reporta
    // como fallido porque los campos extra tuvieran otra forma.
    Ok(serde_json::from_str(&texto).unwrap_or_default())
}

/// Lee los avances (archivos y comentarios) de una tarea. Si el servidor
/// pagina, `has_more`/`truncated`/`next_offset` avisan que la lista quedó corta.
pub async fn task_files<C: ToolCaller + ?Sized>(caller: &C, task_id: &str) -> Result<TaskFiles> {
    let mut args = JsonObject::new();
    args.insert("task_id".to_owned(), Value::from(task_id));
    // `TaskFiles` ya lleva `has_more`/`truncated`/`next_offset`: se devuelven
    // al llamador para que la ventana pueda avisar. `eprintln!` no llega a la
    // app de escritorio (#130).
    caller.call_json("get_task_files", args).await
}

/// Responde a un avance.
pub async fn comment_update<C: ToolCaller + ?Sized>(
    caller: &C,
    task_id: &str,
    update_id: &str,
    body: &str,
) -> Result<()> {
    let mut args = JsonObject::new();
    args.insert("task_id".to_owned(), Value::from(task_id));
    args.insert("update_id".to_owned(), Value::from(update_id));
    args.insert("body".to_owned(), Value::from(body));
    caller.call("comment_task_update", args).await.map(|_| ())
}

impl Client {
    /// Sube un archivo al chat y publica el mensaje con el adjunto.
    pub async fn upload_chat_file(
        &self,
        room_id: &str,
        name: &str,
        mime: &str,
        data: &[u8],
        body: &str,
    ) -> Result<()> {
        upload_chat_file(self, room_id, name, mime, data, body).await
    }

    /// Sube un avance de tarea y devuelve su `update_id`.
    pub async fn upload_advance(
        &self,
        task_id: &str,
        name: &str,
        mime: &str,
        data: &[u8],
        note: &str,
    ) -> Result<String> {
        upload_advance(self, task_id, name, mime, data, note).await
    }

    /// Las alertas de la persona.
    pub async fn alerts(&self) -> Result<Pagina<Alert>> {
        alerts(self).await
    }

    /// Marca leídas alertas concretas.
    pub async fn mark_alerts_read(&self, ids: &[String]) -> Result<()> {
        mark_alerts_read(self, ids).await
    }

    /// Cambia el estado de una tarea.
    pub async fn set_task_status(
        &self,
        task_id: &str,
        status: Status,
        note: &str,
    ) -> Result<Progress> {
        set_task_status(self, task_id, status, note).await
    }

    /// Lee los avances de una tarea.
    pub async fn task_files(&self, task_id: &str) -> Result<TaskFiles> {
        task_files(self, task_id).await
    }

    /// Responde a un avance.
    pub async fn comment_update(&self, task_id: &str, update_id: &str, body: &str) -> Result<()> {
        comment_update(self, task_id, update_id, body).await
    }
}
