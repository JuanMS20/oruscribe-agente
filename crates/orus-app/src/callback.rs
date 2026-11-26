//! Callback loopback del inicio de sesión OAuth.
//!
//! Sirve exactamente una respuesta
//! en `/oauth_callback`, sin framework HTTP y sin bloquear la cancelación ni el
//! vencimiento del flujo.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use orus_auth::CALLBACK_PATH;

/// Lo que el navegador puede tardar en mandar la cabeza de su petición
const ESPERA_CABEZA: Duration = Duration::from_secs(5);
/// La espera entre `accept` no bloqueantes para mirar el cancel.
const ESPERA_ACEPTAR: Duration = Duration::from_millis(10);
/// Una cabeza HTTP más grande que esto no es un callback.
const LIMITE_CABEZA: usize = 16 * 1024;

/// El callback falló antes de entregar un código.
#[derive(Debug, thiserror::Error)]
pub enum LoginError {
    #[error("el servidor rechazó el acceso: {error} {description}")]
    Rejected { error: String, description: String },
    #[error("el estado de la respuesta no coincide")]
    StateMismatch,
    #[error("la respuesta no trae código")]
    NoCode,
    #[error("el inicio de sesión se canceló")]
    Cancelled,
    #[error("el inicio de sesión venció; vuelve a intentarlo")]
    TimedOut,
    #[error("no se pudo leer la respuesta del navegador: {0}")]
    Io(#[from] std::io::Error),
}

/// Sirve exactamente un callback y devuelve el código.
pub fn wait(
    listener: TcpListener,
    expected_state: &str,
    cancel: &AtomicBool,
    timeout: Duration,
) -> Result<String, LoginError> {
    listener.set_nonblocking(true)?;
    let inicio = Instant::now();
    while inicio.elapsed() < timeout {
        if cancel.load(Ordering::SeqCst) {
            return Err(LoginError::Cancelled);
        }
        match listener.accept() {
            Ok((mut flujo, _)) => {
                // El navegador puede abrir conexiones que no son el callback
                // (favicon, por ejemplo): se atienden y se sigue esperando.
                if let Some(resultado) =
                    atender(&mut flujo, expected_state, inicio + timeout, cancel)
                {
                    return resultado;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(ESPERA_ACEPTAR);
            }
            Err(error) => return Err(LoginError::Io(error)),
        }
    }
    Err(LoginError::TimedOut)
}

/// Devuelve `None` cuando la petición no era el callback y hay que seguir.
fn atender(
    flujo: &mut TcpStream,
    esperado: &str,
    deadline: Instant,
    cancel: &AtomicBool,
) -> Option<Result<String, LoginError>> {
    let limite_cabeza = deadline.min(Instant::now() + ESPERA_CABEZA);
    let mut cabeza = Vec::new();
    let mut trozo = [0u8; 1024];
    loop {
        if cancel.load(Ordering::SeqCst) || Instant::now() >= limite_cabeza {
            return None;
        }
        let _ = flujo.set_read_timeout(Some(ESPERA_ACEPTAR));
        match flujo.read(&mut trozo) {
            Ok(0) => return None,
            Ok(n) => cabeza.extend_from_slice(&trozo[..n]),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => return None,
        }
        if cabeza.windows(4).any(|v| v == b"\r\n\r\n") || cabeza.windows(2).any(|v| v == b"\n\n") {
            break;
        }
        if cabeza.len() > LIMITE_CABEZA {
            let _ = responder_texto(
                flujo,
                "400 Bad Request",
                "text/plain; charset=utf-8",
                "400 Bad Request\n",
            );
            return None;
        }
    }
    let cabeza = String::from_utf8_lossy(&cabeza);

    let primera = cabeza.lines().next().unwrap_or_default();
    let mut partes = primera.split_whitespace();
    let metodo = partes.next().unwrap_or_default();
    let objetivo = partes.next().unwrap_or_default();
    if metodo != "GET" {
        let _ = responder_texto(
            flujo,
            "405 Method Not Allowed",
            "text/plain; charset=utf-8",
            "405 Method Not Allowed\n",
        );
        return None;
    }
    let (ruta, consulta) = objetivo.split_once('?').unwrap_or((objetivo, ""));
    if ruta != CALLBACK_PATH {
        let _ = responder_texto(
            flujo,
            "404 Not Found",
            "text/plain; charset=utf-8",
            "404 page not found\n",
        );
        return None;
    }

    let pares = parametros(consulta);
    if !comparar_constante(valor(&pares, "state").as_bytes(), esperado.as_bytes()) {
        let _ = responder(flujo, false);
        return None;
    }
    let error = valor(&pares, "error");
    if !error.is_empty() {
        let descripcion = valor(&pares, "error_description");
        let _ = responder(flujo, false);
        return Some(Err(LoginError::Rejected {
            error: error.to_owned(),
            description: descripcion.to_owned(),
        }));
    }
    let codigo = valor(&pares, "code");
    if codigo.is_empty() {
        let _ = responder(flujo, false);
        return Some(Err(LoginError::NoCode));
    }
    let _ = responder(flujo, true);
    Some(Ok(codigo.to_owned()))
}

fn responder(flujo: &mut TcpStream, ok: bool) -> std::io::Result<()> {
    let cuerpo = page(ok);
    let estado = if ok { "200 OK" } else { "400 Bad Request" };
    responder_texto(flujo, estado, "text/html; charset=utf-8", &cuerpo)
}

fn responder_texto(
    flujo: &mut TcpStream,
    estado: &str,
    tipo: &str,
    cuerpo: &str,
) -> std::io::Result<()> {
    let respuesta = format!(
        "HTTP/1.1 {estado}\r\nContent-Type: {tipo}\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{cuerpo}",
        cuerpo.len()
    );
    flujo.write_all(respuesta.as_bytes())
}

fn parametros(consulta: &str) -> Vec<(String, String)> {
    consulta
        .split('&')
        .filter(|par| !par.is_empty())
        .map(|par| {
            let (clave, valor) = par.split_once('=').unwrap_or((par, ""));
            (decodificar(clave), decodificar(valor))
        })
        .collect()
}

fn valor<'a>(pares: &'a [(String, String)], clave: &str) -> &'a str {
    pares
        .iter()
        .find(|(candidata, _)| candidata == clave)
        .map(|(_, valor)| valor.as_str())
        .unwrap_or_default()
}

/// `url.QueryUnescape` para el subconjunto que produce el servidor OAuth.
fn decodificar(texto: &str) -> String {
    let bytes = texto.as_bytes();
    let mut salida = Vec::with_capacity(bytes.len());
    let mut indice = 0;
    while indice < bytes.len() {
        match bytes[indice] {
            b'+' => {
                salida.push(b' ');
                indice += 1;
            }
            b'%' if indice + 2 < bytes.len() => {
                match (hex(bytes[indice + 1]), hex(bytes[indice + 2])) {
                    (Some(alto), Some(bajo)) => {
                        salida.push(alto * 16 + bajo);
                        indice += 3;
                    }
                    _ => {
                        salida.push(b'%');
                        indice += 1;
                    }
                }
            }
            byte => {
                salida.push(byte);
                indice += 1;
            }
        }
    }
    String::from_utf8_lossy(&salida).into_owned()
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Comparación en tiempo constante.
fn comparar_constante(izquierda: &[u8], derecha: &[u8]) -> bool {
    if izquierda.len() != derecha.len() {
        return false;
    }
    let mut diferencia = 0u8;
    for (a, b) in izquierda.iter().zip(derecha) {
        diferencia |= a ^ b;
    }
    diferencia == 0
}

/// La página bilingüe exacta.
fn page(ok: bool) -> String {
    let (mensaje, mensaje_en) = if ok {
        (
            "Ya puedes cerrar esta pestaña y volver a Oruscribe Agente.",
            "You can close this tab and go back to Oruscribe Agente.",
        )
    } else {
        (
            "No se pudo completar el inicio de sesión. Vuelve a Oruscribe Agente e inténtalo otra vez.",
            "Sign-in could not be completed. Go back to Oruscribe Agente and try again.",
        )
    };
    format!(
        "<!doctype html><html lang=\"es\"><head><meta charset=\"utf-8\"><title>Oruscribe</title>\n<style>body{{font:15px/1.5 system-ui,sans-serif;background:#0b0c10;color:#e8eaf0;display:grid;place-items:center;height:100vh;margin:0}}\nmain{{max-width:34rem;padding:2rem;text-align:center}}p.en{{color:#9aa2b1}}</style></head>\n<body><main><p>{mensaje}</p><p class=\"en\">{mensaje_en}</p></main></body></html>"
    )
}
