//! Rutas de escritura de tareas y mensajes.
//!
//! El backend de prueba
//! anota cada llamada y devuelve valores conocidos; no hay red real, llavero ni
//! variables de entorno.

mod soporte;

use std::sync::Arc;

use orus_app::{App, TaskStatus};
use serde_json::Value;
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

async fn pedir(base: &str, ruta: &str, cuerpo: &str) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{base}{ruta}"))
        .header("content-type", "application/json")
        .body(cuerpo.to_owned())
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
async fn tareas_se_crean_cambian_y_comentan() {
    let backend = Arc::new(BackendDePrueba::de_escritura());
    let (base, _server) = levantar(app_con(Arc::clone(&backend))).await;

    let cuerpo = afirmar_ok(
        pedir(
            &base,
            "/api/tasks",
            r#"{"title":"Nueva","description":"desc"}"#,
        )
        .await,
    )
    .await;
    assert_eq!(cuerpo["ok"], true);
    assert_eq!(cuerpo["task"]["id"], "t-creada");
    assert_eq!(cuerpo["task"]["title"], "Nueva");
    {
        let visto = backend.tarea_creada.lock().expect("tarea creada");
        let tarea = visto.as_ref().expect("tarea");
        assert_eq!(tarea.description, "desc");
    }

    let cuerpo = afirmar_ok(
        pedir(
            &base,
            "/api/tasks/status",
            r#"{"id":"t1","status":"blocked","note":"espero firma"}"#,
        )
        .await,
    )
    .await;
    assert_eq!(cuerpo["ok"], true);
    assert_eq!(cuerpo["progress"]["goal_pct"], 42.5);
    assert_eq!(cuerpo["progress"]["progress_reminder"], "quedan dos");
    {
        let visto = backend.estado_cambiado.lock().expect("estado");
        let (id, estado, nota) = visto.as_ref().expect("estado");
        assert_eq!(id, "t1");
        assert_eq!(estado, &TaskStatus::Blocked);
        assert_eq!(nota, "espero firma");
    }

    let cuerpo = afirmar_ok(
        pedir(
            &base,
            "/api/tasks/files/comment",
            r#"{"task":"t1","update":"u1","body":"bien"}"#,
        )
        .await,
    )
    .await;
    assert_eq!(cuerpo["ok"], true);
    {
        let visto = backend.comentario_creado.lock().expect("comentario");
        let (tarea, update, body) = visto.as_ref().expect("comentario");
        assert_eq!(tarea, "t1");
        assert_eq!(update, "u1");
        assert_eq!(body, "bien");
    }
}

#[tokio::test]
async fn mensajes_eligen_sala_directo_o_avisan() {
    let backend = Arc::new(BackendDePrueba::de_escritura());
    let (base, _server) = levantar(app_con(Arc::clone(&backend))).await;

    let cuerpo = afirmar_ok(
        pedir(
            &base,
            "/api/messages",
            r#"{"room":"r1","body":"hola","replyTo":"m0"}"#,
        )
        .await,
    )
    .await;
    assert_eq!(cuerpo["ok"], true);
    {
        let visto = backend.mensaje_sala.lock().expect("sala");
        let (sala, body, reply) = visto.as_ref().expect("sala");
        assert_eq!(sala, "r1");
        assert_eq!(body, "hola");
        assert_eq!(reply, "m0");
    }

    let cuerpo =
        afirmar_ok(pedir(&base, "/api/messages", r#"{"to":"u2","body":"directo"}"#).await).await;
    assert_eq!(cuerpo["ok"], true);
    {
        let visto = backend.mensaje_directo.lock().expect("directo");
        let (to, body, reply) = visto.as_ref().expect("directo");
        assert_eq!(to, "u2");
        assert_eq!(body, "directo");
        assert_eq!(reply, "");
    }

    let cuerpo = afirmar_error(
        pedir(&base, "/api/messages", r#"{"body":"hola"}"#).await,
        400,
    )
    .await;
    assert_eq!(cuerpo["error"], "falta la sala o la persona");

    let cuerpo = afirmar_error(pedir(&base, "/api/messages", "{}").await, 400).await;
    assert_eq!(cuerpo["error"], "el mensaje está vacío");
}

#[tokio::test]
async fn marcar_lecturas_pasa_el_valor() {
    let backend = Arc::new(BackendDePrueba::de_escritura());
    let (base, _server) = levantar(app_con(Arc::clone(&backend))).await;

    let cuerpo = afirmar_ok(pedir(&base, "/api/rooms/read", r#"{"room":"r1"}"#).await).await;
    assert_eq!(cuerpo["ok"], true);
    {
        let marcada = backend.sala_marcada.lock().expect("sala marcada");
        assert_eq!(marcada.as_deref(), Some("r1"));
    }

    let cuerpo = afirmar_ok(pedir(&base, "/api/alerts/read", r#"{"ids":["a1","a2"]}"#).await).await;
    assert_eq!(cuerpo["ok"], true);
    {
        let leidas = backend.alertas_leidas_ids.lock().expect("alertas");
        assert_eq!(
            leidas.as_deref(),
            Some(&["a1".to_owned(), "a2".to_owned()][..])
        );
    }

    let cuerpo = afirmar_ok(pedir(&base, "/api/mentions/read", r#"{"ids":["m1"]}"#).await).await;
    assert_eq!(cuerpo["ok"], true);
    {
        let leidas = backend.menciones_leidas_ids.lock().expect("menciones");
        assert_eq!(leidas.as_deref(), Some(&["m1".to_owned()][..]));
    }

    // Sin `ids`, el backend recibe un arreglo vacío, no null.
    let cuerpo = afirmar_ok(pedir(&base, "/api/alerts/read", "{}").await).await;
    assert_eq!(cuerpo["ok"], true);
    {
        let leidas = backend.alertas_leidas_ids.lock().expect("alertas");
        assert_eq!(leidas.as_deref(), Some(&[][..]));
    }
}

#[tokio::test]
async fn errores_de_cuerpo_y_tope() {
    let backend = Arc::new(BackendDePrueba::de_escritura());
    let (base, _server) = levantar(app_con(Arc::clone(&backend))).await;

    // El cuerpo vacío se lee como `{}`: la sala queda vacía y la ruta responde.
    let cuerpo = afirmar_ok(pedir(&base, "/api/rooms/read", "").await).await;
    assert_eq!(cuerpo["ok"], true);
    {
        let marcada = backend.sala_marcada.lock().expect("sala marcada");
        assert_eq!(marcada.as_deref(), Some(""));
    }

    let cuerpo = afirmar_error(pedir(&base, "/api/tasks", "no-json").await, 500).await;
    assert!(
        cuerpo["error"]
            .as_str()
            .is_some_and(|texto| !texto.is_empty()),
        "el error de JSON debe traer texto: {cuerpo}"
    );

    let grande = format!(r#"{{"title":"{}"}}"#, "x".repeat(1 << 20));
    let cuerpo = afirmar_error(pedir(&base, "/api/tasks", &grande).await, 413).await;
    assert_eq!(
        cuerpo["error"],
        "el cuerpo de la petición pasa del máximo de la ruta"
    );
    assert_eq!(cuerpo["code"], "cuerpo_grande");
}

#[tokio::test]
async fn sin_sesion_los_400_van_antes_del_401() {
    let (base, _server) = levantar(app_sin_sesion()).await;

    let cuerpo = afirmar_error(pedir(&base, "/api/messages", "{}").await, 400).await;
    assert_eq!(cuerpo["error"], "el mensaje está vacío");

    // El estado desconocido se parsea antes de pedir el cliente: 500, no 401.
    let cuerpo = afirmar_error(
        pedir(&base, "/api/tasks/status", r#"{"id":"t1","status":"x"}"#).await,
        500,
    )
    .await;
    assert_eq!(cuerpo["error"], "estado desconocido: \"x\"");
}
