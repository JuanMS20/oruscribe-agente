//! Desinstalación: accesos, claves y el `.cmd` que termina el trabajo.
//!
//! Port de `install_windows.go:192-232`. El programa no puede borrarse
//! mientras corre, así que un `.cmd` espera a que este proceso termine y borra
//! cada carpeta dos veces. `chcp 65001` es obligatorio: sin él, `cmd` lee las
//! rutas con la página OEM y no encuentra una carpeta con acentos.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::Rutas;
use crate::windows::Error;
use crate::windows::accesos::RutasAccesos;
use crate::windows::registro::{CLAVE_DESINSTALACION, CLAVE_DESINSTALACION_GO, Registro};

/// Pausa del `.cmd` antes de cada `rd` (`install_windows.go:204`).
pub const ESPERA: &str = "ping -n 4 127.0.0.1 >nul";

/// Prefijo del script temporal (`install_windows.go:203`). El sufijo aleatorio
/// evita colisiones en un `%TEMP%` compartido por varios procesos.
pub const PREFIJO_SCRIPT: &str = "oruscribe-desinstalar";

/// Lo que hay que lanzar después de sacar accesos y claves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Desinstalacion {
    pub script: PathBuf,
    pub carpetas: Vec<PathBuf>,
    pub contenido: String,
}

/// Deja escrito el `.cmd` y solo entonces quita accesos y claves, para que un
/// fallo al escribir no deje la app instalada pero invisible para Windows.
///
/// `datos` solo se incluye cuando el llamador ya decidió que se pueden borrar
/// (`install_windows.go:205-208`). No ejecuta el `.cmd`: eso queda en
/// `Desinstalacion::lanzar`.
pub fn preparar_desinstalacion(
    rutas: &Rutas,
    accesos: &RutasAccesos,
    registro: &dyn Registro,
    datos: Option<&Path>,
    temporal: &Path,
) -> Result<Desinstalacion, Error> {
    let mut carpetas = Vec::new();
    let carpeta = rutas.dir();
    if !carpeta.as_os_str().is_empty() {
        carpetas.push(carpeta);
    }
    if let Some(datos) = datos.filter(|datos| !datos.as_os_str().is_empty()) {
        carpetas.push(datos.to_path_buf());
    }

    let contenido = guion(ESPERA, &carpetas);
    fs::create_dir_all(temporal)?;
    let id = rand::random::<u64>();
    let script = temporal.join(format!("{PREFIJO_SCRIPT}-{id:016x}.cmd"));
    escribir_script(&script, &contenido)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&script, fs::Permissions::from_mode(0o600));
    }

    let mut fallos = Vec::new();
    for lnk in accesos.lista() {
        if let Err(error) = fs::remove_file(&lnk) {
            fallos.push(format!("el acceso {} ({error})", lnk.display()));
        }
    }
    for clave in [CLAVE_DESINSTALACION, CLAVE_DESINSTALACION_GO] {
        if let Err(error) = registro.borrar(clave) {
            fallos.push(format!("la clave {clave} ({error})"));
        }
    }
    if !fallos.is_empty() {
        return Err(Error::Mensaje(format!(
            "no se pudo preparar la desinstalación: {}",
            fallos.join("; ")
        )));
    }

    Ok(Desinstalacion {
        script,
        carpetas,
        contenido,
    })
}

/// Crea el `.cmd` sin seguir ni sobrescribir un archivo plantado en ese nombre.
fn escribir_script(script: &Path, contenido: &str) -> Result<(), Error> {
    let mut archivo = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(script)?;
    archivo.write_all(contenido.as_bytes())?;
    Ok(())
}

/// Arma el texto del `.cmd` con saltos CRLF, como Go.
///
/// En un `.cmd` el `%` inicia una variable: en la ruta se escribe `%%`.
pub fn guion(espera: &str, carpetas: &[PathBuf]) -> String {
    let mut lineas = vec!["@echo off".to_owned(), "chcp 65001 >nul".to_owned()];
    for carpeta in carpetas {
        let carpeta = carpeta.to_string_lossy().replace('%', "%%");
        for _ in 0..2 {
            lineas.push(espera.to_owned());
            lineas.push(format!("rd /s /q \"{carpeta}\""));
        }
    }
    lineas.push("del /q \"%~f0\"".to_owned());
    lineas.join("\r\n")
}

#[cfg(windows)]
impl Desinstalacion {
    /// Lanza `cmd.exe /c <script>` sin ventana y sin esperar a que termine.
    pub fn lanzar(&self) -> Result<(), Error> {
        use std::os::windows::process::CommandExt;
        use std::process::{Command, Stdio};

        const CREATE_NO_WINDOW: u32 = 0x0800_0000;

        Command::new(ruta_cmd())
            .arg("/c")
            .arg(&self.script)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map(|_| ())
            .map_err(Error::Io)
    }
}

/// Ruta absoluta del `cmd.exe` del sistema.
///
/// `Command::new("cmd.exe")` busca primero en el directorio de la app y en el
/// CWD: un `cmd.exe` plantado ahí se ejecutaría. La ruta de System32 cierra
/// esa puerta, como `ruta_powershell` en `accesos.rs`.
#[cfg(windows)]
fn ruta_cmd() -> PathBuf {
    ruta_cmd_en(std::env::var_os("SystemRoot").as_deref())
}

#[cfg(windows)]
fn ruta_cmd_en(raiz: Option<&std::ffi::OsStr>) -> PathBuf {
    let raiz = raiz
        .filter(|valor| !valor.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    raiz.join("System32").join("cmd.exe")
}

#[cfg(all(test, windows))]
mod tests {
    use std::ffi::OsStr;
    use std::path::PathBuf;

    use super::ruta_cmd_en;

    // El desinstalador no puede ejecutar un `cmd.exe` plantado junto a la app
    // o en el CWD: va por la ruta absoluta de System32, como PowerShell.
    #[test]
    fn cmd_va_por_la_ruta_absoluta_de_system32() {
        let esperada = PathBuf::from(r"C:\Windows\System32\cmd.exe");
        assert_eq!(ruta_cmd_en(Some(OsStr::new(r"C:\Windows"))), esperada);
        assert_eq!(ruta_cmd_en(None), esperada);
        assert_eq!(ruta_cmd_en(Some(OsStr::new(""))), esperada);
    }
}

#[cfg(test)]
mod tests_escritura {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::fs;

    use super::escribir_script;

    // Cualquier proceso con escritura en %TEMP% puede plantar el `.cmd`. Crear
    // con `create_new` falla en vez de truncar y ejecutar lo que otro puso.
    #[test]
    fn el_script_no_pisa_un_archivo_plantado() {
        let temp = tempfile::tempdir().expect("carpeta temporal");
        let script = temp.path().join("oruscribe-desinstalar.cmd");
        fs::write(&script, b"malicioso").expect("archivo plantado");

        assert!(escribir_script(&script, "contenido bueno").is_err());
        assert_eq!(
            fs::read(&script).expect("archivo original"),
            b"malicioso".to_vec()
        );
    }
}
