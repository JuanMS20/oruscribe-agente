//! Instalación real por usuario en Windows.
//!
//! Port de `internal/install/install_windows.go:21-274`,
//! `internal/install/shortcut.go:12-69`,
//! `single_windows.go:21-51` y del autostart de Wails
//! (`autostart_windows.go:13,30-66`).
//!
//! El registro y la creación de accesos se inyectan: los tests usan un
//! registro en memoria y una carpeta temporal, nunca `HKCU` ni el perfil real.
//! [`RegistroWindows`] y [`CreadorWsh`] son las únicas piezas que tocan el
//! sistema y solo existen en Windows.

mod accesos;
mod desinstalacion;
mod instalacion;
mod registro;
#[cfg(windows)]
mod registro_windows;

#[cfg(windows)]
use std::path::Path;

#[cfg(windows)]
pub use accesos::CreadorWsh;
pub use accesos::{CreadorAccesos, RutasAccesos};
pub use desinstalacion::{Desinstalacion, guion, preparar_desinstalacion};
pub use instalacion::instalar;
pub use registro::{
    CLAVE_DESINSTALACION, CLAVE_DESINSTALACION_GO, CLAVE_DESINSTALACION_PADRE, CLAVE_INICIO,
    Registro, VALOR_AUTOSTART, Valor, activar_autostart, actualizar_version, autostart_activo,
    desactivar_autostart, entradas_oruscribe, escribir_entrada, migrar_entrada_go,
};
#[cfg(windows)]
pub use registro_windows::RegistroWindows;

#[cfg(windows)]
use crate::Rutas;

/// Nombre visible en accesos, menú Inicio y Agregar o quitar programas.
pub const NOMBRE_APP: &str = "Oruscribe Agente";

/// Nombre del ejecutable; el espacio es parte del producto.
pub const NOMBRE_EXE: &str = "Oruscribe Agente.exe";

/// Error de instalación con los textos de Go donde el contrato los fija.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Mensaje(String),
    #[error("{}", texto_windows(.0))]
    Windows(u32),
    #[error("no se pudo registrar la desinstalación: {0}")]
    Registrar(String),
    #[error("acceso directo {ruta}: {mensaje}")]
    Acceso { ruta: String, mensaje: String },
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Mensaje que Windows da para un código, como el `%w` de Go.
///
/// `std::io::Error` agrega " (os error N)"; se quita para dejar solo el texto
/// del sistema, que es lo que conserva `install_windows.go:155-159`.
fn texto_windows(codigo: &u32) -> String {
    let completo = std::io::Error::from_raw_os_error(*codigo as i32).to_string();
    let sufijo = format!(" (os error {codigo})");
    completo
        .strip_suffix(&sufijo)
        .unwrap_or(&completo)
        .to_owned()
}

/// Instala el ejecutable que corre sobre una instalación existente.
///
/// La versión viaja al registro; vacía se escribe como `0.0.0`, igual que
/// `install_windows.go:162-165`.
#[cfg(windows)]
pub fn instalar_sistema(origen: &Path) -> Result<(), Error> {
    instalar(
        &Rutas::system(),
        &RutasAccesos::system(),
        &RegistroWindows,
        &CreadorWsh,
        origen,
        crate::version(),
    )
}

/// Desinstala usando el registro, los accesos y la carpeta temporal reales.
///
/// `remove_data` solo borra datos si además la carpeta es la de por defecto
/// (`internal/config/config.go:135-160`).
#[cfg(windows)]
pub fn desinstalar(remove_data: bool) -> Result<(), Error> {
    let datos = if remove_data && crate::data_dir_removable() {
        Some(orus_config::data_dir())
    } else {
        None
    };
    let plan = preparar_desinstalacion(
        &Rutas::system(),
        &RutasAccesos::system(),
        &RegistroWindows,
        datos.as_deref(),
        &std::env::temp_dir(),
    )?;
    plan.lanzar()
}

/// Lee `--uninstall` y `--remove-data` de la línea de comandos.
///
/// Devuelve `None` si no se pidió desinstalar. `--remove-data` solo cuenta si
/// viene con `--uninstall`, como en `main.go:79-88`.
pub fn argumentos_desinstalacion(args: &[String]) -> Option<bool> {
    if args.iter().any(|arg| arg == "--uninstall") {
        Some(args.iter().any(|arg| arg == "--remove-data"))
    } else {
        None
    }
}
