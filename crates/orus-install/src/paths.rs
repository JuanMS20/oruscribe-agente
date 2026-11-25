//! Rutas del instalador por usuario.
//!
//! Port de `internal/install/install_windows.go:32-77` y de
//! `internal/install/install_other.go:19-70`. La carpeta base y el binario que
//! corre se inyectan: los tests no leen `LOCALAPPDATA`, `XDG_DATA_HOME` ni el
//! registro. El único camino que consulta el entorno es [`Rutas::system`].

use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use orus_config::clean_path;

const WINDOWS_APP_DIR: &str = "Oruscribe";
const WINDOWS_PROGRAMS_DIR: &str = "Programs";
const WINDOWS_EXE: &str = "Oruscribe Agente.exe";

const UNIX_APP_DIR: &str = "oruscribe";
const UNIX_EXE: &str = "oruscribe-agente";

/// Plataforma que decide la forma de las rutas.
///
/// `Linux` cubre las demás plataformas Unix, igual que `install_other.go`
/// cubre todo lo que no es Windows. El campo `platform` de `/api/setup` no usa
/// este enum: usa el nombre real del sistema ([`crate::platform`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Windows,
    Linux,
    Macos,
}

impl Platform {
    fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::Macos
        } else {
            Self::Linux
        }
    }
}

/// Carpeta base ya resuelta de la plataforma: `LOCALAPPDATA` en Windows;
/// `XDG_DATA_HOME` o `~/.local/share` en Unix.
///
/// La base puede quedar vacía cuando el sistema no da ninguna de esas
/// variables; ahí [`Rutas::dir`] y [`Rutas::exe`] devuelven una ruta vacía,
/// como el `""` de Go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rutas {
    platform: Platform,
    base: PathBuf,
}

impl Rutas {
    /// Arma las rutas con la base inyectada. Es el constructor que usan los
    /// tests y el que permite probar Windows y Linux desde cualquier host.
    pub fn new(platform: Platform, base: impl Into<PathBuf>) -> Self {
        Self {
            platform,
            base: base.into(),
        }
    }

    /// Arma las rutas con la base del sistema que corre el proceso.
    pub fn system() -> Self {
        let platform = Platform::current();
        Self::new(platform, system_base(platform))
    }

    /// Carpeta donde vive el programa instalado.
    pub fn dir(&self) -> PathBuf {
        if self.base.as_os_str().is_empty() {
            return PathBuf::new();
        }
        let dir = match self.platform {
            Platform::Windows => self.base.join(WINDOWS_PROGRAMS_DIR).join(WINDOWS_APP_DIR),
            Platform::Linux | Platform::Macos => self.base.join(UNIX_APP_DIR),
        };
        // `filepath.Join` limpia; Path::join no. La limpieza es léxica, igual
        // que Go, y no toca el disco.
        clean_path(&dir)
    }

    /// Ruta del ejecutable instalado.
    pub fn exe(&self) -> PathBuf {
        let dir = self.dir();
        if dir.as_os_str().is_empty() {
            return PathBuf::new();
        }
        dir.join(exe_name(self.platform))
    }

    /// Dice si el proceso corre desde la copia instalada, no solo si el exe
    /// existe en la carpeta de instalación.
    ///
    /// En Linux, un paquete nativo deja el binario en `/usr` o `/opt`: ahí
    /// también cuenta como instalado, como en `install_other.go:43-51`.
    pub fn installed(&self, current_exe: &Path) -> bool {
        // Go Windows: sin `Exe()` no hay instalación. Go Unix, en cambio,
        // siempre cae en `RunningFromInstall`, que dice true cuando no hay
        // destino con el que comparar (`install_windows.go:67-78`,
        // `install_other.go:43-51`).
        match self.platform {
            Platform::Windows if self.exe().as_os_str().is_empty() => false,
            _ => self.running_from_install(current_exe),
        }
    }

    /// Dice si el proceso que corre es la copia instalada.
    ///
    /// Windows compara sin distinguir mayúsculas (`EqualFold`). Linux suma
    /// `/usr` y `/opt`, que es donde deja el binario un paquete del sistema
    /// (`install_other.go:55-70`).
    pub fn running_from_install(&self, current_exe: &Path) -> bool {
        let casa = env_non_empty("HOME").map(PathBuf::from);
        self.running_from_install_con_casa(current_exe, casa.as_deref())
    }

    pub fn running_from_install_con_casa(&self, current_exe: &Path, casa: Option<&Path>) -> bool {
        let want = self.exe();
        if want.as_os_str().is_empty() {
            return true;
        }
        let Some(actual) = absolute(self.platform, current_exe) else {
            return false;
        };
        let Some(installed) = absolute(self.platform, &want) else {
            return false;
        };

        match self.platform {
            Platform::Windows => {
                actual.to_string_lossy().to_lowercase()
                    == installed.to_string_lossy().to_lowercase()
            }
            Platform::Linux | Platform::Macos => {
                if actual == installed || below(&actual, "/usr") || below(&actual, "/opt") {
                    return true;
                }
                if self.platform == Platform::Linux
                    && let Some(casa) = casa
                {
                    let instalador = clean_path(&casa.join(".local").join("bin").join(UNIX_EXE));
                    return actual == instalador;
                }
                false
            }
        }
    }

    /// Lo contrario de [`Rutas::running_from_install`]; es el `portable` de
    /// `internal/app/setup.go:32`.
    pub fn portable(&self, current_exe: &Path) -> bool {
        !self.running_from_install(current_exe)
    }
}

fn exe_name(platform: Platform) -> &'static str {
    match platform {
        Platform::Windows => WINDOWS_EXE,
        Platform::Linux | Platform::Macos => UNIX_EXE,
    }
}

fn system_base(platform: Platform) -> PathBuf {
    match platform {
        Platform::Windows => env_non_empty("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_default(),
        Platform::Linux | Platform::Macos => env_non_empty("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                env_non_empty("HOME").map(|home| PathBuf::from(home).join(".local").join("share"))
            })
            .unwrap_or_default(),
    }
}

fn env_non_empty(key: &str) -> Option<OsString> {
    env::var_os(key).filter(|value| !value.is_empty())
}

/// Absolutiza como `filepath.Abs`: no resuelve enlaces y limpia `.` y `..`.
///
/// En una plataforma Unix se respeta la raíz POSIX aunque el host sea Windows;
/// así la rama Linux se puede probar desde este equipo.
fn absolute(platform: Platform, path: &Path) -> Option<PathBuf> {
    let text = path.to_string_lossy();
    let absolute = match platform {
        Platform::Windows => std::path::absolute(path).ok()?,
        _ if text.starts_with('/') => PathBuf::from(&*text),
        _ => env::current_dir().ok()?.join(path),
    };
    Some(clean_path(&absolute))
}

/// Equivale a `strings.HasPrefix(ruta, "{base}/")`: `/usrx` no está bajo
/// `/usr`, y la base sola tampoco.
fn below(path: &Path, base: &str) -> bool {
    path != Path::new(base) && path.starts_with(base)
}
