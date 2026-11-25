//! Selector de carpetas del agente (`http.go:964-975`).
//!
//! El selector es una costura (`Api::with_pick_folder`): el servidor headless
//! la cablea con `--pick-folder` y el borde sin selector responde 400, como Go
//! con `PickFolder == nil`. La ruta sale limpia como `filepath.Clean`.
//! Cancelar no es un fallo: el trait devuelve `Ok(None)` y el borde responde
//! 200 sin `path`, que es lo que `app.js` ya espera (`if (!r.path) return`).
//!
//! Sin selector nativo (Linux) el cuerpo puede traer `path`: la ruta que la
//! persona escribe a mano en el formulario. Se valida con la misma regla que
//! las carpetas permitidas (`carpetasPermitidas`, `agent.go:483-506`) y se
//! devuelve limpia; es el respaldo cross-platform del selector.

use std::path::Path;
use std::sync::Arc;

use axum::Extension;
use axum::body::Body;
use axum::http::StatusCode;
use axum::response::Response;
use serde::Deserialize;

use crate::{ErrorRuta, decode, write_err, write_json};

/// Lo que el borde usa para pedir una carpeta (`immune_fake.go:590-592`).
/// `Ok(None)` es el usuario que cerró el diálogo sin elegir.
pub trait PickFolder: Send + Sync {
    fn elegir(&self) -> Result<Option<String>, String>;
}

/// Cuerpo opcional de la ruta: `path` escrito a mano cuando no hay selector
/// nativo. Vacío o ausente conserva el contrato de siempre (selector o 400).
#[derive(Deserialize)]
struct Cuerpo {
    #[serde(default)]
    path: String,
}

pub(crate) async fn seleccionar(
    Extension(selector): Extension<Option<Arc<dyn PickFolder>>>,
    body: Body,
) -> Response {
    let cuerpo: Cuerpo = match decode(body).await {
        Ok(cuerpo) => cuerpo,
        Err(error) => return write_err(error),
    };
    match ruta(selector.as_deref(), &cuerpo.path) {
        Ok(Some(ruta)) => write_json(StatusCode::OK, &serde_json::json!({"path": ruta})),
        Ok(None) => write_json(StatusCode::OK, &serde_json::json!({})),
        Err(error) => write_err(error),
    }
}

/// La ruta elegida, limpia como el `filepath.Clean` que aplica Go en el
/// selector (`immune_fake.go:591`). `Ok(None)` es una cancelación, no un error.
/// El error del selector es un 500, igual que cualquier error de `writeErr` en
/// Go. Una ruta manual no abre el diálogo: se valida y se devuelve tal cual.
pub(crate) fn ruta(
    selector: Option<&dyn PickFolder>,
    manual: &str,
) -> Result<Option<String>, ErrorRuta> {
    let manual = manual.trim();
    let elegida = if manual.is_empty() {
        let Some(selector) = selector else {
            return Err(ErrorRuta::Invalido(
                "el selector de carpetas no está disponible aquí".to_owned(),
            ));
        };
        match selector.elegir().map_err(ErrorRuta::Lectura)? {
            Some(ruta) => ruta,
            None => return Ok(None),
        }
    } else {
        carpeta_manual(manual)?
    };
    let limpia = orus_config::clean_path(Path::new(&elegida))
        .display()
        .to_string();
    Ok(Some(if limpia.is_empty() {
        // `filepath.Clean("")` devuelve ".".
        ".".to_owned()
    } else {
        limpia
    }))
}

/// Respaldo cross-platform: la ruta escrita a mano tiene que poder ser una
/// carpeta permitida: absoluta, existente y carpeta, como `carpetasPermitidas`.
fn carpeta_manual(manual: &str) -> Result<String, ErrorRuta> {
    let ruta = Path::new(manual);
    if !ruta.is_absolute() {
        return Err(ErrorRuta::Invalido(
            "cada carpeta permitida tiene que ser una ruta absoluta".to_owned(),
        ));
    }
    if !ruta.is_dir() {
        return Err(ErrorRuta::Invalido(format!(
            "la carpeta no existe o no es una carpeta: {manual}"
        )));
    }
    Ok(manual.to_owned())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    struct Fija(String);
    impl PickFolder for Fija {
        fn elegir(&self) -> Result<Option<String>, String> {
            Ok(Some(self.0.clone()))
        }
    }

    struct Rota;
    impl PickFolder for Rota {
        fn elegir(&self) -> Result<Option<String>, String> {
            Err("no se pudo abrir el diálogo".to_owned())
        }
    }

    struct Cancelada;
    impl PickFolder for Cancelada {
        fn elegir(&self) -> Result<Option<String>, String> {
            Ok(None)
        }
    }

    #[test]
    fn pick_folder_sin_selector_responde_400() {
        let error = ruta(None, "").expect_err("sin selector no hay carpeta");
        assert!(matches!(
            error,
            ErrorRuta::Invalido(ref texto)
                if texto == "el selector de carpetas no está disponible aquí"
        ));
    }

    #[test]
    fn pick_folder_limpia_la_ruta_como_filepath_clean() {
        let carpeta = PathBuf::from("carpeta").join("sub");
        let con_separador = format!("{}{}", carpeta.display(), std::path::MAIN_SEPARATOR);
        let elegida = ruta(Some(&Fija(con_separador)), "")
            .expect("el selector responde")
            .expect("el selector devuelve una carpeta");
        assert_eq!(elegida, carpeta.display().to_string());
    }

    #[test]
    fn pick_folder_vacia_es_punto_como_filepath_clean() {
        let elegida = ruta(Some(&Fija(String::new())), "")
            .expect("el selector responde")
            .expect("el selector devuelve una carpeta");
        assert_eq!(elegida, ".");
    }

    #[test]
    fn pick_folder_cancelar_no_es_error() {
        assert!(
            ruta(Some(&Cancelada), "")
                .expect("cancelar no es un error")
                .is_none(),
            "cancelar devuelve None, no una ruta"
        );
    }

    #[test]
    fn pick_folder_el_error_del_selector_es_500() {
        let error = ruta(Some(&Rota), "").expect_err("el selector roto no devuelve ruta");
        assert!(matches!(
            error,
            ErrorRuta::Lectura(ref texto) if texto == "no se pudo abrir el diálogo"
        ));
    }

    #[test]
    fn pick_folder_manual_limpia_la_ruta_como_filepath_clean() {
        let carpeta = tempfile::tempdir().expect("carpeta temporal");
        let con_separador = format!("{}{}", carpeta.path().display(), std::path::MAIN_SEPARATOR);
        let elegida = ruta(None, &con_separador)
            .expect("la ruta manual es válida")
            .expect("la ruta manual devuelve una carpeta");
        assert_eq!(elegida, carpeta.path().display().to_string());
    }

    #[test]
    fn pick_folder_manual_relativa_es_invalido() {
        let error =
            ruta(None, "carpeta/relativa").expect_err("una ruta relativa no es carpeta permitida");
        assert!(matches!(
            error,
            ErrorRuta::Invalido(ref texto)
                if texto == "cada carpeta permitida tiene que ser una ruta absoluta"
        ));
    }

    #[test]
    fn pick_folder_manual_inexistente_es_invalido() {
        let base = tempfile::tempdir().expect("carpeta temporal");
        let inexistente = base.path().join("no-existe");
        let error = ruta(None, &inexistente.display().to_string())
            .expect_err("una carpeta que no existe no se permite");
        assert!(matches!(
            error,
            ErrorRuta::Invalido(ref texto)
                if texto == &format!(
                    "la carpeta no existe o no es una carpeta: {}",
                    inexistente.display()
                )
        ));
    }

    #[test]
    fn pick_folder_manual_con_selector_roto_la_prefiere() {
        let carpeta = tempfile::tempdir().expect("carpeta temporal");
        let elegida = ruta(Some(&Rota), &carpeta.path().display().to_string())
            .expect("la ruta manual no abre el diálogo")
            .expect("la ruta manual devuelve una carpeta");
        assert_eq!(elegida, carpeta.path().display().to_string());
    }

    #[test]
    fn pick_folder_manual_vacia_cae_al_selector() {
        assert!(
            ruta(Some(&Cancelada), "   ")
                .expect("sin ruta manual manda el selector")
                .is_none(),
            "manual vacía es cancelación del selector, no un error"
        );
    }
}
