//! Rutas GET de lectura del espacio de trabajo.
//!
//! El backend de prueba devuelve datos conocidos y anota lo que recibió. No hay
//! red real, llavero ni variables de entorno: la sesión se arma con un `Store`
//! que entrega un token sin vencimiento y un `Connector` que devuelve el falso.

mod soporte;

use std::sync::Arc;

use chrono::{DateTime, Utc};
use orus_app::App;
use serde_json::{Value, json};
use soporte::{
    AppDePrueba, BackendDePrueba, ConSesion, ConectorCompartido, afirmar_cabeceras, cuerpo_json,
    levantar,
};

fn app_con(backend: Arc<BackendDePrueba>) -> App {
    AppDePrueba::nuevo()
        .con_store(Box::new(ConSesion))
        .con_connector(Arc::new(ConectorCompartido(backend)))
        .construir()
}

fn app_sin_sesion() -> App {
    AppDePrueba::nuevo().construir()
}

async fn pedir(base: &str, ruta: &str) -> reqwest::Response {
    reqwest::Client::new()
        .get(format!("{base}{ruta}"))
        .send()
        .await
        .expect("petición")
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
async fn listas_devuelven_su_clave_y_un_valor() {
    let backend = Arc::new(BackendDePrueba::de_lectura());
    let (base, _server) = levantar(app_con(Arc::clone(&backend))).await;

    let casos = [
        ("/api/tasks", "tasks", "id", "t1"),
        ("/api/rooms", "rooms", "room_id", "r1"),
        ("/api/goals", "goals", "id", "g1"),
        ("/api/periods", "periods", "id", "p1"),
        ("/api/decisions", "decisions", "id", "d1"),
        ("/api/team", "teams", "id", "tm1"),
    ];
    for (ruta, clave, campo, valor) in casos {
        let cuerpo = afirmar_ok(pedir(&base, ruta).await).await;
        assert_eq!(cuerpo[clave][0][campo], valor, "{ruta}");
    }
}

#[tokio::test]
async fn buscar_tareas_pasa_el_estado_y_no_falla_con_basura() {
    let backend = Arc::new(BackendDePrueba::de_lectura());
    let (base, _server) = levantar(app_con(Arc::clone(&backend))).await;

    let cuerpo = afirmar_ok(pedir(&base, "/api/tasks/search?q=factura&status=blocked").await).await;
    assert_eq!(cuerpo["tasks"][0]["title"], "encontrada");
    assert_eq!(backend.status().as_deref(), Some("blocked"));

    // El contrato ignora el error de ParseStatus: 200 y sin filtro.
    let cuerpo = afirmar_ok(pedir(&base, "/api/tasks/search?q=&status=basura").await).await;
    assert_eq!(cuerpo["tasks"][0]["id"], "t2");
    assert_eq!(backend.status().as_deref(), Some("basura"));
}

#[tokio::test]
async fn archivos_de_tarea_mapean_comments() {
    let backend = Arc::new(BackendDePrueba::de_lectura());
    let (base, _server) = levantar(app_con(backend)).await;

    let cuerpo = afirmar_ok(pedir(&base, "/api/tasks/files?task=t1").await).await;
    assert_eq!(cuerpo["files"][0]["file_name"], "informe.pdf");
    assert_eq!(cuerpo["comments"]["u1"][0]["body"], "bien");
}

#[tokio::test]
async fn respuestas_vacias_son_arreglos_y_unread_cero() {
    let backend = Arc::new(BackendDePrueba::vacio());
    let (base, _server) = levantar(app_con(backend)).await;

    let cuerpo = afirmar_ok(pedir(&base, "/api/tasks/files?task=t1").await).await;
    assert_eq!(cuerpo["files"], json!([]));
    assert_eq!(cuerpo["comments"], json!({}));

    for (ruta, clave) in [("/api/alerts", "alerts"), ("/api/mentions", "mentions")] {
        let cuerpo = afirmar_ok(pedir(&base, ruta).await).await;
        assert_eq!(cuerpo[clave], json!([]), "{ruta}");
        assert_eq!(cuerpo["unread"], 0, "{ruta}");
    }
}

#[tokio::test]
async fn mensajes_devuelve_la_pagina_y_normaliza_offset() {
    let backend = Arc::new(BackendDePrueba::de_lectura());
    let (base, _server) = levantar(app_con(Arc::clone(&backend))).await;

    for (ruta, esperado) in [
        ("/api/messages?room=r1", 0),
        ("/api/messages?room=r1&offset=basura", 0),
        ("/api/messages?room=r1&offset=-3", 0),
        ("/api/messages?room=r1&offset=100", 100),
    ] {
        let cuerpo = afirmar_ok(pedir(&base, ruta).await).await;
        assert_eq!(cuerpo["messages"][0]["id"], "m1", "{ruta}");
        assert_eq!(cuerpo["has_more"], true, "{ruta}");
        assert_eq!(cuerpo["next_offset"], 50, "{ruta}");
        assert_eq!(backend.offset(), Some(esperado), "{ruta}");
    }
}

#[tokio::test]
async fn alertas_y_menciones_cuentan_las_no_leidas() {
    let backend = Arc::new(BackendDePrueba::de_lectura());
    let (base, _server) = levantar(app_con(backend)).await;

    let cuerpo = afirmar_ok(pedir(&base, "/api/alerts").await).await;
    assert_eq!(cuerpo["alerts"][1]["id"], "a2");
    assert_eq!(cuerpo["unread"], 1);

    let cuerpo = afirmar_ok(pedir(&base, "/api/mentions").await).await;
    assert_eq!(cuerpo["mentions"][0]["id"], "n1");
    assert_eq!(cuerpo["unread"], 1);
}

#[tokio::test]
async fn detalle_de_meta_cambia_linked_tasks_nulo_por_lista() {
    let backend = Arc::new(BackendDePrueba::de_lectura());
    let (base, _server) = levantar(app_con(backend)).await;

    let cuerpo = afirmar_ok(pedir(&base, "/api/goals/detail?id=g1").await).await;
    assert_eq!(cuerpo["goal"]["id"], "g1");
    assert_eq!(cuerpo["linked_tasks"], json!([]));
}

#[tokio::test]
async fn actividad_prefiere_days_y_descarta_since_invalido() {
    let backend = Arc::new(BackendDePrueba::de_lectura());
    let (base, _server) = levantar(app_con(Arc::clone(&backend))).await;

    let cuerpo = afirmar_ok(pedir(&base, "/api/activity?days=basura").await).await;
    assert_eq!(cuerpo["events"][0]["title"], "avance");
    let hace = Utc::now() - backend.since().expect("since");
    assert!(
        (hace.num_seconds() - 7 * 86_400).abs() < 120,
        "un days inválido debe caer a 7 días: {hace:?}"
    );

    let _ = afirmar_ok(pedir(&base, "/api/activity?days=30&since=basura").await).await;
    let hace = Utc::now() - backend.since().expect("since");
    assert!(
        (hace.num_seconds() - 30 * 86_400).abs() < 120,
        "un since inválido no debe descartar days: {hace:?}"
    );

    let _ =
        afirmar_ok(pedir(&base, "/api/activity?days=30&since=2026-01-02T03:04:05Z").await).await;
    let esperado = DateTime::parse_from_rfc3339("2026-01-02T03:04:05Z")
        .expect("fecha")
        .with_timezone(&Utc);
    assert_eq!(
        backend.since(),
        Some(esperado),
        "un since válido debe pisar days"
    );
}

#[tokio::test]
async fn parametros_vacios_son_400_antes_de_la_sesion() {
    let (base, _server) = levantar(app_sin_sesion()).await;

    for (ruta, texto) in [
        ("/api/tasks/files?task=", "falta la tarea"),
        ("/api/messages?room=", "falta la sala"),
        ("/api/goals/detail?id=", "falta la meta"),
    ] {
        let cuerpo = afirmar_error(pedir(&base, ruta).await, 400).await;
        assert_eq!(cuerpo["error"], texto, "{ruta}");
    }
}
