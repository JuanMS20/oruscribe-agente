//! Abrir y borrar conversaciones guardadas desde el `Agent`.
//!
//! El archivo se lee o se
//! borra fuera del candado y el estado de la conversación abierta se ajusta
//! bajo `estado`. El estado y el guardado ya
//! viven en `agent` y `conversations`; acá no se duplica ninguno.

use crate::agent::{Agent, AgentError, reconstruir_pasos};
use crate::sync::lock;

impl Agent {
    /// Carga una conversación guardada en la pantalla.
    ///
    /// Primero lee el archivo —un error de disco gana sobre `busy`— y recién
    /// después toma el candado una sola vez. No toca el modelo, `cancel`,
    /// `busy` ni la generación: el contrato tampoco lo hace al abrir.
    pub fn open_conversation(&self, id: &str) -> Result<(), AgentError> {
        let generacion = lock(&self.estado).generation;
        let conversacion = self
            .conversations
            .load(id)
            .map_err(|error| AgentError::Conversaciones(error.to_string()))?;
        if let Some(gap) = &self.open_gap {
            gap();
        }
        let mut estado = lock(&self.estado);
        if estado.busy {
            return Err(AgentError::Busy);
        }
        if estado.generation != generacion || self.borrada(&conversacion.id) {
            return Err(AgentError::Invalid(
                "esa conversación ya no está disponible".to_owned(),
            ));
        }
        estado.conversation_id = conversacion.id;
        estado.steps = reconstruir_pasos(&conversacion.messages, conversacion.updated);
        estado.history = conversacion.messages;
        estado.tokens_in = conversacion.tokens_in;
        estado.tokens_out = conversacion.tokens_out;
        estado.cost_usd = conversacion.cost_usd;
        estado.created = Some(conversacion.created);
        estado.error.clear();
        estado.draft.clear();
        Ok(())
    }

    /// Borra una conversación; con id vacío borra todas y deja la pantalla en
    /// una conversación nueva.
    ///
    /// El `busy` se comprueba bajo el candado y se suelta antes del borrado.
    /// Los `reset` vuelven a mirarlo bajo el candado: si el timbre tomó el
    /// agente en el hueco, no le borran el id ni el cancel al turno
    pub fn delete_conversation(&self, id: &str) -> Result<(), AgentError> {
        {
            let estado = lock(&self.estado);
            if estado.busy {
                return Err(AgentError::Busy);
            }
        }
        if let Some(gap) = &self.delete_gap {
            gap();
        }
        if id.is_empty() {
            let ids = self
                .conversations
                .list()
                .map(|lista| lista.into_iter().map(|info| info.id).collect::<Vec<_>>())
                .unwrap_or_default();
            self.conversations
                .clear()
                .map_err(|error| AgentError::Conversaciones(error.to_string()))?;
            let mut borradas = lock(&self.borradas);
            for id in ids {
                borradas.insert(id);
            }
            drop(borradas);
            self.reset_sin_chequeo();
            return Ok(());
        }
        self.conversations
            .delete(id)
            .map_err(|error| AgentError::Conversaciones(error.to_string()))?;
        lock(&self.borradas).insert(id.to_owned());
        self.reset_if_current(id);
        Ok(())
    }
}
