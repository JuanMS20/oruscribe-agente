use std::env;

use crate::Locale;

/// Idioma del sistema en macOS y Linux. Ambos lo exponen como variables POSIX,
/// así que las dos primeras letras deciden.
pub fn system_locale() -> Locale {
    for key in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        let Ok(value) = env::var(key) else {
            continue;
        };
        let value = value.trim().to_lowercase();
        if value.len() >= 2 {
            return if value.starts_with("es") {
                Locale::Spanish
            } else {
                Locale::English
            };
        }
    }
    Locale::English
}
