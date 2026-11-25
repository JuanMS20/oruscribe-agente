//! Rutas de metas, periodos, captura, avisos, reacciones y contexto.
//!
//! Port de `internal/app/http.go:450-565,636-733`. El backend de prueba anota
//! cada llamada y la costura `GitLog` se inyecta: no hay red real, repos
//! reales, llavero ni variables de entorno.

mod soporte;

use std::sync::Arc;

use chrono::{Duration, Local, NaiveDate, TimeZone, Utc};
use orus_api::{Api, GitLog};
use orus_app::App;
use serde_json::Value;
use soporte::{
    AppDePrueba, BackendDePrueba, ConSesion, ConectorCompartido, GitLogDePrueba, afirmar_cabeceras,
    cuerpo_json, levantar_api,
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

async fn levantar(
    app: App,
    git_log: Arc<dyn GitLog>,
) -> (String, tokio::task::JoinHandle<std::io::Result<()>>) {
    levantar_api(Api::with_git_log(Arc::new(app), git_log)).await
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

fn git_log_feliz() -> Arc<GitLogDePrueba> {
    Arc::new(GitLogDePrueba::default())
}

#[tokio::test]
async fn metas_y_periodos_pasan_el_cuerpo_al_backend() {
    let backend = Arc::new(BackendDePrueba::de_captura());
    let (base, _server) = levantar(app_con(Arc::clone(&backend)), git_log_feliz()).await;

    let cuerpo = afirmar_ok(
        pedir(
            &base,
            "/api/goals",
            r#"{"title":"Nueva meta","description":"detalle","weight":2.5}"#,
        )
        .await,
    )
    .await;
    assert_eq!(cuerpo["ok"], true);
    assert_eq!(cuerpo["goal"]["id"], "g-nueva");
    assert_eq!(cuerpo["goal"]["title"], "Nueva meta");
    assert_eq!(cuerpo["goal"]["pct"], 12.5);
    {
        let visto = backend.meta_creada.lock().expect("meta");
        let meta = visto.as_ref().expect("meta");
        assert_eq!(meta.title, "Nueva meta");
        assert_eq!(meta.description, "detalle");
        assert_eq!(meta.weight, 2.5);
    }

    let cuerpo = afirmar_ok(
        pedir(
            &base,
            "/api/periods",
            r#"{"name":"Sprint 1","start_date":"2026-11-01","end_date":"2026-11-15","kind":"sprint"}"#,
        )
        .await,
    )
    .await;
    assert_eq!(cuerpo["ok"], true);
    assert_eq!(cuerpo["period"]["id"], "p-nuevo");
    assert_eq!(cuerpo["period"]["name"], "Sprint 1");
    {
        let visto = backend.periodo_creado.lock().expect("periodo");
        let periodo = visto.as_ref().expect("periodo");
        assert_eq!(periodo.name, "Sprint 1");
        assert_eq!(periodo.kind, "sprint");
    }
}

#[tokio::test]
async fn captura_propone_con_la_fecha_y_los_origenes() {
    let backend = Arc::new(BackendDePrueba::de_captura());
    let git_log = git_log_feliz();
    let (base, _server) = levantar(
        app_con(Arc::clone(&backend)),
        Arc::clone(&git_log) as Arc<dyn GitLog>,
    )
    .await;

    let cuerpo = afirmar_ok(
        pedir(
            &base,
            "/api/capture/propose",
            r#"{"folder":"  C:\\repo  ","since":"2026-11-01"}"#,
        )
        .await,
    )
    .await;
    // El digest va en el nivel superior y sin `ok`.
    assert_eq!(cuerpo["digest_id"], "d-1");
    assert_eq!(cuerpo["expires_at"], "2026-11-14T00:00:00Z");
    assert_eq!(cuerpo["candidates"], serde_json::json!([]));
    assert!(
        cuerpo["skipped"].is_null(),
        "skipped debe ser null: {cuerpo}"
    );
    assert_eq!(cuerpo["next_step"], "elige los candidatos");
    assert!(cuerpo.get("ok").is_none(), "el digest no lleva ok");

    {
        let visto = git_log.visto();
        let (carpeta, since) = visto.as_ref().expect("git_log");
        assert_eq!(carpeta, "  C:\\repo  ");
        assert_eq!(
            since,
            &Some(
                Utc.with_ymd_and_hms(2026, 11, 1, 0, 0, 0)
                    .single()
                    .expect("fecha")
            )
        );
    }
    {
        let visto = backend.digest_propuesto.lock().expect("propuesta");
        let (since, sources) = visto.as_ref().expect("propuesta");
        assert_eq!(since, "2026-11-01");
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].name, "repo");
    }

    let cuerpo = afirmar_ok(
        pedir(
            &base,
            "/api/capture/apply",
            r#"{"digest":"d-1","accept":["c1","c2"]}"#,
        )
        .await,
    )
    .await;
    assert_eq!(cuerpo["ok"], true);
    assert_eq!(cuerpo["result"], "Listo: 2 candidatos");
    {
        let visto = backend.digest_aplicado.lock().expect("aplicado");
        let (digest, accept) = visto.as_ref().expect("aplicado");
        assert_eq!(digest, "d-1");
        assert_eq!(accept, &["c1".to_owned(), "c2".to_owned()]);
    }
}

#[tokio::test]
async fn captura_sin_fecha_usa_siete_dias_locales() {
    let backend = Arc::new(BackendDePrueba::de_captura());
    let git_log = git_log_feliz();
    let (base, _server) = levantar(
        app_con(Arc::clone(&backend)),
        Arc::clone(&git_log) as Arc<dyn GitLog>,
    )
    .await;

    let antes = Local::now() - Duration::days(7);
    let cuerpo =
        afirmar_ok(pedir(&base, "/api/capture/propose", r#"{"folder":"C:\\repo"}"#).await).await;
    let despues = Local::now() - Duration::days(7);

    assert_eq!(cuerpo["digest_id"], "d-1");
    let esperado1 = antes.format("%Y-%m-%d").to_string();
    let esperado2 = despues.format("%Y-%m-%d").to_string();
    {
        let visto = backend.digest_propuesto.lock().expect("propuesta");
        let (since, _) = visto.as_ref().expect("propuesta");
        assert!(
            since == &esperado1 || since == &esperado2,
            "since {since} no es la fecha local de hace 7 días ({esperado1} o {esperado2})"
        );
    }
    let visto = git_log.visto();
    let (_, since) = visto.as_ref().expect("git_log");
    let recibido = since.expect("since");
    let esperado = Utc::now() - Duration::days(7);
    assert!(
        (recibido - esperado).num_seconds().abs() <= 60,
        "git_log recibió {recibido}, se esperaba cerca de {esperado}"
    );
}

#[tokio::test]
async fn captura_valida_carpeta_y_fecha_antes_de_tocar_el_backend() {
    let backend = Arc::new(BackendDePrueba::de_captura());
    let git_log = git_log_feliz();
    let (base, _server) = levantar(
        app_con(Arc::clone(&backend)),
        Arc::clone(&git_log) as Arc<dyn GitLog>,
    )
    .await;

    let cuerpo = afirmar_error(
        pedir(&base, "/api/capture/propose", r#"{"folder":""}"#).await,
        400,
    )
    .await;
    assert_eq!(cuerpo["error"], "falta la carpeta del repositorio");

    let cuerpo = afirmar_error(
        pedir(&base, "/api/capture/propose", r#"{"folder":"   "}"#).await,
        400,
    )
    .await;
    assert_eq!(cuerpo["error"], "falta la carpeta del repositorio");

    let cuerpo = afirmar_error(
        pedir(
            &base,
            "/api/capture/propose",
            r#"{"folder":"C:\\repo","since":"2026-13-01"}"#,
        )
        .await,
        400,
    )
    .await;
    assert_eq!(cuerpo["error"], "la fecha no es válida");

    assert!(
        git_log.visto().is_none(),
        "git_log no debe llamarse cuando la validación falla"
    );
    assert!(
        backend
            .digest_propuesto
            .lock()
            .expect("propuesta")
            .is_none(),
        "el backend no debe llamarse cuando la validación falla"
    );
}

#[tokio::test]
async fn captura_con_carpeta_inexistente_da_500_antes_del_401() {
    let git_log: Arc<dyn GitLog> = Arc::new(GitLogDePrueba::que_falla());
    let (base, _server) = levantar(app_sin_sesion(), git_log).await;

    let cuerpo = afirmar_error(
        pedir(
            &base,
            "/api/capture/propose",
            r#"{"folder":"C:\\no-existe"}"#,
        )
        .await,
        500,
    )
    .await;
    assert_eq!(cuerpo["error"], "esa carpeta no existe: C:\\no-existe");
}

#[tokio::test]
async fn avisos_validan_tipo_y_destino_antes_del_backend() {
    let backend = Arc::new(BackendDePrueba::de_captura());
    let (base, _server) = levantar(app_con(Arc::clone(&backend)), git_log_feliz()).await;

    let cuerpo = afirmar_ok(
        pedir(
            &base,
            "/api/alerts/send",
            r#"{"type":"blocker","message":"freno","team":"t1","user":""}"#,
        )
        .await,
    )
    .await;
    assert_eq!(cuerpo["ok"], true);
    {
        let visto = backend.aviso_enviado.lock().expect("aviso");
        let (tipo, mensaje, equipo, persona) = visto.as_ref().expect("aviso");
        assert_eq!(tipo, "blocker");
        assert_eq!(mensaje, "freno");
        assert_eq!(equipo, "t1");
        assert_eq!(persona, "");
    }

    let cuerpo = afirmar_error(
        pedir(
            &base,
            "/api/alerts/send",
            r#"{"type":"otro","message":"x","team":"t1"}"#,
        )
        .await,
        400,
    )
    .await;
    assert_eq!(cuerpo["error"], "tipo de aviso desconocido");

    let cuerpo = afirmar_error(
        pedir(
            &base,
            "/api/alerts/send",
            r#"{"type":"info","message":"x"}"#,
        )
        .await,
        400,
    )
    .await;
    assert_eq!(cuerpo["error"], "elige a quién va el aviso");

    // El mensaje vacío lo rechaza el backend: 500, no 400.
    let cuerpo = afirmar_error(
        pedir(
            &base,
            "/api/alerts/send",
            r#"{"type":"info","message":"","team":"t1"}"#,
        )
        .await,
        500,
    )
    .await;
    assert_eq!(cuerpo["error"], "el aviso está vacío");
}

#[tokio::test]
async fn reacciones_y_contexto_pasan_los_textos_exactos() {
    let backend = Arc::new(BackendDePrueba::de_captura());
    let (base, _server) = levantar(app_con(Arc::clone(&backend)), git_log_feliz()).await;

    let cuerpo = afirmar_ok(
        pedir(
            &base,
            "/api/messages/react",
            r#"{"message":"m1","emoji":"ok"}"#,
        )
        .await,
    )
    .await;
    assert_eq!(cuerpo["ok"], true);
    {
        let visto = backend.reaccion_enviada.lock().expect("reacción");
        let (mensaje, emoji) = visto.as_ref().expect("reacción");
        assert_eq!(mensaje, "m1");
        assert_eq!(emoji, "ok");
    }

    let cuerpo = afirmar_error(
        pedir(&base, "/api/messages/react", r#"{"message":"m1"}"#).await,
        500,
    )
    .await;
    assert_eq!(cuerpo["error"], "reacción incompleta");

    let cuerpo = afirmar_ok(
        pedir(
            &base,
            "/api/context",
            r#"{"focus":"  foco  ","mood":"on_track","notes":"nota"}"#,
        )
        .await,
    )
    .await;
    assert_eq!(cuerpo["ok"], true);
    {
        let visto = backend.contexto_actualizado.lock().expect("contexto");
        let (foco, animo, notas) = visto.as_ref().expect("contexto");
        assert_eq!(foco, "  foco  ");
        assert_eq!(animo, "on_track");
        assert_eq!(notas, "nota");
    }

    let cuerpo = afirmar_error(pedir(&base, "/api/context", "{}").await, 500).await;
    assert_eq!(cuerpo["error"], "no hay nada que actualizar");
}

#[tokio::test]
async fn captura_aplicar_sin_aceptados_da_500() {
    let backend = Arc::new(BackendDePrueba::de_captura());
    let (base, _server) = levantar(app_con(Arc::clone(&backend)), git_log_feliz()).await;

    let cuerpo = afirmar_error(
        pedir(&base, "/api/capture/apply", r#"{"digest":"d-1"}"#).await,
        500,
    )
    .await;
    assert_eq!(cuerpo["error"], "no hay nada seleccionado");
}

/// La fecha explícita se parsea como Go: medianoche UTC del día indicado.
#[test]
fn fecha_parseada_es_medianoche_utc() {
    let fecha = NaiveDate::parse_from_str("2026-11-01", "%Y-%m-%d").expect("fecha");
    let medianoche = fecha.and_hms_opt(0, 0, 0).expect("medianoche");
    let utc = Utc.from_utc_datetime(&medianoche);
    assert_eq!(utc.to_rfc3339(), "2026-11-01T00:00:00+00:00");
}
