//! Rutas de autenticación: mismo JSON, códigos y textos que el contrato.

mod soporte;

use std::time::Duration;

use soporte::{AppDePrueba, afirmar_cabeceras, cuerpo_json, levantar};

#[tokio::test]
async fn logout_responde_ok() {
    let (base, server) = levantar(AppDePrueba::nuevo().construir()).await;

    let response = reqwest::Client::new()
        .post(format!("{base}/api/auth/logout"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    afirmar_cabeceras(&response);
    assert_eq!(cuerpo_json(response).await, serde_json::json!({"ok": true}));

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn import_con_token_vacio_responde_400() {
    let (base, server) = levantar(AppDePrueba::nuevo().construir()).await;

    for cuerpo in [
        serde_json::json!({"token": ""}),
        serde_json::json!({"token": "   "}),
        serde_json::json!({}),
    ] {
        let response = reqwest::Client::new()
            .post(format!("{base}/api/auth/import"))
            .json(&cuerpo)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400, "cuerpo {cuerpo}");
        afirmar_cabeceras(&response);
        let body = cuerpo_json(response).await;
        assert_eq!(body["error"], "el token está vacío");
    }

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn import_con_cuerpo_grande_responde_413() {
    let (base, server) = levantar(AppDePrueba::nuevo().construir()).await;

    let cuerpo = format!("{{\"token\":\"{}\"}}", "a".repeat(1 << 20));
    assert!(cuerpo.len() > 1 << 20, "el cuerpo de prueba debe pasarse");
    let response = reqwest::Client::new()
        .post(format!("{base}/api/auth/import"))
        .body(cuerpo)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 413);
    afirmar_cabeceras(&response);
    let body = cuerpo_json(response).await;
    assert_eq!(body["code"], "cuerpo_grande");
    assert_eq!(
        body["error"],
        "el cuerpo de la petición pasa del máximo de la ruta"
    );

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn import_con_json_malformado_responde_400() {
    let (base, server) = levantar(AppDePrueba::nuevo().construir()).await;

    let response = reqwest::Client::new()
        .post(format!("{base}/api/auth/import"))
        .body("{token")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    afirmar_cabeceras(&response);
    let body = cuerpo_json(response).await;
    assert!(body["error"].is_string(), "falta el error: {body}");

    server.abort();
    let _ = server.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_sin_red_responde_500() {
    let mut app = AppDePrueba::nuevo().construir();
    app.set_login_issuer("http://127.0.0.1:1");
    let (base, server) = levantar(app).await;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let response = client
        .post(format!("{base}/api/auth/start"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 500);
    afirmar_cabeceras(&response);
    let body = cuerpo_json(response).await;
    assert!(body["error"].is_string(), "falta el error: {body}");

    server.abort();
    let _ = server.await;
}
