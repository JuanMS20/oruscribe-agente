//! Tests portados y golden de los términos.

use orus_install::{TERMS_BODY, TERMS_TITLE, TERMS_VERSION, newer, numbers, valid_sha, version};

#[test]
fn newer_compara_los_numeros() {
    let cases = [
        ("0.2.1", "0.2.0", true),
        ("0.3", "0.2.9", true),
        ("1.0.0", "0.99.99", true),
        ("0.2.0", "0.2.0", false),
        ("0.1.9", "0.2.0", false),
        ("v0.2.1", "0.2.0", true),
        ("0.3.0-beta.1", "0.2.0", true),
        ("", "0.2.0", false),
        ("0.2.0", "", true),
    ];
    for (latest, current, want) in cases {
        assert_eq!(
            newer(latest, current),
            want,
            "newer({latest:?}, {current:?})"
        );
    }
}

#[test]
fn newer_una_final_supera_a_su_prerrelease() {
    let cases = [
        ("1.0.0", "1.0.0-rc1", true),
        ("1.0.0-rc1", "1.0.0", false),
        ("1.0.0+build", "1.0.0", false),
        ("1.0.0", "1.0.1", false),
        ("", "1.0.0", false),
        ("1.0.0", "1.0.0-beta", true),
        ("v1.0.0", "1.0.0-rc1", true),
        ("1.0.0-rc1+build", "1.0.0", false),
        // El build metadata no dice nada; sin `-`, las dos son finales.
        ("1.0.0+build", "1.0.0+otro", false),
    ];
    for (latest, current, want) in cases {
        assert_eq!(
            newer(latest, current),
            want,
            "newer({latest:?}, {current:?})"
        );
    }
}

#[test]
fn newer_desempata_dos_prerreleases() {
    let cases = [
        // Identificadores numéricos: se comparan como números, no como texto.
        ("1.0.0-rc2", "1.0.0-rc1", true),
        ("1.0.0-rc1", "1.0.0-rc2", false),
        ("1.0.0-rc10", "1.0.0-rc2", true),
        ("1.0.0-rc2", "1.0.0-rc10", false),
        // Alfanuméricos: orden ASCII, `beta` < `rc1`.
        ("1.0.0-rc1", "1.0.0-beta", true),
        ("1.0.0-beta", "1.0.0-alpha", true),
        ("1.0.0-alpha", "1.0.0-beta", false),
        // Más identificadores gana cuando los compartidos son iguales.
        ("1.0.0-alpha.1", "1.0.0-alpha", true),
        ("1.0.0-alpha", "1.0.0-alpha.1", false),
        ("1.0.0-alpha.1", "1.0.0-alpha.1", false),
        // Numérico y alfanumérico mezclados: `alpha.1` < `alpha.beta`.
        ("1.0.0-alpha.beta", "1.0.0-alpha.1", true),
        // El build metadata no cambia el desempate.
        ("1.0.0-rc2+build", "1.0.0-rc1", true),
        ("1.0.0-rc1+build", "1.0.0-rc1", false),
        // Entradas raras: no rompen y se tratan como iguales.
        ("1.0.0-alpha..1", "1.0.0-alpha", false),
        ("1.0.0-alpha", "1.0.0-alpha..1", false),
        ("1.0.0-", "1.0.0", false),
    ];
    for (latest, current, want) in cases {
        assert_eq!(
            newer(latest, current),
            want,
            "newer({latest:?}, {current:?})"
        );
    }
}

#[test]
fn numbers_ignora_sufijos() {
    let got = numbers("v1.4.2-rc1");
    assert_eq!(got, [1i64, 4, 2], "{got:?}");
}

#[test]
fn numbers_corta_donde_deja_de_ser_numero() {
    assert_eq!(numbers(""), Vec::<i64>::new());
    assert_eq!(numbers("1..2"), [1i64]);
    assert_eq!(numbers("1.x.3"), [1i64]);
    assert_eq!(numbers("  v2.0.0+meta"), [2i64, 0, 0]);
    // El contrato solo quita la `v` minúscula; una mayúscula corta igual que un texto.
    assert_eq!(numbers("V1.2.3"), Vec::<i64>::new());
}

#[test]
fn valid_sha_exige_64_hex() {
    assert!(valid_sha(&"a".repeat(64)));
    assert!(valid_sha(&"A".repeat(64)));
    assert!(!valid_sha(&"a".repeat(63)));
    assert!(!valid_sha(&"a".repeat(65)));
    assert!(!valid_sha(&"g".repeat(64)));
    assert!(!valid_sha(""));
}

#[test]
fn version_sale_del_paquete() {
    assert_eq!(version(), env!("CARGO_PKG_VERSION"));
    assert!(!numbers(version()).is_empty());
}

#[test]
fn terminos_exactos() {
    assert_eq!(TERMS_VERSION, "2026-11-2");
    assert_eq!(TERMS_TITLE, "Términos de uso");

    let esperado = concat!(
        "Oruscribe Agente · versión de prueba para la organización\n",
        "\n",
        "Qué hace este programa\n",
        "Se conecta a tu cuenta de Oruscribe para mostrarte tus tareas, el chat de tu equipo, tus avisos y tus notas. Cuando algo pasa, te avisa.\n",
        "\n",
        "Qué guarda en este equipo\n",
        "Tu sesión, para no pedirte la contraseña cada vez. Tus notas. La configuración de idioma y arranque. Nada de eso sale de tu computador.\n",
        "\n",
        "Qué no hace\n",
        "No lee tus archivos personales. No ve tu contraseña: el inicio de sesión ocurre en tu navegador. No envía mensajes sin que tú los escribas.\n",
        "\n",
        "Sobre los errores técnicos\n",
        "El reporte de errores está apagado por defecto. Si lo activas en Ajustes, se envían solo fallos técnicos con rutas y secretos tachados; nunca tus mensajes ni el contenido de tus archivos. Puedes apagarlo cuando quieras.\n",
        "\n",
        "Dónde queda\n",
        "El programa y sus accesos directos quedan en tu carpeta de usuario, sin permisos de administrador. Puedes desinstalarlo desde Configuración o desde Agregar o quitar programas.\n",
        "\n",
        "Sobre esta versión\n",
        "Es una versión de prueba. Puede fallar y puede cambiar. Cuéntanos qué se rompe en la sala del equipo.",
    );
    assert_eq!(TERMS_BODY, esperado);
    assert!(!TERMS_BODY.contains('\r'), "el texto debe viajar con LF");
}
