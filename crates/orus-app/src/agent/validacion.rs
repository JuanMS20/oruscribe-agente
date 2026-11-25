//! Helpers de validación de los ajustes y del turno.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer};

use orus_tools::localtools::{self, Config};

use super::tipos::AgentError;

const MAX_PROGRAMA: usize = 200;
const MAX_TEXTO_VOZ: usize = 120;

pub(crate) fn tope(valor: i64, defecto: i64) -> i64 {
    if valor <= 0 { defecto } else { valor }
}

/// El 400 del cerebro cuando todavía no hay timbre, base o token
/// (`agent.go:572-575`).
pub(crate) fn sin_conexion() -> AgentError {
    AgentError::Invalid(
        "aún no hay conexión con Oruscribe: espera a estar en línea y vuelve a intentar".to_owned(),
    )
}

/// La raíz de `AGENTS.md` y skills: la primera carpeta permitida, o el hogar
/// cuando todavía no hay ninguna (`carpetaDeInstrucciones`,
/// `agent.go:1035-1047`). Si el hogar no se puede resolver, la carpeta de
/// datos, como el Go.
pub(crate) fn carpeta_de_instrucciones(folders: &[String]) -> String {
    if let Some(primera) = folders.iter().find(|folder| Path::new(folder).is_dir()) {
        return primera.clone();
    }
    std::env::home_dir()
        .map(|home| home.to_string_lossy().into_owned())
        .unwrap_or_else(|| orus_config::data_dir().to_string_lossy().into_owned())
}

/// Los últimos cuatro si la clave mide más de ocho; una corta no se muestra
/// (`pistaClave`, `agent.go:1115-1121`).
pub(crate) fn pista_clave(clave: &str) -> String {
    let total = clave.chars().count();
    if total <= 8 {
        return String::new();
    }
    clave.chars().skip(total - 4).collect()
}

pub(crate) fn deserializar_texto<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
}

pub(crate) fn deserializar_textos<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<Vec<String>>::deserialize(deserializer)?.unwrap_or_default())
}

pub(crate) fn deserializar_bool<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<bool>::deserialize(deserializer)?.unwrap_or_default())
}

pub(crate) fn deserializar_i64<'de, D>(deserializer: D) -> Result<i64, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<i64>::deserialize(deserializer)?.unwrap_or_default())
}

/// Host con su puerto, en minúsculas; vacío si la dirección no se puede leer
/// (`HostDe`, `internal/agent/openai.go:189-197`). El host sale del parser:
/// interpretar la autoridad a mano deja pasar una `\` que `url` trata como
/// barra y que esconde el host real detrás de un usuario aparente.
pub(crate) fn host_de(raw: &str) -> String {
    let Ok(url) = reqwest::Url::parse(raw) else {
        return String::new();
    };
    let Some(host) = url.host_str() else {
        return String::new();
    };
    let host = host.to_ascii_lowercase();
    match url.port_or_known_default() {
        Some(puerto) => format!("{host}:{puerto}"),
        None => host,
    }
}

/// Voz apagada o una base válida; se guarda la dirección normalizada
/// (`validarVozURL`, `agent_voice.go:33-43`).
pub(crate) fn validar_voz_url(raw: &str) -> Result<String, AgentError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(String::new());
    }
    orus_agent::validar_base(raw)
        .map_err(|error| AgentError::Invalid(format!("la dirección de voz no es válida: {error}")))
}

/// Recorta el texto de voz y aplica el tope de 120 caracteres
/// (`textoDeVoz`, `agent_voice.go:45-51`).
pub(crate) fn texto_de_voz(raw: &str, nombre: &str) -> Result<String, AgentError> {
    let raw = raw.trim();
    if raw.chars().count() > MAX_TEXTO_VOZ {
        return Err(AgentError::Invalid(format!(
            "{nombre} no puede tener más de {MAX_TEXTO_VOZ} caracteres"
        )));
    }
    Ok(raw.to_owned())
}

/// Rutas absolutas que existen y son carpetas; sin carpetas no hay
/// herramientas locales y eso es válido (`carpetasPermitidas`,
/// `agent.go:483-506`).
pub(crate) fn carpetas_permitidas(entrada: &[String]) -> Result<Vec<String>, AgentError> {
    let mut salida = Vec::with_capacity(entrada.len());
    for carpeta in entrada {
        let carpeta = carpeta.trim();
        if carpeta.is_empty() || salida.iter().any(|vista| vista == carpeta) {
            continue;
        }
        if !Path::new(carpeta).is_absolute() {
            return Err(AgentError::Invalid(
                "cada carpeta permitida tiene que ser una ruta absoluta".to_owned(),
            ));
        }
        salida.push(carpeta.to_owned());
    }
    if salida.is_empty() {
        return Ok(salida);
    }
    localtools::Set::new(Config {
        folders: salida.clone(),
        ..Config::default()
    })
    .map_err(|error| AgentError::Invalid(error.to_string()))?;
    Ok(salida)
}

/// Programas permitidos: un nombre suelto se acepta tal cual; una ruta se
/// resuelve, tiene que ser absoluta y un archivo regular
/// (`programasPermitidos`, `agent.go:508-534`).
pub(crate) fn programas_permitidos(entrada: &[String]) -> Result<Vec<String>, AgentError> {
    use std::collections::HashSet;

    let mut salida = Vec::with_capacity(entrada.len());
    let mut vistos = HashSet::new();
    for programa in entrada {
        let programa = programa.trim();
        if programa.is_empty() || programa.len() > MAX_PROGRAMA || vistos.contains(programa) {
            continue;
        }
        let mut guardado = programa.to_owned();
        if programa.contains(['/', '\\']) {
            if !Path::new(programa).is_absolute() {
                return Err(AgentError::Invalid(format!(
                    "el programa {programa:?} tiene que ser una ruta absoluta"
                )));
            }
            let real = std::fs::canonicalize(programa).map_err(|_| {
                AgentError::Invalid(format!(
                    "el programa {programa:?} no existe o no se pudo resolver"
                ))
            })?;
            let real: PathBuf = orus_config::sin_prefijo(&real);
            let info = std::fs::metadata(&real);
            if !info.is_ok_and(|info| info.is_file()) {
                return Err(AgentError::Invalid(format!(
                    "el programa {programa:?} no es un archivo ejecutable"
                )));
            }
            guardado = real.to_string_lossy().into_owned();
        }
        vistos.insert(guardado.clone());
        salida.push(guardado);
    }
    Ok(salida)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn instrucciones_usa_la_primera_carpeta_que_existe() {
        let base = tempfile::tempdir().expect("carpeta temporal");
        let valida = base.path().join("valida");
        std::fs::create_dir(&valida).expect("carpeta valida");
        let falta = base.path().join("desmontada");
        let elegida = carpeta_de_instrucciones(&[
            falta.to_string_lossy().into_owned(),
            valida.to_string_lossy().into_owned(),
        ]);
        assert_eq!(elegida, valida.to_string_lossy().into_owned());
    }
}
