//! Cableado HTTP de `POST /api/agent/pick-folder` con la costura inyectada.

mod soporte;

use std::path::PathBuf;
use std::sync::Arc;

use orus_api::{Api, PickFolder};
use soporte::AppDePrueba;

struct Carpeta(PathBuf);

impl PickFolder for Carpeta {
    fn elegir(&self) -> Result<Option<String>, String> {
        Ok(Some(format!(
            "{}{}",
            self.0.display(),
            std::path::MAIN_SEPARATOR
        )))
    }
}

struct Cancelada;

impl PickFolder for Cancelada {
    fn elegir(&self) -> Result<Option<String>, String> {
        Ok(None)
    }
}

#[tokio::test]
async fn pick_folder_con_costura_responde_200_y_limpia_la_ruta() {
    let carpeta = PathBuf::from("carpeta").join("elegida");
    let app = Arc::new(AppDePrueba::nuevo().construir());
    let api = Api::new(app).with_pick_folder(Arc::new(Carpeta(carpeta.clone())));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener de la api");
    let direccion = listener.local_addr().expect("dirección local");
    let servidor = tokio::spawn(async move {
        axum::serve(listener, api.router()).await.expect("api");
    });

    let respuesta = reqwest::Client::new()
        .post(format!("http://{direccion}/api/agent/pick-folder"))
        .send()
        .await
        .expect("POST pick-folder");
    assert_eq!(respuesta.status(), 200);
    let cuerpo: serde_json::Value = respuesta.json().await.expect("JSON");
    assert_eq!(cuerpo["path"], carpeta.display().to_string());

    servidor.abort();
}

#[tokio::test]
async fn pick_folder_cancelar_responde_200_sin_path() {
    let app = Arc::new(AppDePrueba::nuevo().construir());
    let api = Api::new(app).with_pick_folder(Arc::new(Cancelada));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener de la api");
    let direccion = listener.local_addr().expect("dirección local");
    let servidor = tokio::spawn(async move {
        axum::serve(listener, api.router()).await.expect("api");
    });

    let respuesta = reqwest::Client::new()
        .post(format!("http://{direccion}/api/agent/pick-folder"))
        .send()
        .await
        .expect("POST pick-folder");
    assert_eq!(respuesta.status(), 200);
    let cuerpo: serde_json::Value = respuesta.json().await.expect("JSON");
    assert_eq!(cuerpo, serde_json::json!({}), "cancelar no devuelve path");

    servidor.abort();
}

/// Levanta la api en un puerto efímero para los tests del cuerpo manual.
async fn levantar(api: Api) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener de la api");
    let direccion = listener.local_addr().expect("dirección local").to_string();
    let servidor = tokio::spawn(async move {
        axum::serve(listener, api.router()).await.expect("api");
    });
    (direccion, servidor)
}

/// Respaldo cross-platform (Linux): sin selector nativo, una ruta escrita a
/// mano en `body.path` se valida como carpeta permitida y se devuelve limpia.
#[tokio::test]
async fn pick_folder_path_manual_sin_selector_responde_200_y_limpia() {
    let carpeta = tempfile::tempdir().expect("carpeta temporal");
    let con_separador = format!("{}{}", carpeta.path().display(), std::path::MAIN_SEPARATOR);
    let app = Arc::new(AppDePrueba::nuevo().construir());
    let api = Api::new(app);
    let (direccion, servidor) = levantar(api).await;

    let respuesta = reqwest::Client::new()
        .post(format!("http://{direccion}/api/agent/pick-folder"))
        .json(&serde_json::json!({ "path": con_separador }))
        .send()
        .await
        .expect("POST pick-folder con path manual");
    assert_eq!(respuesta.status(), 200);
    let cuerpo: serde_json::Value = respuesta.json().await.expect("JSON");
    assert_eq!(cuerpo["path"], carpeta.path().display().to_string());

    servidor.abort();
}

/// Con selector nativo cableado, la ruta manual del cuerpo tiene prioridad:
/// es la que la persona escribió y el diálogo no debe abrirse.
#[tokio::test]
async fn pick_folder_path_manual_con_selector_lo_prefiere() {
    let carpeta = tempfile::tempdir().expect("carpeta temporal");
    let del_selector = PathBuf::from("carpeta").join("del_selector");
    let app = Arc::new(AppDePrueba::nuevo().construir());
    let api = Api::new(app).with_pick_folder(Arc::new(Carpeta(del_selector)));
    let (direccion, servidor) = levantar(api).await;

    let respuesta = reqwest::Client::new()
        .post(format!("http://{direccion}/api/agent/pick-folder"))
        .json(&serde_json::json!({ "path": carpeta.path().display().to_string() }))
        .send()
        .await
        .expect("POST pick-folder con path manual");
    assert_eq!(respuesta.status(), 200);
    let cuerpo: serde_json::Value = respuesta.json().await.expect("JSON");
    assert_eq!(cuerpo["path"], carpeta.path().display().to_string());

    servidor.abort();
}

/// La ruta manual se valida con la regla de las carpetas permitidas
/// (`carpetasPermitidas`): absoluta, existente y carpeta.
#[tokio::test]
async fn pick_folder_path_manual_relativo_es_400() {
    let app = Arc::new(AppDePrueba::nuevo().construir());
    let api = Api::new(app);
    let (direccion, servidor) = levantar(api).await;

    let respuesta = reqwest::Client::new()
        .post(format!("http://{direccion}/api/agent/pick-folder"))
        .json(&serde_json::json!({ "path": "carpeta/relativa" }))
        .send()
        .await
        .expect("POST pick-folder con path relativo");
    assert_eq!(respuesta.status(), 400);
    let cuerpo: serde_json::Value = respuesta.json().await.expect("JSON");
    assert_eq!(
        cuerpo["error"],
        "cada carpeta permitida tiene que ser una ruta absoluta"
    );

    servidor.abort();
}

#[tokio::test]
async fn pick_folder_path_manual_inexistente_es_400() {
    let base = tempfile::tempdir().expect("carpeta temporal");
    let inexistente = base.path().join("no-existe");
    let app = Arc::new(AppDePrueba::nuevo().construir());
    let api = Api::new(app);
    let (direccion, servidor) = levantar(api).await;

    let respuesta = reqwest::Client::new()
        .post(format!("http://{direccion}/api/agent/pick-folder"))
        .json(&serde_json::json!({ "path": inexistente.display().to_string() }))
        .send()
        .await
        .expect("POST pick-folder con path inexistente");
    assert_eq!(respuesta.status(), 400);
    let cuerpo: serde_json::Value = respuesta.json().await.expect("JSON");
    assert_eq!(
        cuerpo["error"],
        format!(
            "la carpeta no existe o no es una carpeta: {}",
            inexistente.display()
        )
    );

    servidor.abort();
}

#[tokio::test]
async fn pick_folder_path_manual_archivo_no_carpeta_es_400() {
    let base = tempfile::tempdir().expect("carpeta temporal");
    let archivo = base.path().join("archivo.txt");
    std::fs::write(&archivo, "no es una carpeta").expect("archivo de prueba");
    let app = Arc::new(AppDePrueba::nuevo().construir());
    let api = Api::new(app);
    let (direccion, servidor) = levantar(api).await;

    let respuesta = reqwest::Client::new()
        .post(format!("http://{direccion}/api/agent/pick-folder"))
        .json(&serde_json::json!({ "path": archivo.display().to_string() }))
        .send()
        .await
        .expect("POST pick-folder con path de archivo");
    assert_eq!(respuesta.status(), 400);
    let cuerpo: serde_json::Value = respuesta.json().await.expect("JSON");
    assert_eq!(
        cuerpo["error"],
        format!(
            "la carpeta no existe o no es una carpeta: {}",
            archivo.display()
        )
    );

    servidor.abort();
}

/// El cuerpo ahora se decodifica: un JSON inválido responde el error de
/// `decode` (500, como el writeErr de Go para json.Unmarshal) aunque haya
/// selector cableado; antes el cuerpo se ignoraba y el diálogo se abría.
#[tokio::test]
async fn pick_folder_body_invalido_con_selector_es_error() {
    let carpeta = PathBuf::from("carpeta").join("elegida");
    let app = Arc::new(AppDePrueba::nuevo().construir());
    let api = Api::new(app).with_pick_folder(Arc::new(Carpeta(carpeta)));
    let (direccion, servidor) = levantar(api).await;

    let respuesta = reqwest::Client::new()
        .post(format!("http://{direccion}/api/agent/pick-folder"))
        .header("content-type", "application/json")
        .body("esto no es json")
        .send()
        .await
        .expect("POST pick-folder con cuerpo inválido");
    assert_eq!(respuesta.status(), 500);

    servidor.abort();
}

/// `path` vacío o ausente cae al selector nativo: el contrato de Windows y
/// del servidor headless con `--pick-folder` no cambia.
#[tokio::test]
async fn pick_folder_path_vacio_con_selector_usa_el_selector() {
    let carpeta = PathBuf::from("carpeta").join("elegida");
    let app = Arc::new(AppDePrueba::nuevo().construir());
    let api = Api::new(app).with_pick_folder(Arc::new(Carpeta(carpeta.clone())));
    let (direccion, servidor) = levantar(api).await;

    let respuesta = reqwest::Client::new()
        .post(format!("http://{direccion}/api/agent/pick-folder"))
        .json(&serde_json::json!({ "path": "" }))
        .send()
        .await
        .expect("POST pick-folder con path vacío");
    assert_eq!(respuesta.status(), 200);
    let cuerpo: serde_json::Value = respuesta.json().await.expect("JSON");
    assert_eq!(cuerpo["path"], carpeta.display().to_string());

    servidor.abort();
}
