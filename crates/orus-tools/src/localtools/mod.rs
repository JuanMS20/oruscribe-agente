//! Herramientas locales del agente: carpetas y programas permitidos,
//! resolución de rutas que no escapan de la raíz, lectura y escritura. Port de
//! `internal/localtools/localtools.go:24-97,129-223,315-499` y
//! `canon_windows.go:14-50`.

mod bash;
mod canon;
mod comando;
mod edicion;
mod escritura;
mod lectura;
mod proceso;
mod tool;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use orus_agent::{Risk, Tool};
use serde_json::json;
use thiserror::Error;

use crate::journal::Journal;
use tool::HerramientaLocal;

/// Tiempo máximo por comando cuando `Config::timeout` es cero.
const TIMEOUT_COMANDO: Duration = Duration::from_secs(60);

/// Lo que elige la persona en los ajustes del agente.
#[derive(Debug, Default)]
pub struct Config {
    pub folders: Vec<String>,
    /// Carpetas de solo lectura, como las de las skills cargadas: se pueden
    /// leer y buscar, pero no escribir ni deshacer.
    pub read_folders: Vec<String>,
    pub programs: Vec<String>,
    pub timeout: Duration,
    pub journal: Option<Journal>,
}

/// Carpetas ya resueltas y programas permitidos.
pub struct Set {
    roots: Vec<PathBuf>,
    read_roots: Vec<PathBuf>,
    nombres: HashSet<String>,
    rutas_prog: Vec<String>,
    timeout: Duration,
    journal: Option<Journal>,
}

/// Errores de configuración y de resolución. Los textos se muestran tal cual.
#[derive(Debug, Error)]
pub enum Error {
    #[error("la carpeta {0:?} tiene que ser una ruta absoluta local")]
    CarpetaNoAbsoluta(String),
    #[error("ninguna carpeta permitida está disponible: {}", .0.join(", "))]
    CarpetasNoDisponibles(Vec<String>),
    #[error("sin bash disponible: instala Git for Windows; esta herramienta no ejecuta PowerShell")]
    BashNoDisponible,
    #[error("sin carpetas permitidas no hay herramientas locales")]
    SinCarpetas,
    #[error("el programa {0:?} no existe o no se pudo resolver: {1}")]
    ProgramaSinResolver(String, #[source] std::io::Error),
    #[error("falta la ruta")]
    FaltaRuta,
    #[error("la ruta queda fuera de las carpetas permitidas")]
    Fuera,
    #[error("las rutas con dos puntos no se aceptan")]
    DosPuntos,
    #[error("los nombres de dispositivo de Windows no se aceptan")]
    Dispositivo,
    #[error("la ruta tiene que ser absoluta")]
    RutaRelativa,
    #[error("no se pudo resolver la ruta: {0}")]
    SinResolver(#[source] std::io::Error),
    #[error("argumentos inválidos")]
    ArgumentosInvalidos,
    #[error("esa ruta es una carpeta")]
    EsCarpeta,
    #[error("el archivo pesa más de {0} MiB")]
    ArchivoGrande(u64),
    #[error("el contenido pasa de {0} MiB")]
    ContenidoGrande(u64),
    #[error("el archivo pesa más de {0} MiB y no se puede respaldar para deshacer")]
    RespaldoGrande(u64),
    #[error("la carpeta donde va el archivo no existe")]
    CarpetaDestinoNoExiste,
    #[error("la escritura se aplicó, pero no se pudo registrar para deshacer: {0}")]
    RegistroDiario(#[from] crate::journal::Error),
    #[error("el archivo parece binario y no se lee como texto")]
    Binario,
    #[error("el archivo no está en UTF-8")]
    NoUtf8,
    #[error("falta old_string")]
    FaltaOldString,
    #[error("no encontré el texto en el archivo")]
    SinCoincidencias,
    #[error("hay {0} coincidencias; acota el texto o pasa replace_all")]
    VariasCoincidencias(usize),
    #[error("el resultado pasa de {0} MiB")]
    ResultadoGrande(u64),
    #[error("la ruta cambió entre la comprobación y la apertura")]
    RutaCambio,
    #[error("el archivo cambió entre la lectura y la escritura; no se pisó el cambio")]
    ArchivoCambio,
    #[error("no se pudo buscar en {}: {source}", path.display())]
    BusquedaFallida {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("falta el texto a buscar")]
    FaltaTexto,
    #[error("falta el programa")]
    FaltaPrograma,
    #[error("ese programa no está en la lista permitida")]
    ProgramaNoPermitido,
    #[error(
        "el programa {programa:?} no está en la lista de programas permitidos; permitidos: {permitidos}"
    )]
    ProgramaNoPermitidoEnBash {
        programa: String,
        permitidos: String,
    },
    #[error(
        "el comando usa sustitución de comandos o de procesos ($(...), <(...), >(...), @(...) o backticks); no se puede decidir qué ejecuta sin ejecutarlo y por eso no se puede validar contra la lista de programas permitidos; reescribe el comando sin sustitución"
    )]
    SustitucionNoValidableEnBash,
    #[error(
        "el comando usa ejecución anidada (system, popen o similar); no se puede validar contra la lista de programas permitidos"
    )]
    EjecucionAnidadaNoValidableEnBash,
    #[error("el programa no existe: {0}")]
    ProgramaNoExiste(#[source] std::io::Error),
    #[error("exec: {0:?}: executable file not found in {1}")]
    ProgramaNoEncontrado(String, &'static str),
    #[error("no se pudo preparar el entorno del comando: {0}")]
    PrepararEntorno(#[source] std::io::Error),
    #[error("el directorio de trabajo no es una carpeta")]
    CwdNoCarpeta,
    #[error("no se pudo ejecutar el programa: {0}")]
    Ejecutar(#[source] std::io::Error),
    #[error("el comando se pasó de {duracion} y se cortó. Salida: {salida}")]
    ComandoTimeout { duracion: String, salida: String },
    #[error("el comando terminó con error: {error}. Salida: {salida}")]
    ComandoFallo { error: String, salida: String },
    #[error("cancelado por el contexto")]
    Cancelado,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl Set {
    /// Valida las carpetas; sin carpetas buenas no hay herramientas locales.
    pub fn new(config: Config) -> Result<Self, Error> {
        let timeout = if config.timeout.is_zero() {
            TIMEOUT_COMANDO
        } else {
            config.timeout
        };
        let mut set = Self {
            roots: Vec::new(),
            read_roots: Vec::new(),
            nombres: HashSet::new(),
            rutas_prog: Vec::new(),
            timeout,
            journal: config.journal,
        };
        let mut faltan = Vec::new();
        for folder in config.folders {
            let folder = folder.trim();
            if folder.is_empty() {
                continue;
            }
            let path = Path::new(folder);
            if !path.is_absolute() || folder.starts_with("\\\\") || folder.starts_with("//") {
                return Err(Error::CarpetaNoAbsoluta(folder.to_owned()));
            }
            let Some(real) = canon::canon(path).ok() else {
                faltan.push(folder.to_owned());
                continue;
            };
            if std::fs::metadata(&real).is_ok_and(|info| info.is_dir()) {
                set.roots.push(real);
            } else {
                faltan.push(folder.to_owned());
            }
        }
        if set.roots.is_empty() {
            return Err(if faltan.is_empty() {
                Error::SinCarpetas
            } else {
                Error::CarpetasNoDisponibles(faltan)
            });
        }
        for folder in config.read_folders {
            let Some(real) = carpeta_lectura(&folder) else {
                continue;
            };
            if dentro_de(&set.roots, &real)
                || set.read_roots.iter().any(|vista| dentro(vista, &real))
            {
                continue;
            }
            set.read_roots.push(real);
        }
        for program in config.programs {
            let program = program.trim();
            if program.is_empty() {
                continue;
            }
            if program.contains(['/', '\\']) {
                let real = canon::canon(Path::new(program))
                    .map_err(|error| Error::ProgramaSinResolver(program.to_owned(), error))?;
                set.rutas_prog.push(clave_programa(&real.to_string_lossy()));
                continue;
            }
            set.nombres.insert(clave_programa(program));
        }
        Ok(set)
    }

    /// Deja la ruta real y comprueba que esté dentro de una carpeta permitida.
    /// Rechaza UNC, flujos de datos alternos, dispositivos y rutas relativas.
    /// Es la puerta de escritura y del deshacer: las carpetas de solo lectura
    /// no cuentan.
    fn resolve(&self, ruta: &str) -> Result<PathBuf, Error> {
        let real = canonica(ruta)?;
        if dentro_de(&self.roots, &real) {
            Ok(real)
        } else {
            Err(Error::Fuera)
        }
    }

    /// Igual que `resolve`, pero también acepta las carpetas de solo lectura
    /// (las skills cargadas). Solo la usan las herramientas de lectura.
    fn resolve_lectura(&self, ruta: &str) -> Result<PathBuf, Error> {
        let real = canonica(ruta)?;
        if dentro_de(&self.roots, &real) || dentro_de(&self.read_roots, &real) {
            Ok(real)
        } else {
            Err(Error::Fuera)
        }
    }

    /// Dice si una ruta cae dentro de las carpetas permitidas. El deshacer la
    /// usa para no restaurar ni borrar fuera de lo autorizado.
    pub fn check(&self, ruta: &str) -> Result<(), Error> {
        self.resolve(ruta).map(|_| ())
    }

    /// Las cuatro herramientas locales de Pi, en el orden del contrato 0.7.1.
    /// `read`/`write`/`edit` reutilizan los handlers y esquemas del CLI;
    /// `bash` es la única nueva. El registro del CLI no se toca.
    pub fn tools_pi(self: &Arc<Self>) -> Vec<Arc<dyn Tool>> {
        vec![
            Arc::new(HerramientaLocal::nueva(
                Arc::clone(self),
                "read",
                Risk::Read,
                "Lee un archivo de texto de una carpeta permitida, hasta 1 MiB.",
                lectura::esquema_lectura(),
                lectura::read_file_sin_cancelar,
            )),
            Arc::new(HerramientaLocal::nueva(
                Arc::clone(self),
                "write",
                Risk::Write,
                "Crea o reemplaza un archivo de una carpeta permitida.",
                lectura::esquema_escritura(),
                escritura::write_file,
            )),
            Arc::new(HerramientaLocal::nueva(
                Arc::clone(self),
                "edit",
                Risk::Write,
                "Reemplaza una porción exacta de un archivo (old_string → new_string). Falla sin escribir si no la encuentra; con varias coincidencias exige replace_all.",
                lectura::esquema_edicion(),
                edicion::edit_file,
            )),
            Arc::new(HerramientaLocal::nueva(
                Arc::clone(self),
                "bash",
                Risk::Exec,
                "Ejecuta un comando de shell en una carpeta permitida. En Windows exige Git Bash; en Unix, /bin/bash o sh. El cwd tiene que estar dentro de una carpeta permitida. No es una jaula del sistema: el comando puede tocar lo que el usuario pueda.",
                json!({
                    "type": "object",
                    "properties": {
                        "command": {"type": "string"},
                        "cwd": {"type": "string"},
                        "timeout": {
                            "type": "integer",
                            "minimum": 1,
                            "maximum": 600
                        }
                    },
                    "required": ["command"]
                }),
                bash::run_bash,
            )),
        ]
    }
}

/// La ruta real, con las comprobaciones de forma pero sin mirar las raíces.
fn canonica(ruta: &str) -> Result<PathBuf, Error> {
    let ruta = ruta.trim();
    if ruta.is_empty() {
        return Err(Error::FaltaRuta);
    }
    if ruta.starts_with("\\\\") || ruta.starts_with("//") {
        return Err(Error::Fuera);
    }
    let path = Path::new(ruta);
    // `:` y los nombres de dispositivo son reglas de Windows: en Linux son
    // nombres de archivo válidos y el usuario puede concederlos (#128).
    #[cfg(windows)]
    {
        if sin_volumen(ruta).contains(':') {
            return Err(Error::DosPuntos);
        }
        let base = path
            .file_name()
            .map(|nombre| nombre.to_string_lossy())
            .unwrap_or_default();
        if es_dispositivo(&base) {
            return Err(Error::Dispositivo);
        }
    }
    if !path.is_absolute() {
        return Err(Error::RutaRelativa);
    }
    match canon::canon(path) {
        Ok(real) => Ok(real),
        // Puede ser algo que todavía no existe: se resuelve la carpeta y
        // se conserva el nombre. Así crear un archivo nuevo no falla.
        Err(error) => resolver_sin_archivo(path, error),
    }
}

/// Una carpeta de solo lectura ya resuelta, o `None` si no sirve. La lista
/// viene de las skills cargadas: una puede desaparecer o no existir y eso no
/// invalida las herramientas locales.
fn carpeta_lectura(folder: &str) -> Option<PathBuf> {
    let folder = folder.trim();
    if folder.is_empty() || folder.starts_with("\\\\") || folder.starts_with("//") {
        return None;
    }
    if !Path::new(folder).is_absolute() {
        return None;
    }
    let real = canon::canon(Path::new(folder)).ok()?;
    if !std::fs::metadata(&real).is_ok_and(|info| info.is_dir()) {
        return None;
    }
    Some(real)
}

/// Una ruta que no existe se resuelve por su carpeta y conserva el nombre.
fn resolver_sin_archivo(ruta: &Path, error: std::io::Error) -> Result<PathBuf, Error> {
    let texto = ruta.to_string_lossy();
    if texto.ends_with('\\') || texto.ends_with('/') {
        return Err(Error::SinResolver(error));
    }
    let (Some(dir), Some(base)) = (ruta.parent(), ruta.file_name()) else {
        return Err(Error::SinResolver(error));
    };
    let dir = canon::canon(dir).map_err(Error::SinResolver)?;
    Ok(dir.join(base))
}

/// `filepath.VolumeName`: quita el `C:` del principio de una ruta de Windows.
#[cfg(windows)]
fn sin_volumen(ruta: &str) -> &str {
    let bytes = ruta.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        return &ruta[2..];
    }
    ruta
}

/// Nombres que Windows reserva para dispositivos, con cualquier número de
/// extensiones: `NUL.tar.gz` también es el dispositivo NUL (#94).
#[cfg(windows)]
fn es_dispositivo(base: &str) -> bool {
    let nombre = base.split('.').next().unwrap_or_default();
    let nombre = nombre.to_uppercase();
    if matches!(nombre.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
        return true;
    }
    let bytes = nombre.as_bytes();
    bytes.len() == 4
        && (nombre.starts_with("COM") || nombre.starts_with("LPT"))
        && (b'1'..=b'9').contains(&bytes[3])
}

/// En Windows los nombres no distinguen mayúsculas; en Unix `/opt/tool` y
/// `/opt/TOOL` son ejecutables distintos y plegarlos autorizaría el segundo.
pub(super) fn clave_programa(valor: &str) -> String {
    #[cfg(windows)]
    {
        valor.to_lowercase()
    }
    #[cfg(not(windows))]
    {
        valor.to_owned()
    }
}

/// `filepath.Rel` con el prefijo `..`: en Windows no distingue mayúsculas y
/// compara por componentes para que `C:\raiz2` no pase por `C:\raiz`.
fn dentro(root: &Path, ruta: &Path) -> bool {
    if cfg!(windows) {
        let root = root.to_string_lossy().to_lowercase();
        let ruta = ruta.to_string_lossy().to_lowercase();
        Path::new(&ruta).starts_with(Path::new(&root))
    } else {
        ruta.starts_with(root)
    }
}

/// Dice si la ruta cae dentro de alguna de las raíces.
fn dentro_de(raices: &[PathBuf], ruta: &Path) -> bool {
    raices.iter().any(|raiz| dentro(raiz, ruta))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests;

/// Pruebas del filtro de dispositivos con varias extensiones (issue #94).
/// Viven acá porque `tests.rs` lo trabaja otro lote. Solo Windows: en Linux
/// esos nombres son archivos normales (#128).
#[cfg(all(test, windows))]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests_dispositivo {
    use super::*;

    #[test]
    fn dispositivos_con_varias_extensiones_se_rechazan() {
        for nombre in [
            "NUL.tar.gz",
            "CON.a.b",
            "COM1.log.txt",
            "LPT9.x.y",
            "nul.TAR.GZ",
            "NUL.",
            "aux",
        ] {
            assert!(
                es_dispositivo(nombre),
                "{nombre} debía reconocerse como dispositivo"
            );
        }
        for nombre in [
            "NULx",
            "CONSOLE",
            "COM10",
            "LPT0",
            "archivo.txt",
            "NULX.tar.gz",
            "com10.log",
        ] {
            assert!(!es_dispositivo(nombre), "{nombre} no es un dispositivo");
        }
    }

    #[test]
    fn resolve_rechaza_dispositivo_con_varias_extensiones() {
        let base = tempfile::tempdir().expect("carpeta temporal");
        let set = Set::new(Config {
            folders: vec![base.path().to_string_lossy().into_owned()],
            ..Config::default()
        })
        .expect("Set con una carpeta temporal");
        for nombre in ["NUL.tar.gz", "CON.a.b", "COM1.log.txt"] {
            let ruta = base.path().join(nombre);
            assert!(
                matches!(
                    set.resolve(&ruta.to_string_lossy()),
                    Err(Error::Dispositivo)
                ),
                "{nombre} debía rechazarse como dispositivo"
            );
        }
    }
}
