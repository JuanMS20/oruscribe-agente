//! El cerebro de la organización en `turno_para`: base desde el timbre, token
//! de la sesión en cada llamada y `POST /functions/v1/chat-agent`.
//!
//! El servidor es un `TcpListener` de std: no hace falta otra dependencia y el
//! test no toca la red real ni el llavero del sistema (R12).

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::thread::JoinHandle;
use std::time::Duration;

use async_trait::async_trait;
use orus_app::{
    AgentError, AgentInput, App, Backend, ChatMessage, ChatRequest, Connector, Refresh,
    SessionError,
};
use orus_auth::{Error as AuthError, Store, Token};
use orus_config::Settings;
use orus_org::doorbell::Config as DoorbellConfig;
use orus_org::{JsonObject, Me, MessagesPage, Room, Task, ToolSpec};
use serde_json::json;

const JWT_PRUEBA: &str = "eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJ0ZXN0In0.firma-prueba";

fn token_de_prueba() -> Token {
    serde_json::from_value(json!({"access_token": JWT_PRUEBA})).expect("token de prueba")
}

struct ConSesion;

impl Store for ConSesion {
    fn save(&self, _: Token) -> Result<(), AuthError> {
        Ok(())
    }

    fn load(&self) -> Result<Token, AuthError> {
        Ok(token_de_prueba())
    }

    fn clear(&self) -> Result<(), AuthError> {
        Ok(())
    }
}

struct ConTokenSyn;

impl Store for ConTokenSyn {
    fn save(&self, _: Token) -> Result<(), AuthError> {
        Ok(())
    }

    fn load(&self) -> Result<Token, AuthError> {
        Ok(
            serde_json::from_value(json!({"access_token": "syn_secreto"}))
                .expect("token syn de prueba"),
        )
    }

    fn clear(&self) -> Result<(), AuthError> {
        Ok(())
    }
}

struct SinRed;

#[async_trait]
impl Refresh for SinRed {
    async fn refresh(&self, _: &Token) -> Result<Token, SessionError> {
        Err(SessionError::RefreshRejected)
    }
}

struct BackendTimbre {
    base: String,
}

#[async_trait]
impl Backend for BackendTimbre {
    async fn me(&self) -> Result<Me, SessionError> {
        Ok(Me::default())
    }

    async fn tasks(&self) -> Result<Vec<Task>, SessionError> {
        Ok(Vec::new())
    }

    async fn rooms(&self) -> Result<Vec<Room>, SessionError> {
        Ok(Vec::new())
    }

    async fn messages(&self, _: &str, _: i64, _: i64) -> Result<MessagesPage, SessionError> {
        Ok(MessagesPage::default())
    }

    async fn send(&self, _: &str, _: &str, _: &str) -> Result<(), SessionError> {
        Ok(())
    }

    async fn tool_specs(&self) -> Result<Vec<ToolSpec>, SessionError> {
        Ok(Vec::new())
    }

    async fn call_tool(&self, _: &str, _: JsonObject) -> Result<String, SessionError> {
        Ok(String::new())
    }

    async fn doorbell(&self) -> Result<DoorbellConfig, SessionError> {
        Ok(DoorbellConfig {
            supabase_url: self.base.clone(),
            ..DoorbellConfig::default()
        })
    }
}

struct ConectorTimbre {
    base: String,
}

#[async_trait]
impl Connector for ConectorTimbre {
    async fn open(&self, _: &str, _: &str, _: &str) -> Result<Arc<dyn Backend>, SessionError> {
        Ok(Arc::new(BackendTimbre {
            base: self.base.clone(),
        }))
    }
}

/// Acepta una petición HTTP/1.1, la entrega al test y contesta el cerebro.
fn servidor(
    respuesta: &'static str,
    estado: &'static str,
) -> (String, Receiver<String>, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("el servidor de prueba escucha");
    let direccion = listener.local_addr().expect("dirección del servidor");
    let (envio, recibo) = std::sync::mpsc::channel();
    let hilo = std::thread::spawn(move || {
        let (mut flujo, _) = listener.accept().expect("aceptar la petición");
        flujo
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("tiempo de lectura");
        let mut datos = Vec::new();
        let mut buffer = [0u8; 4096];
        let fin = loop {
            let leidos = flujo.read(&mut buffer).expect("leer la petición");
            if leidos == 0 {
                panic!("la petición se cortó antes de las cabeceras");
            }
            datos.extend_from_slice(&buffer[..leidos]);
            if let Some(posicion) = datos.windows(4).position(|trozo| trozo == b"\r\n\r\n") {
                break posicion + 4;
            }
        };
        let cabeceras = String::from_utf8_lossy(&datos[..fin]).into_owned();
        let largo = cabeceras
            .lines()
            .find_map(|linea| {
                linea
                    .to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .and_then(|valor| valor.trim().parse::<usize>().ok())
            })
            .unwrap_or(0);
        while datos.len() < fin + largo {
            let leidos = flujo.read(&mut buffer).expect("leer el cuerpo");
            if leidos == 0 {
                break;
            }
            datos.extend_from_slice(&buffer[..leidos]);
        }
        envio.send(cabeceras).expect("entregar la petición al test");
        let respuesta = format!(
            "{estado}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{respuesta}",
            respuesta.len()
        );
        flujo
            .write_all(respuesta.as_bytes())
            .expect("escribir la respuesta");
        flujo.flush().expect("volcar la respuesta");
    });
    (format!("http://{direccion}"), recibo, hilo)
}

fn app_con(base: &str) -> App {
    App::new(
        Box::new(ConSesion),
        Settings::default(),
        Arc::new(ConectorTimbre {
            base: base.to_owned(),
        }),
        Arc::new(SinRed),
    )
}

#[tokio::test]
async fn org_pide_la_base_al_timbre_y_manda_el_token_a_chat_agent() {
    let (base, peticiones, hilo) = servidor(
        r#"{"content":"hola de vuelta","steps":[]}"#,
        "HTTP/1.1 200 OK",
    );
    let app = app_con(&base);

    let turno = app
        .turno_para("hola")
        .await
        .expect("el cerebro de la organización arma el turno");
    assert_eq!(turno.base_url, format!("{base}/functions/v1"));
    assert_eq!(
        turno.modelo, "",
        "el cerebro elige el modelo del lado servidor"
    );
    assert_eq!(turno.clave, "", "el cerebro no usa la clave del proveedor");
    assert!(
        turno.cerebro,
        "el cerebro no arma locales, instrucciones ni skills"
    );
    assert!(
        turno.org_tools.is_empty(),
        "el cerebro trae sus herramientas"
    );
    let proveedor = turno
        .proveedor
        .clone()
        .expect("el turno org lleva su proveedor");

    let respuesta = proveedor
        .chat(ChatRequest {
            model: String::new(),
            messages: vec![ChatMessage {
                role: "user".to_owned(),
                content: "hola".to_owned(),
                ..ChatMessage::default()
            }],
            tools: Vec::new(),
            on_delta: None,
        })
        .await
        .expect("chat-agent responde");
    assert_eq!(respuesta.message.content, "hola de vuelta");

    let peticion = peticiones
        .recv_timeout(Duration::from_secs(5))
        .expect("el servidor recibió la petición");
    assert!(
        peticion.starts_with("POST /functions/v1/chat-agent HTTP/1.1"),
        "la base del timbre arma la URL: {peticion}"
    );
    assert!(
        peticion.to_lowercase().contains(&format!(
            "authorization: bearer {}",
            JWT_PRUEBA.to_lowercase()
        )),
        "el token de la sesión viaja en cada llamada: {peticion}"
    );
    hilo.join().expect("el servidor de prueba termina");
}

#[tokio::test]
async fn org_sin_supabase_devuelve_el_400_de_sin_conexion() {
    let app = app_con("");

    let error = match app.turno_para("hola").await {
        Ok(_) => panic!("sin base del timbre no hay cerebro"),
        Err(error) => error,
    };
    assert_eq!(
        error.to_string(),
        "aún no hay conexión con Oruscribe: espera a estar en línea y vuelve a intentar"
    );
}

#[tokio::test]
async fn org_sin_sesion_dice_que_hay_que_entrar_y_no_que_falta_red() {
    struct SinSesion;

    impl Store for SinSesion {
        fn save(&self, _: Token) -> Result<(), AuthError> {
            Ok(())
        }

        fn load(&self) -> Result<Token, AuthError> {
            Err(AuthError::NoSession)
        }

        fn clear(&self) -> Result<(), AuthError> {
            Ok(())
        }
    }

    let app = App::new(
        Box::new(SinSesion),
        Settings::default(),
        Arc::new(ConectorTimbre {
            base: "http://127.0.0.1:1".to_owned(),
        }),
        Arc::new(SinRed),
    );

    let error = match app.turno_para("hola").await {
        Ok(_) => panic!("sin sesión no hay turno"),
        Err(error) => error,
    };

    assert!(
        matches!(error, AgentError::Sesion(SessionError::LoggedOut)),
        "una sesión ausente debe mandar al login, no culpar a la red: {error:?}"
    );
}

#[tokio::test]
async fn probar_el_cerebro_con_token_syn_no_lo_manda_y_dice_como_entrar() {
    let app = App::new(
        Box::new(ConTokenSyn),
        Settings::default(),
        Arc::new(ConectorTimbre {
            base: "http://127.0.0.1:1".to_owned(),
        }),
        Arc::new(SinRed),
    );
    app.set_bell_config(DoorbellConfig {
        supabase_url: "http://127.0.0.1:1".to_owned(),
        ..DoorbellConfig::default()
    });

    let probe = app
        .agent_test(AgentInput {
            kind: "org".to_owned(),
            ..AgentInput::default()
        })
        .await
        .expect("probar no debe fallar la ruta");

    assert!(
        !probe.ok,
        "un token syn_ no puede pasar la prueba del cerebro: {probe:?}"
    );
    assert!(
        probe.error.contains("este token no sirve para el cerebro"),
        "la prueba debe decir cómo entrar en vez de culpar al proveedor: {:?}",
        probe.error
    );
}

#[tokio::test]
async fn org_con_token_syn_no_manda_bearer_y_dice_como_entrar() {
    let app = App::new(
        Box::new(ConTokenSyn),
        Settings::default(),
        Arc::new(ConectorTimbre {
            base: "http://127.0.0.1:1".to_owned(),
        }),
        Arc::new(SinRed),
    );

    let error = match app.turno_para("hola").await {
        Ok(_) => panic!("un token syn_ no puede llegar al cerebro como Bearer"),
        Err(error) => error,
    };
    assert_eq!(
        error.to_string(),
        "este token no sirve para el cerebro: inicia sesión con el navegador y vuelve a intentar"
    );
}
