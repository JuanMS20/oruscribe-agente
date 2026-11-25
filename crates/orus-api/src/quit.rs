//! La ruta `POST /api/quit`: termina el proceso después de responder.
//!
//! Antes solo se podía salir desde el menú de la bandeja (`Aviso::Salir`). La
//! ventana no tenía forma de pedirlo y el cierre exigía el Administrador de
//! tareas. El cierre real lo hace el borde inyectando el trait; acá solo se
//! responde 200 y se delega en la implementación programar el cierre para que
//! la respuesta HTTP alcance a escribirse.

use std::sync::Arc;

use axum::Extension;
use axum::http::StatusCode;
use axum::response::Response;

use crate::{error_de_accion, write_json};

/// Lo que el borde debe poder hacer para que exista la ruta de salida.
///
/// `quit` programa el cierre del proceso **dentro de la implementación**
/// (thread + sleep, no en el handler) para que la respuesta HTTP 200 alcance a
/// salir antes de que el proceso termine. Un fallo se devuelve con el texto de
/// la implementación y el borde lo convierte en 500.
pub trait Quit: Send + Sync {
    fn quit(&self) -> Result<(), String>;
}

/// La costura de un borde que todavía no conectó la salida: 500 con un texto
/// claro, nunca 404 ni un éxito inventado.
pub(crate) struct SinQuit;

impl Quit for SinQuit {
    fn quit(&self) -> Result<(), String> {
        Err("la salida no está conectada en este borde".to_owned())
    }
}

/// El cuerpo de `POST /api/quit`: 200 con `{"ok":true}` y el cierre se programa
/// en la implementación.
pub(crate) async fn salir(Extension(quit): Extension<Arc<dyn Quit>>) -> Response {
    match quit.quit() {
        Ok(()) => write_json(StatusCode::OK, &serde_json::json!({"ok": true})),
        Err(error) => error_de_accion(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn sin_quit_responde_500() {
        let salida: Arc<dyn Quit> = Arc::new(SinQuit);
        let respuesta = salir(Extension(salida)).await;

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
}
