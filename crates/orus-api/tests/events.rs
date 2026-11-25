//! Hub de eventos y long poll de `GET /api/events`
//! (`internal/app/watch.go:25-74`).

mod soporte;

use std::sync::Arc;
use std::time::Duration;

use orus_api::Api;
use orus_api::events::Hub;
use orus_app::{App, Event};
use serde_json::Value;
use soporte::{AppDePrueba, afirmar_cabeceras, levantar_api};
use tokio::sync::mpsc::error::TryRecvError;

fn app_logged_out() -> App {
    AppDePrueba::nuevo().construir()
}

/// Un `Event` con el `at` fijo, construido por serde para no depender de chrono.
fn evento(open: i64) -> Event {
    serde_json::from_value(serde_json::json!({
        "kind": "ping",
        "at": "2026-01-02T03:04:05Z",
        "open": open,
        "chat": 0,
    }))
    .unwrap()
}

/// Espera a que el hub tenga `esperados` suscriptores, sin dormir.
async fn esperar_suscriptores(hub: &Hub, esperados: usize) {
    let condicion = async {
        while hub.len() != esperados {
            tokio::task::yield_now().await;
        }
    };
    tokio::time::timeout(Duration::from_secs(5), condicion)
        .await
        .unwrap_or_else(|_| {
            panic!(
                "el hub no llegó a {esperados} suscriptores; quedan {}",
                hub.len()
            )
        });
}

#[tokio::test]
async fn hub_descarta_el_evento_que_no_cabe_y_no_cierra_el_canal() {
    let hub = Hub::new();
    let (id, mut rx) = hub.subscribe();
    assert_eq!(hub.len(), 1);

    for open in 0..9 {
        hub.publish(evento(open));
    }

    for open in 0..8 {
        let recibido = rx.try_recv().unwrap();
        assert_eq!(recibido.open, open, "llegó un evento fuera de orden");
    }
    assert_eq!(
        rx.try_recv().unwrap_err(),
        TryRecvError::Empty,
        "el noveno evento entró o el canal se cerró"
    );

    hub.unsubscribe(id);
    assert_eq!(hub.len(), 0);
}

#[tokio::test]
async fn eventos_entrega_el_siguiente_evento() {
    let hub = Arc::new(Hub::new());
    let api = Api::with_hub(Arc::new(app_logged_out()), Arc::clone(&hub))
        .with_wait(Duration::from_millis(50));
    let (base, server) = levantar_api(api).await;

    let peticion = tokio::spawn(reqwest::get(format!("{base}/api/events")));
    esperar_suscriptores(&hub, 1).await;

    let esperado = evento(3);
    hub.publish(esperado.clone());

    let response = peticion.await.unwrap().unwrap();
    assert_eq!(response.status(), 200);
    afirmar_cabeceras(&response);
    let texto = response.text().await.unwrap();
    assert!(
        texto.ends_with('\n'),
        "el cuerpo no termina en \\n: {texto:?}"
    );
    let body: Value = serde_json::from_str(&texto).unwrap();
    assert_eq!(body, serde_json::to_value(&esperado).unwrap());
    esperar_suscriptores(&hub, 0).await;

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn eventos_responde_el_silencio_al_vencer() {
    let hub = Arc::new(Hub::new());
    let api = Api::with_hub(Arc::new(app_logged_out()), Arc::clone(&hub))
        .with_wait(Duration::from_millis(50));
    let (base, server) = levantar_api(api).await;

    let response = reqwest::get(format!("{base}/api/events")).await.unwrap();
    assert_eq!(response.status(), 200);
    afirmar_cabeceras(&response);
    assert_eq!(
        response.text().await.unwrap(),
        "{\"kind\":\"silencio\",\"at\":\"0001-01-01T00:00:00Z\",\"open\":0,\"chat\":0}\n"
    );
    assert_eq!(hub.len(), 0);

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn cliente_que_corta_deja_el_hub_en_cero() {
    let hub = Arc::new(Hub::new());
    let api = Api::with_hub(Arc::new(app_logged_out()), Arc::clone(&hub))
        .with_wait(Duration::from_secs(10));
    let (base, server) = levantar_api(api).await;

    let peticion = tokio::spawn(reqwest::get(format!("{base}/api/events")));
    esperar_suscriptores(&hub, 1).await;
    peticion.abort();
    let _ = peticion.await;
    esperar_suscriptores(&hub, 0).await;

    server.abort();
    let _ = server.await;
}
