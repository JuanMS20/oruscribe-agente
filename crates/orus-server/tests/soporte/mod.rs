//! Arranque compartido de los tests del servidor headless.
//!
//! Usa el `App` real de `orus_server::api` sobre una carpeta temporal; no hay
//! backend falso.

use tokio::net::TcpListener;

/// Construye el `App` real sobre `datos`, escucha en `127.0.0.1:0` y devuelve
/// la dirección y la tarea que sirve hasta que el test la aborta.
pub async fn levantar(
    datos: &std::path::Path,
) -> (String, String, tokio::task::JoinHandle<std::io::Result<()>>) {
    temp_env::async_with_vars([("ORUSCRIBE_DATA_DIR", Some(datos.as_os_str()))], async {
        let api = orus_server::api(None).expect("el App real se construye sin sesión");
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener efímero");
        let addr = listener.local_addr().expect("dirección local").to_string();
        let guardia = orus_server::Guardia::nuevo(addr.clone());
        let token = guardia.token().to_owned();
        let server = tokio::spawn(orus_server::servir(listener, api, guardia));
        (addr, token, server)
    })
    .await
}
