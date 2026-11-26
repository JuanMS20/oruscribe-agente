//! Normalización de las imágenes que adjunta la ventana.
//!
//! Topes, MIME real por firma,
//! base64 y vista previa.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use orus_core::Image;

use super::{AgentError, AgentImage};

/// Los topes del adjunto. El cuerpo HTTP de la ruta
/// deja aire para 6 MB de imágenes en base64.
pub(super) const MAX_IMAGENES: usize = 4;
pub(super) const MAX_IMAGEN_BYTES: usize = 2 << 20;
const MAX_IMAGENES_BYTES: usize = 6 << 20;
pub(super) const MAX_PREVIEW_BYTES: usize = 256 << 10;

/// Valida el adjunto antes de que toque la conversación: tipo real por los
/// primeros bytes, tamaño por imagen y en total, y base64 decodificable. El
/// MIME que declara la ventana no se cree solo
pub fn normalizar_imagenes(entrada: Vec<AgentImage>) -> Result<Vec<Image>, AgentError> {
    if entrada.is_empty() {
        return Ok(Vec::new());
    }
    if entrada.len() > MAX_IMAGENES {
        return Err(invalida(format!(
            "puedes adjuntar hasta {MAX_IMAGENES} imágenes por mensaje"
        )));
    }
    let mut salida = Vec::with_capacity(entrada.len());
    let mut total = 0usize;
    for imagen in entrada {
        let firma = firma_de(&imagen.mime)
            .ok_or_else(|| invalida("la imagen debe ser PNG, JPEG, GIF o WebP".to_owned()))?;
        let datos = decodificar_base64(&imagen.data)
            .filter(|datos| !datos.is_empty())
            .ok_or_else(|| {
                invalida("la imagen no llegó completa; vuelve a adjuntarla".to_owned())
            })?;
        if datos.len() > MAX_IMAGEN_BYTES {
            return Err(invalida(
                "cada imagen puede pesar hasta 2 MB después de comprimirla".to_owned(),
            ));
        }
        total += datos.len();
        if total > MAX_IMAGENES_BYTES {
            return Err(invalida(
                "las imágenes del mensaje suman más de 6 MB; quita alguna".to_owned(),
            ));
        }
        if !coincide_la_firma(&datos, &imagen.mime, firma) {
            return Err(invalida(
                "el archivo no es la imagen que dice ser".to_owned(),
            ));
        }
        let preview = normalizar_preview(&imagen.preview)?;
        salida.push(Image {
            mime: imagen.mime,
            data: STANDARD.encode(&datos),
            preview,
        });
    }
    Ok(salida)
}

fn invalida(texto: String) -> AgentError {
    AgentError::Invalid(texto)
}

/// La firma que declara cada MIME que entienden los dos adaptadores de visión
fn firma_de(mime: &str) -> Option<&'static [u8]> {
    match mime {
        "image/png" => Some(b"\x89PNG\r\n\x1a\n".as_slice()),
        "image/jpeg" => Some(b"\xff\xd8\xff".as_slice()),
        "image/gif" => Some(b"GIF8".as_slice()),
        "image/webp" => Some(b"RIFF".as_slice()),
        _ => None,
    }
}

fn coincide_la_firma(datos: &[u8], mime: &str, firma: &[u8]) -> bool {
    if mime == "image/webp" {
        // RIFF....WEBP: los cuatro bytes del tamaño van en medio.
        datos.len() >= 12 && datos.starts_with(firma) && &datos[8..12] == b"WEBP"
    } else {
        datos.starts_with(firma)
    }
}

/// Acepta solo una miniatura ya empaquetada como data URL de un tipo que la
/// ventana sepa pintar: el historial no puede volverse un portador de HTML ni
/// de un archivo arbitrario.
fn normalizar_preview(bruto: &str) -> Result<String, AgentError> {
    let bruto = bruto.trim();
    if bruto.is_empty() {
        return Ok(String::new());
    }
    if bruto.len() > MAX_PREVIEW_BYTES {
        return Err(invalida(
            "la vista previa de la imagen es demasiado grande".to_owned(),
        ));
    }
    let no_valida = || invalida("la vista previa de la imagen no es válida".to_owned());
    let (cabecera, datos) = bruto.split_once(',').ok_or_else(no_valida)?;
    let tipo = cabecera
        .strip_prefix("data:")
        .and_then(|resto| resto.strip_suffix(";base64"))
        .ok_or_else(no_valida)?;
    if firma_de(tipo).is_none() {
        return Err(no_valida());
    }
    decodificar_base64(datos).ok_or_else(no_valida)?;
    Ok(bruto.to_owned())
}

/// `base64.StdEncoding` del contrato ignora los saltos de línea; el motor de Rust no.
fn decodificar_base64(texto: &str) -> Option<Vec<u8>> {
    if texto.bytes().any(|byte| byte == b'\r' || byte == b'\n') {
        let limpio: Vec<u8> = texto
            .bytes()
            .filter(|byte| *byte != b'\r' && *byte != b'\n')
            .collect();
        STANDARD.decode(limpio).ok()
    } else {
        STANDARD.decode(texto.as_bytes()).ok()
    }
}
