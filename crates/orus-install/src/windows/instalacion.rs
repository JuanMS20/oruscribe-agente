//! Copia atómica del ejecutable y registro de la instalación.
//!
//! Primero `.nuevo`, luego
//! `sync_all` y recién entonces el rename sobre el destino. Un corte de
//! energía no deja un `.exe` truncado en su sitio.

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

use crate::Rutas;
use crate::windows::Error;
use crate::windows::accesos::{CreadorAccesos, RutasAccesos, crear_accesos};
use crate::windows::registro::{Registro, escribir_entrada, migrar_entrada_anterior};

/// Instala `origen` bajo la carpeta por usuario y deja accesos y registro.
pub fn instalar(
    rutas: &Rutas,
    accesos: &RutasAccesos,
    registro: &dyn Registro,
    creador: &dyn CreadorAccesos,
    origen: &Path,
    version: &str,
) -> Result<(), Error> {
    let carpeta = rutas.dir();
    if carpeta.as_os_str().is_empty() {
        return Err(Error::Mensaje(
            "este Windows no tiene carpeta de usuario".to_owned(),
        ));
    }
    fs::create_dir_all(&carpeta).map_err(|error| {
        Error::Mensaje(format!("no se pudo crear {}: {error}", carpeta.display()))
    })?;

    let destino = rutas.exe();
    if !misma_ruta(origen, &destino) {
        copiar_atomico(origen, &destino)?;
    }

    escribir_entrada(registro, &destino, version)?;
    // El contrato no migra y sigue; borrar la clave vieja es un extra que no puede
    // impedir que queden el registro nuevo y los accesos.
    if let Err(error) = migrar_entrada_anterior(registro) {
        eprintln!("no se pudo borrar la entrada anterior de Oruscribe: {error}");
    }

    crear_accesos(accesos, &destino, creador)
}

fn copiar_atomico(origen: &Path, destino: &Path) -> Result<(), Error> {
    let nuevo = con_sufijo(destino, ".nuevo");
    let mut entrada = File::open(origen).map_err(error_copia)?;
    // Un symlink plantado en `.nuevo` no se sigue: se borra el enlace y el
    // archivo se crea exclusivo, como `download.rs`.
    let _ = fs::remove_file(&nuevo);
    let mut salida = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&nuevo)
        .map_err(error_copia)?;
    io::copy(&mut entrada, &mut salida).map_err(error_copia)?;
    salida.sync_all().map_err(error_copia)?;
    drop(salida);

    if let Err(error) = fs::rename(&nuevo, destino) {
        let _ = fs::remove_file(&nuevo);
        return Err(Error::Mensaje(format!(
            "no se pudo colocar el programa: {error}"
        )));
    }
    Ok(())
}

fn error_copia(error: io::Error) -> Error {
    Error::Mensaje(format!("no se pudo copiar el programa: {error}"))
}

fn con_sufijo(ruta: &Path, sufijo: &str) -> PathBuf {
    let mut texto = ruta.as_os_str().to_owned();
    texto.push(sufijo);
    PathBuf::from(texto)
}

/// Windows compara sin distinguir mayúsculas.
fn misma_ruta(a: &Path, b: &Path) -> bool {
    #[cfg(windows)]
    {
        a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    }
    #[cfg(not(windows))]
    {
        a == b
    }
}
