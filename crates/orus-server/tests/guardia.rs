//! El borde headless exige token, Host del loopback y Origin propio.

mod soporte;

use serial_test::serial;
use soporte::levantar;

#[tokio::test]
#[serial]
async fn headless_exige_token_host_y_origen() {
    let datos = tempfile::tempdir().expect("carpeta temporal");
    let (addr, token, servidor) = levantar(datos.path()).await;
    let cliente = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("cliente sin redirects");

    let sin_token = cliente
        .get(format!("http://{addr}/api/state"))
        .send()
        .await
        .expect("GET sin token");
    assert_eq!(sin_token.status(), 401, "la API sin token no responde");

    let host_ajeno = cliente
        .get(format!("http://{addr}/api/state"))
        .header(
            "host",
            format!(
                "oruscribe.atacante:{}",
                addr.rsplit(':').next().unwrap_or("0")
            ),
        )
        .header("cookie", format!("orus_token={token}"))
        .send()
        .await
        .expect("GET con Host ajeno");
    assert_eq!(host_ajeno.status(), 421, "DNS rebinding con Host ajeno");

    let entrada = cliente
        .get(format!("http://{addr}/?token={token}"))
        .send()
        .await
        .expect("entrada con token");
    assert!(entrada.status().is_redirection());
    let cookie = entrada
        .headers()
        .get(reqwest::header::SET_COOKIE)
        .and_then(|valor| valor.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    assert!(cookie.contains("HttpOnly"), "{cookie}");
    assert!(cookie.contains(&format!("orus_token={token}")), "{cookie}");

    let con_token = cliente
        .get(format!("http://{addr}/api/state"))
        .header("cookie", format!("orus_token={token}"))
        .send()
        .await
        .expect("GET con token");
    assert_eq!(con_token.status(), 200);

    let origen_ajeno = cliente
        .post(format!("http://{addr}/api/agent/pick-folder"))
        .header("cookie", format!("orus_token={token}"))
        .header("origin", "https://oruscribe.atacante")
        .send()
        .await
        .expect("POST con Origin ajeno");
    assert_eq!(origen_ajeno.status(), 403, "el Origin ajeno no muta estado");

    let propio = cliente
        .post(format!("http://{addr}/api/agent/pick-folder"))
        .header("cookie", format!("orus_token={token}"))
        .header("origin", format!("http://{addr}"))
        .send()
        .await
        .expect("POST con Origin propio");
    assert_eq!(propio.status(), 400, "sin selector el router responde 400");

    servidor.abort();
}
