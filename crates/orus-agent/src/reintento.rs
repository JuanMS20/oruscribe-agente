//! Constantes y espera del reintento HTTP. Port de
//! `internal/agent/reintentar.go:14-17,87-110`.

use std::time::Duration;

/// El primero y dos reintentos.
pub const MAX_INTENTOS: u32 = 3;

/// Ningún reintento espera más que esto.
pub const ESPERA_MAXIMA: Duration = Duration::from_secs(5);

/// Respeta `Retry-After` cuando el proveedor lo manda en segundos y, si no,
/// espera medio segundo, uno, dos... con tope. El tope evita que un mensaje se
/// quede media hora esperando a un proveedor caído.
///
/// Port de `esperaReintento` (`internal/agent/reintentar.go:90-110`). `intento`
/// empieza en 1, como el bucle de Go.
pub fn espera_reintento(intento: u32, retry_after: Option<&str>) -> Duration {
    if let Some(valor) = retry_after
        && let Ok(segundos) = valor.parse::<u64>()
    {
        return Duration::from_secs(segundos).min(ESPERA_MAXIMA);
    }

    // 500 ms * 2^(intento-1). Con exponente 4 ya son 8 s, más que el tope, así
    // que no hace falta seguir creciendo.
    let exponente = intento.saturating_sub(1).min(4);
    Duration::from_millis(500 * (1u64 << exponente)).min(ESPERA_MAXIMA)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// Port de `TestEsperaReintentoRespetaRetryAfterYTope`
    /// (`internal/agent/openai_test.go:428-438`).
    #[test]
    fn espera_reintento_respeta_retry_after_y_tope() {
        assert_eq!(espera_reintento(1, Some("2")), Duration::from_secs(2));
        assert_eq!(espera_reintento(1, Some("3600")), ESPERA_MAXIMA);
    }

    /// La espera por defecto de `internal/agent/reintentar.go:105-108`.
    #[test]
    fn espera_reintento_crece_y_se_corta() {
        assert_eq!(espera_reintento(1, None), Duration::from_millis(500));
        assert_eq!(espera_reintento(2, None), Duration::from_secs(1));
        assert_eq!(espera_reintento(3, None), Duration::from_secs(2));
        assert_eq!(espera_reintento(4, None), Duration::from_secs(4));
        assert_eq!(espera_reintento(5, None), ESPERA_MAXIMA);
        assert_eq!(
            espera_reintento(1, Some("basura")),
            Duration::from_millis(500),
            "un Retry-After ilegible cae a la espera por defecto"
        );
    }

    /// Las constantes de `internal/agent/reintentar.go:14-17`.
    #[test]
    fn constantes_de_reintento() {
        assert_eq!(MAX_INTENTOS, 3);
        assert_eq!(ESPERA_MAXIMA, Duration::from_secs(5));
    }
}
