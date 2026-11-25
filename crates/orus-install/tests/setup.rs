//! Tests portados de `internal/install/install_windows_test.go:12-23,67-78` y
//! del contrato de `internal/app/setup.go:13-40`.
//!
//! La carpeta base y el binario que corre se inyectan: ningún test lee
//! `LOCALAPPDATA`, `XDG_DATA_HOME` ni el registro.

use std::fs;
use std::path::{Path, PathBuf};

use orus_install::{
    Platform, Rutas, TERMS_BODY, TERMS_TITLE, TERMS_VERSION, platform, setup_info, setup_json,
};

fn base_temporal() -> tempfile::TempDir {
    tempfile::tempdir().expect("carpeta temporal")
}

// Port de `TestDirStaysUnderTheUserProfile`: la carpeta cuelga de Programas y
// el ejecutable cuelga de la carpeta.
#[test]
fn dir_de_windows_queda_bajo_el_perfil() {
    let base = base_temporal();
    let rutas = Rutas::new(Platform::Windows, base.path());

    let dir = rutas.dir();
    assert!(
        dir.to_string_lossy().to_lowercase().contains("programs"),
        "{} no parece una carpeta de programas por usuario",
        dir.display()
    );
    assert_eq!(rutas.exe(), dir.join("Oruscribe Agente.exe"));
}

#[test]
fn dir_de_linux_respeta_su_carpeta_de_datos() {
    let base = base_temporal();
    let rutas = Rutas::new(Platform::Linux, base.path());

    assert_eq!(rutas.dir(), base.path().join("oruscribe"));
    assert_eq!(
        rutas.exe(),
        base.path().join("oruscribe").join("oruscribe-agente")
    );
}

// Port de `TestRunningFromInstallIsFalseForTheTestBinary`: el binario que se
// descargó no se cree instalado; el que está en su carpeta, sí.
#[test]
fn el_binario_de_prueba_no_se_cree_instalado() {
    let base = base_temporal();
    let rutas = Rutas::new(Platform::Windows, base.path());
    let descarga = base.path().join("Descargas").join("Oruscribe Agente.exe");

    assert!(!rutas.installed(&descarga));
    assert!(!rutas.running_from_install(&descarga));
    assert!(rutas.portable(&descarga));

    fs::create_dir_all(rutas.dir()).expect("carpeta de instalación");
    fs::write(rutas.exe(), b"binario").expect("exe instalado");

    assert!(!rutas.installed(&descarga));
    assert!(rutas.running_from_install(&rutas.exe()));
    assert!(!rutas.portable(&rutas.exe()));
    assert!(!rutas.running_from_install(&descarga));
}

#[test]
fn portable_con_instalacion_previa_no_se_cree_instalada() {
    let base = base_temporal();
    let rutas = Rutas::new(Platform::Windows, base.path());
    fs::create_dir_all(rutas.dir()).expect("carpeta de instalación");
    fs::write(rutas.exe(), b"binario").expect("exe instalado");

    let descarga = base.path().join("Descargas").join("Oruscribe Agente.exe");
    assert!(
        !rutas.installed(&descarga),
        "la portátil en {} se cree instalada solo porque existe el exe instalado",
        descarga.display()
    );
    assert!(rutas.installed(&rutas.exe()));

    let gritado = PathBuf::from(rutas.exe().to_string_lossy().to_uppercase());
    assert!(rutas.installed(&gritado));
}

// Go compara con `strings.EqualFold`; una ruta gritada sigue siendo la misma.
#[test]
fn windows_compara_la_ruta_sin_distinguir_mayusculas() {
    let base = base_temporal();
    let rutas = Rutas::new(Platform::Windows, base.path());
    fs::create_dir_all(rutas.dir()).expect("carpeta de instalación");
    fs::write(rutas.exe(), b"binario").expect("exe instalado");

    let gritado = PathBuf::from(rutas.exe().to_string_lossy().to_uppercase());
    assert!(rutas.running_from_install(&gritado));
}

// `install_other.go:43-70`: el paquete del sistema ya cuenta como instalado.
#[test]
fn linux_reconoce_el_paquete_del_sistema() {
    let rutas = Rutas::new(Platform::Linux, Path::new("/home/ana/.local/share"));

    assert!(rutas.installed(Path::new("/usr/bin/oruscribe-agente")));
    assert!(rutas.running_from_install(Path::new("/usr/bin/oruscribe-agente")));
    assert!(rutas.running_from_install(Path::new("/opt/oruscribe/oruscribe-agente")));
    assert!(!rutas.running_from_install(Path::new("/home/ana/Descargas/oruscribe-agente")));
    assert!(!rutas.installed(Path::new("/home/ana/Descargas/oruscribe-agente")));
}

#[test]
fn linux_reconoce_la_instalacion_de_install_sh() {
    let rutas = Rutas::new(Platform::Linux, Path::new("/home/ana/.local/share"));

    let casa = Path::new("/home/ana");
    assert!(rutas.running_from_install_con_casa(
        Path::new("/home/ana/.local/bin/oruscribe-agente"),
        Some(casa)
    ));
    assert!(!rutas.running_from_install_con_casa(
        Path::new("/home/ana/Descargas/oruscribe-agente"),
        Some(casa)
    ));
}

// Sin carpeta base, Go devuelve `""` en Dir y Exe, y RunningFromInstall dice
// true porque no hay destino con el que comparar (`install_windows.go:33-77`).
#[test]
fn sin_carpeta_base_no_hay_destino_ni_instalacion() {
    let rutas = Rutas::new(Platform::Windows, PathBuf::new());

    assert!(rutas.dir().as_os_str().is_empty());
    assert!(rutas.exe().as_os_str().is_empty());
    assert!(!rutas.installed(Path::new("descarga.exe")));
    assert!(rutas.running_from_install(Path::new("descarga.exe")));
    assert!(!rutas.portable(Path::new("descarga.exe")));
}

// En Unix, sin carpeta base Go no corta en `Installed`: cae en
// `RunningFromInstall`, que dice true porque no hay destino (`install_other.go:43-51,55-70`).
#[test]
fn linux_sin_carpeta_base_cuenta_como_instalado() {
    let rutas = Rutas::new(Platform::Linux, PathBuf::new());

    assert!(rutas.dir().as_os_str().is_empty());
    assert!(rutas.exe().as_os_str().is_empty());
    assert!(rutas.installed(Path::new("descarga")));
    assert!(rutas.running_from_install(Path::new("descarga")));
}

// `setup.go:14-23`: ocho claves, sin omitir ninguna y con los nombres de Go.
#[test]
fn setup_lleva_las_ocho_claves_de_go() {
    let base = base_temporal();
    let rutas = Rutas::new(Platform::Windows, base.path());
    let descarga = base.path().join("Descargas").join("Oruscribe Agente.exe");
    let json = setup_json(&rutas, &descarga, "2026-11-1").expect("json de setup");

    let value: serde_json::Value = serde_json::from_str(&json).expect("json válido");
    let objeto = value.as_object().expect("objeto");
    let mut claves: Vec<&str> = objeto.keys().map(String::as_str).collect();
    claves.sort_unstable();
    assert_eq!(
        claves,
        [
            "acceptedTerms",
            "dir",
            "installed",
            "platform",
            "portable",
            "termsBody",
            "termsTitle",
            "termsVersion",
        ]
    );

    let dir = rutas.dir().to_string_lossy().into_owned();
    assert_eq!(objeto["installed"], false);
    assert_eq!(objeto["portable"], true);
    assert_eq!(objeto["dir"], dir);
    assert_eq!(objeto["termsTitle"], TERMS_TITLE);
    assert_eq!(objeto["termsBody"], TERMS_BODY);
    assert_eq!(objeto["termsVersion"], TERMS_VERSION);
    assert_eq!(objeto["acceptedTerms"], "2026-11-1");
    assert_eq!(objeto["platform"], platform());
}

// El orden de la struct de Go se conserva: `encoding/json` y serde escriben en
// el orden de declaración.
#[test]
fn setup_conserva_el_orden_de_la_struct_de_go() {
    let base = base_temporal();
    let rutas = Rutas::new(Platform::Windows, base.path());
    let json = setup_json(&rutas, &base.path().join("x.exe"), "").expect("json de setup");

    assert!(json.starts_with(r#"{"installed":false,"portable":true,"dir":""#));
    assert!(json.contains(r#""termsTitle":"Términos de uso","termsBody":"#));
    assert!(json.contains(r#""termsVersion":"2026-11-2","acceptedTerms":"","platform":"#));
}

#[test]
fn setup_info_copia_la_aceptacion_guardada() {
    let base = base_temporal();
    let rutas = Rutas::new(Platform::Windows, base.path());
    let info = setup_info(&rutas, &base.path().join("x.exe"), "");

    assert!(!info.installed);
    assert!(info.portable);
    assert_eq!(info.dir, rutas.dir().to_string_lossy().into_owned());
    assert_eq!(info.terms_title, TERMS_TITLE);
    assert_eq!(info.terms_body, TERMS_BODY);
    assert_eq!(info.terms_version, TERMS_VERSION);
    assert_eq!(info.accepted_terms, "");
    assert_eq!(info.platform, platform());
}

// La desinstalación usa la misma regla que `internal/config`; aquí solo se
// fija que el símbolo existe con su firma. El comportamiento lo prueba su crate.
#[test]
fn data_dir_removable_se_reexporta() {
    let _: fn() -> bool = orus_install::data_dir_removable;
}
