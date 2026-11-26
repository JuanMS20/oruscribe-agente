//! Subidas multipart de tareas y chat.
//!
//! El backend de
//! prueba anota lo que recibe; no hay red real, llavero ni variables de
//! entorno. El caso de 25 MB envía los bytes por loopback sin tocar el disco.

mod soporte;

use std::sync::Arc;

use orus_app::App;
use serde_json::Value;
use soporte::{
    AppDePrueba, BackendDePrueba, ConSesion, ConectorCompartido, afirmar_cabeceras, cuerpo_json,
    levantar,
};

const TOPE_ARCHIVO: usize = 25 * 1024 * 1024;

fn app_con(backend: Arc<BackendDePrueba>) -> App {
    AppDePrueba::nuevo()
        .con_store(Box::new(ConSesion))
        .con_connector(Arc::new(ConectorCompartido(backend)))
        .construir()
}

async fn enviar(base: &str, ruta: &str, formulario: reqwest::multipart::Form) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{base}{ruta}"))
        .multipart(formulario)
        .send()
        .await
        .expect("petición")
}

fn archivo(datos: &[u8], nombre: &str, mime: &str) -> reqwest::multipart::Part {
    reqwest::multipart::Part::bytes(datos.to_vec())
        .file_name(nombre.to_owned())
        .mime_str(mime)
        .expect("mime de prueba")
}

/// Afirma código, cabeceras y `\n`, y devuelve el JSON.
async fn afirmar_ok(response: reqwest::Response) -> Value {
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    afirmar_cabeceras(&response);
    cuerpo_json(response).await
}

async fn afirmar_error(response: reqwest::Response, esperado: u16) -> Value {
    assert_eq!(
        response.status(),
        reqwest::StatusCode::from_u16(esperado).expect("código")
    );
    cuerpo_json(response).await
}

#[tokio::test]
async fn tareas_suben_el_archivo_con_su_nombre_y_nota() {
    let backend = Arc::new(BackendDePrueba::default());
    let (base, _server) = levantar(app_con(Arc::clone(&backend))).await;

    // El archivo va primero: el orden de los campos no puede importar.
    let formulario = reqwest::multipart::Form::new()
        .part("file", archivo(b"hola", "carpeta/prueba.txt", "text/plain"))
        .text("task", "t1")
        .text("note", "avance inmune");

    let cuerpo = afirmar_ok(enviar(&base, "/api/tasks/files", formulario).await).await;
    assert_eq!(cuerpo["update_id"], "u-1");

    let visto = backend.subida_avance.lock().expect("avance");
    let (task, nombre, mime, datos, nota) = visto.as_ref().expect("avance");
    assert_eq!(task, "t1");
    assert_eq!(nombre, "prueba.txt");
    assert_eq!(mime, "text/plain");
    assert_eq!(datos, b"hola");
    assert_eq!(nota, "avance inmune");
}

#[tokio::test]
async fn chat_sube_el_archivo_y_usa_el_tipo_por_defecto() {
    let backend = Arc::new(BackendDePrueba::default());
    let (base, _server) = levantar(app_con(Arc::clone(&backend))).await;

    // Sin `Content-Type` en la parte, el contrato manda `application/octet-stream`.
    let formulario = reqwest::multipart::Form::new()
        .part(
            "file",
            reqwest::multipart::Part::bytes(b"binario".to_vec()).file_name("b.bin"),
        )
        .text("room", "r1")
        .text("body", "adjunto inmune");

    let cuerpo = afirmar_ok(enviar(&base, "/api/chat/files", formulario).await).await;
    assert_eq!(cuerpo["ok"], true);

    let visto = backend.subida_chat.lock().expect("chat");
    let (sala, nombre, mime, datos, body) = visto.as_ref().expect("chat");
    assert_eq!(sala, "r1");
    assert_eq!(nombre, "b.bin");
    assert_eq!(mime, "application/octet-stream");
    assert_eq!(datos, b"binario");
    assert_eq!(body, "adjunto inmune");
}

#[tokio::test]
async fn falta_el_archivo_en_las_dos_rutas() {
    let backend = Arc::new(BackendDePrueba::default());
    let (base, _server) = levantar(app_con(Arc::clone(&backend))).await;

    let formulario = reqwest::multipart::Form::new().text("task", "t1");
    let cuerpo = afirmar_error(enviar(&base, "/api/tasks/files", formulario).await, 400).await;
    assert_eq!(cuerpo["error"], "falta el archivo");

    let formulario = reqwest::multipart::Form::new().text("room", "r1");
    let cuerpo = afirmar_error(enviar(&base, "/api/chat/files", formulario).await, 400).await;
    assert_eq!(cuerpo["error"], "falta el archivo");

    assert!(backend.subida_avance.lock().expect("avance").is_none());
    assert!(backend.subida_chat.lock().expect("chat").is_none());
}

#[tokio::test]
async fn chat_valida_la_sala_despues_del_archivo() {
    let backend = Arc::new(BackendDePrueba::default());
    let (base, _server) = levantar(app_con(Arc::clone(&backend))).await;

    // Sin archivo y sin sala gana el archivo, como `subida`.
    let formulario = reqwest::multipart::Form::new().text("body", "solo texto");
    let cuerpo = afirmar_error(enviar(&base, "/api/chat/files", formulario).await, 400).await;
    assert_eq!(cuerpo["error"], "falta el archivo");

    let formulario =
        reqwest::multipart::Form::new().part("file", archivo(b"hola", "a.txt", "text/plain"));
    let cuerpo = afirmar_error(enviar(&base, "/api/chat/files", formulario).await, 400).await;
    assert_eq!(cuerpo["error"], "falta la sala");

    assert!(
        backend.subida_chat.lock().expect("chat").is_none(),
        "el backend no debe llamarse"
    );
}

#[tokio::test]
async fn un_archivo_de_3_mb_pasa_del_tope_por_defecto_de_axum() {
    let backend = Arc::new(BackendDePrueba::default());
    let (base, _server) = levantar(app_con(Arc::clone(&backend))).await;

    // 3 MB supera el `DefaultBodyLimit` de 2 MB: si la ruta no lo desactivara,
    // la subida fallaría aunque el contrato la acepta sin problema.
    let datos = vec![b'x'; 3 * 1024 * 1024];
    let formulario = reqwest::multipart::Form::new()
        .part(
            "file",
            archivo(&datos, "tres.bin", "application/octet-stream"),
        )
        .text("task", "t1");

    let cuerpo = afirmar_ok(enviar(&base, "/api/tasks/files", formulario).await).await;
    assert_eq!(cuerpo["update_id"], "u-1");

    let visto = backend.subida_avance.lock().expect("avance");
    let (_, _, _, recibido, _) = visto.as_ref().expect("avance");
    assert_eq!(recibido.len(), datos.len());
}

#[tokio::test]
async fn un_archivo_sobre_25_mb_es_400_archivo_grande() {
    let backend = Arc::new(BackendDePrueba::default());
    let (base, _server) = levantar(app_con(Arc::clone(&backend))).await;

    // 25 MB + 1: supera el tope del archivo pero no el de la petición. Si el
    // `DefaultBodyLimit` de axum siguiera activo, esto sería un 413.
    let grande = vec![b'x'; TOPE_ARCHIVO + 1];
    let formulario = reqwest::multipart::Form::new()
        .part(
            "file",
            archivo(&grande, "grande.bin", "application/octet-stream"),
        )
        .text("task", "t1");

    let cuerpo = afirmar_error(enviar(&base, "/api/tasks/files", formulario).await, 400).await;
    assert_eq!(cuerpo["error"], "el archivo supera los 25 MB");
    assert_eq!(cuerpo["code"], "archivo_grande");
}

#[tokio::test]
async fn una_peticion_sobre_26_mb_es_400_archivo_grande() {
    let backend = Arc::new(BackendDePrueba::default());
    let (base, _server) = levantar(app_con(Arc::clone(&backend))).await;

    // El cuerpo completo pasa de 26 MB: el tope de la petición gana antes de
    // mirar el del archivo, y también es 400, nunca 413.
    let grande = vec![b'x'; 26 * 1024 * 1024 + 1];
    let formulario = reqwest::multipart::Form::new()
        .part(
            "file",
            archivo(&grande, "enorme.bin", "application/octet-stream"),
        )
        .text("task", "t1");

    let cuerpo = afirmar_error(enviar(&base, "/api/tasks/files", formulario).await, 400).await;
    assert_eq!(cuerpo["error"], "el archivo supera los 25 MB");
    assert_eq!(cuerpo["code"], "archivo_grande");
}

#[tokio::test]
async fn multipart_mal_formado_es_400_archivo_grande() {
    let backend = Arc::new(BackendDePrueba::default());
    let (base, _server) = levantar(app_con(Arc::clone(&backend))).await;

    // Cabecera de parte sin la línea de cierre: multer no puede terminarla.
    let response = reqwest::Client::new()
        .post(format!("{base}/api/tasks/files"))
        .header("content-type", "multipart/form-data; boundary=limite")
        .body(
            "--limite\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a.txt\"\r\n\r\nhola",
        )
        .send()
        .await
        .expect("petición");

    let cuerpo = afirmar_error(response, 400).await;
    assert_eq!(cuerpo["error"], "el archivo supera los 25 MB");
    assert_eq!(cuerpo["code"], "archivo_grande");
}

#[tokio::test]
async fn la_cuota_agotada_es_507_sin_espacio() {
    let backend = Arc::new(BackendDePrueba::sin_espacio());
    let (base, _server) = levantar(app_con(Arc::clone(&backend))).await;

    let formulario = reqwest::multipart::Form::new()
        .part("file", archivo(b"hola", "a.txt", "text/plain"))
        .text("task", "t1");
    let response = enviar(&base, "/api/tasks/files", formulario).await;

    let cuerpo = afirmar_error(response, 507).await;
    assert_eq!(
        cuerpo["error"],
        "la organización se quedó sin espacio para archivos: cuota llena"
    );
    assert_eq!(cuerpo["code"], "sin_espacio");
}
