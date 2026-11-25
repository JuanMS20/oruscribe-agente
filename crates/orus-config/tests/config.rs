use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use orus_config::{
    AgentSettings, AllowRule, DEFAULT_AGENT_MODE, Locale, Settings, data_dir, data_dir_removable,
    editar, load, save, system_locale,
};
use serial_test::serial;
use tempfile::TempDir;

fn temp_data_dir() -> (TempDir, String) {
    let dir = tempfile::tempdir().expect("crear carpeta temporal");
    let path = dir.path().to_str().expect("ruta temporal UTF-8").to_owned();
    (dir, path)
}

fn write_settings(dir: &Path, contents: &str) {
    fs::write(dir.join("settings.json"), contents).expect("escribir settings.json");
}

fn read_settings_json(dir: &Path) -> serde_json::Value {
    let raw = fs::read_to_string(dir.join("settings.json")).expect("leer settings.json crudo");
    serde_json::from_str(&raw).expect("settings.json debe ser JSON")
}

fn top_level_keys(settings: &serde_json::Value) -> BTreeSet<String> {
    settings
        .as_object()
        .expect("settings.json debe ser un objeto")
        .keys()
        .cloned()
        .collect()
}

fn muestra_completa() -> Settings {
    let created_at = chrono::DateTime::parse_from_rfc3339("2026-01-02T03:04:05Z")
        .expect("fecha RFC3339")
        .with_timezone(&chrono::Utc);
    Settings {
        locale: Locale::English,
        client_id: "cliente".to_owned(),
        redirect_uri: "https://ejemplo.test/volver".to_owned(),
        org_slug: "mi-org".to_owned(),
        org_fijada: false,
        endpoint: "https://supabase.test".to_owned(),
        terms_version: "2026-01".to_owned(),
        autostart_asked: true,
        auto_update: Some(false),
        telemetry: Some(false),
        agent: AgentSettings {
            kind: "openai".to_owned(),
            base_url: "https://proveedor.test/v1".to_owned(),
            model: "modelo".to_owned(),
            mode: "autonomous".to_owned(),
            mode_explicit: true,
            folders: vec![r"C:\proyecto".to_owned()],
            programs: vec!["git".to_owned()],
            allow_rules: vec![AllowRule {
                id: "r1".to_owned(),
                tool: "write_file".to_owned(),
                pattern: r"C:\proyecto\*".to_owned(),
                note: "elegido por la persona".to_owned(),
                created_at,
            }],
            wake_enabled: true,
            wake_per_minute: 2,
            wake_per_day: 10,
            wake_tokens_per_day: 5000,
            voice_stt_url: "http://localhost:8080/v1".to_owned(),
            voice_stt_model: "whisper-1".to_owned(),
            voice_tts_url: "http://localhost:8081/v1".to_owned(),
            voice_tts_model: "tts-1".to_owned(),
            voice_tts_voice: "es-MX".to_owned(),
            window_tokens: 128000,
            cost_input_per_mtok: 0.5,
            cost_output_per_mtok: 1.5,
        },
    }
}

#[test]
#[serial]
fn test_data_dir_follows_the_environment() {
    let (dir, path) = temp_data_dir();
    temp_env::with_var("ORUSCRIBE_DATA_DIR", Some(path.as_str()), || {
        assert_eq!(data_dir(), dir.path());
    });
}

#[test]
#[serial]
fn test_save_and_load_round_trip() {
    let (_dir, path) = temp_data_dir();
    temp_env::with_var("ORUSCRIBE_DATA_DIR", Some(path.as_str()), || {
        save(Settings {
            locale: Locale::English,
            ..Default::default()
        })
        .expect("guardar ajustes");
        assert_eq!(load().locale, Locale::English);
    });
}

#[test]
#[serial]
fn test_broken_settings_fall_back_to_the_system_locale() {
    let (dir, path) = temp_data_dir();
    write_settings(dir.path(), "{no es json");
    temp_env::with_var("ORUSCRIBE_DATA_DIR", Some(path.as_str()), || {
        assert_eq!(load().locale, system_locale());
    });
}

#[test]
#[serial]
fn test_unknown_locale_is_ignored() {
    let (dir, path) = temp_data_dir();
    write_settings(dir.path(), r#"{"locale":"fr"}"#);
    temp_env::with_var("ORUSCRIBE_DATA_DIR", Some(path.as_str()), || {
        assert_eq!(load().locale, system_locale());
    });
}

#[test]
#[serial]
fn test_system_locale_is_one_of_the_two() {
    let got = system_locale();
    assert!(
        matches!(got, Locale::Spanish | Locale::English),
        "SystemLocale = {got}"
    );
}

#[test]
#[serial]
fn test_agent_mode_arranca_en_autonomo() {
    let (_dir, path) = temp_data_dir();
    temp_env::with_var("ORUSCRIBE_DATA_DIR", Some(path.as_str()), || {
        assert_eq!(load().agent.mode, DEFAULT_AGENT_MODE);
    });
}

#[test]
#[serial]
fn test_agent_mode_invalido_vuelve_a_pedir_permiso() {
    let (_dir, path) = temp_data_dir();
    temp_env::with_var("ORUSCRIBE_DATA_DIR", Some(path.as_str()), || {
        save(Settings {
            agent: AgentSettings {
                mode: "cualquiera".to_owned(),
                ..Default::default()
            },
            ..Default::default()
        })
        .expect("guardar ajustes");
        assert_eq!(load().agent.mode, DEFAULT_AGENT_MODE);
    });
}

#[test]
#[serial]
fn test_agent_mode_guardado_como_autonomous_se_respeta() {
    let (_dir, path) = temp_data_dir();
    temp_env::with_var("ORUSCRIBE_DATA_DIR", Some(path.as_str()), || {
        save(Settings {
            agent: AgentSettings {
                mode: "autonomous".to_owned(),
                ..Default::default()
            },
            ..Default::default()
        })
        .expect("guardar ajustes");
        assert_eq!(load().agent.mode, "autonomous");
    });
}

#[test]
#[serial]
fn test_save_y_load_conservan_todos_los_ajustes() {
    let (_dir, path) = temp_data_dir();
    let quiero = muestra_completa();
    let esperado = serde_json::to_vec(&quiero).expect("serializar lo guardado");
    temp_env::with_var("ORUSCRIBE_DATA_DIR", Some(path.as_str()), || {
        save(quiero).expect("guardar ajustes");
        let obtenido = serde_json::to_vec(&load()).expect("serializar lo cargado");
        assert_eq!(
            esperado,
            obtenido,
            "Load perdió ajustes:\nantes: {}\nahora: {}",
            String::from_utf8_lossy(&esperado),
            String::from_utf8_lossy(&obtenido)
        );
    });
}

#[test]
#[serial]
fn test_load_conserva_mas_de_cien_reglas() {
    let (_dir, path) = temp_data_dir();
    let reglas: Vec<AllowRule> = (0..101)
        .map(|i| AllowRule {
            id: format!("r{i}"),
            tool: "write_file".to_owned(),
            pattern: format!(r"C:\proyecto\{i}\*"),
            note: String::new(),
            created_at: chrono::DateTime::UNIX_EPOCH,
        })
        .collect();
    temp_env::with_var("ORUSCRIBE_DATA_DIR", Some(path.as_str()), || {
        save(Settings {
            agent: AgentSettings {
                allow_rules: reglas,
                ..Default::default()
            },
            ..Default::default()
        })
        .expect("guardar ajustes");
        assert_eq!(load().agent.allow_rules.len(), 101);
    });
}

#[test]
#[serial]
fn test_save_no_deja_temporales() {
    let (dir, path) = temp_data_dir();
    temp_env::with_var("ORUSCRIBE_DATA_DIR", Some(path.as_str()), || {
        save(Settings {
            locale: Locale::English,
            ..Default::default()
        })
        .expect("guardar ajustes");
        let sobrantes: Vec<String> = fs::read_dir(dir.path())
            .expect("leer carpeta de datos")
            .filter_map(|entry| {
                let entry = entry.ok()?;
                let name = entry.file_name().to_string_lossy().into_owned();
                (name.starts_with("settings-") && name.ends_with(".tmp")).then_some(name)
            })
            .collect();
        assert!(sobrantes.is_empty(), "quedaron temporales: {sobrantes:?}");
    });
}

#[test]
#[serial]
#[cfg_attr(windows, ignore = "en Windows los bits de permiso no significan nada")]
fn test_save_crea_la_carpeta_de_datos_privada() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let base = tempfile::tempdir().expect("crear carpeta temporal");
        let dir = base.path().join("datos");
        let path = dir.to_str().expect("ruta UTF-8").to_owned();
        temp_env::with_var("ORUSCRIBE_DATA_DIR", Some(path.as_str()), || {
            save(Settings {
                locale: Locale::English,
                ..Default::default()
            })
            .expect("guardar ajustes");
            let mode = fs::metadata(&dir)
                .expect("leer permisos de la carpeta")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o700, "permisos de la carpeta = {mode:o}");
        });
    }
}

#[test]
#[serial]
fn test_agent_kind_arranca_en_el_cerebro_de_la_organizacion() {
    let (_dir, path) = temp_data_dir();
    temp_env::with_var("ORUSCRIBE_DATA_DIR", Some(path.as_str()), || {
        assert_eq!(load().agent.kind, "org");
    });
}

#[test]
#[serial]
fn test_agent_kind_con_modelo_guardado_sigue_en_clave_propia() {
    let (dir, path) = temp_data_dir();
    write_settings(
        dir.path(),
        r#"{"agent":{"base_url":"https://openrouter.ai/api/v1","model":"modelo-x","mode":"ask"}}"#,
    );
    temp_env::with_var("ORUSCRIBE_DATA_DIR", Some(path.as_str()), || {
        assert_eq!(load().agent.kind, "openai");
    });
}

#[test]
#[serial]
fn test_agent_kind_guardado_gana() {
    let (dir, path) = temp_data_dir();
    temp_env::with_var("ORUSCRIBE_DATA_DIR", Some(path.as_str()), || {
        write_settings(dir.path(), r#"{"agent":{"kind":"org","model":"modelo-x"}}"#);
        assert_eq!(load().agent.kind, "org");

        write_settings(dir.path(), r#"{"agent":{"kind":"openai"}}"#);
        assert_eq!(load().agent.kind, "openai");
    });
}

#[test]
#[serial]
fn test_auto_update_nace_encendido_y_respeta_el_no() {
    assert!(Settings::default().auto_update_on());
    assert!(
        !Settings {
            auto_update: Some(false),
            ..Default::default()
        }
        .auto_update_on()
    );
    assert!(
        Settings {
            auto_update: Some(true),
            ..Default::default()
        }
        .auto_update_on()
    );
}

#[test]
#[serial]
fn test_data_dir_removable() {
    let base = tempfile::tempdir().expect("crear carpeta temporal");
    let base_path = base
        .path()
        .to_str()
        .expect("ruta temporal UTF-8")
        .to_owned();

    temp_env::with_vars(
        [
            ("ORUSCRIBE_DATA_DIR", Some("")),
            ("LOCALAPPDATA", Some(base_path.as_str())),
            ("XDG_CONFIG_HOME", Some(base_path.as_str())),
        ],
        || {
            assert!(
                data_dir_removable(),
                "la carpeta por defecto {} debería poder borrarse",
                data_dir().display()
            );

            let redirect = tempfile::tempdir().expect("crear carpeta temporal");
            let redirect_path = redirect
                .path()
                .to_str()
                .expect("ruta temporal UTF-8")
                .to_owned();
            temp_env::with_var("ORUSCRIBE_DATA_DIR", Some(redirect_path.as_str()), || {
                assert!(
                    !data_dir_removable(),
                    "una carpeta redirigida por ORUSCRIBE_DATA_DIR no debe borrarse"
                );
            });
        },
    );
}

// D195: paridad estricta con Go. No hay versión, no se preservan claves
// desconocidas y agregar una clave nueva obliga a actualizar estos tests.
#[test]
#[serial]
fn test_settings_guardados_tienen_el_conjunto_exacto_de_claves() {
    let (dir, path) = temp_data_dir();
    temp_env::with_var("ORUSCRIBE_DATA_DIR", Some(path.as_str()), || {
        save(muestra_completa()).expect("guardar ajustes");
    });

    let raw = read_settings_json(dir.path());
    let esperadas: BTreeSet<String> = [
        "locale",
        "client_id",
        "redirect_uri",
        "org_slug",
        "endpoint",
        "terms_version",
        "autostart_asked",
        "auto_update",
        "telemetry",
        "agent",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    assert_eq!(
        top_level_keys(&raw),
        esperadas,
        "cambió el conjunto de claves de settings.json; si es a propósito, actualiza D195: {raw}"
    );
}

#[test]
#[serial]
fn test_save_borra_claves_desconocidas() {
    let (dir, path) = temp_data_dir();
    write_settings(
        dir.path(),
        r#"{"locale":"en","client_id":"cliente","clave_futura":{"anidada":1}}"#,
    );
    temp_env::with_var("ORUSCRIBE_DATA_DIR", Some(path.as_str()), || {
        // Solo prueba que `save` poda claves desconocidas. No es el patrón de
        // arranque: `save(load())` con un archivo roto lo sobrescribiría con
        // los valores de fábrica (#57/#95).
        save(load()).expect("guardar ajustes");
    });

    let raw = read_settings_json(dir.path());
    assert!(
        raw.get("clave_futura").is_none(),
        "save conservó una clave desconocida: {raw}"
    );
    assert_eq!(
        raw["client_id"], "cliente",
        "save perdió una clave conocida: {raw}"
    );
    assert_eq!(raw["locale"], "en", "save perdió el locale: {raw}");
}

#[test]
#[serial]
fn test_editar_borra_claves_desconocidas_y_conserva_lo_ajeno() {
    let (dir, path) = temp_data_dir();
    write_settings(
        dir.path(),
        r#"{"locale":"en","client_id":"cliente","clave_futura":true}"#,
    );
    temp_env::with_var("ORUSCRIBE_DATA_DIR", Some(path.as_str()), || {
        editar(|settings| settings.org_slug = "mi-org".to_owned()).expect("editar ajustes");
    });

    let raw = read_settings_json(dir.path());
    assert!(
        raw.get("clave_futura").is_none(),
        "editar conservó una clave desconocida: {raw}"
    );
    assert_eq!(
        raw["client_id"], "cliente",
        "editar perdió lo que ya estaba en disco: {raw}"
    );
    assert_eq!(
        raw["org_slug"], "mi-org",
        "editar no guardó el cambio: {raw}"
    );
}

// #57: un campo con el tipo equivocado hacía que editar partiera de los
// defaults y, al guardar, borrara todo lo demás.
#[test]
#[serial]
fn test_editar_falla_y_no_pisa_un_settings_que_no_puede_leer() {
    let (dir, path) = temp_data_dir();
    let roto = r#"{"locale":"en","client_id":7,"org_slug":"mi-org"}"#;
    write_settings(dir.path(), roto);
    temp_env::with_var("ORUSCRIBE_DATA_DIR", Some(path.as_str()), || {
        // La app tiene que seguir arrancando con defaults: load no falla.
        assert_eq!(load().locale, system_locale());

        let error = editar(|settings| settings.org_slug = "otra".to_owned())
            .expect_err("editar debía fallar con un settings.json ilegible");
        assert!(
            error.to_string().contains("settings.json"),
            "el error debe nombrar el archivo que no se pudo leer: {error}"
        );
    });

    let en_disco =
        fs::read_to_string(dir.path().join("settings.json")).expect("leer settings.json");
    assert_eq!(en_disco, roto, "editar pisó el archivo que no pudo leer");
}

/// #95: `load` sigue siendo tolerante (la app arranca con fábrica), pero el
/// error de lectura tiene que quedar diagnosticado, no tragado. Se corre en
/// un proceso hijo para capturar su stderr sin tocar el del resto de tests.
#[test]
#[serial]
fn load_avisa_por_stderr_ante_un_archivo_roto() {
    const MARCA: &str = "ORUSCRIBE_CONFIG_TEST_HIJO";
    if std::env::var_os(MARCA).is_some() {
        let (dir, path) = temp_data_dir();
        write_settings(dir.path(), r#"{"locale":"en","client_id":7}"#);
        temp_env::with_var("ORUSCRIBE_DATA_DIR", Some(path.as_str()), || {
            let _ = load();
        });
        return;
    }
    let exe = std::env::current_exe().expect("binario de tests");
    let salida = std::process::Command::new(exe)
        .args([
            "--exact",
            "load_avisa_por_stderr_ante_un_archivo_roto",
            "--test-threads=1",
            // Sin --nocapture el harness se traga el eprintln del test.
            "--nocapture",
        ])
        .env(MARCA, "1")
        .output()
        .expect("correr el test hijo");
    assert!(salida.status.success(), "el test hijo falló");
    let stdout = String::from_utf8_lossy(&salida.stdout);
    assert!(
        stdout.contains("1 passed"),
        "el hijo no ejecutó el test: {stdout}"
    );
    let stderr = String::from_utf8_lossy(&salida.stderr);
    assert!(
        stderr.contains("settings.json no se pudo leer"),
        "load cayó a fábrica sin dejar diagnóstico: {stderr}"
    );
}

#[test]
#[serial]
fn test_editar_sin_archivo_previo_parte_de_los_defaults() {
    let (dir, path) = temp_data_dir();
    temp_env::with_var("ORUSCRIBE_DATA_DIR", Some(path.as_str()), || {
        editar(|settings| settings.org_slug = "mi-org".to_owned())
            .expect("editar sin archivo previo");
    });

    let raw = read_settings_json(dir.path());
    assert_eq!(
        raw["org_slug"], "mi-org",
        "editar no guardó el cambio en un archivo nuevo: {raw}"
    );
}

#[test]
#[serial]
fn telemetria_nace_activada_y_respeta_el_apagado() {
    assert!(
        Settings::default().telemetry_on(),
        "sin elección guardada la telemetría va activada por defecto"
    );
    let settings = Settings {
        telemetry: Some(true),
        ..Default::default()
    };
    assert!(settings.telemetry_on());
    let settings = Settings {
        telemetry: Some(false),
        ..Default::default()
    };
    assert!(!settings.telemetry_on());

    let (dir, path) = temp_data_dir();
    temp_env::with_var("ORUSCRIBE_DATA_DIR", Some(path.as_str()), || {
        write_settings(dir.path(), r#"{"telemetry":false}"#);
        assert!(
            !load().telemetry_on(),
            "quien la apagó conserva su elección guardada"
        );
    });
}
