//! Cableado HTTP de `GET /api/agent/models`: consulta válida, caché y orden.

mod soporte;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::routing::get;
use orus_api::Api;
use soporte::AppDePrueba;

async fn catalogo() -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    let golpes = Arc::new(AtomicUsize::new(0));
    let contador = Arc::clone(&golpes);
    let app = Router::new().route(
        "/",
        get(move || {
            let contador = Arc::clone(&contador);
            async move {
                contador.fetch_add(1, Ordering::Relaxed);
                r#"{"openai":{"z-ultimo":{"id":"z-ultimo","name":"Zeta"},"a-primero":{"id":"a-primero","name":"Alfa"}}}"#
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener del catálogo");
    let direccion = listener.local_addr().expect("dirección local");
    let servidor = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("servidor del catálogo");
    });
    (format!("http://{direccion}"), golpes, servidor)
}

#[tokio::test]
async fn agent_models_consulta_por_http_y_usa_la_cache() {
    let (catalogo_url, golpes, catalogo_servidor) = catalogo().await;
    let app = Arc::new(AppDePrueba::nuevo().construir());
    let api = Api::new(app).with_models_url(catalogo_url);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener de la api");
    let direccion = listener.local_addr().expect("dirección local");
    let servidor = tokio::spawn(async move {
        axum::serve(listener, api.router()).await.expect("api");
    });

    let respuesta = reqwest::get(format!(
        "http://{direccion}/api/agent/models?provider=openai"
    ))
    .await
    .expect("GET models");
    let estado = respuesta.status();
    let texto = respuesta.text().await.expect("cuerpo");
    assert_eq!(estado, 200, "cuerpo: {texto}");
    let cuerpo: serde_json::Value = serde_json::from_str(&texto).expect("JSON");
    let modelos = cuerpo["models"].as_array().expect("lista de modelos");
    assert_eq!(modelos.len(), 2);
    assert_eq!(modelos[0]["id"], "a-primero");
    assert_eq!(modelos[1]["id"], "z-ultimo");
    assert_eq!(golpes.load(Ordering::Relaxed), 1);

    let otra = reqwest::get(format!(
        "http://{direccion}/api/agent/models?provider=openai"
    ))
    .await
    .expect("segundo GET models");
    assert_eq!(otra.status(), 200);
    assert_eq!(
        golpes.load(Ordering::Relaxed),
        1,
        "el catálogo se pide una sola vez por ventana de caché"
    );

    servidor.abort();
    catalogo_servidor.abort();
}

#[tokio::test]
async fn agent_models_acepta_catalogo_con_campos_ausentes_o_nulos() {
    let catalogo_app = Router::new().route(
        "/",
        get(|| async {
            r#"{"openai":{"sin-nombre":{"id":"sin-nombre"},"nulos":{"id":null,"name":null}}}"#
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener del catálogo");
    let direccion = listener.local_addr().expect("dirección local");
    let catalogo_servidor = tokio::spawn(async move {
        axum::serve(listener, catalogo_app)
            .await
            .expect("servidor del catálogo");
    });
    let app = Arc::new(AppDePrueba::nuevo().construir());
    let api = Api::new(app).with_models_url(format!("http://{direccion}"));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener de la api");
    let addr = listener.local_addr().expect("dirección local");
    let servidor = tokio::spawn(async move {
        axum::serve(listener, api.router()).await.expect("api");
    });

    let respuesta = reqwest::get(format!("http://{addr}/api/agent/models?provider=openai"))
        .await
        .expect("GET models");
    let estado = respuesta.status();
    let texto = respuesta.text().await.expect("cuerpo");
    assert_eq!(estado, 200, "cuerpo: {texto}");
    let cuerpo: serde_json::Value = serde_json::from_str(&texto).expect("JSON");
    let modelos = cuerpo["models"].as_array().expect("lista de modelos");
    assert_eq!(modelos.len(), 2, "{cuerpo}");
    assert_eq!(modelos[0]["id"], "nulos");
    assert_eq!(modelos[1]["id"], "sin-nombre");
    assert!(modelos[0].get("name").is_none());
    assert!(modelos[1].get("name").is_none());

    servidor.abort();
    catalogo_servidor.abort();
}
