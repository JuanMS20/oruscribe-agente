//! `--pick-folder` cablea el selector del servidor headless.

use serial_test::serial;
use tokio::net::TcpListener;

async fn levantar(
    datos: &std::path::Path,
    selector: Option<String>,
) -> (String, String, tokio::task::JoinHandle<std::io::Result<()>>) {
    temp_env::async_with_vars([("ORUSCRIBE_DATA_DIR", Some(datos.as_os_str()))], async {
        let api = orus_server::api(selector).expect("el App real se construye sin sesión");
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

#[tokio::test]
#[serial]
async fn pick_folder_la_bandera_cablea_el_selector() {
    let datos = tempfile::tempdir().expect("carpeta temporal");
    let elegida = tempfile::tempdir().expect("carpeta elegida");
    let (addr, token, server) =
        levantar(datos.path(), Some(elegida.path().display().to_string())).await;

    let respuesta = reqwest::Client::new()
        .post(format!("http://{addr}/api/agent/pick-folder"))
        .header("cookie", format!("orus_token={token}"))
        .header("origin", format!("http://{addr}"))
        .send()
        .await
        .expect("POST pick-folder");
    assert_eq!(respuesta.status(), 200);
    let cuerpo: serde_json::Value = respuesta.json().await.expect("JSON");
    assert_eq!(cuerpo["path"], elegida.path().display().to_string());

    server.abort();
}

#[tokio::test]
#[serial]
async fn pick_folder_sin_bandera_responde_400() {
    let datos = tempfile::tempdir().expect("carpeta temporal");
    let (addr, token, server) = levantar(datos.path(), None).await;

    let respuesta = reqwest::Client::new()
        .post(format!("http://{addr}/api/agent/pick-folder"))
        .header("cookie", format!("orus_token={token}"))
        .header("origin", format!("http://{addr}"))
        .send()
        .await
        .expect("POST pick-folder");
    assert_eq!(respuesta.status(), 400);
    let cuerpo: serde_json::Value = respuesta.json().await.expect("JSON");
    assert_eq!(
        cuerpo["error"],
        "el selector de carpetas no está disponible aquí"
    );

    server.abort();
}

#[tokio::test]
#[serial]
async fn pick_folder_vacia_cuenta_como_sin_bandera() {
    let datos = tempfile::tempdir().expect("carpeta temporal");
    let (addr, token, server) = levantar(datos.path(), Some(String::new())).await;

    let respuesta = reqwest::Client::new()
        .post(format!("http://{addr}/api/agent/pick-folder"))
        .header("cookie", format!("orus_token={token}"))
        .header("origin", format!("http://{addr}"))
        .send()
        .await
        .expect("POST pick-folder");
    assert_eq!(respuesta.status(), 400);

    server.abort();
}
