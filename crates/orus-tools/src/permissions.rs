//! Reglas de permiso recordadas: una aprobación con un patrón exacto evita
//! volver a preguntar por ese patrón. Viven en `settings.json` (0600) y nunca
//! amplían lo que la política ya niega: solo responden la pregunta.
//!
//! Port de `internal/permissions/permissions.go:1-159`.

use chrono::{DateTime, Utc};
use orus_agent::Risk;
use orus_config::{AllowRule, editar, load};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Errores del paquete. En Go son `errors.New`; acá son variantes para que
/// quien llama pueda distinguirlos sin comparar textos (R8).
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// `permissions.go:92`.
    #[error("la regla necesita herramienta y patrón")]
    ReglaIncompleta,

    /// `permissions.go:128`.
    #[error("falta el id de la regla")]
    FaltaId,

    #[error(transparent)]
    Config(#[from] orus_config::Error),
}

// Go usa `strings.EqualFold`; acá se comparan las dos cadenas en minúsculas
// Unicode, que cubre los nombres de herramienta y los patrones reales.
fn igual(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// Dice si la llamada admite una regla recordada y cuál es su patrón.
///
/// Elegibles: `local_run_command` (patrón `program:<programa>`, sin distinguir
/// mayúsculas) y las herramientas MCP de la organización (patrón `tool`). Las
/// herramientas locales (`local_*`, `read`, `write`, `edit`, `bash`) nunca
/// admiten una regla global `"tool"` para evitar aprobar escrituras o comandos
/// arbitrarios de forma permanente.
pub fn eligible(nombre: &str, args: &Value, risk: Risk) -> Option<String> {
    let nombre = nombre.trim();
    if nombre == "local_run_command" {
        let program = args.get("program")?.as_str()?.trim();
        if program.is_empty() {
            return None;
        }
        return Some(format!("program:{program}"));
    }
    if nombre.starts_with("local_") || matches!(nombre, "read" | "write" | "edit" | "bash") {
        return None;
    }
    if matches!(risk, Risk::Write | Risk::Exec) {
        return Some("tool".to_owned());
    }
    None
}

/// Identidad estable de una regla: mismo tool y patrón, mismo id. Evita
/// duplicados sin guardar estado aparte. `permissions.go:58-61`.
fn id_de_rule(tool: &str, pattern: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(tool.to_lowercase().as_bytes());
    hasher.update(b"\0");
    hasher.update(pattern.to_lowercase().as_bytes());
    hex::encode(hasher.finalize())[..12].to_owned()
}

/// Devuelve la regla que aplica, si existe. `permissions.go:66-73`.
pub fn find<'a>(rules: &'a [AllowRule], tool: &str, pattern: &str) -> Option<&'a AllowRule> {
    rules
        .iter()
        .find(|rule| igual(&rule.tool, tool) && igual(&rule.pattern, pattern))
}

/// Dice si una llamada ya tiene permiso recordado. Se llama solo cuando la
/// política pediría confirmación; un `Allow` o `Deny` previos no pasan por
/// aquí. `permissions.go:77-84`.
pub fn check(tool: &str, args: &Value, risk: Risk) -> bool {
    let Some(pattern) = eligible(tool, args, risk) else {
        return false;
    };
    find(&load().agent.allow_rules, tool, &pattern).is_some()
}

/// Guarda una regla. Si ya existe una igual, no cambia nada: la operación es
/// idempotente. `permissions.go:88-122`.
pub fn remember(tool: &str, pattern: &str, note: &str) -> Result<AllowRule, Error> {
    let tool = tool.trim();
    let pattern = pattern.trim();
    if tool.is_empty() || pattern.is_empty() {
        return Err(Error::ReglaIncompleta);
    }
    // `editar` parte de disco fresco y guarda bajo el candado de guardado:
    // guardar una copia vieja pisaba lo que otro escritor dejó mientras tanto.
    let mut creada: Option<AllowRule> = None;
    editar(|settings| {
        if find(&settings.agent.allow_rules, tool, pattern).is_some() {
            return;
        }
        let regla = AllowRule {
            id: id_de_rule(tool, pattern),
            tool: tool.to_owned(),
            pattern: pattern.to_owned(),
            note: note.trim().to_owned(),
            created_at: Utc::now(),
        };
        settings.agent.allow_rules.push(regla.clone());
        creada = Some(regla);
    })?;
    match creada {
        Some(regla) => Ok(regla),
        // Ya existía: Go devuelve solo id, herramienta y patrón, con la fecha
        // cero (`permissions.go:118-120`). `MIN_UTC` es ese cero.
        None => Ok(AllowRule {
            id: id_de_rule(tool, pattern),
            tool: tool.to_owned(),
            pattern: pattern.to_owned(),
            note: String::new(),
            created_at: DateTime::<Utc>::MIN_UTC,
        }),
    }
}

/// Quita la regla con ese id. Devuelve `false` si no existía.
/// `permissions.go:125-150`.
pub fn revoke(id: &str) -> Result<bool, Error> {
    let id = id.trim();
    if id.is_empty() {
        return Err(Error::FaltaId);
    }
    let mut borrada = false;
    editar(|settings| {
        let antes = settings.agent.allow_rules.len();
        settings
            .agent
            .allow_rules
            .retain(|regla| !igual(&regla.id, id));
        borrada = settings.agent.allow_rules.len() != antes;
    })?;
    Ok(borrada)
}

/// Devuelve las reglas, de la más vieja a la más nueva. `permissions.go:153-158`.
pub fn list() -> Vec<AllowRule> {
    let mut reglas = load().agent.allow_rules;
    reglas.sort_by_key(|a| a.created_at);
    reglas
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::process::Command;
    use std::sync::{Arc, Barrier};
    use std::thread;

    use orus_agent::{Decision, Mode, decide};
    use serde_json::json;

    use super::*;

    const MARCA_HIJO: &str = "ORUSCRIBE_PERMISSIONS_TEST_HIJO";
    const MARCA_HECHO: &str = "ORUSCRIBE_PERMISSIONS_TEST_HECHO";

    /// Corre `cuerpo` en un proceso hijo con su propio `ORUSCRIBE_DATA_DIR`.
    ///
    /// `std::env::set_var` es `unsafe` en edition 2024 y el crate prohíbe
    /// `unsafe`; además los tests del crate corren en hilos del mismo proceso.
    /// El hijo hereda una carpeta temporal y ejecuta solo este test, así que
    /// cada caso ve su propio `settings.json` sin carreras. Es el aislamiento
    /// de `temp_env` + `serial_test` que usa `orus-config`, sin agregar
    /// dependencias al `Cargo.toml`. El marcador distingue "corrió y pasó" de
    /// "el `--exact` no encontró el test", que también sale con código 0.
    fn en_datos_aislados(nombre: &str, cuerpo: fn()) {
        if std::env::var_os(MARCA_HIJO).is_some() {
            cuerpo();
            if let Some(ruta) = std::env::var_os(MARCA_HECHO) {
                std::fs::write(ruta, b"ok").expect("marcar el test ejecutado");
            }
            return;
        }
        let base = tempfile::tempdir().expect("carpeta temporal");
        let datos = base.path().join("datos");
        let hecho = base.path().join("hecho");
        let exe = std::env::current_exe().expect("ruta del binario de tests");
        let completo = format!("permissions::tests::{nombre}");
        let estado = Command::new(exe)
            .args([
                "--exact",
                completo.as_str(),
                "--test-threads=1",
                "--nocapture",
            ])
            .env(MARCA_HIJO, "1")
            .env(MARCA_HECHO, &hecho)
            .env("ORUSCRIBE_DATA_DIR", &datos)
            .status()
            .expect("correr el test aislado en su proceso");
        assert!(
            estado.success(),
            "el test aislado {nombre} falló en su proceso"
        );
        assert!(
            hecho.exists(),
            "el hijo no ejecutó {nombre}: revisa el nombre completo del test"
        );
    }

    fn modo_solo_lectura() {
        let mut ajustes = load();
        ajustes.agent.mode = "readonly".to_owned();
        orus_config::save(ajustes).expect("guardar el modo");
    }

    #[test]
    fn eligible_run_command_uses_program_pattern() {
        let args = json!({"program": "git", "args": ["status"]});
        assert_eq!(
            eligible("local_run_command", &args, Risk::Exec),
            Some("program:git".to_owned())
        );
        assert_eq!(
            eligible(
                " local_run_command ",
                &json!({"program": "  git  "}),
                Risk::Exec
            ),
            Some("program:git".to_owned()),
            "el nombre y el programa se recortan"
        );
        assert_eq!(
            eligible("local_run_command", &json!({"args": []}), Risk::Exec),
            None,
            "sin programa no admite regla"
        );
        assert_eq!(
            eligible("local_run_command", &json!({"program": "  "}), Risk::Exec),
            None,
            "un programa vacío no admite regla"
        );
    }

    #[test]
    fn eligible_mcp_tool_uses_tool() {
        let args = json!({"title": "x"});
        assert_eq!(
            eligible("create_task", &args, Risk::Write),
            Some("tool".to_owned())
        );
        assert_eq!(
            eligible("run_task", &args, Risk::Exec),
            Some("tool".to_owned())
        );
        assert_eq!(
            eligible("get_tasks", &json!({}), Risk::Read),
            None,
            "una herramienta de lectura ya la permite la política sola"
        );
    }

    #[test]
    fn eligible_local_tools_except_run_command_are_not() {
        assert_eq!(
            eligible("local_write_file", &json!({"path": "c:/x"}), Risk::Write),
            None,
            "las herramientas locales no admiten reglas, salvo local_run_command"
        );
        assert_eq!(
            eligible("bash", &json!({"command": "git status"}), Risk::Exec),
            None,
            "bash nunca admite regla global 'tool'"
        );
        assert_eq!(
            eligible("write", &json!({"path": "c:/x"}), Risk::Write),
            None,
            "write no admite regla global 'tool'"
        );
        assert_eq!(
            eligible("edit", &json!({"path": "c:/x"}), Risk::Write),
            None,
            "edit no admite regla global 'tool'"
        );
        assert_eq!(
            eligible("read", &json!({"path": "c:/x"}), Risk::Read),
            None,
            "read es de lectura y no admite regla"
        );
    }

    #[test]
    fn remember_is_case_insensitive_and_idempotent() {
        en_datos_aislados("remember_is_case_insensitive_and_idempotent", || {
            let primera =
                remember("local_run_command", "program:GIT", "").expect("recordar la primera");
            assert!(check(
                "local_run_command",
                &json!({"program": "git"}),
                Risk::Exec
            ));
            let segunda =
                remember("local_run_command", "program:git", "").expect("recordar otra vez");
            assert_eq!(primera.id, segunda.id, "mismo tool y patrón, mismo id");
            let reglas = list();
            assert_eq!(reglas.len(), 1, "recordar dos veces no duplica");
            assert_eq!(reglas[0].pattern, "program:GIT", "se conserva la primera");
        });
    }

    #[test]
    fn remember_list_revoke_roundtrip() {
        en_datos_aislados("remember_list_revoke_roundtrip", || {
            let mut ajustes = load();
            ajustes.agent.mode = "autonomous".to_owned();
            orus_config::save(ajustes).expect("guardar el modo");

            let r1 = remember("local_run_command", "program:git", "").expect("recordar");
            assert_eq!(r1.id, "4ed073f77c0b", "id de 12 hex igual al de Go");
            let r2 = remember("local_run_command", "program:git", "").expect("recordar de nuevo");
            assert_eq!(r1.id, r2.id);
            let reglas = list();
            assert_eq!(reglas.len(), 1);

            assert!(check(
                "local_run_command",
                &json!({"program": "git"}),
                Risk::Exec
            ));
            assert!(check(
                "local_run_command",
                &json!({"program": "GIT"}),
                Risk::Exec
            ));
            assert!(!check(
                "local_run_command",
                &json!({"program": "npm"}),
                Risk::Exec
            ));

            assert!(revoke(&reglas[0].id).expect("revocar"));
            assert!(!check(
                "local_run_command",
                &json!({"program": "git"}),
                Risk::Exec
            ));
            assert!(!revoke("no-existe").expect("revocar un id desconocido"));
            assert!(matches!(revoke("   "), Err(Error::FaltaId)));
            assert!(matches!(
                remember("", "tool", ""),
                Err(Error::ReglaIncompleta)
            ));
        });
    }

    #[test]
    fn remember_persists_allow_rules() {
        en_datos_aislados("remember_persists_allow_rules", || {
            remember("create_task", "tool", "").expect("recordar");
            let ruta = orus_config::data_dir().join("settings.json");
            let datos = std::fs::read(&ruta).expect("leer settings.json");
            let valor: serde_json::Value =
                serde_json::from_slice(&datos).expect("settings.json válido");
            let reglas = valor["agent"]["allow_rules"]
                .as_array()
                .expect("allow_rules es una lista");
            assert_eq!(reglas.len(), 1);
            assert_eq!(reglas[0]["tool"], "create_task");
            assert_eq!(reglas[0]["pattern"], "tool");
        });
    }

    #[test]
    fn remember_concurrent_does_not_lose_rules() {
        en_datos_aislados("remember_concurrent_does_not_lose_rules", || {
            const N: usize = 50;
            let barrera = Arc::new(Barrier::new(N));
            let mut hilos = Vec::with_capacity(N);
            for i in 0..N {
                let barrera = Arc::clone(&barrera);
                hilos.push(thread::spawn(move || {
                    let nombre = format!("mcp_herramienta_{i}");
                    // Todos arrancan a la vez: sin candado, el último guardado
                    // pisaría la lista de los demás.
                    barrera.wait();
                    remember(&nombre, "tool", "").expect("recordar en paralelo")
                }));
            }
            for hilo in hilos {
                hilo.join().expect("el hilo no puede entrar en pánico");
            }
            let reglas = list();
            assert_eq!(reglas.len(), N, "la escritura concurrente pierde reglas");
            let mut nombres: Vec<String> = reglas.iter().map(|r| r.tool.clone()).collect();
            nombres.sort();
            for i in 0..N {
                let buscado = format!("mcp_herramienta_{i}");
                assert!(
                    nombres.binary_search(&buscado).is_ok(),
                    "falta la regla {buscado}"
                );
            }
        });
    }

    #[test]
    fn readonly_policy_still_denies() {
        en_datos_aislados("readonly_policy_still_denies", || {
            modo_solo_lectura();
            remember("local_run_command", "program:git", "").expect("recordar");
            let args = json!({"program": "git"});
            assert!(
                check("local_run_command", &args, Risk::Exec),
                "la regla quedó recordada"
            );
            assert_eq!(
                decide(Some(Mode::ReadOnly), Some(Risk::Exec)),
                Decision::Deny
            );
            // El orden del runner (`runner_tools.rs:253-281`): `decide` manda
            // y la regla solo se consulta después de un `AskUser`. Un `Deny`
            // no llega a `check`.
            let permitida = match decide(Some(Mode::ReadOnly), Some(Risk::Exec)) {
                Decision::Allow => true,
                Decision::AskUser => check("local_run_command", &args, Risk::Exec),
                Decision::Deny => false,
            };
            assert!(!permitida, "una regla recordada no puede abrir un Deny");
        });
    }
}
