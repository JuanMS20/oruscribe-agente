//! Instrucciones del proyecto: `AGENTS.md` y `ORUSCRIBE.md`.
//!
//! Port de `internal/harness/instructions.go:13-140`.

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use orus_config::sin_prefijo;
use thiserror::Error;

/// Tope por archivo (32 KiB) y total (64 KiB), en bytes.
pub const MAX_INSTRUCCION: usize = 32 << 10;
pub const MAX_INSTRUCCIONES: usize = 64 << 10;
const MARCA: &str = "\n[se omitieron instrucciones adicionales por tamaño]";

/// Un archivo ya leído, con su ruta para que el modelo sepa de dónde salió.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instruction {
    pub path: PathBuf,
    pub content: String,
}

/// Las instrucciones en el orden en que se renderizan.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Instructions {
    items: Vec<Instruction>,
}

impl Instructions {
    pub fn items(&self) -> &[Instruction] {
        &self.items
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Arma el bloque que va en el mensaje de sistema (`Render`).
    pub fn render(&self) -> String {
        if self.items.is_empty() {
            return String::new();
        }
        let mut bloque = String::from("<instrucciones_del_proyecto>\n");
        for instruccion in &self.items {
            bloque.push_str("Instructions from: ");
            bloque.push_str(&instruccion.path.display().to_string());
            bloque.push('\n');
            bloque.push_str(instruccion.content.trim_end_matches('\n'));
            bloque.push_str("\n\n");
        }
        bloque.push_str("</instrucciones_del_proyecto>");
        bloque
    }
}

/// Error del descubrimiento en la frontera del crate (R8).
#[derive(Debug, Error)]
pub enum InstructionsError {
    #[error("no se pudo resolver {cwd:?}: {source}")]
    Resolve {
        cwd: String,
        #[source]
        source: io::Error,
    },
    #[error("{0:?} no es una carpeta")]
    NotADirectory(String),
}

/// Busca las instrucciones desde la raíz hasta `cwd`, con la global delante.
pub fn discover(cwd: &Path, include_global: bool) -> Result<Instructions, InstructionsError> {
    discover_with(cwd, include_global, &orus_config::data_dir())
}

/// Igual que `discover`, con la carpeta de datos explícita para los tests.
pub(crate) fn discover_with(
    cwd: &Path,
    include_global: bool,
    data_dir: &Path,
) -> Result<Instructions, InstructionsError> {
    let original = cwd.display().to_string();
    let absoluta = if cwd.is_absolute() {
        cwd.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|source| InstructionsError::Resolve {
                cwd: original.clone(),
                source,
            })?
            .join(cwd)
    };
    let real = fs::canonicalize(&absoluta).map_err(|source| InstructionsError::Resolve {
        cwd: original.clone(),
        source,
    })?;
    let real = sin_prefijo(&real);
    match fs::metadata(&real) {
        Ok(info) if info.is_dir() => {}
        _ => return Err(InstructionsError::NotADirectory(original)),
    }

    let mut out = Instructions::default();
    let mut total = 0usize;
    let mut agregar = |path: PathBuf| {
        if total >= MAX_INSTRUCCIONES {
            return;
        }
        let Some(texto) = leer(&path) else {
            return;
        };
        let restante = MAX_INSTRUCCIONES - total;
        let texto = if texto.len() > restante {
            if restante > MARCA.len() {
                recortar(&texto, restante - MARCA.len()) + MARCA
            } else {
                return;
            }
        } else {
            texto
        };
        if texto.trim().is_empty() {
            return;
        }
        total += texto.len();
        out.items.push(Instruction {
            path,
            content: texto,
        });
    };

    if include_global {
        agregar(data_dir.join("AGENTS.md"));
    }
    let mut dirs: Vec<&Path> = real.ancestors().collect();
    dirs.reverse();
    for dir in dirs {
        for nombre in ["AGENTS.md", "ORUSCRIBE.md"] {
            agregar(dir.join(nombre));
        }
    }
    Ok(out)
}

/// Lee un archivo regular, sin seguir symlinks (`Lstat`).
pub(crate) fn leer_archivo_seguro(path: &Path, max: usize) -> Option<Vec<u8>> {
    let info = fs::symlink_metadata(path).ok()?;
    if !info.file_type().is_file() || info.len() > max as u64 {
        return None;
    }
    let archivo = fs::File::open(path).ok()?;
    let abierto = archivo.metadata().ok()?;
    if !abierto.file_type().is_file() || !mismo_archivo(&info, &abierto) {
        return None;
    }
    let mut datos = Vec::new();
    archivo.take(max as u64 + 1).read_to_end(&mut datos).ok()?;
    if datos.is_empty() || datos.len() > max {
        return None;
    }
    Some(datos)
}

#[cfg(unix)]
fn mismo_archivo(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    a.dev() == b.dev() && a.ino() == b.ino()
}

#[cfg(windows)]
fn mismo_archivo(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    a.file_size() == b.file_size()
        && a.creation_time() == b.creation_time()
        && a.last_write_time() == b.last_write_time()
}

fn leer(path: &Path) -> Option<String> {
    String::from_utf8(leer_archivo_seguro(path, MAX_INSTRUCCION)?).ok()
}

/// Corta sin partir un carácter UTF-8.
fn recortar(texto: &str, max: usize) -> String {
    if max == 0 || texto.len() <= max {
        return texto.to_owned();
    }
    let mut n = max;
    while n > 0 && !texto.is_char_boundary(n) {
        n -= 1;
    }
    texto[..n].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn escribir(base: &Path, ruta: &str, datos: impl AsRef<[u8]>) {
        let path = base.join(ruta);
        fs::create_dir_all(path.parent().expect("padre")).expect("carpeta");
        fs::write(path, datos).expect("archivo");
    }

    fn data(base: &Path) -> PathBuf {
        base.join("data")
    }

    #[test]
    fn discover_orders_root_to_cwd() {
        let tmp = tempfile::tempdir().expect("tmp");
        let cwd = tmp.path().join("a").join("b");
        escribir(tmp.path(), "AGENTS.md", "raiz");
        escribir(cwd.parent().expect("a"), "ORUSCRIBE.md", "medio");
        escribir(&cwd, "AGENTS.md", "cerca");

        let instrucciones = discover_with(&cwd, false, &data(tmp.path())).expect("discover");
        let textos: Vec<&str> = instrucciones
            .items()
            .iter()
            .map(|i| i.content.as_str())
            .filter(|texto| ["raiz", "medio", "cerca"].contains(texto))
            .collect();
        assert_eq!(textos, ["raiz", "medio", "cerca"]);
    }

    #[test]
    fn discover_respects_both_names() {
        let tmp = tempfile::tempdir().expect("tmp");
        let cwd = tmp.path().join("proj");
        escribir(&cwd, "AGENTS.md", "primero");
        escribir(&cwd, "ORUSCRIBE.md", "segundo");

        let instrucciones = discover_with(&cwd, false, &data(tmp.path())).expect("discover");
        let textos: Vec<&str> = instrucciones
            .items()
            .iter()
            .map(|i| i.content.as_str())
            .filter(|texto| ["primero", "segundo"].contains(texto))
            .collect();
        assert_eq!(textos, ["primero", "segundo"]);
    }

    #[test]
    fn discover_skips_symlink_and_bad_utf8() {
        let tmp = tempfile::tempdir().expect("tmp");
        let cwd = tmp.path().join("proj");
        fs::create_dir_all(&cwd).expect("cwd");
        escribir(&cwd, "AGENTS.md", b"\xff\xfe");
        let fuera = tmp.path().join("fuera.md");
        fs::write(&fuera, "secreto").expect("fuera");
        let enlace = cwd.join("ORUSCRIBE.md");
        #[cfg(windows)]
        let creado = std::os::windows::fs::symlink_file(&fuera, &enlace).is_ok();
        #[cfg(not(windows))]
        let creado = std::os::unix::fs::symlink(&fuera, &enlace).is_ok();

        // Sin privilegio para symlinks, una junction con el mismo nombre
        // ejercita el rechazo de reparse points de `leer` (issue #98): no es
        // un archivo regular y no debe aportar instrucciones.
        #[cfg(windows)]
        let junta = if creado {
            false
        } else {
            let fuera_dir = tmp.path().join("fuera-dir");
            fs::create_dir(&fuera_dir).expect("fuera-dir");
            fs::write(fuera_dir.join("secreto.md"), "secreto").expect("secreto");
            std::process::Command::new("cmd")
                .args(["/c", "mklink", "/J"])
                .arg(&enlace)
                .arg(&fuera_dir)
                .output()
                .is_ok_and(|salida| salida.status.success())
        };
        #[cfg(not(windows))]
        let junta = false;

        if !creado && !junta {
            // Skip explícito y ruidoso: sin enlace ni junction no se ejercitó
            // la rama de enlaces y el verde no significa cobertura.
            eprintln!(
                "SKIP #98: no se pudo crear symlink ni junction; \
                 la rama de enlaces de discover no se ejercitó"
            );
            return;
        }
        if !creado {
            eprintln!(
                "aviso #98: sin privilegio para el symlink de archivo; \
                 se ejercita el rechazo de la junction (el lstat del enlace \
                 de archivo queda sin ejercitar en este equipo)"
            );
        }

        // `discover_with` guarda rutas sin el prefijo `\\?\`: comparar contra
        // el canonicalize con prefijo haría la aserción vacua en Windows.
        let real = sin_prefijo(&fs::canonicalize(&cwd).expect("real"));
        let instrucciones = discover_with(&cwd, false, &data(tmp.path())).expect("discover");
        assert!(
            instrucciones
                .items()
                .iter()
                .all(|i| !i.path.starts_with(&real)),
            "la carpeta no aportó instrucciones: {instrucciones:?}"
        );
    }

    #[test]
    fn discover_caps_files_and_total() {
        let tmp = tempfile::tempdir().expect("tmp");
        let cwd = tmp.path().join("x").join("proj");
        // La cadena de ancestros puede traer instrucciones del equipo (por
        // ejemplo ~/AGENTS.md): se mide lo que ya ocupa para probar el mismo
        // tope en cualquier máquina.
        let sonda = tmp.path().join("sonda");
        fs::create_dir_all(&sonda).expect("sonda");
        let heredado: usize = discover_with(&sonda, false, &data(tmp.path()))
            .expect("discover")
            .items()
            .iter()
            .map(|i| i.content.len())
            .sum();
        let disponible = MAX_INSTRUCCIONES - heredado;
        let tercio = disponible / 3;

        escribir(tmp.path(), "AGENTS.md", "z".repeat(MAX_INSTRUCCION + 1));
        escribir(cwd.parent().expect("x"), "AGENTS.md", "a".repeat(tercio));
        escribir(cwd.parent().expect("x"), "ORUSCRIBE.md", "b".repeat(tercio));
        escribir(&cwd, "AGENTS.md", "c".repeat(disponible - 2 * tercio + 1));

        let instrucciones = discover_with(&cwd, false, &data(tmp.path())).expect("discover");
        let real_tmp = sin_prefijo(&fs::canonicalize(tmp.path()).expect("real tmp"));
        let items: Vec<&Instruction> = instrucciones
            .items()
            .iter()
            .filter(|i| i.path.starts_with(&real_tmp))
            .collect();
        assert_eq!(items.len(), 3, "el archivo de 32 KiB+ se omite entero");
        assert!(items.iter().all(|i| i.content.len() <= MAX_INSTRUCCION));
        assert!(items[2].content.ends_with(MARCA));
        assert!(
            items.iter().map(|i| i.content.len()).sum::<usize>() <= MAX_INSTRUCCIONES,
            "el total no pasa de 64 KiB"
        );
    }

    #[test]
    fn recorta_sin_partir_utf8() {
        let texto = "áéíóú";
        assert_eq!(recortar(texto, 3), "á");
        assert_eq!(recortar(texto, 0), texto);
    }

    #[test]
    fn render_exact_block() {
        let instrucciones = Instructions {
            items: vec![
                Instruction {
                    path: PathBuf::from("/a/AGENTS.md"),
                    content: "uno\n\n".to_owned(),
                },
                Instruction {
                    path: PathBuf::from("/a/b/ORUSCRIBE.md"),
                    content: "dos".to_owned(),
                },
            ],
        };
        assert_eq!(
            instrucciones.render(),
            "<instrucciones_del_proyecto>\nInstructions from: /a/AGENTS.md\nuno\n\nInstructions from: /a/b/ORUSCRIBE.md\ndos\n\n</instrucciones_del_proyecto>"
        );
        assert_eq!(Instructions::default().render(), "");
    }
}
