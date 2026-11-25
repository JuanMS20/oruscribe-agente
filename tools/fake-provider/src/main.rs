//! Proveedor falso: responde `chat completions` con un guion, sin red externa.
//!
//! Sirve para dos cosas:
//! 1. Grabar un turno del binario Go sin pagar tokens.
//! 2. Reproducir ese mismo turno contra el arnés en Rust.
//!
//! El guion se lee de `--guion <ruta>` o de `ORUSCRIBE_FAKE_PROVIDER_GUION`.
//! Cada petición consume la siguiente respuesta del guion. Cuando el cliente
//! pide `"stream": true` se responde SSE (`data: {...}` y cierre `data: [DONE]`);
//! si no, una completion normal en JSON.
//!
//! Uso: fake-provider --guion <ruta> [--puerto 8787] [--registro <ruta>]

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use axum::{
    Json, Router,
    body::Bytes,
    extract::State,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use serde::Deserialize;
use serde_json::{Value, json};

const PUERTO_POR_DEFECTO: u16 = 8787;
const VARIABLE_GUION: &str = "ORUSCRIBE_FAKE_PROVIDER_GUION";

/// El guion completo. Cada elemento de `respuestas` es lo que devuelve una
/// petición, en orden; el cliente que hace un bucle de herramientas hace una
/// petición por vuelta.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Guion {
    respuestas: Vec<Respuesta>,
    /// Uso por defecto cuando la respuesta no trae el suyo.
    #[serde(default)]
    uso: Uso,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Respuesta {
    /// Trozos de texto, cada uno un evento `delta` distinto.
    #[serde(default)]
    texto: Vec<String>,
    /// Llamada a herramienta que acompaña al texto, si hay.
    #[serde(default)]
    herramienta: Option<Herramienta>,
    /// `finish_reason` del último evento. Por defecto `stop`.
    #[serde(default = "final_por_defecto")]
    #[serde(rename = "final")]
    motivo_final: String,
    /// Uso que reporta esta respuesta; si falta, se usa el del guion.
    #[serde(default)]
    uso: Option<Uso>,
}

fn final_por_defecto() -> String {
    "stop".to_string()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Herramienta {
    /// Id de la llamada. Si falta, el cliente inventa `call_N`.
    #[serde(default)]
    id: Option<String>,
    nombre: String,
    /// Argumentos como objeto JSON; se serializan a la cadena que espera el
    /// protocolo (`function.arguments` es una cadena, no un objeto).
    argumentos: Value,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Uso {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
}

struct Estado {
    guion: Guion,
    /// Índice de la respuesta que toca. Se consume una por petición.
    siguiente: Mutex<usize>,
    /// Si está, cada cuerpo de petición se agrega como una línea JSON.
    registro: Option<PathBuf>,
}

struct Opciones {
    guion: PathBuf,
    puerto: u16,
    registro: Option<PathBuf>,
}

#[tokio::main]
async fn main() {
    let opciones = match leer_opciones() {
        Ok(opciones) => opciones,
        Err(motivo) => {
            eprintln!("{motivo}");
            eprintln!("uso: fake-provider --guion <ruta> [--puerto 8787] [--registro <ruta>]");
            std::process::exit(2);
        }
    };
    let guion = match cargar_guion(&opciones.guion) {
        Ok(guion) => guion,
        Err(motivo) => {
            eprintln!("{motivo}");
            std::process::exit(2);
        }
    };
    if let Some(registro) = &opciones.registro
        && let Some(carpeta) = registro.parent()
        && let Err(error) = std::fs::create_dir_all(carpeta)
    {
        eprintln!(
            "no pude crear la carpeta del registro {}: {error}",
            carpeta.display()
        );
        std::process::exit(2);
    }

    let estado = Arc::new(Estado {
        guion,
        siguiente: Mutex::new(0),
        registro: opciones.registro,
    });
    let app = Router::new()
        .route("/v1/chat/completions", post(completions))
        .with_state(estado);
    let direccion = format!("127.0.0.1:{}", opciones.puerto);

    let listener = match tokio::net::TcpListener::bind(&direccion).await {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("no pude escuchar en {direccion}: {error}");
            std::process::exit(1);
        }
    };
    println!(
        "proveedor falso escuchando en http://{direccion}/v1 (guion: {})",
        opciones.guion.display()
    );

    if let Err(error) = axum::serve(listener, app).await {
        eprintln!("el servidor se cayó: {error}");
        std::process::exit(1);
    }
}

fn leer_opciones() -> Result<Opciones, String> {
    let mut guion = std::env::var_os(VARIABLE_GUION).map(PathBuf::from);
    let mut puerto = PUERTO_POR_DEFECTO;
    let mut registro = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--guion" => {
                guion = Some(PathBuf::from(
                    args.next().ok_or("falta el valor de --guion")?,
                ));
            }
            "--puerto" => {
                let valor = args.next().ok_or("falta el valor de --puerto")?;
                puerto = valor
                    .parse()
                    .map_err(|_| format!("puerto inválido: {valor}"))?;
            }
            "--registro" => {
                registro = Some(PathBuf::from(
                    args.next().ok_or("falta el valor de --registro")?,
                ));
            }
            otro => return Err(format!("argumento desconocido: {otro}")),
        }
    }
    let guion = guion
        .ok_or_else(|| format!("falta el guion: pasa --guion <ruta> o exporta {VARIABLE_GUION}"))?;
    Ok(Opciones {
        guion,
        puerto,
        registro,
    })
}

fn cargar_guion(ruta: &Path) -> Result<Guion, String> {
    let texto = std::fs::read_to_string(ruta)
        .map_err(|error| format!("no pude leer {}: {error}", ruta.display()))?;
    let guion: Guion = serde_json::from_str(&texto)
        .map_err(|error| format!("{} no es un guion válido: {error}", ruta.display()))?;
    if guion.respuestas.is_empty() {
        return Err(format!(
            "{} no tiene respuestas: el guion necesita al menos una",
            ruta.display()
        ));
    }
    for (numero, respuesta) in guion.respuestas.iter().enumerate() {
        if let Some(herramienta) = &respuesta.herramienta
            && herramienta.nombre.trim().is_empty()
        {
            return Err(format!(
                "la respuesta {} tiene una herramienta sin nombre",
                numero + 1
            ));
        }
    }
    Ok(guion)
}

/// Responde una petición de chat con la siguiente respuesta del guion.
async fn completions(State(estado): State<Arc<Estado>>, cuerpo: Bytes) -> Response {
    let peticion: Value = match serde_json::from_slice(&cuerpo) {
        Ok(valor) => valor,
        Err(error) => {
            return error_json(
                StatusCode::BAD_REQUEST,
                format!("el cuerpo no es JSON: {error}"),
            );
        }
    };
    if let Some(ruta) = &estado.registro
        && let Err(error) = registrar(ruta, &cuerpo)
    {
        eprintln!("no pude escribir el registro {}: {error}", ruta.display());
    }

    let modelo = peticion
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("fake")
        .to_string();
    let stream = peticion
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mensajes = peticion
        .get("messages")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let herramientas = peticion
        .get("tools")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);

    let (numero, respuesta) = {
        let mut siguiente = match estado.siguiente.lock() {
            Ok(guardia) => guardia,
            Err(veneno) => veneno.into_inner(),
        };
        let numero = *siguiente;
        let Some(respuesta) = estado.guion.respuestas.get(numero).cloned() else {
            return error_json(
                StatusCode::BAD_REQUEST,
                format!(
                    "el guion se agotó: llegó la petición {} y solo hay {} respuestas",
                    numero + 1,
                    estado.guion.respuestas.len()
                ),
            );
        };
        *siguiente += 1;
        (numero, respuesta)
    };

    eprintln!(
        "peticion {}: modelo={modelo} stream={stream} mensajes={mensajes} herramientas={herramientas}",
        numero + 1
    );

    let uso = respuesta.uso.unwrap_or(estado.guion.uso);
    let id = format!("chatcmpl-fake-{}", numero + 1);
    if stream {
        let cuerpo = cuerpo_stream(&respuesta, uso, &modelo, &id);
        (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/event-stream")],
            cuerpo,
        )
            .into_response()
    } else {
        let cuerpo = cuerpo_json(&respuesta, uso, &modelo, &id);
        (StatusCode::OK, Json(cuerpo)).into_response()
    }
}

fn registrar(ruta: &Path, cuerpo: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut archivo = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(ruta)?;
    archivo.write_all(cuerpo)?;
    archivo.write_all(b"\n")
}

fn argumentos_cadena(herramienta: &Herramienta) -> String {
    serde_json::to_string(&herramienta.argumentos).unwrap_or_else(|_| "{}".to_string())
}

/// Cuerpo SSE con el formato que lee `OpenAICompat.leerStream`
/// (`internal/agent/openai.go:235-305`): `data: {...}` por evento, un chunk de
/// texto por trozo, la herramienta partida en nombre y argumentos (el cliente
/// los concatena por índice), un chunk de cierre, uno de uso y `data: [DONE]`.
fn cuerpo_stream(respuesta: &Respuesta, uso: Uso, modelo: &str, id: &str) -> String {
    let mut cuerpo = String::new();
    for trozo in &respuesta.texto {
        let evento = json!({
            "id": id,
            "object": "chat.completion.chunk",
            "created": 0,
            "model": modelo,
            "choices": [{
                "index": 0,
                "delta": { "content": trozo },
                "finish_reason": null
            }]
        });
        cuerpo.push_str(&sse(&evento));
    }
    if let Some(herramienta) = &respuesta.herramienta {
        let nombre = json!({
            "id": id,
            "object": "chat.completion.chunk",
            "created": 0,
            "model": modelo,
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": herramienta.id.clone().unwrap_or_default(),
                        "type": "function",
                        "function": {
                            "name": herramienta.nombre.clone(),
                            "arguments": ""
                        }
                    }]
                },
                "finish_reason": null
            }]
        });
        cuerpo.push_str(&sse(&nombre));
        let argumentos = json!({
            "id": id,
            "object": "chat.completion.chunk",
            "created": 0,
            "model": modelo,
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "function": { "arguments": argumentos_cadena(herramienta) }
                    }]
                },
                "finish_reason": null
            }]
        });
        cuerpo.push_str(&sse(&argumentos));
    }
    let cierre = json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": 0,
        "model": modelo,
        "choices": [{
            "index": 0,
            "delta": {},
            "finish_reason": respuesta.motivo_final.clone()
        }]
    });
    cuerpo.push_str(&sse(&cierre));
    // Con `stream_options.include_usage` el uso llega en un chunk propio y
    // `choices` va vacío (openai.go:279-282).
    let uso_evento = json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": 0,
        "model": modelo,
        "choices": [],
        "usage": {
            "prompt_tokens": uso.prompt_tokens,
            "completion_tokens": uso.completion_tokens,
            "total_tokens": uso.prompt_tokens + uso.completion_tokens
        }
    });
    cuerpo.push_str(&sse(&uso_evento));
    cuerpo.push_str("data: [DONE]\n\n");
    cuerpo
}

fn sse(evento: &Value) -> String {
    format!("data: {evento}\n\n")
}

/// Completion sin streaming, en el formato de `oaiRespuesta`
/// (`internal/agent/openai.go:93-105`).
fn cuerpo_json(respuesta: &Respuesta, uso: Uso, modelo: &str, id: &str) -> Value {
    let texto: String = respuesta.texto.concat();
    let mut mensaje = json!({ "role": "assistant", "content": texto });
    if let Some(herramienta) = &respuesta.herramienta {
        mensaje["tool_calls"] = json!([{
            "id": herramienta.id.clone().unwrap_or_default(),
            "type": "function",
            "function": {
                "name": herramienta.nombre.clone(),
                "arguments": argumentos_cadena(herramienta)
            }
        }]);
    }
    json!({
        "id": id,
        "object": "chat.completion",
        "created": 0,
        "model": modelo,
        "choices": [{
            "index": 0,
            "message": mensaje,
            "finish_reason": respuesta.motivo_final.clone()
        }],
        "usage": {
            "prompt_tokens": uso.prompt_tokens,
            "completion_tokens": uso.completion_tokens,
            "total_tokens": uso.prompt_tokens + uso.completion_tokens
        }
    })
}

fn error_json(estado: StatusCode, mensaje: String) -> Response {
    (estado, Json(json!({ "error": { "message": mensaje } }))).into_response()
}

#[cfg(test)]
mod pruebas {
    use super::*;

    fn respuesta(texto: &[&str], herramienta: Option<Herramienta>) -> Respuesta {
        Respuesta {
            texto: texto.iter().map(|t| t.to_string()).collect(),
            herramienta,
            motivo_final: "stop".to_string(),
            uso: None,
        }
    }

    fn lineas_sse(cuerpo: &str) -> Vec<Value> {
        cuerpo
            .lines()
            .filter_map(|linea| linea.strip_prefix("data: "))
            .filter(|dato| *dato != "[DONE]")
            .map(|dato| serde_json::from_str(dato).expect("cada dato es JSON"))
            .collect()
    }

    #[test]
    fn el_stream_termina_con_el_marcador_y_arma_la_herramienta() {
        let herramienta = Herramienta {
            id: Some("call_1".to_string()),
            nombre: "local_read_file".to_string(),
            argumentos: json!({ "path": "C:\\x\\README.md" }),
        };
        let cuerpo = cuerpo_stream(
            &respuesta(&["Miro. ", "Listo."], Some(herramienta)),
            Uso {
                prompt_tokens: 10,
                completion_tokens: 4,
            },
            "fake",
            "chatcmpl-fake-1",
        );
        assert!(cuerpo.ends_with("data: [DONE]\n\n"));
        let eventos = lineas_sse(&cuerpo);
        // dos textos + nombre de herramienta + argumentos + cierre + uso
        assert_eq!(eventos.len(), 6);
        let textos: Vec<&str> = eventos
            .iter()
            .filter_map(|e| e["choices"][0]["delta"]["content"].as_str())
            .collect();
        assert_eq!(textos, vec!["Miro. ", "Listo."]);
        let nombre = eventos
            .iter()
            .find_map(|e| e["choices"][0]["delta"]["tool_calls"][0]["function"]["name"].as_str());
        assert_eq!(nombre, Some("local_read_file"));
        let argumentos = eventos
            .iter()
            .find_map(|e| {
                e["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"]
                    .as_str()
                    .filter(|cadena| !cadena.is_empty())
            })
            .expect("los argumentos llegan como cadena");
        assert_eq!(
            serde_json::from_str::<Value>(argumentos).expect("argumentos JSON"),
            json!({ "path": "C:\\x\\README.md" })
        );
        assert_eq!(eventos[4]["choices"][0]["finish_reason"], "stop");
        assert_eq!(eventos[5]["usage"]["prompt_tokens"], 10);
        assert_eq!(eventos[5]["choices"].as_array().map(Vec::len), Some(0));
    }

    #[test]
    fn sin_stream_la_herramienta_va_en_el_mensaje() {
        let herramienta = Herramienta {
            id: None,
            nombre: "local_read_file".to_string(),
            argumentos: json!({ "path": "C:\\x\\README.md" }),
        };
        let cuerpo = cuerpo_json(
            &respuesta(&["Miro."], Some(herramienta)),
            Uso {
                prompt_tokens: 7,
                completion_tokens: 2,
            },
            "fake",
            "chatcmpl-fake-2",
        );
        assert_eq!(cuerpo["choices"][0]["message"]["content"], "Miro.");
        assert_eq!(
            cuerpo["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
            "local_read_file"
        );
        assert_eq!(cuerpo["usage"]["completion_tokens"], 2);
    }
}
