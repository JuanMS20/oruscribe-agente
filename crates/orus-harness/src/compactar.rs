//! Recorte de una línea para los prompts. El marcador es `…`.

/// Reemplaza saltos por espacios, colapsa los repetidos y corta en runas.
pub fn compactar(texto: &str, max: usize) -> String {
    let compacto = texto.split_whitespace().collect::<Vec<_>>().join(" ");
    if compacto.chars().count() > max {
        let mut corto: String = compacto.chars().take(max).collect();
        corto.push('…');
        return corto;
    }
    compacto
}

#[cfg(test)]
mod tests {
    use super::compactar;

    #[test]
    fn compactar_colapsa_espacios_y_corta_en_runas() {
        assert_eq!(compactar("a \n\r b\t c", 40), "a b c");
        let corto = compactar(&"x".repeat(41), 40);
        assert_eq!(corto.chars().count(), 41);
        assert!(corto.ends_with('…'));
        assert_eq!(compactar("ñ".repeat(40).as_str(), 40), "ñ".repeat(40));
    }
}
