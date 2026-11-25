//! El binario headless sirve `/api/*` con el router real y los archivos de
//! `frontend/dist` embebidos, con la misma semántica que `http.FileServer`.

mod soporte;

use serial_test::serial;
use soporte::levantar;

const INDEX: &str = include_str!("../../../frontend/dist/index.html");
const APP_CSS: &str = include_str!("../../../frontend/dist/app.css");
const LOGO_SVG: &str = include_str!("../../../frontend/dist/logo.svg");
const LOGO_PNG: &[u8] = include_bytes!("../../../frontend/dist/logo-512.png");
const FONT: &[u8] = include_bytes!("../../../frontend/dist/fonts/inter.woff2");

#[tokio::test]
#[serial]
async fn api_state_responde_json() {
    let datos = tempfile::tempdir().expect("carpeta temporal");
    let (addr, token, server) = levantar(datos.path()).await;

    let response = reqwest::Client::new()
        .get(format!("http://{addr}/api/state"))
        .header("cookie", format!("orus_token={token}"))
        .send()
        .await
        .expect("GET /api/state");
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.headers()["content-type"],
        "application/json; charset=utf-8"
    );
    let cuerpo: serde_json::Value = response.json().await.expect("JSON");
    assert!(cuerpo.is_object(), "el estado no es un objeto: {cuerpo}");

    server.abort();
}

#[tokio::test]
#[serial]
async fn la_raiz_devuelve_index() {
    let datos = tempfile::tempdir().expect("carpeta temporal");
    let (addr, _token, server) = levantar(datos.path()).await;

    let response = reqwest::get(format!("http://{addr}/"))
        .await
        .expect("GET /");
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.headers()["content-type"],
        "text/html; charset=utf-8"
    );
    assert_eq!(response.text().await.expect("cuerpo"), INDEX);

    server.abort();
}

#[tokio::test]
#[serial]
async fn modulos_js_devuelven_javascript() {
    let datos = tempfile::tempdir().expect("carpeta temporal");
    let (addr, _token, server) = levantar(datos.path()).await;

    for (ruta, esperado) in orus_server::JS_MODULOS {
        let response = reqwest::get(format!("http://{addr}{ruta}"))
            .await
            .unwrap_or_else(|e| panic!("GET {ruta}: {e}"));
        assert_eq!(response.status(), 200, "GET {ruta}");
        assert_eq!(
            response.headers()["content-type"],
            "text/javascript; charset=utf-8",
            "Content-Type de {ruta}"
        );
        assert_eq!(response.text().await.expect("cuerpo"), *esperado);
    }

    server.abort();
}

#[tokio::test]
#[serial]
async fn app_css_y_svg_devuelven_su_tipo() {
    let datos = tempfile::tempdir().expect("carpeta temporal");
    let (addr, _token, server) = levantar(datos.path()).await;

    let css = reqwest::get(format!("http://{addr}/app.css"))
        .await
        .expect("GET /app.css");
    assert_eq!(css.status(), 200);
    assert_eq!(css.headers()["content-type"], "text/css; charset=utf-8");
    assert_eq!(css.text().await.expect("cuerpo"), APP_CSS);

    let svg = reqwest::get(format!("http://{addr}/logo.svg"))
        .await
        .expect("GET /logo.svg");
    assert_eq!(svg.status(), 200);
    assert_eq!(svg.headers()["content-type"], "image/svg+xml");
    assert_eq!(svg.text().await.expect("cuerpo"), LOGO_SVG);

    server.abort();
}

#[tokio::test]
#[serial]
async fn logo_png_devuelve_png() {
    let datos = tempfile::tempdir().expect("carpeta temporal");
    let (addr, _token, server) = levantar(datos.path()).await;

    let response = reqwest::get(format!("http://{addr}/logo-512.png"))
        .await
        .expect("GET /logo-512.png");
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["content-type"], "image/png");
    assert_eq!(response.bytes().await.expect("cuerpo").as_ref(), LOGO_PNG);

    server.abort();
}

#[tokio::test]
#[serial]
async fn la_fuente_inter_se_sirve() {
    let datos = tempfile::tempdir().expect("carpeta temporal");
    let (addr, _token, server) = levantar(datos.path()).await;

    let response = reqwest::get(format!("http://{addr}/fonts/inter.woff2"))
        .await
        .expect("GET /fonts/inter.woff2");
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["content-type"], "font/woff2");
    assert_eq!(response.bytes().await.expect("cuerpo").as_ref(), FONT);

    server.abort();
}

#[tokio::test]
#[serial]
async fn index_html_redirige_a_la_raiz() {
    let datos = tempfile::tempdir().expect("carpeta temporal");
    let (addr, _token, server) = levantar(datos.path()).await;
    let cliente = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("cliente sin redirects");

    let response = cliente
        .get(format!("http://{addr}/index.html"))
        .send()
        .await
        .expect("GET /index.html");
    assert_eq!(response.status(), 301);
    assert_eq!(response.headers()["location"], "./");

    server.abort();
}

#[tokio::test]
#[serial]
async fn una_ruta_desconocida_da_404() {
    let datos = tempfile::tempdir().expect("carpeta temporal");
    let (addr, _token, server) = levantar(datos.path()).await;

    let response = reqwest::get(format!("http://{addr}/no-existe.js"))
        .await
        .expect("GET /no-existe.js");
    assert_eq!(response.status(), 404);
    assert_eq!(
        response.headers()["content-type"],
        "text/plain; charset=utf-8"
    );
    assert_eq!(
        response.text().await.expect("cuerpo"),
        "404 page not found\n"
    );

    server.abort();
}
