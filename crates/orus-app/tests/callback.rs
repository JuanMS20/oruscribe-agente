//! Contrato del callback loopback.
//!
//! Todo corre contra un `TcpListener` local; no hay navegador ni red real.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use orus_app::{CALLBACK_PATH, LoginError, wait};

/// Hace un GET y devuelve (estado, cabeceras, cuerpo).
fn pedir(url: &str) -> (u16, String, String) {
    let resto = url.strip_prefix("http://").expect("URL loopback");
    let (host, ruta) = resto.split_once('/').expect("host y ruta");
    let mut flujo = TcpStream::connect(host).expect("conecta al callback");
    flujo
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("tiempo de lectura");
    write!(
        flujo,
        "GET /{ruta} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
    )
    .expect("escribe la petición");
    flujo.flush().expect("manda la petición");
    let mut respuesta = String::new();
    flujo
        .read_to_string(&mut respuesta)
        .expect("lee la respuesta");
    let (cabeza, cuerpo) = respuesta.split_once("\r\n\r\n").expect("cabeza y cuerpo");
    let estado = cabeza
        .lines()
        .next()
        .and_then(|linea| linea.split_whitespace().nth(1))
        .and_then(|valor| valor.parse().ok())
        .expect("estado HTTP");
    (estado, cabeza.to_owned(), cuerpo.to_owned())
}

#[test]
fn callback_ignora_state_ajeno_y_acepta_el_legitimo() {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("puerto libre");
    let puerto = listener.local_addr().expect("dirección").port();
    let cancel = Arc::new(AtomicBool::new(false));
    let hilo = {
        let cancel = Arc::clone(&cancel);
        thread::spawn(move || wait(listener, "esperado", &cancel, Duration::from_secs(5)))
    };

    let (estado, _, _) = pedir(&format!(
        "http://127.0.0.1:{puerto}{CALLBACK_PATH}?state=otro&code=codigo"
    ));
    assert_eq!(estado, 400);
    let (estado, _, cuerpo) = pedir(&format!(
        "http://127.0.0.1:{puerto}{CALLBACK_PATH}?state=esperado&code=codigo"
    ));
    assert_eq!(estado, 200);
    assert!(cuerpo.contains("Ya puedes cerrar esta pestaña"));
    let codigo = hilo
        .join()
        .expect("el callback termina")
        .expect("el callback legítimo gana");
    assert_eq!(codigo, "codigo");
}

#[test]
fn callback_reports_server_error() {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("puerto libre");
    let puerto = listener.local_addr().expect("dirección").port();
    let cancel = Arc::new(AtomicBool::new(false));
    let hilo = {
        let cancel = Arc::clone(&cancel);
        thread::spawn(move || wait(listener, "esperado", &cancel, Duration::from_secs(5)))
    };

    let (estado, _, cuerpo) = pedir(&format!(
        "http://127.0.0.1:{puerto}{CALLBACK_PATH}?error=access_denied&error_description=el+usuario+rechaz%C3%B3&state=esperado"
    ));
    let error = hilo
        .join()
        .expect("el callback termina")
        .expect_err("el servidor rechazó");

    assert_eq!(estado, 400);
    assert!(cuerpo.contains("No se pudo completar el inicio de sesión."));
    assert!(matches!(error, LoginError::Rejected { .. }));
    assert_eq!(
        error.to_string(),
        "el servidor rechazó el acceso: access_denied el usuario rechazó"
    );
}

#[test]
fn cabecera_lenta_respeta_plazo_y_cancel() {
    for (nombre, cancelar) in [("plazo", false), ("cancel", true)] {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("puerto libre");
        let puerto = listener.local_addr().expect("dirección").port();
        let cancel = Arc::new(AtomicBool::new(false));
        let (termino, recibo) = mpsc::channel();
        let espera = Arc::clone(&cancel);
        thread::spawn(move || {
            let resultado = wait(listener, "esperado", &espera, Duration::from_millis(250));
            let _ = termino.send(resultado);
        });

        let mut flujo = TcpStream::connect(("127.0.0.1", puerto)).expect("conecta al callback");
        let inicio = Instant::now();
        while inicio.elapsed() < Duration::from_millis(600) {
            if flujo.write_all(b"X").and_then(|()| flujo.flush()).is_err() {
                break;
            }
            if cancelar && inicio.elapsed() > Duration::from_millis(100) {
                cancel.store(true, Ordering::SeqCst);
            }
            thread::sleep(Duration::from_millis(40));
        }
        let error = recibo
            .recv_timeout(Duration::from_secs(3))
            .expect("{nombre}: wait no respetó el plazo ni el cancel")
            .expect_err("{nombre}: la cabecera incompleta no es un callback");
        if cancelar {
            assert!(
                matches!(error, LoginError::Cancelled),
                "{nombre}: {error:?}"
            );
        } else {
            assert!(matches!(error, LoginError::TimedOut), "{nombre}: {error:?}");
        }
    }
}
