//! Las tres rutas de actualización: `GET /api/update`, `POST /api/update/apply`
//! y `GET /api/update/progress`.
//!
//! La implementación real (elegir `ORUSCRIBE_UPDATE_URL` o la URL de la app,
//! descargar a `<exe>.new`, verificar SHA, hacer el swap y programar
//! la acción posterior) vive en el borde que inyecta el trait; acá solo se escriben
//! códigos y textos.

use std::sync::Arc;

use async_trait::async_trait;
use axum::Extension;
use axum::http::StatusCode;
use axum::response::Response;
use orus_install::UpdateInfo;
use serde::Serialize;

use crate::{error_de_accion, write_json};

/// Fase de la actualización que la ventana puede mostrar.
///
/// Es un enum y no texto libre: una fase que no existe no compila. El JSON la
/// publica como cadena ([`Fase::as_str`]) para no romper al cliente.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fase {
    Idle,
    Checking,
    Downloading,
    Verifying,
    Applying,
    Restarting,
    Error,
}

impl Fase {
    pub fn as_str(self) -> &'static str {
        match self {
            Fase::Idle => "idle",
            Fase::Checking => "checking",
            Fase::Downloading => "downloading",
            Fase::Verifying => "verifying",
            Fase::Applying => "applying",
            Fase::Restarting => "restarting",
            Fase::Error => "error",
        }
    }
}

/// Avance de la actualización tal como lo lee `GET /api/update/progress`.
///
/// `total` es 0 cuando el servidor no mandó `Content-Length`; `error` queda
/// vacío salvo en la fase `error`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Progreso {
    pub phase: String,
    pub received: u64,
    pub total: u64,
    pub error: String,
}

impl Progreso {
    /// Estado en reposo: sin actualización en curso.
    pub fn nueva(fase: Fase) -> Self {
        Self {
            phase: fase.as_str().to_owned(),
            received: 0,
            total: 0,
            error: String::new(),
        }
    }

    /// Fase `error` con el texto que se le muestra a la persona.
    pub fn fallo(mensaje: impl Into<String>) -> Self {
        Self {
            phase: Fase::Error.as_str().to_owned(),
            received: 0,
            total: 0,
            error: mensaje.into(),
        }
    }
}

impl Default for Progreso {
    fn default() -> Self {
        Self::nueva(Fase::Idle)
    }
}

/// Lo que el borde debe poder hacer para que existan las rutas de update.
///
/// `check_update` usa `ORUSCRIBE_UPDATE_URL` o la URL de la app. Sin URL
/// devuelve `configured:false`, `available:false`, `current` y las claves
/// vacías se omiten. Un fallo de red, firma, SHA o URL es `Err` con el texto
/// exacto del contrato.
///
/// `apply_update` toma el candado de "una actualización a la vez", exige una
/// versión nueva, descarga a `<exe>.new` (o al `.tar.gz` y extrae, en Linux),
/// verifica, hace el swap y programa la acción posterior 1,2 s después **dentro de
/// la implementación** (tokio spawn + sleep, no en el handler). Un segundo
/// clic responde "ya hay una actualización en curso"; sin versión nueva, "no
/// hay una versión nueva para descargar".
///
/// `progreso` es la foto del avance; se lee sin bloquear la descarga.
#[async_trait]
pub trait Update: Send + Sync {
    async fn check_update(&self) -> Result<UpdateInfo, String>;
    async fn apply_update(&self) -> Result<(), String>;
    fn progreso(&self) -> Progreso;
}

/// La costura de un borde que todavía no conectó la actualización: 500 con un
/// texto claro, nunca 404 ni un "al día" inventado.
pub(crate) struct SinActualizador;

#[async_trait]
impl Update for SinActualizador {
    async fn check_update(&self) -> Result<UpdateInfo, String> {
        Err("la actualización no está conectada en este borde".to_owned())
    }

    async fn apply_update(&self) -> Result<(), String> {
        Err("la actualización no está conectada en este borde".to_owned())
    }

    fn progreso(&self) -> Progreso {
        Progreso::default()
    }
}

pub(crate) async fn leer(Extension(update): Extension<Arc<dyn Update>>) -> Response {
    match update.check_update().await {
        Ok(info) => write_json(StatusCode::OK, &info),
        Err(error) => error_de_accion(error),
    }
}

/// `POST /api/update/apply` no lee el cuerpo: el handler
/// no recibe `Body` y la petición se contesta apenas termina la acción.
pub(crate) async fn aplicar(Extension(update): Extension<Arc<dyn Update>>) -> Response {
    match update.apply_update().await {
        Ok(()) => write_json(StatusCode::OK, &serde_json::json!({"ok": true})),
        Err(error) => error_de_accion(error),
    }
}

/// `GET /api/update/progress`: siempre 200, también en fase `error`; el fallo
/// va en el campo `error` para que la ventana no confunda "no se pudo" con
/// "no hay ruta".
pub(crate) async fn progreso(Extension(update): Extension<Arc<dyn Update>>) -> Response {
    write_json(StatusCode::OK, &update.progreso())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[tokio::test]
    async fn sin_actualizador_responde_500() {
        let actualizador: Arc<dyn Update> = Arc::new(SinActualizador);
        let respuesta = leer(Extension(actualizador)).await;

        assert_eq!(respuesta.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let cuerpo = axum::body::to_bytes(respuesta.into_body(), 1024)
            .await
            .unwrap_or_default();
        let texto = String::from_utf8_lossy(&cuerpo);
        assert!(
            texto.contains("no está conectada"),
            "el texto no explica la falta de costura: {texto}"
        );
    }

    /// Las fases son las que el cliente ya conoce; si alguien agrega una, el
    /// test falla y obliga a decidir qué muestra la ventana.
    #[test]
    fn las_fases_tienen_los_nombres_del_contrato() {
        let fases = [
            (Fase::Idle, "idle"),
            (Fase::Checking, "checking"),
            (Fase::Downloading, "downloading"),
            (Fase::Verifying, "verifying"),
            (Fase::Applying, "applying"),
            (Fase::Restarting, "restarting"),
            (Fase::Error, "error"),
        ];
        for (fase, nombre) in fases {
            assert_eq!(fase.as_str(), nombre);
            assert_eq!(Progreso::nueva(fase).phase, nombre);
        }
    }

    #[test]
    fn el_progreso_en_reposo_serializa_las_cuatro_claves() {
        let valor = serde_json::to_value(Progreso::default()).expect("serializar");
        assert_eq!(
            valor,
            serde_json::json!({"phase": "idle", "received": 0, "total": 0, "error": ""})
        );
    }

    #[tokio::test]
    async fn la_ruta_de_progreso_devuelve_la_foto_del_actualizador() {
        let actualizador: Arc<dyn Update> = Arc::new(SinActualizador);
        let respuesta = progreso(Extension(actualizador)).await;

        assert_eq!(respuesta.status(), StatusCode::OK);
        let cuerpo = axum::body::to_bytes(respuesta.into_body(), 1024)
            .await
            .unwrap_or_default();
        let valor: serde_json::Value = serde_json::from_slice(&cuerpo).expect("json");
        assert_eq!(valor["phase"], "idle");
    }
}
