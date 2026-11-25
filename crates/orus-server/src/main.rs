//! Binario headless: sirve la UI actual y `/api/*` sin ventana ni bandeja.
//!
//! Port de `cmd/immuneserver/main.go:18-55`. Solo acepta direcciones
//! loopback; `--data` existe para no tocar el perfil real en desarrollo.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::error::Error;
use std::io;

use tokio::net::TcpListener;

const DIRECCION_POR_DEFECTO: &str = "127.0.0.1:8098";

fn main() -> Result<(), Box<dyn Error>> {
    let mut direccion = DIRECCION_POR_DEFECTO.to_owned();
    let mut datos: Option<String> = None;
    let mut selector: Option<String> = None;

    let mut argumentos = std::env::args().skip(1);
    while let Some(argumento) = argumentos.next() {
        match argumento.as_str() {
            "--addr" => direccion = valor(&mut argumentos, "--addr")?,
            "--data" => datos = Some(valor(&mut argumentos, "--data")?),
            "--pick-folder" => selector = Some(valor(&mut argumentos, "--pick-folder")?),
            _ => {
                if let Some(valor) = argumento.strip_prefix("--addr=") {
                    direccion = valor.to_owned();
                } else if let Some(valor) = argumento.strip_prefix("--data=") {
                    datos = Some(valor.to_owned());
                } else if let Some(valor) = argumento.strip_prefix("--pick-folder=") {
                    selector = Some(valor.to_owned());
                } else {
                    return Err(error_invalido(&format!(
                        "argumento desconocido: {argumento}"
                    )));
                }
            }
        }
    }

    if let Some(directorio) = datos {
        // SAFETY: corre al arranque del proceso, antes de crear el runtime de
        // Tokio y de que exista otro hilo que lea el entorno. Nadie más toca
        // variables de entorno en este binario.
        unsafe {
            std::env::set_var("ORUSCRIBE_DATA_DIR", &directorio);
        }
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let api = orus_server::api(selector)?;
        // El `Boot()` de Go restaura la sesión guardada; sin esto el borde
        // headless también se queda en `connecting`.
        api.boot();
        let destino: std::net::SocketAddr = direccion.parse().map_err(|_| {
            error_invalido(&format!("--addr no es una dirección válida: {direccion}"))
        })?;
        if !destino.ip().is_loopback() {
            return Err(error_invalido("--addr solo admite direcciones loopback"));
        }
        let listener = TcpListener::bind(destino).await?;
        let escucha = listener.local_addr()?;
        let guardia = orus_server::Guardia::nuevo(escucha.to_string());
        println!("escuchando en http://{escucha}/?token={}", guardia.token());
        orus_server::servir(listener, api, guardia).await?;
        Ok::<(), Box<dyn Error>>(())
    })
}

fn valor(
    argumentos: &mut impl Iterator<Item = String>,
    nombre: &str,
) -> Result<String, Box<dyn Error>> {
    argumentos
        .next()
        .ok_or_else(|| error_invalido(&format!("{nombre} necesita un valor")))
}

fn error_invalido(texto: &str) -> Box<dyn Error> {
    Box::new(io::Error::new(io::ErrorKind::InvalidInput, texto))
}
