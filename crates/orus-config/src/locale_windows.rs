use windows_sys::Win32::Globalization::{GetUserDefaultLocaleName, GetUserDefaultUILanguage};

use crate::Locale;

const MAX_LOCALE_NAME: usize = 85;

/// Idioma al que Windows está configurado. Español en una instalación en
/// español, inglés en cualquier otra, porque un tercer idioma no es parte del
/// producto.
pub fn system_locale() -> Locale {
    if let Some(name) = user_default_locale_name()
        && name.len() >= 2
    {
        return if name.starts_with("es") {
            Locale::Spanish
        } else {
            Locale::English
        };
    }

    // SAFETY: GetUserDefaultUILanguage no recibe punteros ni tiene
    // precondiciones; devuelve el LANGID del usuario.
    let language_id = unsafe { GetUserDefaultUILanguage() };
    // El byte bajo del LANGID es el idioma primario; 0x0A es español.
    if language_id != 0 && (language_id & 0x00FF) as u8 == 0x0A {
        Locale::Spanish
    } else {
        Locale::English
    }
}

fn user_default_locale_name() -> Option<String> {
    let mut buffer = [0u16; MAX_LOCALE_NAME];
    // SAFETY: buffer tiene MAX_LOCALE_NAME elementos u16 y el puntero es
    // válido para que Windows escriba durante la llamada; el segundo argumento
    // declara exactamente esa longitud.
    let len = unsafe { GetUserDefaultLocaleName(buffer.as_mut_ptr(), MAX_LOCALE_NAME as i32) };
    if len <= 0 {
        return None;
    }
    let written = (len as usize).saturating_sub(1).min(buffer.len());
    Some(String::from_utf16_lossy(&buffer[..written]))
}
