//! Las tres rutas del instalador: `GET /api/setup`,
//! `POST /api/setup/install` y `POST /api/setup/uninstall`.
//!
//! Las acciones reales (instalar, desinstalar,
//! guardar la aceptación y programar el cierre) viven en el borde que inyecta
//! el trait; acá solo se leen cuerpos, se valida el sobre y se escriben códigos,
//! cabeceras y textos.

use std::sync::{Arc, Mutex, MutexGuard};

use axum::Extension;
use axum::body::Body;
use axum::http::StatusCode;
use axum::response::Response;
use orus_install::SetupInfo;
use serde::Deserialize;

use crate::{decode, error_de_accion, write_err, write_json};

/// Lo que el borde debe poder hacer para que existan las rutas de setup.
///
/// `setup_info` no escribe nada. `install` exige `terms_version` exacta,
/// instala, actualiza `settings.terms_version` en memoria, guarda y programa
/// la acción posterior a la instalación 1,2 s después **dentro de la implementación** (tokio spawn +
/// sleep, no en el handler). `uninstall` hace `Logout()` ignorando el error,
/// apaga el autostart si se puede, desinstala y programa la acción posterior a la desinstalación de la
/// misma forma. Los errores se devuelven con el texto exacto del contrato y el borde
/// los convierte en 500.
pub trait Setup: Send + Sync {
    fn setup_info(&self) -> Result<SetupInfo, String>;
    fn install(&self, autostart: bool, terms_version: &str) -> Result<(), String>;
    fn uninstall(&self, remove_data: bool) -> Result<(), String>;
}

static CANDADO: Mutex<()> = Mutex::new(());

fn candado() -> MutexGuard<'static, ()> {
    CANDADO
        .lock()
        .unwrap_or_else(|envenenado| envenenado.into_inner())
}

/// La costura de un borde que todavía no conectó el instalador: 500 con un
/// texto claro, nunca 404 ni un éxito inventado.
pub(crate) struct SinInstalador;

impl Setup for SinInstalador {
    fn setup_info(&self) -> Result<SetupInfo, String> {
        Err("el instalador no está conectado en este borde".to_owned())
    }

    fn install(&self, _: bool, _: &str) -> Result<(), String> {
        Err("el instalador no está conectado en este borde".to_owned())
    }

    fn uninstall(&self, _: bool) -> Result<(), String> {
        Err("el instalador no está conectado en este borde".to_owned())
    }
}

/// El cuerpo de `POST /api/setup/install`. En el contrato un `null`
/// deja el valor cero; por eso los dos campos son opcionales.
#[derive(Deserialize, Default)]
#[serde(default)]
struct CuerpoInstalar {
    autostart: Option<bool>,
    #[serde(rename = "termsVersion")]
    terms_version: Option<String>,
}

/// El cuerpo de `POST /api/setup/uninstall`: solo un
/// `false` explícito autoriza borrar los datos.
#[derive(Deserialize, Default)]
#[serde(default)]
struct CuerpoDesinstalar {
    #[serde(rename = "keepData")]
    keep_data: Option<bool>,
}

pub(crate) async fn leer(Extension(setup): Extension<Arc<dyn Setup>>) -> Response {
    match setup.setup_info() {
        Ok(info) => write_json(StatusCode::OK, &info),
        Err(error) => error_de_accion(error),
    }
}

pub(crate) async fn instalar(Extension(setup): Extension<Arc<dyn Setup>>, body: Body) -> Response {
    let cuerpo: Option<CuerpoInstalar> = match decode(body).await {
        Ok(cuerpo) => cuerpo,
        Err(error) => return write_err(error),
    };
    let cuerpo = cuerpo.unwrap_or_default();
    let autostart = cuerpo.autostart.unwrap_or(false);
    let terms_version = cuerpo.terms_version.unwrap_or_default();
    // Instalar copia el exe y toca el registro: IO síncrono pesado que va a
    // un hilo bloqueante, no al worker async (#95).
    let resultado = tokio::task::spawn_blocking(move || {
        let _guardia = candado();
        setup.install(autostart, &terms_version)
    })
    .await
    .map_err(|error| error.to_string())
    .and_then(|resultado| resultado);
    match resultado {
        Ok(()) => write_json(
            StatusCode::OK,
            &serde_json::json!({"ok": true, "restarting": true}),
        ),
        Err(error) => error_de_accion(error),
    }
}

pub(crate) async fn desinstalar(
    Extension(setup): Extension<Arc<dyn Setup>>,
    body: Body,
) -> Response {
    let cuerpo: Option<CuerpoDesinstalar> = match decode(body).await {
        Ok(cuerpo) => cuerpo,
        Err(error) => return write_err(error),
    };
    let cuerpo = cuerpo.unwrap_or_default();
    let remove_data = cuerpo.keep_data == Some(false);
    // Desinstalar borra sesión y archivos: también va a un hilo bloqueante
    // para no frenar el resto de la API (#95).
    let resultado = tokio::task::spawn_blocking(move || {
        let _guardia = candado();
        setup.uninstall(remove_data)
    })
    .await
    .map_err(|error| error.to_string())
    .and_then(|resultado| resultado);
    match resultado {
        Ok(()) => write_json(StatusCode::OK, &serde_json::json!({"ok": true})),
        Err(error) => error_de_accion(error),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use super::*;

    #[tokio::test]
    async fn sin_instalador_responde_500() {
        let instalador: Arc<dyn Setup> = Arc::new(SinInstalador);
        let respuesta = leer(Extension(instalador)).await;

        assert_eq!(respuesta.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let cuerpo = axum::body::to_bytes(respuesta.into_body(), 1024)
            .await
            .unwrap_or_default();
        let texto = String::from_utf8_lossy(&cuerpo);
        assert!(
            texto.contains("no está conectado"),
            "el texto no explica la falta de costura: {texto}"
        );
    }

    /// Un instalador que espera a que el worker async quede libre antes de
    /// terminar. Si la acción corre síncrona en el worker, el testigo nunca
    /// llega a correr y la espera se agota (#95).
    struct InstaladorQueEspera {
        iniciado: Arc<AtomicBool>,
        libre: Arc<AtomicBool>,
        libre_al_terminar: Arc<AtomicBool>,
    }

    impl InstaladorQueEspera {
        fn esperar_libre(&self) {
            self.iniciado.store(true, Ordering::SeqCst);
            let limite = Instant::now() + Duration::from_secs(2);
            while !self.libre.load(Ordering::SeqCst) && Instant::now() < limite {
                std::thread::sleep(Duration::from_millis(1));
            }
            self.libre_al_terminar
                .store(self.libre.load(Ordering::SeqCst), Ordering::SeqCst);
        }
    }

    impl Setup for InstaladorQueEspera {
        fn setup_info(&self) -> Result<SetupInfo, String> {
            Err("no usado en este test".to_owned())
        }

        fn install(&self, _: bool, _: &str) -> Result<(), String> {
            self.esperar_libre();
            Ok(())
        }

        fn uninstall(&self, _: bool) -> Result<(), String> {
            self.esperar_libre();
            Ok(())
        }
    }

    fn instalador_que_espera() -> (
        Arc<dyn Setup>,
        Arc<AtomicBool>,
        Arc<AtomicBool>,
        Arc<AtomicBool>,
    ) {
        let iniciado = Arc::new(AtomicBool::new(false));
        let libre = Arc::new(AtomicBool::new(false));
        let libre_al_terminar = Arc::new(AtomicBool::new(false));
        let setup: Arc<dyn Setup> = Arc::new(InstaladorQueEspera {
            iniciado: Arc::clone(&iniciado),
            libre: Arc::clone(&libre),
            libre_al_terminar: Arc::clone(&libre_al_terminar),
        });
        (setup, iniciado, libre, libre_al_terminar)
    }

    async fn testigo_de_worker(
        iniciado: Arc<AtomicBool>,
        libre: Arc<AtomicBool>,
        fin: Arc<AtomicBool>,
    ) {
        while !fin.load(Ordering::SeqCst) {
            if iniciado.load(Ordering::SeqCst) {
                libre.store(true, Ordering::SeqCst);
            }
            tokio::task::yield_now().await;
        }
    }

    /// #95: instalar hace IO síncrono pesado; tiene que correr en un hilo
    /// bloqueante, no en el worker async que atiende el resto de la API.
    #[tokio::test]
    async fn instalar_no_bloquea_el_worker_async() {
        let (setup, iniciado, libre, libre_al_terminar) = instalador_que_espera();
        let fin = Arc::new(AtomicBool::new(false));
        let handler = tokio::spawn({
            let setup = Arc::clone(&setup);
            let fin = Arc::clone(&fin);
            async move {
                let respuesta = instalar(Extension(setup), Body::empty()).await;
                fin.store(true, Ordering::SeqCst);
                respuesta
            }
        });
        let testigo = tokio::spawn(testigo_de_worker(
            iniciado,
            Arc::clone(&libre),
            Arc::clone(&fin),
        ));
        let respuesta = handler.await.expect("el handler termina");
        testigo.await.expect("el testigo termina");
        assert_eq!(respuesta.status(), StatusCode::OK);
        assert!(
            libre_al_terminar.load(Ordering::SeqCst),
            "install bloqueó el worker async hasta terminar"
        );
    }

    /// #95: desinstalar también hace IO síncrono (borrar sesión y archivos).
    #[tokio::test]
    async fn desinstalar_no_bloquea_el_worker_async() {
        let (setup, iniciado, libre, libre_al_terminar) = instalador_que_espera();
        let fin = Arc::new(AtomicBool::new(false));
        let handler = tokio::spawn({
            let setup = Arc::clone(&setup);
            let fin = Arc::clone(&fin);
            async move {
                let respuesta = desinstalar(Extension(setup), Body::empty()).await;
                fin.store(true, Ordering::SeqCst);
                respuesta
            }
        });
        let testigo = tokio::spawn(testigo_de_worker(
            iniciado,
            Arc::clone(&libre),
            Arc::clone(&fin),
        ));
        let respuesta = handler.await.expect("el handler termina");
        testigo.await.expect("el testigo termina");
        assert_eq!(respuesta.status(), StatusCode::OK);
        assert!(
            libre_al_terminar.load(Ordering::SeqCst),
            "uninstall bloqueó el worker async hasta terminar"
        );
    }
}
