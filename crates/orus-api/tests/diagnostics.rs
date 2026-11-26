//! `GET /api/diagnostics`.
//!
//! El texto no sale del equipo y no lleva nombres ni claves. Estos tests fijan
//! las líneas, el orden y la redacción; el error crudo de sesión se arma con
//! una conexión fallida real.

mod soporte;

use std::sync::Arc;
use std::time::Duration;

use orus_api::{Api, BellStatus};
use orus_app::{App, Backend, Connector, Refresh, Secrets, SessionError};
use orus_auth::{Error as AuthError, Store, Token};
use orus_config::Settings;
use serde_json::Value;
use soporte::{AppDePrueba, con_runtime, levantar_api as levantar, token_de_prueba};

struct StoreConToken;

impl Store for StoreConToken {
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

struct SinRed(String);

#[async_trait::async_trait]
impl Connector for SinRed {
    async fn open(&self, _: &str, _: &str, _: &str) -> Result<Arc<dyn Backend>, SessionError> {
        Err(SessionError::Connect(self.0.clone()))
    }
}

#[async_trait::async_trait]
impl Refresh for SinRed {
    async fn refresh(&self, _: &Token) -> Result<Token, SessionError> {
        Err(SessionError::RefreshRejected)
    }
}

struct Timbre {
    error: String,
}

impl BellStatus for Timbre {
    fn doorbell(&self) -> bool {
        true
    }

    fn error(&self) -> String {
        self.error.clone()
    }
}

struct SecretosRoto;

impl Secrets for SecretosRoto {
    fn get(&self) -> Result<Option<String>, String> {
        Err("llavero roto".to_owned())
    }

    fn set(&self, _: &str) -> Result<(), String> {
        Ok(())
    }

    fn delete(&self) -> Result<(), String> {
        Ok(())
    }
}

/// Una app que falla al conectar: deja `Estado::last_error` con el texto de
/// `SinRed` y `status = failed`, como la restauración del contrato.
async fn app_fallida(error: &str) -> App {
    let mut app = App::new(
        Box::new(StoreConToken),
        Settings::default(),
        Arc::new(SinRed(error.to_owned())),
        Arc::new(SinRed(error.to_owned())),
    );
    app.set_connect_backoff(Duration::ZERO);
    let _ = app.client().await;
    app
}

fn api(app: App, version: &str, timbre: Option<Arc<dyn BellStatus>>) -> Api {
    let api = Api::new(Arc::new(app)).with_version(version.to_owned());
    match timbre {
        Some(timbre) => api.with_bell_status(timbre),
        None => api,
    }
}

async fn texto(base: &str) -> String {
    let response = reqwest::Client::new()
        .get(format!("{base}/api/diagnostics"))
        .send()
        .await
        .expect("pedir diagnóstico");
    assert_eq!(response.status(), 200, "el diagnóstico nunca es un 500");
    assert_eq!(
        response.headers()["content-type"],
        "application/json; charset=utf-8"
    );
    assert_eq!(response.headers()["cache-control"], "no-store");
    let body: Value =
        serde_json::from_str(&response.text().await.expect("leer el cuerpo")).expect("JSON válido");
    body["text"]
        .as_str()
        .expect("texto del diagnóstico")
        .to_owned()
}

fn sistema_esperado() -> String {
    let arch = if cfg!(target_arch = "x86_64") {
        "amd64"
    } else if cfg!(target_arch = "aarch64") {
        "arm64"
    } else if cfg!(target_arch = "x86") {
        "386"
    } else {
        std::env::consts::ARCH
    };
    format!("Sistema: {}/{}", std::env::consts::OS, arch)
}

#[test]
fn diagnostico_completo_con_error_de_sesion_y_timbre() {
    con_runtime(async {
        let app = app_fallida("sin red").await;
        assert_eq!(app.last_error_sesion(), "sin red");
        let (base, server) = levantar(api(
            app,
            "1.2.3",
            Some(Arc::new(Timbre {
                error: "timbre roto".to_owned(),
            })),
        ))
        .await;

        let completo = texto(&base).await;
        assert!(
            completo.contains("session.connect: sin red"),
            "el fallo de sesión debe quedar en los eventos locales: {completo}"
        );
        let texto: String = completo
            .lines()
            .filter(|linea| {
                !linea.starts_with("- [") && !linea.starts_with("Últimos eventos locales")
            })
            .map(|linea| format!("{linea}\n"))
            .collect();
        let esperado = format!(
            "Oruscribe Agente 1.2.3\n{}\nMotor: Rust\nEstado de la sesión: failed\nAvatares: 0 cargados\nTimbre: timbre roto\nÚltimo error de sesión: sin red\nAgente: en reposo\nConversación abierta: no\nDespertar: 0 eventos en cola, 0 ejecuciones hoy, 0 tokens hoy\nTelemetría: local (sin consentimiento o sin DSN configurado).\nSin telemetría: este texto no sale de este equipo salvo que lo pegues tú.\n",
            sistema_esperado()
        );
        assert_eq!(texto, esperado);
        #[cfg(windows)]
        assert!(
            texto.contains("Sistema: windows/amd64"),
            "el diagnóstico usa windows/amd64: {texto}"
        );

        server.abort();
        let _ = server.await;
    });
}

#[test]
fn diagnostico_tacha_los_secretos_del_error() {
    let clave = "sk-".to_owned() + &"a".repeat(24);
    let token = "syn_".to_owned() + &"b".repeat(16);
    let error = format!("falló con {clave} y {token}");
    con_runtime(async {
        let app = app_fallida(&error).await;
        let (base, server) = levantar(api(app, "0.0.0-test", None)).await;

        let texto = texto(&base).await;
        assert!(
            texto.contains("Último error de sesión: falló con *** y ***"),
            "el error crudo se conserva y se tacha: {texto}"
        );
        assert!(!texto.contains(&clave), "la clave salió en el diagnóstico");
        assert!(
            !texto.contains("syn_"),
            "el token salió en el diagnóstico: {texto}"
        );

        server.abort();
        let _ = server.await;
    });
}

#[test]
fn diagnostico_no_usa_el_error_del_llavero() {
    con_runtime(async {
        // El agente está en reposo y su `Agent::error()` vacío; el llavero sí
        // falla. `agent_view().last_error` traería "llavero roto" y no debe
        // aparecer (`agent.rs:1313-1318`).
        let app = AppDePrueba::nuevo()
            .con_secretos(Arc::new(SecretosRoto))
            .construir();
        let (base, server) = levantar(api(app, "0.0.0-test", None)).await;

        let texto = texto(&base).await;
        assert!(texto.contains("Agente: en reposo"), "{texto}");
        assert!(texto.contains("Conversación abierta: no"), "{texto}");
        assert!(
            !texto.contains("llavero roto"),
            "el diagnóstico mezcló el error del llavero: {texto}"
        );
        assert!(
            !texto.contains("Último error del agente"),
            "sin error del agente no va la línea: {texto}"
        );

        server.abort();
        let _ = server.await;
    });
}

#[test]
fn diagnostico_sin_agente_ni_sesion_no_inventa_lineas() {
    con_runtime(async {
        let app = App::new(
            Box::new(soporte::SinSesion),
            Settings::default(),
            Arc::new(soporte::SinRed),
            Arc::new(soporte::SinRed),
        );
        let (base, server) = levantar(api(app, "", None)).await;

        let texto = texto(&base).await;
        assert!(texto.starts_with("Oruscribe Agente \n"), "{texto}");
        assert!(texto.contains("Estado de la sesión: logged_out"), "{texto}");
        assert!(texto.contains("Timbre: sin errores"), "{texto}");
        assert!(texto.contains("Agente: en reposo"), "{texto}");
        assert!(!texto.contains("Último error de sesión"), "{texto}");
        assert!(!texto.contains("Último error del agente"), "{texto}");
        assert!(texto.ends_with("salvo que lo pegues tú.\n"), "{texto}");

        server.abort();
        let _ = server.await;
    });
}
