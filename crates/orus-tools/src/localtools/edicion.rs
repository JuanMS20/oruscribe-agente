//! Edición de archivos: reemplazo exacto, con respaldo en el diario y sin
//! escribir si el texto no está o hay varias coincidencias.

use std::path::Path;

use orus_agent::CancelToken;
use serde::{Deserialize, Deserializer};
use serde_json::Value;

use super::escritura::{Escritura, guardar, modo_de};
use super::lectura::{leer_args, leer_texto, texto_o_vacio};
use super::{Error, Set};

/// 1 MiB por lectura y por resultado.
const MAX_CONTENIDO: usize = 1 << 20;

#[derive(Default, Deserialize)]
struct ArgsEdicion {
    #[serde(default, deserialize_with = "texto_o_vacio")]
    path: String,
    #[serde(default, deserialize_with = "texto_o_vacio")]
    old_string: String,
    #[serde(default, deserialize_with = "texto_o_vacio")]
    new_string: String,
    #[serde(default, deserialize_with = "booleano_o_falso")]
    replace_all: bool,
}

/// `encoding/json` deja `false` cuando el campo vale `null`; serde no.
fn booleano_o_falso<'de, D: Deserializer<'de>>(deserializador: D) -> Result<bool, D::Error> {
    Ok(Option::<bool>::deserialize(deserializador)?.unwrap_or_default())
}

/// Reemplaza una porción exacta y falla.
/// sin escribir si no está o si hay varias coincidencias sin `replace_all`.
pub(super) fn edit_file(set: &Set, args: Value, cancel: &CancelToken) -> Result<String, Error> {
    edit_file_con(set, args, cancel, sin_cambio)
}

/// Costura de prueba entre la lectura y la escritura, como el `Chmod` de
/// `escritura.rs`: simula un cambio concurrente sin tocar un global.
fn sin_cambio(_real: &Path) {}

fn edit_file_con(
    set: &Set,
    args: Value,
    cancel: &CancelToken,
    tras_leer: fn(&Path),
) -> Result<String, Error> {
    let args: ArgsEdicion = leer_args(args)?;
    if args.old_string.is_empty() {
        return Err(Error::FaltaOldString);
    }
    let real = set.resolve(&args.path)?;
    let dir = real.parent().unwrap_or(&real);
    set.resolve(&dir.to_string_lossy())?;
    let antes = leer_texto(&real)?;
    tras_leer(&real);
    let coincidencias = antes.matches(args.old_string.as_str()).count();
    if coincidencias == 0 {
        return Err(Error::SinCoincidencias);
    }
    if !args.replace_all && coincidencias > 1 {
        return Err(Error::VariasCoincidencias(coincidencias));
    }
    let reemplazos = if args.replace_all { coincidencias } else { 1 };
    let despues = if args.replace_all {
        antes.replace(args.old_string.as_str(), &args.new_string)
    } else {
        antes.replacen(args.old_string.as_str(), &args.new_string, 1)
    };
    if despues.len() > MAX_CONTENIDO {
        return Err(Error::ResultadoGrande((MAX_CONTENIDO >> 20) as u64));
    }
    // El archivo pudo cambiar desde la lectura (la persona u otro proceso):
    // se relee y se compara antes de pisar, como el deshacer del diario
    // comprueba sha_after. Sin esto el cambio concurrente se pierde en
    // silencio y el respaldo del diario tampoco lo recupera (#94).
    let actual = leer_texto(&real)?;
    if actual != antes {
        return Err(Error::ArchivoCambio);
    }
    let modo = match std::fs::metadata(&real) {
        Ok(info) => modo_de(&info),
        Err(_) => 0o600,
    };
    guardar(
        set,
        cancel,
        Escritura {
            real: &real,
            datos: despues.as_bytes(),
            antes: antes.as_bytes(),
            existia: true,
            modo,
        },
    )?;
    let sustantivo = if reemplazos == 1 {
        "reemplazo"
    } else {
        "reemplazos"
    };
    Ok(format!(
        "editado {} ({reemplazos} {sustantivo}, {} bytes)",
        real.display(),
        despues.len()
    ))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use orus_agent::Risk;
    use serde_json::{Value, json};

    use super::*;
    use crate::journal::Journal;
    use crate::localtools::Config;

    fn conjunto(carpeta: &Path) -> Arc<Set> {
        Arc::new(
            Set::new(Config {
                folders: vec![carpeta.to_string_lossy().into_owned()],
                ..Config::default()
            })
            .expect("Set con una carpeta temporal"),
        )
    }

    async fn correr(set: &Arc<Set>, nombre: &str, args: Value) -> Result<String, String> {
        let herramientas = set.tools();
        let herramienta = herramientas
            .iter()
            .find(|herramienta| herramienta.spec().name == nombre)
            .expect("herramienta registrada");
        if nombre == "local_edit_file" {
            assert_eq!(herramienta.risk(), Some(Risk::Write));
        }
        herramienta
            .run(args)
            .await
            .map_err(|error| error.to_string())
    }

    #[tokio::test]
    async fn edit_unique_replaces() {
        let raiz = tempfile::tempdir().expect("raiz");
        let ruta = raiz.path().join("nota.txt");
        std::fs::write(&ruta, "uno dos tres").expect("nota");
        let set = conjunto(raiz.path());

        let salida = correr(
            &set,
            "local_edit_file",
            json!({
                "path": ruta.to_string_lossy(),
                "old_string": "dos",
                "new_string": "DOS"
            }),
        )
        .await
        .expect("una sola coincidencia se reemplaza");

        assert_eq!(
            std::fs::read_to_string(&ruta).expect("leer"),
            "uno DOS tres",
            "el contenido quedó distinto"
        );
        // La herramienta informa la ruta real (canonizada); en un runner el
        // TEMP corto (RUNNER~1) y el largo difieren, así que la expectativa
        // pasa por la misma canonización.
        let real = super::super::canon::canon(&ruta).expect("ruta real");
        assert_eq!(
            salida,
            format!("editado {} (1 reemplazo, 12 bytes)", real.display())
        );
    }

    #[tokio::test]
    async fn edit_multiple_requires_replace_all() {
        let raiz = tempfile::tempdir().expect("raiz");
        let ruta = raiz.path().join("nota.txt");
        std::fs::write(&ruta, "uno dos uno").expect("nota");
        let set = conjunto(raiz.path());

        let error = correr(
            &set,
            "local_edit_file",
            json!({
                "path": ruta.to_string_lossy(),
                "old_string": "uno",
                "new_string": "1"
            }),
        )
        .await
        .expect_err("varias coincidencias sin replace_all fallan");
        assert_eq!(
            error,
            "hay 2 coincidencias; acota el texto o pasa replace_all"
        );
        assert_eq!(
            std::fs::read_to_string(&ruta).expect("leer"),
            "uno dos uno",
            "el archivo no debe cambiar cuando hay varias coincidencias"
        );

        correr(
            &set,
            "local_edit_file",
            json!({
                "path": ruta.to_string_lossy(),
                "old_string": "uno",
                "new_string": "1",
                "replace_all": true
            }),
        )
        .await
        .expect("replace_all reemplaza todas");
        assert_eq!(std::fs::read_to_string(&ruta).expect("leer"), "1 dos 1");
    }

    #[tokio::test]
    async fn edit_no_match_does_not_touch() {
        let raiz = tempfile::tempdir().expect("raiz");
        let ruta = raiz.path().join("nota.txt");
        std::fs::write(&ruta, "contenido original").expect("nota");
        let set = conjunto(raiz.path());

        let error = correr(
            &set,
            "local_edit_file",
            json!({
                "path": ruta.to_string_lossy(),
                "old_string": "xxx",
                "new_string": "y"
            }),
        )
        .await
        .expect_err("sin coincidencia falla");
        assert_eq!(error, "no encontré el texto en el archivo");
        assert_eq!(
            std::fs::read_to_string(&ruta).expect("leer"),
            "contenido original",
            "el archivo no debe cambiar cuando no hay coincidencia"
        );
    }

    #[tokio::test]
    async fn edit_outside_rejected() {
        let raiz = tempfile::tempdir().expect("raiz");
        let fuera = tempfile::tempdir().expect("fuera");
        let ruta = fuera.path().join("secreto.txt");
        std::fs::write(&ruta, "no se debe tocar").expect("secreto");
        let set = conjunto(raiz.path());

        let error = correr(
            &set,
            "local_edit_file",
            json!({
                "path": ruta.to_string_lossy(),
                "old_string": "x",
                "new_string": "y"
            }),
        )
        .await
        .expect_err("editar afuera falla");
        assert_eq!(error, "la ruta queda fuera de las carpetas permitidas");
        assert_eq!(
            std::fs::read_to_string(&ruta).expect("leer"),
            "no se debe tocar",
            "el archivo de afuera no debe cambiar"
        );
    }

    #[tokio::test]
    async fn edit_missing_file_fails() {
        let raiz = tempfile::tempdir().expect("raiz");
        let ruta = raiz.path().join("no-existe.txt");
        let set = conjunto(raiz.path());

        let error = correr(
            &set,
            "local_edit_file",
            json!({
                "path": ruta.to_string_lossy(),
                "old_string": "x",
                "new_string": "y"
            }),
        )
        .await
        .expect_err("editar un archivo que no existe falla");
        assert!(
            !error.contains("argumentos inválidos"),
            "el error salió de la lectura, no de los argumentos: {error}"
        );
        assert!(!ruta.exists(), "no debe crear el archivo que falta");
    }

    #[tokio::test]
    async fn write_and_edit_undo_roundtrip() {
        let raiz = tempfile::tempdir().expect("raiz");
        let carpeta_diario = tempfile::tempdir().expect("diario");
        let journal = Journal::open(carpeta_diario.path()).expect("abrir el diario");
        let set = Arc::new(
            Set::new(Config {
                folders: vec![raiz.path().to_string_lossy().into_owned()],
                journal: Some(journal.clone()),
                ..Config::default()
            })
            .expect("Set con diario"),
        );
        let ruta = raiz.path().join("nota.txt");
        std::fs::write(&ruta, "original").expect("nota");

        correr(
            &set,
            "local_write_file",
            json!({"path": ruta.to_string_lossy(), "content": "original"}),
        )
        .await
        .expect("escribir");
        correr(
            &set,
            "local_edit_file",
            json!({
                "path": ruta.to_string_lossy(),
                "old_string": "original",
                "new_string": "cambiado"
            }),
        )
        .await
        .expect("editar");

        let entradas = journal.list().expect("listar el diario");
        assert_eq!(entradas.len(), 2, "el diario debe tener 2 entradas");

        let (hechas, error) = journal.undo(2, None, false);
        assert_eq!(hechas, 2, "quería deshacer 2");
        assert!(error.is_none(), "{error:?}");
        assert_eq!(
            std::fs::read_to_string(&ruta).expect("leer"),
            "original",
            "tras deshacer ambas, quería el contenido original"
        );
    }

    fn cambio_concurrente(real: &Path) {
        std::fs::write(real, "cambio de la persona").expect("cambio concurrente");
    }

    /// Issue #94: editar no puede pisar un cambio hecho después de leer. El
    /// gancho simula el cambio concurrente entre la lectura y la escritura.
    #[test]
    fn edit_no_pisa_un_cambio_concurrente() {
        let raiz = tempfile::tempdir().expect("raiz");
        let ruta = raiz.path().join("nota.txt");
        std::fs::write(&ruta, "uno dos tres").expect("nota");
        let set = conjunto(raiz.path());
        let error = edit_file_con(
            &set,
            json!({
                "path": ruta.to_string_lossy(),
                "old_string": "dos",
                "new_string": "DOS"
            }),
            &CancelToken::new(),
            cambio_concurrente,
        )
        .expect_err("un cambio concurrente no se pisa en silencio");
        assert!(error.to_string().contains("cambió"), "{error}");
        assert_eq!(
            std::fs::read_to_string(&ruta).expect("leer"),
            "cambio de la persona",
            "el cambio de la persona quedó pisado"
        );
    }
}
