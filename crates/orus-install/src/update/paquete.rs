//! Extracción del binario desde el paquete `.tar.gz` de Linux.
//!
//! El paquete es el que publica la release (`oruscribe-agente-<versión>-linux-
//! <arch>.tar.gz`). De todo el archivo se usa **una** cosa: la entrada cuyo
//! nombre sea [`NOMBRE_BINARIO`]. Su contenido se escribe en la ruta que elige
//! el llamador; las rutas que traiga el tar no se usan nunca, ni para escribir
//! ni para comprobar, así que una entrada `../../.bashrc` no puede salir de
//! `destino` (frontera: los bytes del tar no eligen la ruta).

use std::fs::{File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::path::Path;

use flate2::read::GzDecoder;

use super::{Error, LIMITE_DESCARGA};

/// Nombre que tiene el ejecutable dentro del paquete.
pub const NOMBRE_BINARIO: &str = "oruscribe-agente";

/// Extrae `oruscribe-agente` de `paquete` (gzip + tar) y lo deja en `destino`.
///
/// `destino` se crea o se trunca con modo 0755. Si el paquete no trae la
/// entrada, o el gzip/tar está roto, devuelve error y no deja el archivo a
/// medias. Un contenido mayor que [`LIMITE_DESCARGA`] se corta con error: el
/// paquete no puede pedir más disco del que se acepta al bajar.
pub fn extraer_binario(paquete: &Path, destino: &Path) -> Result<(), Error> {
    extraer_binario_hasta(paquete, destino, LIMITE_DESCARGA)
}

/// Igual que [`extraer_binario`] con un tope explícito, para poder probar el
/// corte sin escribir 512 MiB en disco.
fn extraer_binario_hasta(paquete: &Path, destino: &Path, tope: usize) -> Result<(), Error> {
    let archivo = File::open(paquete)?;
    let mut comprimido = tar::Archive::new(GzDecoder::new(archivo));
    let entradas = comprimido
        .entries()
        .map_err(|error| Error::Paquete(error.to_string()))?;

    for entrada in entradas {
        let entrada = entrada.map_err(|error| Error::Paquete(error.to_string()))?;
        if !entrada.header().entry_type().is_file() || !se_llama(&entrada, NOMBRE_BINARIO) {
            continue;
        }
        return escribir(entrada, destino, tope);
    }
    Err(Error::PaqueteSinBinario(NOMBRE_BINARIO.to_owned()))
}

/// ¿La entrada se llama `nombre`? Solo se mira el último componente, sin usar
/// la ruta para nada más.
fn se_llama<R: std::io::Read>(entrada: &tar::Entry<'_, R>, nombre: &str) -> bool {
    entrada
        .path()
        .ok()
        .and_then(|ruta| ruta.file_name().map(|final_| final_ == nombre))
        .unwrap_or(false)
}

/// Copia la entrada a `destino` con tope y permisos de ejecución.
fn escribir<R: std::io::Read>(
    mut entrada: tar::Entry<'_, R>,
    destino: &Path,
    tope: usize,
) -> Result<(), Error> {
    let _ = std::fs::remove_file(destino);
    let mut opciones = OpenOptions::new();
    opciones.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opciones.mode(0o755);
    }
    let mut salida = opciones.open(destino)?;
    // Un byte de más para poder distinguir "justo en el tope" de "se pasó".
    let limite = tope as u64 + 1;
    let copiados = match std::io::copy(&mut entrada.by_ref().take(limite), &mut salida) {
        Ok(copiados) => copiados,
        Err(error) => {
            drop(salida);
            let _ = std::fs::remove_file(destino);
            return Err(Error::Paquete(error.to_string()));
        }
    };
    salida.flush()?;
    drop(salida);
    if copiados > tope as u64 {
        let _ = std::fs::remove_file(destino);
        return Err(Error::PaqueteDemasiadoGrande);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(destino, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::{NOMBRE_BINARIO, extraer_binario_hasta};

    /// Un `.tar.gz` con una sola entrada; el contenido se repite para que el
    /// tope pequeño del test corte de verdad.
    fn paquete_de(cuerpo: &[u8]) -> Vec<u8> {
        let comprimido = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut constructor = tar::Builder::new(comprimido);
        let mut cabecera = tar::Header::new_gnu();
        cabecera.set_mode(0o755);
        cabecera.set_size(cuerpo.len() as u64);
        cabecera.set_cksum();
        constructor
            .append_data(&mut cabecera, NOMBRE_BINARIO, cuerpo)
            .expect("entrada del tar");
        constructor
            .into_inner()
            .expect("cerrar el tar")
            .finish()
            .expect("cerrar el gzip")
    }

    /// El tope real son 512 MiB: escribirlos en el test costaría medio
    /// gigabyte de disco, así que se prueba el mismo corte con un tope chico.
    #[test]
    fn una_entrada_mayor_que_el_tope_se_corta_y_no_deja_archivo() {
        let dir = tempfile::tempdir().expect("carpeta temporal");
        let paquete = dir.path().join("paquete.tar.gz");
        std::fs::write(&paquete, paquete_de(&vec![b'x'; 5000])).expect("paquete");
        let destino = dir.path().join("nuevo");

        let error = extraer_binario_hasta(&paquete, &destino, 1024).expect_err("se pasa del tope");

        assert_eq!(
            error.to_string(),
            "el paquete trae un binario más grande que el tope de descarga"
        );
        assert!(!destino.exists(), "el archivo cortado quedó en disco");
    }

    #[test]
    fn una_entrada_justo_en_el_tope_se_extrae() {
        let dir = tempfile::tempdir().expect("carpeta temporal");
        let paquete = dir.path().join("paquete.tar.gz");
        std::fs::write(&paquete, paquete_de(&vec![b'x'; 1024])).expect("paquete");
        let destino = dir.path().join("nuevo");

        extraer_binario_hasta(&paquete, &destino, 1024).expect("justo en el tope");

        assert_eq!(std::fs::read(&destino).expect("binario").len(), 1024);
    }
}
