//! Registro de Windows: desinstalación, autostart y consulta de entradas.
//!
//! [`Registro`] es la interfaz inyectable; el backend real
//! ([`crate::windows::RegistroWindows`]) vive en `registro_windows.rs` y no se
//! usa en tests.

use std::path::Path;

use crate::windows::{Error, NOMBRE_APP};

/// Raíz donde Agregar o quitar programas busca las entradas.
pub const CLAVE_DESINSTALACION_PADRE: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall";

/// Clave canónica (D201), la que escribe NSIS y la que adopta Rust.
pub const CLAVE_DESINSTALACION: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Oruscribe Agente";

/// Clave del instalador anterior; se migra y se borra (D201).
pub const CLAVE_DESINSTALACION_GO: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Uninstall\OruscribeAgente";

/// Clave de arranque automático por usuario.
pub const CLAVE_INICIO: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// Valor que usaba Wails.
pub const VALOR_AUTOSTART: &str = "oruscribe-agente";

/// Editor de la entrada de desinstalación.
pub const EDITOR: &str = "Oruscribe";

/// Un valor de registro: cadena o DWORD.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Valor {
    Texto(String),
    Dword(u32),
}

/// Operaciones de registro que necesita el instalador.
pub trait Registro {
    /// Crea (o abre) la clave. El contrato envuelve solo este error con
    /// "no se pudo registrar la desinstalación".
    fn crear(&self, clave: &str) -> Result<(), Error>;
    fn escribir(&self, clave: &str, nombre: &str, valor: Valor) -> Result<(), Error>;
    /// Texto de un valor `REG_SZ` o `REG_EXPAND_SZ`, crudo y sin expandir;
    /// cualquier otro tipo devuelve `None`. Quien llama expande variables.
    fn leer_texto(&self, clave: &str, nombre: &str) -> Result<Option<String>, Error>;
    fn borrar_valor(&self, clave: &str, nombre: &str) -> Result<(), Error>;
    fn borrar(&self, clave: &str) -> Result<(), Error>;
    fn subclaves(&self, clave: &str) -> Result<Vec<String>, Error>;
    /// Valores de texto de una clave. El autostart de Wails busca por ruta
    /// entre todos los nombres, no por un nombre fijo.
    fn valores(&self, clave: &str) -> Result<Vec<(String, String)>, Error>;
}

/// Escribe los valores de Agregar o quitar programas con los nombres del contrato.
pub fn escribir_entrada(registro: &dyn Registro, exe: &Path, version: &str) -> Result<(), Error> {
    let version = if version.is_empty() { "0.0.0" } else { version };
    let exe_texto = exe.to_string_lossy();
    let carpeta = exe
        .parent()
        .map(|padre| padre.to_string_lossy().into_owned())
        .unwrap_or_default();
    let desinstalar = format!("\"{exe_texto}\" --uninstall");

    registro
        .crear(CLAVE_DESINSTALACION)
        .map_err(|error| Error::Registrar(error.to_string()))?;
    let textos = [
        ("DisplayName", NOMBRE_APP.to_owned()),
        ("DisplayVersion", version.to_owned()),
        ("Publisher", EDITOR.to_owned()),
        ("DisplayIcon", exe_texto.into_owned()),
        ("InstallLocation", carpeta),
        ("UninstallString", desinstalar.clone()),
        ("QuietUninstallString", desinstalar),
    ];
    for (nombre, valor) in textos {
        registro.escribir(CLAVE_DESINSTALACION, nombre, Valor::Texto(valor))?;
    }
    for nombre in ["NoModify", "NoRepair"] {
        registro.escribir(CLAVE_DESINSTALACION, nombre, Valor::Dword(1))?;
    }
    Ok(())
}

/// Tras una actualización interna, Agregar o quitar programas debe mostrar la
/// versión nueva. Solo cambia `DisplayVersion`: `UninstallString` sigue
/// apuntando al mismo exe y el setup no se toca.
pub fn actualizar_version(registro: &dyn Registro, version: &str) -> Result<(), Error> {
    let version = if version.is_empty() { "0.0.0" } else { version };
    registro.escribir(
        CLAVE_DESINSTALACION,
        "DisplayVersion",
        Valor::Texto(version.to_owned()),
    )
}

/// Borra la clave del instalador anterior, si quedó (D201).
pub fn migrar_entrada_anterior(registro: &dyn Registro) -> Result<(), Error> {
    registro.borrar(CLAVE_DESINSTALACION_GO)
}

/// Nombres de las entradas de Oruscribe bajo Desinstalar.
///
/// El gate exige una sola: por eso también mira la clave vieja.
pub fn entradas_oruscribe(registro: &dyn Registro) -> Result<Vec<String>, Error> {
    let mut nombres = registro.subclaves(CLAVE_DESINSTALACION_PADRE)?;
    nombres.retain(|nombre| nombre.to_lowercase().contains("oruscribe"));
    nombres.sort();
    Ok(nombres)
}

/// Escribe `oruscribe-agente` en `HKCU\...\Run` con la ruta citada si tiene
/// espacios.
///
/// Antes borra una entrada vieja con otro nombre que apunte al mismo exe, como
pub fn activar_autostart(registro: &dyn Registro, exe: &Path) -> Result<(), Error> {
    if let Ok(nombres) = encontrar(registro, exe) {
        for nombre in nombres
            .into_iter()
            .filter(|nombre| nombre != VALOR_AUTOSTART)
        {
            let _ = registro.borrar_valor(CLAVE_INICIO, &nombre);
        }
    }
    registro.escribir(CLAVE_INICIO, VALOR_AUTOSTART, Valor::Texto(citar(exe)))
}

/// Borra todos los valores de arranque que apuntan a `exe`; si no hay, no es error.
///
/// Se busca por ruta porque el nombre puede no ser `oruscribe-agente` (Wails
/// usaba un identificador configurable).
pub fn desactivar_autostart(registro: &dyn Registro, exe: &Path) -> Result<(), Error> {
    for nombre in encontrar(registro, exe)? {
        registro.borrar_valor(CLAVE_INICIO, &nombre)?;
    }
    Ok(())
}

/// Dice si hay un valor de arranque que apunta al exe actual.
pub fn autostart_activo(registro: &dyn Registro, exe: &Path) -> Result<bool, Error> {
    Ok(!encontrar(registro, exe)?.is_empty())
}

/// Nombres de los valores de `Run` cuyo comando apunta a `exe`.
fn encontrar(registro: &dyn Registro, exe: &Path) -> Result<Vec<String>, Error> {
    let buscado = exe.to_string_lossy().to_lowercase();
    let mut nombres = Vec::new();
    for (nombre, comando) in registro.valores(CLAVE_INICIO)? {
        if comando_exe(&comando) == buscado {
            nombres.push(nombre);
        }
    }
    Ok(nombres)
}

/// Primer token de una línea de comando, sin comillas y en minúsculas, como
fn comando_exe(comando: &str) -> String {
    let comando = comando.trim();
    if comando.is_empty() {
        return String::new();
    }
    if let Some(resto) = comando.strip_prefix('"') {
        let fin = resto.find('"').unwrap_or(resto.len());
        return resto[..fin].to_lowercase();
    }
    let fin = comando.find([' ', '\t']).unwrap_or(comando.len());
    comando[..fin].to_lowercase()
}

fn citar(ruta: &Path) -> String {
    let texto = ruta.to_string_lossy();
    if texto.contains([' ', '\t', '"']) {
        format!("\"{}\"", texto.replace('"', "\\\""))
    } else {
        texto.into_owned()
    }
}
