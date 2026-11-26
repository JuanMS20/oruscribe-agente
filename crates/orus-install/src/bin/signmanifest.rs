//! Firma `latest.json` con la llave privada de Oruscribe.
//!
//! Contrato de firma (D203):
//!
//! ```text
//! signmanifest -gen -out llave.txt
//! signmanifest -key llave.txt -sign build/release/latest.json
//! signmanifest -verify build/release/latest.json -pub <llave publica base64>
//! ```
//!
//! La llave privada es la semilla Ed25519 de 32 bytes en base64, con salto de
//! línea. La firma se escribe en `<manifiesto>.sig`, también en base64, sin
//! tocar el manifiesto.

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::{Signature, Signer as _, SigningKey, VerifyingKey};
use rand::RngCore as _;

fn main() -> ExitCode {
    match ejecutar(std::env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(Fallo::Uso(mensaje)) => {
            eprintln!("signmanifest: {mensaje}");
            eprintln!(
                "uso: signmanifest -gen -out <llave> | -key <llave> -sign <manifiesto> | -verify <manifiesto> -pub <base64>"
            );
            ExitCode::from(2)
        }
        Err(Fallo::Error(mensaje)) => {
            eprintln!("signmanifest: {mensaje}");
            ExitCode::FAILURE
        }
    }
}

enum Fallo {
    Uso(String),
    Error(String),
}

impl From<String> for Fallo {
    fn from(mensaje: String) -> Self {
        Self::Error(mensaje)
    }
}

fn ejecutar(args: Vec<String>) -> Result<(), Fallo> {
    let mut crear = false;
    let mut out = None;
    let mut key = None;
    let mut sign = None;
    let mut verify = None;
    let mut pub_key = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-gen" => crear = true,
            "-out" => {
                i += 1;
                out = Some(valor(&args, i, "-out")?);
            }
            "-key" => {
                i += 1;
                key = Some(valor(&args, i, "-key")?);
            }
            "-sign" => {
                i += 1;
                sign = Some(valor(&args, i, "-sign")?);
            }
            "-verify" => {
                i += 1;
                verify = Some(valor(&args, i, "-verify")?);
            }
            "-pub" => {
                i += 1;
                pub_key = Some(valor(&args, i, "-pub")?);
            }
            otro => return Err(Fallo::Uso(format!("argumento desconocido: {otro}"))),
        }
        i += 1;
    }

    match (crear, sign, verify) {
        (true, None, None) => generar(
            out.as_deref()
                .ok_or_else(|| Fallo::Uso("falta -out".to_owned()))?,
        )
        .map_err(Fallo::Error),
        (false, Some(manifiesto), None) => firmar(
            key.as_deref()
                .ok_or_else(|| Fallo::Uso("falta -key".to_owned()))?,
            &manifiesto,
        )
        .map_err(Fallo::Error),
        (false, None, Some(manifiesto)) => verificar(
            &manifiesto,
            pub_key
                .as_deref()
                .ok_or_else(|| Fallo::Uso("falta -pub".to_owned()))?,
        )
        .map_err(Fallo::Error),
        _ => Err(Fallo::Uso(
            "elige una acción: -gen, -sign o -verify".to_owned(),
        )),
    }
}

fn valor(args: &[String], indice: usize, nombre: &str) -> Result<String, Fallo> {
    args.get(indice)
        .cloned()
        .ok_or_else(|| Fallo::Uso(format!("{nombre} necesita un valor")))
}

fn generar(out: &str) -> Result<(), String> {
    let mut semilla = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut semilla);
    let llave = SigningKey::from_bytes(&semilla);

    let mut opciones = OpenOptions::new();
    opciones.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opciones.mode(0o600);
    }
    let mut archivo = opciones
        .open(out)
        .map_err(|error| format!("no se pudo crear {out}: {error}"))?;
    if let Err(error) = writeln!(archivo, "{}", STANDARD.encode(semilla)) {
        drop(archivo);
        let _ = fs::remove_file(out);
        return Err(error.to_string());
    }

    println!("Llave pública (pégala en TRUSTED_KEYS_B64 de keys.rs):");
    println!("{}", STANDARD.encode(llave.verifying_key().to_bytes()));
    Ok(())
}

fn firmar(key_path: &str, manifiesto: &str) -> Result<(), String> {
    let crudo = fs::read_to_string(key_path).map_err(|error| error.to_string())?;
    let semilla = STANDARD
        .decode(crudo.trim())
        .map_err(|_| format!("{key_path} no tiene una llave privada válida"))?;
    let semilla: [u8; 32] = semilla
        .try_into()
        .map_err(|_| format!("{key_path} no tiene una llave privada válida"))?;

    let cuerpo = fs::read(manifiesto).map_err(|error| error.to_string())?;
    let firma = SigningKey::from_bytes(&semilla).sign(&cuerpo);
    let sig_path = PathBuf::from(format!("{manifiesto}.sig"));
    fs::write(&sig_path, STANDARD.encode(firma.to_bytes())).map_err(|error| error.to_string())?;
    println!("Firma escrita en {}", sig_path.display());
    Ok(())
}

fn verificar(manifiesto: &str, publica_b64: &str) -> Result<(), String> {
    let publica = STANDARD
        .decode(publica_b64.trim())
        .map_err(|_| "falta -pub o la llave pública no es válida".to_owned())?;
    let publica: [u8; 32] = publica
        .try_into()
        .map_err(|_| "falta -pub o la llave pública no es válida".to_owned())?;
    let llave = VerifyingKey::from_bytes(&publica)
        .map_err(|_| "falta -pub o la llave pública no es válida".to_owned())?;

    let cuerpo = fs::read(manifiesto).map_err(|error| error.to_string())?;
    let sig_path = PathBuf::from(format!("{manifiesto}.sig"));
    let sig_b64 = fs::read_to_string(&sig_path)
        .map_err(|error| format!("no se pudo leer la firma: {error}"))?;
    let firma = STANDARD
        .decode(sig_b64.trim())
        .map_err(|_| format!("{} no está en base64 válido", sig_path.display()))?;
    let firma = Signature::from_slice(&firma)
        .map_err(|_| format!("{} no tiene una firma válida", sig_path.display()))?;
    llave.verify_strict(&cuerpo, &firma).map_err(|_| {
        "firma INVÁLIDA: el manifiesto no lo firmó esa llave o cambió después".to_owned()
    })?;
    println!("Firma VÁLIDA");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base64_semilla(semilla: [u8; 32]) -> String {
        STANDARD.encode(semilla)
    }

    #[test]
    fn firma_y_verifica_los_bytes_exactos() {
        let dir = tempfile::tempdir().expect("carpeta temporal");
        let key = dir.path().join("llave.txt");
        let manifiesto = dir.path().join("latest.json");
        let semilla = [7u8; 32];
        fs::write(&key, format!("{}\n", base64_semilla(semilla))).expect("llave");
        fs::write(&manifiesto, br#"{"version":"0.6.0"}"#).expect("manifiesto");

        firmar(
            key.to_str().expect("ruta"),
            manifiesto.to_str().expect("ruta"),
        )
        .expect("firmar");
        let publica = STANDARD.encode(SigningKey::from_bytes(&semilla).verifying_key().to_bytes());
        verificar(manifiesto.to_str().expect("ruta"), &publica).expect("verificar");
    }

    #[test]
    fn un_byte_distinto_invalida_la_firma() {
        let dir = tempfile::tempdir().expect("carpeta temporal");
        let key = dir.path().join("llave.txt");
        let manifiesto = dir.path().join("latest.json");
        let semilla = [9u8; 32];
        fs::write(&key, format!("{}\n", base64_semilla(semilla))).expect("llave");
        fs::write(&manifiesto, b"original").expect("manifiesto");

        firmar(
            key.to_str().expect("ruta"),
            manifiesto.to_str().expect("ruta"),
        )
        .expect("firmar");
        fs::write(&manifiesto, b"cambiado").expect("manifiesto cambiado");
        let publica = STANDARD.encode(SigningKey::from_bytes(&semilla).verifying_key().to_bytes());
        assert!(verificar(manifiesto.to_str().expect("ruta"), &publica).is_err());
    }

    #[test]
    fn una_llave_que_no_es_de_32_bytes_falla() {
        let dir = tempfile::tempdir().expect("carpeta temporal");
        let key = dir.path().join("llave.txt");
        let manifiesto = dir.path().join("latest.json");
        fs::write(&key, STANDARD.encode([1u8; 31])).expect("llave corta");
        fs::write(&manifiesto, b"x").expect("manifiesto");

        let error = firmar(
            key.to_str().expect("ruta"),
            manifiesto.to_str().expect("ruta"),
        )
        .expect_err("debe fallar");
        assert!(error.contains("no tiene una llave privada válida"));
    }

    #[test]
    fn el_archivo_de_llave_no_se_sobrescribe() {
        let dir = tempfile::tempdir().expect("carpeta temporal");
        let key = dir.path().join("llave.txt");
        fs::write(&key, "existente").expect("llave previa");
        let error = generar(key.to_str().expect("ruta")).expect_err("no debe sobrescribir");
        assert!(error.contains("no se pudo crear"));
    }
}
