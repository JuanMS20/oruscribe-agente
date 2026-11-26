//! Versión del binario y comparación de versiones.
//!
//! `numbers` y.
//! La comparación es numérica por tramo y tolerante: ignora prefijo `v`, los
//! sufijos de prerrelease al contar tramos y el build metadata tras `+`, y no
//! inventa un número dondel contrato no lo tiene. Entre tramos iguales, una versión
//! final gana a su prerrelease (`1.0.0` supera a `1.0.0-rc1`) y dos
//! prerreleases se comparan con la precedencia de semver (`rc1` queda antes
//! que `rc2`).

use std::cmp::Ordering;

/// Versión de este paquete, tomada de `CARGO_PKG_VERSION`.
///
/// La versión de release se fija en el workspace (D204); esta función solo la
/// refleja, no la cambia.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Indica si `latest` es una versión posterior a `current`.
///
/// `latest` vacío no es más nuevo que nada Si los tramos
/// numéricos coinciden, desempata la prerrelease: la final gana a la rc o beta
/// (`1.0.0` supera a `1.0.0-rc1`) y dos prerreleases se comparan con la
/// precedencia de semver (`1.0.0-rc2` supera a `1.0.0-rc1`). El sufijo `+`
/// (build metadata) no cuenta para nada.
pub fn newer(latest: &str, current: &str) -> bool {
    let a = numbers(latest);
    let b = numbers(current);
    for i in 0..a.len().max(b.len()) {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        if x != y {
            return x > y;
        }
    }
    if a.is_empty() {
        return false;
    }
    let pre_latest = prerelease(latest);
    let pre_current = prerelease(current);
    if pre_current.is_empty() {
        // `current` es final: ninguna prerrelease la supera.
        return false;
    }
    if pre_latest.is_empty() {
        // Una final gana a cualquier prerrelease.
        return true;
    }
    cmp_prerelease(pre_latest, pre_current) == Some(Ordering::Greater)
}

/// Devuelve la parte de prerrelease: lo que va tras el primer `-` y antes del
/// build metadata `+`. Sin `-` con algo detrás, devuelve `""` (versión final).
fn prerelease(version: &str) -> &str {
    let trimmed = version.trim();
    let v = trimmed.strip_prefix('v').unwrap_or(trimmed);
    let v = match v.split_once('+') {
        Some((head, _)) => head,
        None => v,
    };
    match v.split_once('-') {
        Some((_, pre)) => pre,
        None => "",
    }
}

/// Compara dos prerreleases con la precedencia de semver.
///
/// Los identificadores se separan por `.`: dos numéricos se comparan como
/// números, dos alfanuméricos por su texto en ASCII y un numérico siempre pesa
/// menos que uno alfanumérico. Un alfanumérico que termina en dígitos
/// (`rc10`) compara primero la letra y después el número, para que `rc2` quede
/// antes que `rc10` y no al revés como haría el texto puro. Si los compartidos
/// son iguales, la que tiene más identificadores es mayor (`alpha` <
/// `alpha.1`). Devuelve `None` ante una entrada rara (identificador vacío,
/// como en `alpha..1`) para que el llamador las trate como iguales en vez de
/// inventar un orden.
fn cmp_prerelease(latest: &str, current: &str) -> Option<Ordering> {
    let a: Vec<&str> = latest.split('.').collect();
    let b: Vec<&str> = current.split('.').collect();
    if a.iter().chain(b.iter()).any(|id| id.is_empty()) {
        return None;
    }
    for (x, y) in a.iter().zip(b.iter()) {
        let ord = match (numeric_id(x), numeric_id(y)) {
            (Some(n), Some(m)) => n.len().cmp(&m.len()).then_with(|| n.cmp(m)),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => {
                let (letras_x, numero_x) = split_trailing_number(x);
                let (letras_y, numero_y) = split_trailing_number(y);
                letras_x
                    .cmp(letras_y)
                    .then_with(|| match (numero_x, numero_y) {
                        (Some(n), Some(m)) => n.len().cmp(&m.len()).then_with(|| n.cmp(m)),
                        // Sin número final, el identificador es el prefijo pelado:
                        // `rc` queda antes que `rc1`.
                        (None, Some(_)) => Ordering::Less,
                        (Some(_), None) => Ordering::Greater,
                        (None, None) => Ordering::Equal,
                    })
            }
        };
        if ord != Ordering::Equal {
            return Some(ord);
        }
    }
    Some(a.len().cmp(&b.len()))
}

/// Devuelve los dígitos de un identificador numérico, sin ceros a la izquierda
/// para poder comparar por largo y luego por texto. `None` si no es numérico.
fn numeric_id(id: &str) -> Option<&str> {
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(sin_ceros(id))
}

/// Parte un identificador en su texto y el número con que termina (`rc10` da
/// `("rc", Some("10"))`). Sirve para que el número pese como número.
fn split_trailing_number(id: &str) -> (&str, Option<&str>) {
    let bytes = id.as_bytes();
    let mut i = bytes.len();
    while i > 0 && bytes[i - 1].is_ascii_digit() {
        i -= 1;
    }
    if i == bytes.len() {
        return (id, None);
    }
    (&id[..i], Some(sin_ceros(&id[i..])))
}

/// Quita los ceros a la izquierda; un número de puros ceros vale `"0"`.
fn sin_ceros(digits: &str) -> &str {
    let recortado = digits.trim_start_matches('0');
    if recortado.is_empty() { "0" } else { recortado }
}

/// Separa una versión en sus tramos numéricos.
///
/// Hace los mismos cortes que el contrato: recorta espacios, quita un `v` inicial
/// minúsculo y descarta todo desde el primer `-` o `+`. Si un tramo no es
/// número, devuelve lo acumulado y deja de mirar; por eso `1..2` da `[1]`.
pub fn numbers(version: &str) -> Vec<i64> {
    let trimmed = version.trim();
    let v = trimmed.strip_prefix('v').unwrap_or(trimmed);
    let v = match v.find(['-', '+']) {
        Some(i) => &v[..i],
        None => v,
    };

    let mut out = Vec::new();
    for part in v.split('.') {
        // `i64` es el `int` del contrato en windows/amd64 y linux/amd64: un tramo que
        // no entra también corta igual, en vez de desbordar.
        match part.parse::<i64>() {
            Ok(n) => out.push(n),
            Err(_) => break,
        }
    }
    out
}

/// Indica si `s` es una huella SHA-256 válida: exactamente 64 hex.
///
/// Acepta mayúsculas y minúsculas, como `hex.DecodeString`.
pub fn valid_sha(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}
