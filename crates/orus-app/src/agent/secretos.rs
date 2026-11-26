//! El llavero del proveedor y el registro de despertar.

use std::path::Path;

use crate::wake::WakeEntry;

/// La clave del proveedor: solo su pista, nunca la clave entera
/// Sin proveedor no hay clave.
pub trait Secrets: Send + Sync {
    fn get(&self) -> Result<Option<String>, String>;
    fn set(&self, value: &str) -> Result<(), String>;
    fn delete(&self) -> Result<(), String>;
}

/// Sin llavero: la app arranca, pero no hay clave guardada ni forma de guardar.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoSecrets;

impl Secrets for NoSecrets {
    fn get(&self) -> Result<Option<String>, String> {
        Ok(None)
    }

    fn set(&self, _: &str) -> Result<(), String> {
        Err("no hay un llavero de proveedor configurado".to_owned())
    }

    fn delete(&self) -> Result<(), String> {
        Err("no hay un llavero de proveedor configurado".to_owned())
    }
}

/// La clave del proveedor en el llavero del sistema: servicio
/// `oruscribe-agent`, nombre `agente-clave`.
///
/// El borde HTTP construye la `App` con
/// `with_secrets(Arc::new(AgentSecrets::new()))`; sin ese cableado, guardar una
/// clave responde 500 `no hay un llavero de proveedor configurado`.
pub struct AgentSecrets {
    store: orus_auth::SecretStore,
}

impl AgentSecrets {
    pub fn new() -> Self {
        Self::with_data_dir(orus_config::data_dir())
    }

    pub fn with_data_dir(dir: impl AsRef<Path>) -> Self {
        Self {
            store: orus_auth::SecretStore::with_file_fallback(
                "oruscribe-agent",
                "agente-clave",
                dir,
            ),
        }
    }
}

impl Default for AgentSecrets {
    fn default() -> Self {
        Self::new()
    }
}

impl Secrets for AgentSecrets {
    fn get(&self) -> Result<Option<String>, String> {
        self.store.get().map_err(|error| error.to_string())
    }

    fn set(&self, value: &str) -> Result<(), String> {
        self.store.set(value).map_err(|error| error.to_string())
    }

    fn delete(&self) -> Result<(), String> {
        self.store.delete().map_err(|error| error.to_string())
    }
}

pub(crate) fn clave_para(secrets: &dyn Secrets, base: &str) -> Result<Option<String>, String> {
    let guardada = secrets.get()?;
    let Some(valor) = guardada else {
        return Ok(None);
    };
    match valor.split_once('\n') {
        Some((host, clave)) if host == super::validacion::host_de(base) => {
            Ok(Some(clave.to_owned()))
        }
        Some(_) => Ok(None),
        None => Ok(Some(valor)),
    }
}

pub(crate) fn guardar_clave_para(
    secrets: &dyn Secrets,
    base: &str,
    clave: &str,
) -> Result<(), String> {
    secrets.set(&format!("{}\n{}", super::validacion::host_de(base), clave))
}

/// El registro de lo que el agente hizo solo. Sin proveedor la vista es `[]`.
pub trait WakeLog: Send + Sync {
    fn entries(&self) -> Vec<WakeEntry>;
}
