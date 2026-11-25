//! Cableado HTTP de `POST /api/agent/transcribe` y `POST /api/agent/speak`.
//!
//! Los servidores de voz son locales: ningún caso sale a la red. El orden de
//! validaciones y los textos son los de `internal/app/agent_voice.go`.

mod soporte;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, Multipart, State};
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use orus_api::Api;
use orus_app::App;
use orus_config::{AgentSettings, Locale, Settings};
use serde_json::Value;
use soporte::{AppDePrueba, cuerpo_json};

fn ajustes(stt: String, tts: String) -> Settings {
    Settings {
        locale: Locale::Spanish,
        agent: AgentSettings {
            base_url: "http://127.0.0.1:1/v1".to_owned(),
            voice_stt_url: stt,
            voice_tts_url: tts,
            ..AgentSettings::default()
        },
        ..Settings::default()
    }
}

fn levantar(app: App) -> (String, tokio::task::JoinHandle<()>) {
    let api = Api::new(Arc::new(app));
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener de la api");
    listener
        .set_nonblocking(true)
        .expect("listener sin bloqueo");
    let direccion = listener.local_addr().expect("dirección local");
    let listener = tokio::net::TcpListener::from_std(listener).expect("listener de tokio");
    let servidor = tokio::spawn(async move {
        axum::serve(listener, api.router()).await.expect("api");
    });
    (format!("http://{direccion}"), servidor)
}

// ---------------------------------------------------------------------------
// Servidor de transcripción falso
// ---------------------------------------------------------------------------

#[derive(Default)]
struct VistoStt {
    ruta: String,
    autorizacion: String,
    modelo: String,
    idioma: String,
    nombre: String,
    archivo: Vec<u8>,
}

async fn stt_falso(
    uri: Uri,
    State(visto): State<Arc<Mutex<VistoStt>>>,
    cabeceras: HeaderMap,
    mut formulario: Multipart,
) -> impl IntoResponse {
    let mut capturado = VistoStt {
        ruta: uri.path().to_owned(),
        autorizacion: cabeceras
            .get(header::AUTHORIZATION)
            .and_then(|valor| valor.to_str().ok())
            .unwrap_or_default()
            .to_owned(),
        ..VistoStt::default()
    };
    while let Some(campo) = formulario.next_field().await.expect("campo") {
        let nombre = campo.name().unwrap_or_default().to_owned();
        let archivo = campo.file_name().map(str::to_owned);
        let datos = campo.bytes().await.expect("datos").to_vec();
        match nombre.as_str() {
            "file" => {
                capturado.nombre = archivo.unwrap_or_default();
                capturado.archivo = datos;
            }
            "model" => capturado.modelo = String::from_utf8_lossy(&datos).into_owned(),
            "language" => capturado.idioma = String::from_utf8_lossy(&datos).into_owned(),
            _ => {}
        }
    }
    *visto.lock().expect("visto stt") = capturado;
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        r#"{"text":"  hola desde el dictado  "}"#,
    )
}

async fn servidor_stt(visto: Arc<Mutex<VistoStt>>) -> (String, tokio::task::JoinHandle<()>) {
    let app = Router::new()
        .route(
            "/v1/audio/transcriptions",
            post(stt_falso).layer(DefaultBodyLimit::max(10 << 20)),
        )
        .with_state(visto);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener stt");
    let direccion = listener.local_addr().expect("dirección stt");
    let servidor = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("servidor stt");
    });
    (format!("http://{direccion}"), servidor)
}

async fn servidor_stt_con(
    estado: StatusCode,
    cuerpo: &'static str,
) -> (String, tokio::task::JoinHandle<()>) {
    let app = Router::new().route(
        "/v1/audio/transcriptions",
        post(move |_: Bytes| async move {
            (estado, [(header::CONTENT_TYPE, "application/json")], cuerpo)
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener stt de respuesta");
    let direccion = listener.local_addr().expect("dirección stt de respuesta");
    let servidor = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("servidor stt de respuesta");
    });
    (format!("http://{direccion}"), servidor)
}

// ---------------------------------------------------------------------------
// Servidor de lectura falso
// ---------------------------------------------------------------------------

#[derive(Default)]
struct VistoTts {
    ruta: String,
    autorizacion: String,
    tipo: String,
    cuerpo: Value,
}

async fn tts_falso(
    uri: Uri,
    State(visto): State<Arc<Mutex<VistoTts>>>,
    cabeceras: HeaderMap,
    cuerpo: Bytes,
) -> impl IntoResponse {
    *visto.lock().expect("visto tts") = VistoTts {
        ruta: uri.path().to_owned(),
        autorizacion: cabeceras
            .get(header::AUTHORIZATION)
            .and_then(|valor| valor.to_str().ok())
            .unwrap_or_default()
            .to_owned(),
        tipo: cabeceras
            .get(header::CONTENT_TYPE)
            .and_then(|valor| valor.to_str().ok())
            .unwrap_or_default()
            .to_owned(),
        cuerpo: serde_json::from_slice(&cuerpo).unwrap_or(Value::Null),
    };
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "audio/mpeg")],
        "mp3-de-prueba",
    )
}

async fn servidor_tts(visto: Arc<Mutex<VistoTts>>) -> (String, tokio::task::JoinHandle<()>) {
    let app = Router::new()
        .route("/audio/speech", post(tts_falso))
        .with_state(visto);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener tts");
    let direccion = listener.local_addr().expect("dirección tts");
    let servidor = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("servidor tts");
    });
    (format!("http://{direccion}"), servidor)
}

async fn servidor_tts_mudo() -> (String, tokio::task::JoinHandle<()>) {
    async fn sin_tipo() -> Response {
        Response::new(Body::from("sin-tipo"))
    }
    async fn grande() -> Response {
        let datos = vec![7u8; (8 << 20) + 64];
        let mut respuesta = Response::new(Body::from(datos));
        respuesta.headers_mut().insert(
            header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("audio/mpeg"),
        );
        respuesta
    }
    let app = Router::new()
        .route("/sin-tipo/audio/speech", post(sin_tipo))
        .route("/grande/audio/speech", post(grande));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener tts mudo");
    let direccion = listener.local_addr().expect("dirección tts mudo");
    let servidor = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("servidor tts mudo");
    });
    (format!("http://{direccion}"), servidor)
}

// ---------------------------------------------------------------------------
// Transcripción
// ---------------------------------------------------------------------------

#[tokio::test]
async fn voice_transcribe_habla_con_el_servidor_local() {
    let visto = Arc::new(Mutex::new(VistoStt::default()));
    let (stt_url, servidor) = servidor_stt(Arc::clone(&visto)).await;
    let mut settings = ajustes(format!("{stt_url}/v1"), String::new());
    settings.agent.voice_stt_model = "whisper-local".to_owned();
    let app = AppDePrueba::nuevo()
        .con_settings(settings)
        .con_clave(Some("clave-que-no-viaja"))
        .construir();
    let (base, api) = levantar(app);

    let formulario = reqwest::multipart::Form::new().part(
        "audio",
        reqwest::multipart::Part::bytes(b"audio-de-prueba".to_vec())
            .file_name("nota.webm")
            .mime_str("audio/webm")
            .expect("tipo de audio"),
    );
    let respuesta = reqwest::Client::new()
        .post(format!("{base}/api/agent/transcribe"))
        .multipart(formulario)
        .send()
        .await
        .expect("POST transcribe");
    assert_eq!(respuesta.status(), 200);
    assert_eq!(
        respuesta
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|valor| valor.to_str().ok()),
        Some("no-store")
    );
    let cuerpo: Value = respuesta.json().await.expect("JSON de transcripción");
    assert_eq!(cuerpo["text"], "hola desde el dictado");

    let visto = visto.lock().expect("visto stt");
    assert_eq!(visto.ruta, "/v1/audio/transcriptions");
    assert_eq!(visto.modelo, "whisper-local");
    assert_eq!(visto.idioma, "es");
    assert_eq!(visto.nombre, "nota.webm");
    assert_eq!(visto.archivo, b"audio-de-prueba".to_vec());
    assert_eq!(visto.autorizacion, "");

    api.abort();
    servidor.abort();
}

#[tokio::test]
async fn voice_transcribe_manda_la_clave_solo_al_mismo_host() {
    let visto = Arc::new(Mutex::new(VistoStt::default()));
    let (stt_url, servidor) = servidor_stt(Arc::clone(&visto)).await;
    let mut settings = ajustes(format!("{stt_url}/v1"), String::new());
    settings.agent.base_url = format!("{stt_url}/v1");
    let app = AppDePrueba::nuevo()
        .con_settings(settings)
        .con_clave(Some("clave-del-proveedor"))
        .construir();
    let (base, api) = levantar(app);

    let formulario = reqwest::multipart::Form::new().part(
        "audio",
        reqwest::multipart::Part::bytes(b"audio".to_vec()).file_name("a.webm"),
    );
    let respuesta = reqwest::Client::new()
        .post(format!("{base}/api/agent/transcribe"))
        .multipart(formulario)
        .send()
        .await
        .expect("POST transcribe con clave");
    assert_eq!(respuesta.status(), 200);
    assert_eq!(
        visto.lock().expect("visto stt").autorizacion,
        "Bearer clave-del-proveedor"
    );

    api.abort();
    servidor.abort();
}

#[tokio::test]
async fn voice_transcribe_no_duplica_el_recurso_y_usa_los_valores_por_defecto() {
    let visto = Arc::new(Mutex::new(VistoStt::default()));
    let (stt_url, servidor) = servidor_stt(Arc::clone(&visto)).await;
    let mut settings = ajustes(format!("{stt_url}/v1/audio/transcriptions"), String::new());
    settings.locale = Locale::English;
    let app = AppDePrueba::nuevo().con_settings(settings).construir();
    let (base, api) = levantar(app);

    let formulario = reqwest::multipart::Form::new().part(
        "audio",
        reqwest::multipart::Part::bytes(b"audio".to_vec()).file_name("a.webm"),
    );
    let respuesta = reqwest::Client::new()
        .post(format!("{base}/api/agent/transcribe"))
        .multipart(formulario)
        .send()
        .await
        .expect("POST transcribe");
    assert_eq!(respuesta.status(), 200);

    let visto = visto.lock().expect("visto stt");
    assert_eq!(visto.ruta, "/v1/audio/transcriptions");
    assert_eq!(visto.modelo, "whisper-1");
    assert_eq!(visto.idioma, "en");

    api.abort();
    servidor.abort();
}

#[tokio::test]
async fn voice_transcribe_reporta_lo_que_devuelve_el_servidor() {
    for (estado, respuesta, esperado) in [
        (
            StatusCode::OK,
            r#"{"text":"   "}"#,
            "el servidor de voz no entendió el audio",
        ),
        (
            StatusCode::OK,
            r#"{"text":null}"#,
            "el servidor de voz no entendió el audio",
        ),
        (
            StatusCode::OK,
            "sin json",
            "el servidor de voz no devolvió texto entendible",
        ),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "{}",
            "el servidor de voz respondió 503",
        ),
    ] {
        let (stt_url, servidor) = servidor_stt_con(estado, respuesta).await;
        let app = AppDePrueba::nuevo()
            .con_settings(ajustes(format!("{stt_url}/v1"), String::new()))
            .construir();
        let (base, api) = levantar(app);

        let formulario = reqwest::multipart::Form::new().part(
            "audio",
            reqwest::multipart::Part::bytes(b"audio".to_vec()).file_name("a.webm"),
        );
        let respuesta = reqwest::Client::new()
            .post(format!("{base}/api/agent/transcribe"))
            .multipart(formulario)
            .send()
            .await
            .expect("POST transcribe con respuesta rara");
        assert_eq!(respuesta.status(), 500);
        let cuerpo: Value = respuesta.json().await.expect("error del servidor de voz");
        assert_eq!(cuerpo["error"], esperado);

        api.abort();
        servidor.abort();
    }
}

#[tokio::test]
async fn voice_transcribe_sin_archivo_es_400_antes_de_mirar_la_url() {
    let app = AppDePrueba::nuevo()
        .con_settings(ajustes(String::new(), String::new()))
        .construir();
    let (base, api) = levantar(app);
    let cliente = reqwest::Client::new();

    let formulario = reqwest::multipart::Form::new().text("otro", "sin audio");
    let respuesta = cliente
        .post(format!("{base}/api/agent/transcribe"))
        .multipart(formulario)
        .send()
        .await
        .expect("multipart sin audio");
    assert_eq!(respuesta.status(), 400);
    let cuerpo: Value = respuesta.json().await.expect("error de audio");
    assert_eq!(cuerpo["error"], "falta el audio");

    let respuesta = cliente
        .post(format!("{base}/api/agent/transcribe"))
        .header(header::CONTENT_TYPE, "multipart/form-data")
        .body("esto no es multipart")
        .send()
        .await
        .expect("multipart roto");
    assert_eq!(respuesta.status(), 400);
    let cuerpo: Value = respuesta.json().await.expect("error de audio roto");
    assert_eq!(cuerpo["error"], "el audio no se pudo leer o pasa de 10 MB");

    api.abort();
}

#[tokio::test]
async fn voice_transcribe_acepta_audio_entre_2_y_10_mb() {
    let visto = Arc::new(Mutex::new(VistoStt::default()));
    let (stt_url, servidor) = servidor_stt(Arc::clone(&visto)).await;
    let app = AppDePrueba::nuevo()
        .con_settings(ajustes(format!("{stt_url}/v1"), String::new()))
        .construir();
    let (base, api) = levantar(app);

    let audio = vec![3u8; 3 << 20];
    let formulario = reqwest::multipart::Form::new().part(
        "audio",
        reqwest::multipart::Part::bytes(audio).file_name("largo.webm"),
    );
    let respuesta = reqwest::Client::new()
        .post(format!("{base}/api/agent/transcribe"))
        .multipart(formulario)
        .send()
        .await
        .expect("multipart de 3 MB");
    assert_eq!(respuesta.status(), 200);
    assert_eq!(visto.lock().expect("visto stt").archivo.len(), 3 << 20);

    api.abort();
    servidor.abort();
}

#[tokio::test]
async fn voice_transcribe_pasado_de_10_mb_es_400() {
    let app = AppDePrueba::nuevo()
        .con_settings(ajustes(String::new(), String::new()))
        .construir();
    let (base, api) = levantar(app);

    let formulario = reqwest::multipart::Form::new().part(
        "audio",
        reqwest::multipart::Part::bytes(vec![0u8; (10 << 20) + 1]).file_name("grande.webm"),
    );
    let respuesta = reqwest::Client::new()
        .post(format!("{base}/api/agent/transcribe"))
        .multipart(formulario)
        .send()
        .await
        .expect("multipart grande");
    assert_eq!(respuesta.status(), 400);
    let cuerpo: Value = respuesta.json().await.expect("error de tamaño");
    assert_eq!(cuerpo["error"], "el audio no se pudo leer o pasa de 10 MB");

    api.abort();
}

// ---------------------------------------------------------------------------
// Lectura
// ---------------------------------------------------------------------------

#[tokio::test]
async fn voice_speak_devuelve_el_audio_crudo() {
    let visto = Arc::new(Mutex::new(VistoTts::default()));
    let (tts_url, servidor) = servidor_tts(Arc::clone(&visto)).await;
    let mut settings = ajustes(String::new(), tts_url);
    settings.agent.voice_tts_voice = "alloy".to_owned();
    let app = AppDePrueba::nuevo().con_settings(settings).construir();
    let (base, api) = levantar(app);

    let respuesta = reqwest::Client::new()
        .post(format!("{base}/api/agent/speak"))
        .json(&serde_json::json!({"text": "hola"}))
        .send()
        .await
        .expect("POST speak");
    assert_eq!(respuesta.status(), 200);
    assert_eq!(
        respuesta
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|valor| valor.to_str().ok()),
        Some("audio/mpeg")
    );
    assert_eq!(
        respuesta
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|valor| valor.to_str().ok()),
        Some("no-store")
    );
    let audio = respuesta.bytes().await.expect("audio");
    assert_eq!(audio.to_vec(), b"mp3-de-prueba".to_vec());

    let visto = visto.lock().expect("visto tts");
    assert_eq!(visto.ruta, "/audio/speech");
    assert_eq!(visto.tipo, "application/json");
    assert_eq!(visto.cuerpo["model"], "tts-1");
    assert_eq!(visto.cuerpo["input"], "hola");
    assert_eq!(visto.cuerpo["voice"], "alloy");
    assert_eq!(visto.cuerpo["response_format"], "mp3");
    assert_eq!(visto.autorizacion, "");

    api.abort();
    servidor.abort();
}

#[tokio::test]
async fn voice_speak_recorta_a_6000_runas() {
    let visto = Arc::new(Mutex::new(VistoTts::default()));
    let (tts_url, servidor) = servidor_tts(Arc::clone(&visto)).await;
    let app = AppDePrueba::nuevo()
        .con_settings(ajustes(String::new(), tts_url))
        .construir();
    let (base, api) = levantar(app);

    let texto = "á".repeat(6001);
    let respuesta = reqwest::Client::new()
        .post(format!("{base}/api/agent/speak"))
        .json(&serde_json::json!({"text": texto}))
        .send()
        .await
        .expect("POST speak largo");
    assert_eq!(respuesta.status(), 200);

    let visto = visto.lock().expect("visto tts");
    let enviado = visto.cuerpo["input"].as_str().expect("input");
    assert_eq!(enviado.chars().count(), 6000);
    assert_eq!(enviado.len(), 12_000, "6000 runas de dos bytes");

    api.abort();
    servidor.abort();
}

#[tokio::test]
async fn voice_speak_valida_el_texto_antes_de_mirar_la_url() {
    let app = AppDePrueba::nuevo()
        .con_settings(ajustes(String::new(), String::new()))
        .construir();
    let (base, api) = levantar(app);
    let cliente = reqwest::Client::new();

    for cuerpo in ["", "{}", "{\"text\":null}", "null", "{\"text\":\"   \"}"] {
        let respuesta = cliente
            .post(format!("{base}/api/agent/speak"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(cuerpo)
            .send()
            .await
            .expect("POST speak vacío");
        assert_eq!(respuesta.status(), 400, "cuerpo: {cuerpo:?}");
        let valor: Value = respuesta.json().await.expect("error de texto");
        assert_eq!(
            valor["error"], "no hay texto para leer",
            "cuerpo: {cuerpo:?}"
        );
    }

    api.abort();
}

#[tokio::test]
async fn voice_speak_con_texto_sin_url_avisa_la_configuracion() {
    let app = AppDePrueba::nuevo()
        .con_settings(ajustes(String::new(), String::new()))
        .construir();
    let (base, api) = levantar(app);

    let respuesta = reqwest::Client::new()
        .post(format!("{base}/api/agent/speak"))
        .json(&serde_json::json!({"text": "hola"}))
        .send()
        .await
        .expect("POST speak sin URL");
    assert_eq!(respuesta.status(), 400);
    let cuerpo: Value = respuesta.json().await.expect("error de voz");
    assert_eq!(
        cuerpo["error"],
        "la lectura en voz alta no está configurada: abre Ajustes del chat y escribe la dirección de tu servidor de voz"
    );

    api.abort();
}

#[tokio::test]
async fn voice_speak_pasado_de_1_mb_es_413_cuerpo_grande() {
    let app = AppDePrueba::nuevo()
        .con_settings(ajustes(String::new(), String::new()))
        .construir();
    let (base, api) = levantar(app);

    let cuerpo = format!("{{\"text\":\"{}\"}}", "a".repeat(1 << 20));
    let respuesta = reqwest::Client::new()
        .post(format!("{base}/api/agent/speak"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(cuerpo)
        .send()
        .await
        .expect("POST speak grande");
    assert_eq!(respuesta.status(), 413);
    let valor: Value = respuesta.json().await.expect("error de cuerpo grande");
    assert_eq!(
        valor["error"],
        "el cuerpo de la petición pasa del máximo de la ruta"
    );
    assert_eq!(valor["code"], "cuerpo_grande");

    api.abort();
}

#[tokio::test]
async fn voice_speak_usa_audio_mpeg_si_el_servidor_no_manda_tipo() {
    let (tts_url, servidor) = servidor_tts_mudo().await;
    let app = AppDePrueba::nuevo()
        .con_settings(ajustes(String::new(), format!("{tts_url}/sin-tipo")))
        .construir();
    let (base, api) = levantar(app);

    let respuesta = reqwest::Client::new()
        .post(format!("{base}/api/agent/speak"))
        .json(&serde_json::json!({"text": "hola"}))
        .send()
        .await
        .expect("POST speak sin tipo");
    assert_eq!(respuesta.status(), 200);
    assert_eq!(
        respuesta
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|valor| valor.to_str().ok()),
        Some("audio/mpeg")
    );

    api.abort();
    servidor.abort();
}

#[tokio::test]
async fn voice_speak_rechaza_la_respuesta_pasada_de_8_mb() {
    let (tts_url, servidor) = servidor_tts_mudo().await;
    let app = AppDePrueba::nuevo()
        .con_settings(ajustes(String::new(), format!("{tts_url}/grande")))
        .construir();
    let (base, api) = levantar(app);

    let respuesta = reqwest::Client::new()
        .post(format!("{base}/api/agent/speak"))
        .json(&serde_json::json!({"text": "hola"}))
        .send()
        .await
        .expect("POST speak grande");
    assert_ne!(respuesta.status(), 200, "no se entrega audio truncado");
    let cuerpo: Value = respuesta.json().await.expect("error de la voz");
    assert!(
        cuerpo["error"]
            .as_str()
            .unwrap_or_default()
            .contains("pasa de 8 MB"),
        "el error explica el tope: {cuerpo}"
    );

    api.abort();
    servidor.abort();
}

#[tokio::test]
async fn voice_speak_no_sigue_redirects() {
    async fn destino() -> &'static str {
        "no debería llegar"
    }
    let golpes = Arc::new(AtomicUsize::new(0));
    let contador = Arc::clone(&golpes);
    let app = Router::new()
        .route(
            "/audio/speech",
            post(|| async {
                (
                    StatusCode::FOUND,
                    [(header::LOCATION, "/destino")],
                    "redirigido",
                )
            }),
        )
        .route(
            "/destino",
            get(move || {
                let contador = Arc::clone(&contador);
                async move {
                    contador.fetch_add(1, Ordering::Relaxed);
                    destino().await
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener redirect");
    let direccion = listener.local_addr().expect("dirección redirect");
    let servidor = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("servidor redirect");
    });

    let app = AppDePrueba::nuevo()
        .con_settings(ajustes(String::new(), format!("http://{direccion}")))
        .construir();
    let (base, api) = levantar(app);
    let respuesta = reqwest::Client::new()
        .post(format!("{base}/api/agent/speak"))
        .json(&serde_json::json!({"text": "hola"}))
        .send()
        .await
        .expect("POST speak redirigido");
    assert_eq!(respuesta.status(), 500);
    let cuerpo: Value = respuesta.json().await.expect("error de redirect");
    assert_eq!(cuerpo["error"], "el servidor de voz respondió 302");
    assert_eq!(golpes.load(Ordering::Relaxed), 0);

    api.abort();
    servidor.abort();
}

#[tokio::test]
async fn voice_speak_con_servidor_caido_envuelve_el_error_de_red() {
    let app = AppDePrueba::nuevo()
        .con_settings(ajustes(String::new(), "http://127.0.0.1:1".to_owned()))
        .construir();
    let (base, api) = levantar(app);

    let respuesta = reqwest::Client::new()
        .post(format!("{base}/api/agent/speak"))
        .json(&serde_json::json!({"text": "hola"}))
        .send()
        .await
        .expect("POST speak caído");
    assert_eq!(respuesta.status(), 500);
    let cuerpo: Value = respuesta.json().await.expect("error de red");
    assert!(
        cuerpo["error"]
            .as_str()
            .is_some_and(|texto| texto.starts_with("no se pudo hablar con el servidor de voz: ")),
        "error inesperado: {cuerpo}"
    );

    api.abort();
}

#[tokio::test]
async fn voice_transcribe_sin_url_avisa_la_configuracion() {
    let app = AppDePrueba::nuevo()
        .con_settings(ajustes(String::new(), String::new()))
        .construir();
    let (base, api) = levantar(app);

    let formulario = reqwest::multipart::Form::new().part(
        "audio",
        reqwest::multipart::Part::bytes(b"audio".to_vec()).file_name("dictado.wav"),
    );
    let respuesta = reqwest::Client::new()
        .post(format!("{base}/api/agent/transcribe"))
        .multipart(formulario)
        .send()
        .await
        .expect("POST transcribe sin url");
    assert_eq!(respuesta.status(), 400);
    let cuerpo = cuerpo_json(respuesta).await;
    assert_eq!(
        cuerpo["error"],
        "el dictado no está configurado: abre Ajustes del chat y escribe la dirección de tu servidor de voz"
    );

    api.abort();
}
