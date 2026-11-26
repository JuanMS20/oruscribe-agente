//! Acumulación de bytes y armado de eventos SSE.
//!
//! Los dos proveedores comparten el mismo escáner; cada uno conserva su parser
//! de eventos y su texto de error de línea larga.

use crate::error::ProviderError;
use crate::openai::MAX_RESPUESTA;

/// El escáner del contrato arranca con 64 KiB y crece hasta [`MAX_RESPUESTA`].
const BUFFER_INICIAL: usize = 64 << 10;

/// Lee el cuerpo de `respuesta` y llama a `procesar` con cada evento completo,
/// con las líneas `data:` unidas por `\n`. Devuelve `true` cuando pidió terminar.
/// Los bytes partidos y los saltos CR, LF o CRLF se acumulan; al EOF se descarta
/// el evento pendiente, porque sin línea vacía no es un evento.
pub(crate) async fn leer_lineas(
    mut respuesta: reqwest::Response,
    linea_larga: fn() -> ProviderError,
    mut procesar: impl FnMut(&[u8]) -> Result<bool, ProviderError>,
) -> Result<bool, ProviderError> {
    let mut pendiente: Vec<u8> = Vec::with_capacity(BUFFER_INICIAL);
    let mut datos: Vec<u8> = Vec::new();
    let mut terminado = false;
    let mut inicio = true;
    let mut eof = false;

    while !terminado {
        if !eof {
            match respuesta.chunk().await {
                Ok(Some(trozo)) => pendiente.extend_from_slice(&trozo),
                Ok(None) => eof = true,
                Err(error) => return Err(ProviderError::transporte(error)),
            }
        }
        if inicio && (pendiente.len() >= 3 || eof) {
            if pendiente.starts_with(&[0xEF, 0xBB, 0xBF]) {
                pendiente.drain(..3);
            }
            inicio = false;
        }
        while let Some((fin, salto)) = fin_de_linea(&pendiente, eof) {
            if fin > MAX_RESPUESTA {
                return Err(linea_larga());
            }
            let linea: Vec<u8> = pendiente.drain(..salto).collect();
            if agregar_linea(&linea[..fin], &mut datos, linea_larga, &mut procesar)? {
                terminado = true;
                break;
            }
        }
        if !terminado && pendiente.len() > MAX_RESPUESTA {
            return Err(linea_larga());
        }
        if eof {
            break;
        }
    }
    Ok(terminado)
}

/// Posición del final de la primera línea y cuántos bytes consume el salto.
/// Un `\r` al final del trozo espera al siguiente: puede venir un `\n` después.
fn fin_de_linea(pendiente: &[u8], eof: bool) -> Option<(usize, usize)> {
    for (indice, byte) in pendiente.iter().enumerate() {
        if *byte == b'\n' {
            return Some((indice, indice + 1));
        }
        if *byte == b'\r' {
            if indice + 1 == pendiente.len() && !eof {
                return None;
            }
            let salto = if pendiente.get(indice + 1) == Some(&b'\n') {
                2
            } else {
                1
            };
            return Some((indice, indice + salto));
        }
    }
    None
}

/// Acumula los campos `data:` y despacha solo al ver la línea vacía.
fn agregar_linea(
    linea: &[u8],
    datos: &mut Vec<u8>,
    linea_larga: fn() -> ProviderError,
    procesar: &mut impl FnMut(&[u8]) -> Result<bool, ProviderError>,
) -> Result<bool, ProviderError> {
    if linea.is_empty() {
        if datos.is_empty() {
            return Ok(false);
        }
        let mut evento = Vec::with_capacity(datos.len() + 6);
        evento.extend_from_slice(b"data: ");
        evento.extend_from_slice(datos);
        datos.clear();
        return procesar(&evento);
    }
    let Some(valor) = linea.strip_prefix(b"data:") else {
        return Ok(false);
    };
    let valor = valor.strip_prefix(b" ").unwrap_or(valor);
    let separador = usize::from(!datos.is_empty());
    if datos
        .len()
        .saturating_add(separador)
        .saturating_add(valor.len())
        > MAX_RESPUESTA
    {
        return Err(linea_larga());
    }
    if separador == 1 {
        datos.push(b'\n');
    }
    datos.extend_from_slice(valor);
    Ok(false)
}
