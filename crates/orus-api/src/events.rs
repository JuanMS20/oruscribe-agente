//! Hub de eventos y long poll de `GET /api/events`.
//!
//! El contrato reparte cada evento a un canal por ventana y descarta el que no cabe
//! Aquí cada suscriptor es un `mpsc` con id, para poder
//! darlo de baja cuando la petición termina, incluso si el cliente corta.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use axum::Extension;
use axum::http::StatusCode;
use axum::response::Response;
use orus_app::Event;
use serde::Serialize;
use tokio::sync::mpsc;

use crate::write_json;

/// La capacidad de cada suscriptor: `make(chan Event, 8)`.
const CAPACIDAD_SUSCRIPTOR: usize = 8;

/// Reparte cada evento a todos los que escuchan. Clonarlo comparte el estado.
#[derive(Clone, Default)]
pub struct Hub {
    interior: Arc<Interior>,
}

#[derive(Default)]
struct Interior {
    siguiente_id: AtomicU64,
    suscriptores: Mutex<HashMap<u64, mpsc::Sender<Event>>>,
}

impl Hub {
    pub fn new() -> Self {
        Self::default()
    }

    /// Abre un canal de 8 y devuelve su id para la baja.
    pub fn subscribe(&self) -> (u64, mpsc::Receiver<Event>) {
        let (tx, rx) = mpsc::channel(CAPACIDAD_SUSCRIPTOR);
        let id = self.interior.siguiente_id.fetch_add(1, Ordering::Relaxed);
        self.suscriptores().insert(id, tx);
        (id, rx)
    }

    /// Quita el suscriptor.
    pub fn unsubscribe(&self, id: u64) {
        self.suscriptores().remove(&id);
    }

    /// Manda a todos y descarta el que no cabe: nunca
    /// bloquea y nunca cierra el canal de un suscriptor lento.
    pub fn publish(&self, event: Event) {
        let suscriptores = self.suscriptores();
        for sender in suscriptores.values() {
            let _ = sender.try_send(event.clone());
        }
    }

    /// Cuántos suscriptores quedan. Lo usan los tests y el borde.
    #[allow(clippy::len_without_is_empty)] // no hay llamador de `is_empty`
    pub fn len(&self) -> usize {
        self.suscriptores().len()
    }

    fn suscriptores(&self) -> MutexGuard<'_, HashMap<u64, mpsc::Sender<Event>>> {
        self.interior
            .suscriptores
            .lock()
            .unwrap_or_else(|envenenado| envenenado.into_inner())
    }
}

/// El silencio del contrato es `Event{Kind:"silencio"}` con el tiempo cero
/// No un `Event::new`, que pondría `at` en ahora.
#[derive(Serialize)]
struct Silencio {
    kind: &'static str,
    at: &'static str,
    open: i64,
    chat: i64,
}

fn silencio() -> Silencio {
    Silencio {
        kind: "silencio",
        at: "0001-01-01T00:00:00Z",
        open: 0,
        chat: 0,
    }
}

/// Da de baja al suscriptor al salir del handler, también si el cliente corta
/// y el futuro se descarta.
#[allow(dead_code)] // se leen en Drop; clippy no lo ve
struct Baja {
    hub: Arc<Hub>,
    id: u64,
}

impl Drop for Baja {
    fn drop(&mut self) {
        self.hub.unsubscribe(self.id);
    }
}

/// `GET /api/events`: long poll de un evento o del silencio.
pub(crate) async fn handle_events(
    Extension(hub): Extension<Arc<Hub>>,
    Extension(wait): Extension<Duration>,
) -> Response {
    let (id, mut rx) = hub.subscribe();
    let _baja = Baja {
        hub: Arc::clone(&hub),
        id,
    };
    match tokio::time::timeout(wait, rx.recv()).await {
        Ok(Some(event)) => write_json(StatusCode::OK, &event),
        _ => write_json(StatusCode::OK, &silencio()),
    }
}
