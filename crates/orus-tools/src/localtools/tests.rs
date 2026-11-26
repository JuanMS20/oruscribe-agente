use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use orus_agent::{CancelToken, Risk, Tool};
use serde_json::json;

use super::*;

fn con_raiz(root: &Path) -> Set {
    Set::new(config_con(root)).expect("New con una carpeta temporal")
}

fn config_con(carpeta: impl AsRef<Path>) -> Config {
    Config {
        folders: vec![carpeta.as_ref().to_string_lossy().into_owned()],
        ..Config::default()
    }
}

#[test]
fn new_rejects_relative_or_missing_folder() {
    let base = tempfile::tempdir().expect("carpeta temporal");
    let relativa = Config {
        folders: vec!["relativa".to_owned()],
        ..Config::default()
    };
    assert!(matches!(
        Set::new(relativa),
        Err(Error::CarpetaNoAbsoluta(_))
    ));

    let falta = base.path().join("no-existe");
    let error = match Set::new(config_con(&falta)) {
        Ok(_) => panic!("sin carpetas válidas debe fallar"),
        Err(error) => error,
    };
    assert!(
        matches!(error, Error::CarpetasNoDisponibles(_)),
        "{error:?}"
    );
    assert!(error.to_string().contains("no-existe"), "{error}");
    assert!(matches!(
        Set::new(Config::default()),
        Err(Error::SinCarpetas)
    ));
}

#[test]
fn new_ignora_carpetas_ausentes_si_queda_una_valida() {
    let base = tempfile::tempdir().expect("carpeta temporal");
    let valida = base.path().join("valida");
    std::fs::create_dir(&valida).expect("carpeta valida");
    let falta = base.path().join("desmontada");
    let set = Set::new(Config {
        folders: vec![
            falta.to_string_lossy().into_owned(),
            valida.to_string_lossy().into_owned(),
        ],
        ..Config::default()
    })
    .expect("una carpeta válida basta");

    let archivo = valida.join("nota.txt");
    std::fs::write(&archivo, "hola").expect("escribir en la válida");
    assert!(set.check(&archivo.to_string_lossy()).is_ok());
    assert!(
        set.check(&falta.join("nota.txt").to_string_lossy())
            .is_err()
    );
}

#[test]
fn new_rejects_missing_program_path() {
    let base = tempfile::tempdir().expect("carpeta temporal");
    let mut config = config_con(base.path());
    config.programs = vec![base.path().join("no-existe").to_string_lossy().into_owned()];
    assert!(matches!(
        Set::new(config),
        Err(Error::ProgramaSinResolver(..))
    ));
}

#[test]
fn resolve_rejects_unc_ads_device_relative() {
    let base = tempfile::tempdir().expect("carpeta temporal");
    let dentro = base.path().join("a.txt");
    std::fs::write(&dentro, "x").expect("escribir");
    let set = con_raiz(base.path());

    assert!(matches!(
        set.resolve(r"\\servidor\recurso\archivo.txt"),
        Err(Error::Fuera)
    ));
    #[cfg(windows)]
    {
        let flujo = format!("{}:flujo", dentro.display());
        assert!(matches!(set.resolve(&flujo), Err(Error::DosPuntos)));
        for nombre in ["CON", "CON.txt", "NUL"] {
            let ruta = base.path().join(nombre);
            assert!(
                matches!(
                    set.resolve(&ruta.to_string_lossy()),
                    Err(Error::Dispositivo)
                ),
                "{nombre} debía rechazarse como dispositivo"
            );
        }
    }
    assert!(matches!(set.resolve("a.txt"), Err(Error::RutaRelativa)));
}

#[test]
fn resolve_rejects_symlink_and_junction_escape() {
    let base = tempfile::tempdir().expect("carpeta temporal");
    let fuera = tempfile::tempdir().expect("carpeta temporal");
    std::fs::write(fuera.path().join("secreto.txt"), "no").expect("escribir");
    let set = con_raiz(base.path());
    // Control positivo: un resolve que rechaza todo también pasaría el
    // bucle de abajo; esto asegura que la puerta deja pasar lo de dentro.
    let dentro = base.path().join("dentro.txt");
    std::fs::write(&dentro, "x").expect("escribir");
    assert!(
        set.resolve(&dentro.to_string_lossy()).is_ok(),
        "una ruta regular dentro de la raíz tenía que resolverse"
    );
    let mut enlaces = Vec::new();

    #[cfg(windows)]
    {
        let enlace = base.path().join("enlace");
        if std::os::windows::fs::symlink_dir(fuera.path(), &enlace).is_ok() {
            enlaces.push(enlace);
        }
        // La junction no pide privilegio: es la rama que se ejercita siempre
        // en Windows, aunque el symlink de arriba falle (issue #98).
        let junta = base.path().join("junta");
        let creada = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&junta)
            .arg(fuera.path())
            .output()
            .is_ok_and(|salida| salida.status.success());
        if creada {
            enlaces.push(junta);
        }
    }
    #[cfg(not(windows))]
    {
        let enlace = base.path().join("enlace");
        if std::os::unix::fs::symlink(fuera.path(), &enlace).is_ok() {
            enlaces.push(enlace);
        }
    }

    if enlaces.is_empty() {
        // Skip explícito y ruidoso, no verde silencioso: sin enlaces no se
        // comprobó la contención y el gate no debe contar esto como cobertura.
        eprintln!(
            "SKIP #98: no se pudo crear ningún symlink ni junction; \
             la rama de escape por enlace no se ejercitó en este equipo"
        );
        return;
    }
    for enlace in &enlaces {
        let ruta = enlace.join("secreto.txt");
        assert!(
            matches!(set.resolve(&ruta.to_string_lossy()), Err(Error::Fuera)),
            "un enlace que sale de la raíz debía rechazarse"
        );
    }
}

#[test]
fn resolve_allows_case_variants_on_windows() {
    if !cfg!(windows) {
        eprintln!("solo Windows tiene rutas sin distinguir mayúsculas: caso saltado");
        return;
    }
    let base = tempfile::tempdir().expect("carpeta temporal");
    std::fs::write(base.path().join("a.txt"), "x").expect("escribir");
    let set = con_raiz(base.path());
    let ruta = base.path().join("a.txt").to_string_lossy().to_uppercase();
    assert!(
        set.resolve(&ruta).is_ok(),
        "la misma ruta con mayúsculas sigue dentro"
    );
}

/// Issue #55: si el comando deja un nieto vivo con la tubería heredada, el
/// lector no recibe EOF y la herramienta se quedaba colgada para siempre
/// (`comando.rs`, `cerrar`). Tiene que volver igual y matar al nieto.
#[test]
fn command_returns_when_a_nieto_holds_the_pipe_open() {
    use std::sync::{Arc, mpsc};
    use std::time::{Duration, Instant};

    use orus_agent::CancelToken;
    use serde_json::json;

    let raiz = tempfile::tempdir().expect("carpeta temporal");
    let marca = raiz.path().join("nieto-vivo");
    let exe = std::env::current_exe().expect("exe de pruebas");
    #[allow(clippy::needless_update)]
    let set = Arc::new(
        Set::new(Config {
            folders: vec![raiz.path().to_string_lossy().into_owned()],
            programs: vec![exe.to_string_lossy().into_owned()],
            timeout: Duration::from_secs(30),
            journal: None,
            ..Config::default()
        })
        .expect("Set con una carpeta temporal"),
    );

    let programa = exe.to_string_lossy().into_owned();
    let cwd = raiz.path().to_string_lossy().into_owned();
    let marca_arg = format!("marker={}", marca.to_string_lossy());
    let (aviso, espera) = mpsc::channel();
    let hilo = std::thread::spawn(move || {
        let resultado = super::comando::run_command(
            &set,
            json!({
                "program": programa,
                "args": [
                    "--exact",
                    super::comando::NOMBRE_AYUDANTE,
                    "--nocapture",
                    "--",
                    "mode=nieto",
                    marca_arg,
                ],
                "cwd": cwd,
            }),
            &CancelToken::new(),
        )
        .map_err(|error| error.to_string());
        let _ = aviso.send(resultado);
    });

    let inicio = Instant::now();
    let resultado = espera.recv_timeout(Duration::from_secs(10)).expect(
        "la herramienta no volvió en 10 s: un nieto con la tubería abierta la dejó colgada",
    );
    assert!(
        inicio.elapsed() < Duration::from_secs(5),
        "tardó {:?} en volver",
        inicio.elapsed()
    );
    let salida = resultado.expect("la herramienta devolvió un error");
    assert!(
        salida.contains("padre-listo"),
        "falta la salida del comando normal: {salida}"
    );
    hilo.join().expect("hilo del comando");

    std::thread::sleep(Duration::from_secs(4));
    assert!(
        !marca.exists(),
        "el nieto sobrevivió al cierre del árbol y siguió con la tubería abierta"
    );
}

#[test]
fn check_rejects_outside() {
    let base = tempfile::tempdir().expect("carpeta temporal");
    let fuera = tempfile::tempdir().expect("carpeta temporal");
    std::fs::write(fuera.path().join("secreto.txt"), "no").expect("escribir");
    let set = con_raiz(base.path());

    assert!(set.check(&base.path().to_string_lossy()).is_ok());
    assert!(
        set.check(&base.path().join("nuevo.txt").to_string_lossy())
            .is_ok()
    );
    let secreto = fuera.path().join("secreto.txt");
    assert!(matches!(
        set.check(&secreto.to_string_lossy()),
        Err(Error::Fuera)
    ));
}

/// Issue #128: en Linux `:` es válido en un nombre de archivo y `NUL`/`CON`
/// no son dispositivos. Con las reglas de Windows el agente no podía leer
/// archivos legítimos que el usuario sí le había concedido.
#[cfg(not(windows))]
#[test]
fn resolve_acepta_dos_puntos_y_nombres_de_dispositivo_en_linux() {
    let base = tempfile::tempdir().expect("carpeta temporal");
    let set = con_raiz(base.path());
    for nombre in ["a:b.txt", "nota:con:dos.txt", "NUL", "CON.tar.gz", "COM1"] {
        let ruta = base.path().join(nombre);
        std::fs::write(&ruta, b"contenido").expect("crear archivo con nombre válido en Linux");
        let esperada = std::fs::canonicalize(&ruta).expect("ruta real");
        let resuelta = set
            .resolve(&ruta.to_string_lossy())
            .unwrap_or_else(|error| panic!("{nombre} tenía que resolverse en Linux, dio {error}"));
        assert_eq!(resuelta, esperada, "{nombre} resolvió a otra ruta");
    }
}

/// T1a: las herramientas Pi comparten raíz y resolvedor con el CLI, pero no
/// el registro: `tools_pi` publica exactamente las cuatro locales de Pi.
fn conjunto_pi(raiz: &Path) -> Arc<Set> {
    Arc::new(Set::new(config_con(raiz)).expect("Set con una carpeta temporal"))
}

fn herramienta<'a>(herramientas: &'a [Arc<dyn Tool>], nombre: &str) -> &'a Arc<dyn Tool> {
    herramientas
        .iter()
        .find(|herramienta| herramienta.spec().name == nombre)
        .unwrap_or_else(|| panic!("falta la herramienta {nombre}"))
}

#[test]
fn tools_pi_devuelve_solo_read_write_edit_bash() {
    let base = tempfile::tempdir().expect("carpeta temporal");
    let set = conjunto_pi(base.path());
    let nombres: Vec<String> = set
        .tools_pi()
        .iter()
        .map(|herramienta| herramienta.spec().name)
        .collect();
    assert_eq!(nombres, ["read", "write", "edit", "bash"]);
}

#[tokio::test]
async fn bash_ejecuta_echo_en_la_carpeta_permitida() {
    let base = tempfile::tempdir().expect("carpeta temporal");
    let set = conjunto_pi(base.path());
    let pi = set.tools_pi();
    let bash = herramienta(&pi, "bash");
    // Sin `cwd`: la primera carpeta permitida, como.
    let salida = bash
        .run(json!({"command": "echo hola"}))
        .await
        .expect("bash tiene que correr echo");
    assert!(salida.contains("hola"), "salida sin el echo: {salida:?}");
}

#[test]
fn bash_rechaza_cwd_fuera_de_las_carpetas() {
    let base = tempfile::tempdir().expect("carpeta temporal");
    let fuera = tempfile::tempdir().expect("carpeta temporal");
    let set = conjunto_pi(base.path());
    let error = super::bash::run_bash(
        &set,
        json!({
            "command": "echo hola",
            "cwd": fuera.path().to_string_lossy(),
        }),
        &CancelToken::new(),
    )
    .expect_err("un cwd fuera de la raíz no puede correr");
    assert!(
        matches!(error, Error::Fuera | Error::RutaRelativa),
        "error inesperado: {error:?}"
    );
}

#[test]
fn bash_corta_por_timeout() {
    if let Ok(shell) = super::bash::resolver_shell() {
        assert!(shell.args.iter().any(|arg| arg == "-c"), "{:?}", shell.args);
    }
    let comando = "sleep 5";
    let base = tempfile::tempdir().expect("carpeta temporal");
    let set = conjunto_pi(base.path());
    let inicio = Instant::now();
    let error = super::bash::run_bash(
        &set,
        json!({
            "command": comando,
            "cwd": base.path().to_string_lossy(),
            "timeout": 1,
        }),
        &CancelToken::new(),
    )
    .expect_err("un comando de 5 s con timeout de 1 s tiene que cortarse");
    assert!(
        matches!(error, Error::ComandoTimeout { .. }),
        "error inesperado: {error:?}"
    );
    assert!(
        inicio.elapsed() < Duration::from_secs(4),
        "tardó {:?} en cortar",
        inicio.elapsed()
    );
}

#[test]
fn bash_con_timeout_conserva_la_salida_parcial() {
    let base = tempfile::tempdir().expect("carpeta temporal");
    let set = conjunto_pi(base.path());
    let error = super::bash::run_bash(
        &set,
        json!({
            "command": "echo ORUS_PARCIAL; sleep 5",
            "cwd": base.path().to_string_lossy(),
            "timeout": 1,
        }),
        &CancelToken::new(),
    )
    .expect_err("el timeout interno corta el comando");
    assert!(
        matches!(error, Error::ComandoTimeout { .. }),
        "error inesperado: {error:?}"
    );
    assert!(
        error.to_string().contains("ORUS_PARCIAL"),
        "se perdió la salida parcial: {error}"
    );
}

#[test]
fn bash_rechaza_comando_vacio() {
    let base = tempfile::tempdir().expect("carpeta temporal");
    let set = conjunto_pi(base.path());
    let error = super::bash::run_bash(
        &set,
        json!({"command": "   ", "cwd": base.path().to_string_lossy()}),
        &CancelToken::new(),
    )
    .expect_err("un comando vacío no corre");
    assert!(matches!(error, Error::ArgumentosInvalidos), "{error:?}");
}

#[test]
fn shell_resuelto_tiene_la_forma_esperada() {
    #[cfg(windows)]
    let shell = match super::bash::resolver_shell() {
        Ok(shell) => shell,
        Err(error) => {
            eprintln!("saltando: Git Bash no disponible: {error}");
            return;
        }
    };
    #[cfg(unix)]
    let shell = super::bash::resolver_shell().expect("shell disponible");
    assert!(
        !shell.programa.as_os_str().is_empty(),
        "el shell resuelto no tiene programa"
    );
    #[cfg(windows)]
    {
        assert!(
            shell.args.iter().any(|arg| arg == "-c"),
            "args sin -c: {:?}",
            shell.args
        );
        assert!(
            !shell
                .programa
                .to_string_lossy()
                .to_lowercase()
                .ends_with("cmd.exe"),
            "cmd.exe no es un shell permitido"
        );
    }
    #[cfg(unix)]
    {
        let nombre = shell
            .programa
            .file_name()
            .expect("nombre del shell")
            .to_string_lossy();
        assert!(nombre == "bash" || nombre == "sh", "{nombre}");
        assert!(shell.args.iter().any(|arg| arg == "-c"), "{:?}", shell.args);
    }
}

#[test]
fn tools_pi_conserva_riesgo_y_esquema_de_read_write_edit_y_exec() {
    let base = tempfile::tempdir().expect("carpeta temporal");
    let set = conjunto_pi(base.path());
    let actuales = set.tools();
    let pi = set.tools_pi();
    for (nombre_pi, nombre_actual, riesgo) in [
        ("read", Some("local_read_file"), Risk::Read),
        ("write", Some("local_write_file"), Risk::Write),
        ("edit", Some("local_edit_file"), Risk::Write),
        ("bash", None, Risk::Exec),
    ] {
        let herramienta_pi = herramienta(&pi, nombre_pi);
        assert_eq!(
            herramienta_pi.risk(),
            Some(riesgo),
            "{nombre_pi} cambió de riesgo"
        );
        if let Some(nombre_actual) = nombre_actual {
            let actual = herramienta(&actuales, nombre_actual);
            assert_eq!(
                herramienta_pi.spec().schema,
                actual.spec().schema,
                "{nombre_pi} cambió de esquema"
            );
        }
    }
}
