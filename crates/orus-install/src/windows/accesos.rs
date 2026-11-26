//! Accesos directos del menú Inicio y del escritorio.
//!
//! Las rutas se
//! inyectan; los tests nunca escriben en el escritorio real.

use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use crate::windows::{Error, NOMBRE_APP, Registro};

const CLAVE_SHELL_FOLDERS: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Explorer\Shell Folders";

const CLAVE_USER_SHELL_FOLDERS: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Explorer\User Shell Folders";

/// Las dos carpetas donde el usuario busca el programa.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RutasAccesos {
    menu_inicio: PathBuf,
    escritorio: PathBuf,
}

impl RutasAccesos {
    pub fn new(menu_inicio: impl Into<PathBuf>, escritorio: impl Into<PathBuf>) -> Self {
        Self {
            menu_inicio: menu_inicio.into(),
            escritorio: escritorio.into(),
        }
    }

    /// `APPDATA\Microsoft\Windows\Start Menu\Programs` y la carpeta
    /// Desktop que dice el registro del usuario.
    pub fn system() -> Self {
        #[cfg(windows)]
        {
            Self::desde_registro(
                &crate::windows::RegistroWindows,
                env::var_os("USERPROFILE").as_deref(),
            )
        }
        #[cfg(not(windows))]
        {
            let perfil = env::var_os("USERPROFILE");
            Self::new(menu_inicio(), home_dir(perfil.as_deref()).join("Desktop"))
        }
    }

    /// Rutas con un registro inyectable y el perfil del usuario.
    /// `Shell Folders` (REG_SZ, ya expandido) manda; si falta se usa `User
    /// Shell Folders` (REG_EXPAND_SZ, crudo) expandiendo `%USERPROFILE%`, y
    /// al final el perfil.
    pub fn desde_registro(registro: &dyn Registro, perfil: Option<&OsStr>) -> Self {
        let escritorio = registro
            .leer_texto(CLAVE_SHELL_FOLDERS, "Desktop")
            .ok()
            .flatten()
            .or_else(|| {
                registro
                    .leer_texto(CLAVE_USER_SHELL_FOLDERS, "Desktop")
                    .ok()
                    .flatten()
                    .map(|texto| expandir_userprofile(&texto, perfil))
            })
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir(perfil).join("Desktop"));
        Self::new(menu_inicio(), escritorio)
    }

    /// Las dos rutas `.lnk` en el orden del contrato: menú Inicio y escritorio.
    pub fn lista(&self) -> [PathBuf; 2] {
        let nombre = format!("{NOMBRE_APP}.lnk");
        [
            self.menu_inicio.join(&nombre),
            self.escritorio.join(&nombre),
        ]
    }
}

/// Escribe un `.lnk`; en producción lo hace Windows y en tests una falsa.
pub trait CreadorAccesos {
    fn crear(&self, lnk: &Path, destino: &Path, trabajo: &Path) -> Result<(), String>;
}

/// Crea los dos accesos; si una carpeta no se puede crear, se salta ese
/// acceso, como.
pub fn crear_accesos(
    accesos: &RutasAccesos,
    destino: &Path,
    creador: &dyn CreadorAccesos,
) -> Result<(), Error> {
    let trabajo = destino.parent().unwrap_or(Path::new(""));
    for lnk in accesos.lista() {
        let padre = match lnk.parent() {
            Some(padre) if !padre.as_os_str().is_empty() => padre,
            _ => Path::new("."),
        };
        if fs::create_dir_all(padre).is_err() {
            continue;
        }
        let _ = fs::remove_file(&lnk);
        creador
            .crear(&lnk, destino, trabajo)
            .map_err(|mensaje| Error::Acceso {
                ruta: lnk.display().to_string(),
                mensaje,
            })?;
    }
    Ok(())
}

/// Crea el `.lnk` con WScript.Shell, igual que el contrato.
///
/// shortcut: se llama a PowerShell con WScript.Shell porque `windows-sys` no
/// expone `IShellLinkW` y D202 no aprueba la crate `windows`; si el instalador
/// necesita más propiedades, se cambia a COM directo.
#[cfg(windows)]
pub struct CreadorWsh;

#[cfg(windows)]
impl CreadorAccesos for CreadorWsh {
    fn crear(&self, lnk: &Path, destino: &Path, trabajo: &Path) -> Result<(), String> {
        use std::os::windows::process::CommandExt;
        use std::process::{Command, Stdio};

        use base64::Engine as _;

        const CREATE_NO_WINDOW: u32 = 0x0800_0000;

        let icono = format!("{},0", destino.display());
        let guion = format!(
            "$w = New-Object -ComObject WScript.Shell; \
             $s = $w.CreateShortcut({lnk}); \
             $s.TargetPath = {destino}; \
             $s.WorkingDirectory = {trabajo}; \
             $s.IconLocation = {icono}; \
             $s.Description = {descripcion}; \
             $s.Save()",
            lnk = ps(lnk),
            destino = ps(destino),
            trabajo = ps(trabajo),
            icono = ps_texto(&icono),
            descripcion = ps_texto(NOMBRE_APP),
        );
        let utf16: Vec<u8> = guion
            .encode_utf16()
            .flat_map(|unidad| unidad.to_le_bytes())
            .collect();
        let codificado = base64::engine::general_purpose::STANDARD.encode(utf16);

        let estado = Command::new(ruta_powershell())
            .args(["-NoProfile", "-NonInteractive", "-EncodedCommand"])
            .arg(codificado)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .status()
            .map_err(|error| error.to_string())?;
        if !estado.success() {
            return Err(format!(
                "PowerShell devolvió {}",
                estado.code().unwrap_or(-1)
            ));
        }
        Ok(())
    }
}

/// Ruta absoluta del PowerShell del sistema.
///
/// `Command::new("powershell.exe")` busca primero en el directorio actual: un
/// `powershell.exe` plantado junto al instalador se ejecutaría. La ruta de
/// System32 cierra esa puerta.
#[cfg(windows)]
fn ruta_powershell() -> PathBuf {
    ruta_powershell_en(env::var_os("SystemRoot").as_deref())
}

#[cfg(windows)]
fn ruta_powershell_en(raiz: Option<&std::ffi::OsStr>) -> PathBuf {
    let raiz = raiz
        .filter(|valor| !valor.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    raiz.join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe")
}

#[cfg(windows)]
fn ps(ruta: &Path) -> String {
    ps_texto(&ruta.display().to_string())
}

fn menu_inicio() -> PathBuf {
    env::var_os("APPDATA")
        .filter(|valor| !valor.is_empty())
        .map(PathBuf::from)
        .map(|base| {
            base.join("Microsoft")
                .join("Windows")
                .join("Start Menu")
                .join("Programs")
        })
        .unwrap_or_default()
}

fn expandir_userprofile(texto: &str, perfil: Option<&OsStr>) -> String {
    let perfil = perfil
        .map(|valor| valor.to_string_lossy().into_owned())
        .unwrap_or_default();
    texto.replace("%USERPROFILE%", &perfil)
}

/// `os.UserHomeDir` en Windows solo mira `USERPROFILE`. Si falta, el contrato deja
/// `home` vacío y el escritorio queda relativo (`Desktop`), como
/// No se inventa `HOMEDRIVE` + `HOMEPATH`.
fn home_dir(perfil: Option<&std::ffi::OsStr>) -> PathBuf {
    perfil
        .filter(|valor| !valor.is_empty())
        .map(PathBuf::from)
        .unwrap_or_default()
}

#[cfg(windows)]
fn ps_texto(texto: &str) -> String {
    format!("'{}'", texto.replace('\'', "''"))
}

#[cfg(all(test, windows))]
mod tests {
    use std::ffi::OsStr;

    use super::*;

    // Hallazgo 3: nunca se invoca `powershell.exe` por nombre.
    #[test]
    fn powershell_va_por_la_ruta_absoluta_de_system32() {
        let esperada = PathBuf::from(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe");
        assert_eq!(
            ruta_powershell_en(Some(OsStr::new(r"C:\Windows"))),
            esperada
        );
        assert_eq!(ruta_powershell_en(None), esperada);
        assert_eq!(ruta_powershell_en(Some(OsStr::new(""))), esperada);
    }

    // Hallazgo 6: sin `USERPROFILE` el escritorio es relativo; HOMEDRIVE y
    // HOMEPATH no se consultan.
    #[test]
    fn home_dir_solo_usa_userprofile() {
        assert_eq!(home_dir(None), PathBuf::new());
        assert_eq!(home_dir(Some(OsStr::new(""))), PathBuf::new());
        assert_eq!(home_dir(None).join("Desktop"), PathBuf::from("Desktop"));
        assert_eq!(
            home_dir(Some(OsStr::new(r"C:\Users\Ana"))).join("Desktop"),
            PathBuf::from(r"C:\Users\Ana\Desktop")
        );
    }
}
