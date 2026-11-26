//! Cuerpo de `GET /api/setup`.
//!
//! Las ocho claves, sin `omitempty`, en
//! el mismo orden y con los mismos nombres camelCase. No abre HTTP ni toca el
//! registro; 7-04 solo conecta [`setup_json`] a la ruta.

use std::path::Path;

use serde::Serialize;

use crate::paths::Rutas;
use crate::terms::{TERMS_BODY, TERMS_TITLE, TERMS_VERSION};

/// Lo que la ventana necesita para decidir si ofrece instalar.
///
/// Las claves JSON son las del contrato: `installed`, `portable`, `dir`,
/// `termsTitle`, `termsBody`, `termsVersion`, `acceptedTerms` y `platform`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupInfo {
    pub installed: bool,
    pub portable: bool,
    pub dir: String,
    pub terms_title: String,
    pub terms_body: String,
    pub terms_version: String,
    pub accepted_terms: String,
    pub platform: String,
}

/// Arma [`SetupInfo`] con el estado del disco y la aceptación guardada.
///
/// `current_exe` es el binario que corre: en producción lo da
/// `std::env::current_exe`, en tests se inyecta para no depender del ejecutable
/// de la suite.
pub fn setup_info(rutas: &Rutas, current_exe: &Path, accepted_terms: &str) -> SetupInfo {
    SetupInfo {
        installed: rutas.installed(current_exe),
        portable: rutas.portable(current_exe),
        dir: rutas.dir().to_string_lossy().into_owned(),
        terms_title: TERMS_TITLE.to_owned(),
        terms_body: TERMS_BODY.to_owned(),
        terms_version: TERMS_VERSION.to_owned(),
        accepted_terms: accepted_terms.to_owned(),
        platform: platform().to_owned(),
    }
}

/// JSON compacto de `/api/setup`, sin salto de línea final: la ruta lo agrega.
pub fn setup_json(
    rutas: &Rutas,
    current_exe: &Path,
    accepted_terms: &str,
) -> Result<String, serde_json::Error> {
    serde_json::to_string(&setup_info(rutas, current_exe, accepted_terms))
}

/// Nombre de la plataforma para el campo `platform`.
///
/// Es el valor de `std::env::consts::OS`.
pub fn platform() -> &'static str {
    std::env::consts::OS
}
