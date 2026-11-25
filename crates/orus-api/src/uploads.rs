//! Subidas multipart de archivos para tareas y chat.
//!
//! Port de `internal/app/http.go:137-158,690-712,1065-1114`. `subida` limita la
//! petición a 26 MB y el archivo a 25 MB; los dos excesos son 400 con
//! `archivo_grande`, nunca 413. El formulario se recorre completo antes de
//! validar, así que el orden de los campos no importa.

use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::extract::{FromRequest, Multipart, Request, State as AxumState};
use axum::http::StatusCode;
use axum::http::header::CONTENT_TYPE;
use axum::response::Response;
use orus_app::App;
use serde_json::json;

use crate::{ErrorRuta, write_json};

/// `http.MaxBytesReader(w, r.Body, 26<<20)` (`http.go:1084`).
const TOPE_PETICION: usize = 26 * 1024 * 1024;
/// `25*1024*1024` (`http.go:1095,1104`).
const TOPE_ARCHIVO: usize = 25 * 1024 * 1024;
/// `subida` rellena el tipo cuando la parte no trae `Content-Type`.
const TIPO_POR_DEFECTO: &str = "application/octet-stream";

/// Un error del parser multipart: el 400/413 de multer es `archivo_grande`
/// (Go mapea todo `ParseMultipartForm` a ese texto); un fallo de transporte
/// real es 500, como el `writeErr(err)` de Go.
fn error_multipart(error: axum::extract::multipart::MultipartError) -> ErrorRuta {
    match error.status() {
        StatusCode::BAD_REQUEST | StatusCode::PAYLOAD_TOO_LARGE => ErrorRuta::ArchivoGrande,
        _ => ErrorRuta::Lectura(error.body_text()),
    }
}

/// Go guarda `FileHeader.Filename` después de `filepath.Base`
/// (`mime/multipart.Part.FileName`); en Windows eso también quita las barras
/// invertidas. `Path::file_name` usa la misma regla del sistema.
fn nombre_base(nombre: &str) -> String {
    std::path::Path::new(nombre)
        .file_name()
        .map(|parte| parte.to_string_lossy().into_owned())
        .unwrap_or_else(|| nombre.to_owned())
}

struct Archivo {
    nombre: String,
    mime: String,
    datos: Vec<u8>,
}

/// Solo las claves que leen las dos rutas. Gana la primera aparición de cada
/// campo, igual que `FormValue` dentro de un mismo formulario.
#[derive(Default)]
struct Formulario {
    archivo: Option<Archivo>,
    task: Option<String>,
    note: Option<String>,
    room: Option<String>,
    body: Option<String>,
}

/// Port de `subida` (`http.go:1081-1114`): lee el cuerpo entero con el tope de
/// la petición y después parsea el multipart. Solo el exceso y el multipart mal
/// formado son `ArchivoGrande`; un fallo real de lectura es 500, como
/// `writeErr(err)` con el `io.ReadAll` de Go.
async fn leer_formulario(request: Request) -> Result<Formulario, ErrorRuta> {
    let (partes, cuerpo) = request.into_parts();
    let bytes = match to_bytes(cuerpo, TOPE_PETICION.saturating_add(1)).await {
        Ok(bytes) => bytes,
        Err(error) => {
            // Igual que `decode_con_limite`: el tope se reconoce por la fuente.
            let limite = std::error::Error::source(&error)
                .is_some_and(|fuente| fuente.to_string() == "length limit exceeded");
            return Err(if limite {
                ErrorRuta::ArchivoGrande
            } else {
                ErrorRuta::Lectura(error.to_string())
            });
        }
    };
    if bytes.len() > TOPE_PETICION {
        return Err(ErrorRuta::ArchivoGrande);
    }
    let peticion = Request::from_parts(partes, Body::from(bytes));
    let mut multipart = Multipart::from_request(peticion, &())
        .await
        .map_err(|_| ErrorRuta::ArchivoGrande)?;

    let mut formulario = Formulario::default();
    while let Some(mut campo) = multipart.next_field().await.map_err(error_multipart)? {
        let nombre = campo.name().unwrap_or_default().to_owned();
        let nombre_archivo = campo.file_name().unwrap_or_default().to_owned();
        // Como `header.Header.Get("Content-Type")`: el valor crudo de la parte.
        let mime = campo
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|valor| valor.to_str().ok())
            .map(str::to_owned);
        let tope = if nombre == "file" {
            TOPE_ARCHIVO
        } else {
            TOPE_PETICION
        };

        // Sin `field.bytes()`: el tope se comprueba mientras llegan los trozos.
        let mut datos = Vec::new();
        while let Some(trozo) = campo.chunk().await.map_err(error_multipart)? {
            if datos.len().saturating_add(trozo.len()) > tope {
                return Err(ErrorRuta::ArchivoGrande);
            }
            datos.extend_from_slice(&trozo);
        }

        // Go separa por `filename`: con nombre va al mapa de archivos y sin
        // nombre al de valores. `FormFile("file")` exige nombre.
        if nombre == "file" && !nombre_archivo.is_empty() {
            if formulario.archivo.is_none() {
                formulario.archivo = Some(Archivo {
                    nombre: nombre_base(&nombre_archivo),
                    mime: mime
                        .filter(|valor| !valor.is_empty())
                        .unwrap_or_else(|| TIPO_POR_DEFECTO.to_owned()),
                    datos,
                });
            }
        } else if nombre_archivo.is_empty() {
            let texto = String::from_utf8_lossy(&datos).into_owned();
            let destino = match nombre.as_str() {
                "task" => &mut formulario.task,
                "note" => &mut formulario.note,
                "room" => &mut formulario.room,
                "body" => &mut formulario.body,
                _ => continue,
            };
            if destino.is_none() {
                *destino = Some(texto);
            }
        }
    }
    Ok(formulario)
}

/// `POST /api/tasks/files` (`http.go:137-156`): la tarea y la nota no se
/// validan en el adaptador; el backend recibe la cadena tal cual.
pub(crate) async fn tarea(
    AxumState(app): AxumState<Arc<App>>,
    request: Request,
) -> Result<Response, ErrorRuta> {
    let formulario = leer_formulario(request).await?;
    let Some(archivo) = formulario.archivo else {
        return Err(ErrorRuta::Invalido("falta el archivo".to_owned()));
    };
    let cliente = app.client().await?;
    let update_id = cliente
        .upload_advance(
            formulario.task.as_deref().unwrap_or_default(),
            &archivo.nombre,
            &archivo.mime,
            &archivo.datos,
            formulario.note.as_deref().unwrap_or_default(),
        )
        .await?;
    Ok(write_json(StatusCode::OK, &json!({"update_id": update_id})))
}

/// `POST /api/chat/files` (`http.go:690-711`): el archivo se lee antes de
/// mirar la sala; sin archivo la respuesta es `falta el archivo`.
pub(crate) async fn chat(
    AxumState(app): AxumState<Arc<App>>,
    request: Request,
) -> Result<Response, ErrorRuta> {
    let formulario = leer_formulario(request).await?;
    let Some(archivo) = formulario.archivo else {
        return Err(ErrorRuta::Invalido("falta el archivo".to_owned()));
    };
    let room = formulario.room.as_deref().unwrap_or_default();
    if room.is_empty() {
        return Err(ErrorRuta::Invalido("falta la sala".to_owned()));
    }
    let cliente = app.client().await?;
    cliente
        .upload_chat_file(
            room,
            &archivo.nombre,
            &archivo.mime,
            &archivo.datos,
            formulario.body.as_deref().unwrap_or_default(),
        )
        .await?;
    Ok(write_json(StatusCode::OK, &json!({"ok": true})))
}
