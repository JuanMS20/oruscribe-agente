//! `bash`: un comando de shell dentro de una carpeta permitida, con timeout,
//! tope de salida, entorno limpio y corte del árbol de procesos. Usa la misma
//! puerta de rutas y el mismo cuerpo de ejecución que `runCommand`; lo único
//! propio es el resolvedor de shell y su entorno.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use orus_agent::CancelToken;
use serde::Deserialize;
use serde_json::Value;

#[cfg(unix)]
use super::comando::entorno_unix;
use super::comando::{HomeTemporal, aplicar_entorno, ejecutar, reenviar_variables};
#[cfg(windows)]
use super::comando::{buscar_en_path, entorno_windows};
use super::lectura::{leer_args, texto_o_vacio};
use super::{Error, Set};

#[cfg(unix)]
const CONFIG_UNIX: &[&str] = &[
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "SSH_AUTH_SOCK",
    "GIT_CONFIG_GLOBAL",
    "GH_CONFIG_DIR",
    "CARGO_HOME",
    "RUSTUP_HOME",
    "PYENV_ROOT",
    "NVM_DIR",
    "PIP_CONFIG_FILE",
    "NPM_CONFIG_USERCONFIG",
];

#[cfg(windows)]
const CONFIG_WINDOWS: &[&str] = &[
    "XDG_CONFIG_HOME",
    "SSH_AUTH_SOCK",
    "GIT_CONFIG_GLOBAL",
    "GH_CONFIG_DIR",
    "CARGO_HOME",
    "RUSTUP_HOME",
    "PYENV_ROOT",
    "NVM_DIR",
    "PIP_CONFIG_FILE",
    "NPM_CONFIG_USERCONFIG",
];

/// El shell que corre el comando y cómo hay que alimentarlo.
pub(super) struct Shell {
    pub(super) programa: PathBuf,
    pub(super) args: Vec<String>,
    clase: Clase,
}

/// Lo que cambia el entorno: Git Bash quiere sus carpetas de utilidades en el
/// PATH.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Clase {
    #[cfg(windows)]
    GitBash { base: Option<PathBuf> },
    #[cfg(unix)]
    Unix,
}

const BANDERAS_PROGRAMA: &[&str] = &["-exec", "-execdir", "-ok", "-okdir", "--to-command"];
const BANDERAS_CODIGO: &[&str] = &["-c", "-e", "--eval"];

fn validar_programas(set: &Set, command: &str, cwd: &Path) -> Result<(), Error> {
    validar_programas_con(set, command, cwd, 0)
}

fn validar_programas_con(
    set: &Set,
    command: &str,
    cwd: &Path,
    profundidad: usize,
) -> Result<(), Error> {
    if set.nombres.is_empty() && set.rutas_prog.is_empty() {
        return Ok(());
    }
    if profundidad > 8 {
        return Err(Error::SustitucionNoValidableEnBash);
    }
    if expansion_no_validable(command) {
        return Err(Error::SustitucionNoValidableEnBash);
    }
    if ejecucion_anidada(command) {
        return Err(Error::EjecucionAnidadaNoValidableEnBash);
    }
    let mut lista: Vec<String> = set.nombres.iter().cloned().collect();
    lista.extend(set.rutas_prog.iter().cloned());
    lista.sort();
    let permitidos = lista.join(", ");
    let rechazar = |programa: &str| Error::ProgramaNoPermitidoEnBash {
        programa: programa.to_owned(),
        permitidos: permitidos.clone(),
    };
    for segmento in segmentos(command) {
        let palabras = palabras(segmento);
        let mut indice = 0;
        while palabras
            .get(indice)
            .is_some_and(|palabra| es_asignacion(palabra))
        {
            indice += 1;
        }
        let Some(programa) = palabras.get(indice).map(String::as_str) else {
            continue;
        };
        if programa.starts_with(['$', '`', '(', '<', '>', '&', ';', '|', '\'', '"']) {
            return Err(rechazar(programa));
        }
        if super::comando::comprobar_lista(set, programa, cwd).is_err() {
            return Err(rechazar(programa));
        }
        for indice in 1..palabras.len() {
            if let Some(interno) = programa_interno(&palabras, indice)
                && super::comando::comprobar_lista(set, interno, cwd).is_err()
            {
                return Err(rechazar(interno));
            }
            if let Some(codigo) = codigo_de(&palabras, indice)
                && !codigo.trim().is_empty()
            {
                validar_programas_con(set, codigo, cwd, profundidad + 1)?;
            }
        }
    }
    Ok(())
}

fn expansion_no_validable(command: &str) -> bool {
    let bytes = command.as_bytes();
    let mut comilla = 0u8;
    let mut escape = false;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if escape {
            escape = false;
        } else if comilla == b'\'' {
            if c == b'\'' {
                comilla = 0;
            }
        } else if c == b'\\' {
            escape = true;
        } else if comilla == b'"' && c == b'"' {
            comilla = 0;
        } else if comilla == 0 && (c == b'\'' || c == b'"') {
            comilla = c;
        } else if c == b'`'
            || ((c == b'$' || c == b'<' || c == b'>' || c == b'@')
                && bytes.get(i + 1) == Some(&b'('))
        {
            return true;
        }
        i += 1;
    }
    false
}

fn ejecucion_anidada(command: &str) -> bool {
    let minusculas = command.to_ascii_lowercase();
    for nombre in ["system", "popen", "os.system"] {
        let mut desde = 0;
        while let Some(rel) = minusculas[desde..].find(nombre) {
            let ini = desde + rel;
            let fin = ini + nombre.len();
            let antes = ini
                .checked_sub(1)
                .and_then(|i| minusculas.as_bytes().get(i))
                .copied();
            if !antes.is_some_and(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'.')
                && minusculas[fin..].trim_start().starts_with('(')
            {
                return true;
            }
            desde = fin;
        }
    }
    false
}

fn es_asignacion(palabra: &str) -> bool {
    let Some((nombre, _)) = palabra.split_once('=') else {
        return false;
    };
    !nombre.is_empty()
        && nombre
            .chars()
            .enumerate()
            .all(|(i, c)| c == '_' || c.is_ascii_alphanumeric() && (i > 0 || !c.is_ascii_digit()))
}

fn codigo_de(palabras: &[String], indice: usize) -> Option<&str> {
    let palabra = palabras.get(indice)?.as_str();
    if BANDERAS_PROGRAMA.contains(&palabra) {
        return None;
    }
    if BANDERAS_CODIGO.contains(&palabra) {
        return palabras.get(indice + 1).map(String::as_str);
    }
    if let Some(valor) = palabra.strip_prefix("--eval=") {
        return Some(valor);
    }
    let cortas = palabra.strip_prefix('-')?;
    if cortas.is_empty() || cortas.starts_with('-') {
        return None;
    }
    if cortas.bytes().any(|c| c == b'c' || c == b'e') {
        return palabras.get(indice + 1).map(String::as_str);
    }
    None
}

fn programa_interno(palabras: &[String], indice: usize) -> Option<&str> {
    let palabra = palabras.get(indice)?.as_str();
    if BANDERAS_PROGRAMA.contains(&palabra) {
        return programa_de(palabras.get(indice + 1)?.as_str());
    }
    if let Some(valor) = BANDERAS_PROGRAMA
        .iter()
        .find_map(|bandera| palabra.strip_prefix(bandera))
        .and_then(|resto| resto.strip_prefix('='))
    {
        return programa_de(valor);
    }
    None
}

fn programa_de(valor: &str) -> Option<&str> {
    let primero = valor.split_whitespace().next()?;
    let primero = primero
        .split_once('=')
        .map(|(_, valor)| valor)
        .unwrap_or(primero);
    let primero = primero.trim_matches(['\'', '"']);
    if primero.is_empty()
        || primero.starts_with(['$', '`', '(', '<', '>', '&', ';', '|', '-', '{', '}'])
    {
        return None;
    }
    Some(primero)
}

fn segmentos(command: &str) -> Vec<&str> {
    let bytes = command.as_bytes();
    let mut cortes = Vec::new();
    let mut inicio = 0;
    let mut comilla = None;
    let mut escape = false;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if escape {
            escape = false;
        } else if c == b'\\' && comilla != Some(b'\'') {
            escape = true;
        } else if let Some(q) = comilla {
            if c == q {
                comilla = None;
            }
        } else if matches!(c, b'\'' | b'"') {
            comilla = Some(c);
        } else if matches!(c, b';' | b'|' | b'&' | b'\n' | b'\r') {
            if c == b'&' && (i > 0 && bytes[i - 1] == b'>' || bytes.get(i + 1) == Some(&b'>')) {
                i += 1;
                continue;
            }
            cortes.push(&command[inicio..i]);
            i += if bytes.get(i + 1) == Some(&c) { 2 } else { 1 };
            inicio = i;
            continue;
        }
        i += 1;
    }
    if inicio < command.len() {
        cortes.push(&command[inicio..]);
    }
    cortes
}

fn palabras(segmento: &str) -> Vec<String> {
    let mut salida = Vec::new();
    let mut actual = String::new();
    let mut comilla = None;
    let mut escape = false;
    for c in segmento.chars() {
        if escape {
            actual.push(c);
            escape = false;
        } else if c == '\\' && comilla != Some('\'') {
            escape = true;
        } else if let Some(q) = comilla {
            if c == q {
                comilla = None;
            } else {
                actual.push(c);
            }
        } else if c == '\'' || c == '"' {
            comilla = Some(c);
        } else if c.is_whitespace() {
            if !actual.is_empty() {
                salida.push(std::mem::take(&mut actual));
            }
        } else {
            actual.push(c);
        }
    }
    if !actual.is_empty() {
        salida.push(actual);
    }
    salida
}

#[derive(Default, Deserialize)]
struct ArgsBash {
    #[serde(default, deserialize_with = "texto_o_vacio")]
    command: String,
    #[serde(default, deserialize_with = "texto_o_vacio")]
    cwd: String,
    #[serde(default)]
    timeout: Option<u64>,
}

/// `bash` del contrato Pi: comando de shell en una carpeta permitida.
pub(super) fn run_bash(set: &Set, args: Value, cancel: &CancelToken) -> Result<String, Error> {
    let args: ArgsBash = leer_args(args)?;
    if args.command.trim().is_empty() {
        return Err(Error::ArgumentosInvalidos);
    }
    let timeout = match args.timeout {
        Some(segundos) if (1..=600).contains(&segundos) => Duration::from_secs(segundos),
        Some(_) => return Err(Error::ArgumentosInvalidos),
        None => set.timeout,
    };
    // El cwd pasa por la misma puerta que `runCommand`: sin `cwd` se usa la
    // primera carpeta permitida, y una ruta de fuera no llega al shell.
    let cwd = if args.cwd.trim().is_empty() {
        set.roots[0].clone()
    } else {
        set.resolve(&args.cwd)?
    };
    if !std::fs::metadata(&cwd).is_ok_and(|info| info.is_dir()) {
        return Err(Error::CwdNoCarpeta);
    }

    validar_programas(set, &args.command, &cwd)?;

    let shell = resolver_shell()?;
    let mut cmd = Command::new(&shell.programa);
    cmd.args(&shell.args).arg(&args.command);
    cmd.current_dir(&cwd);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.env_clear();

    let (entorno, _scratch) = entorno_de(&shell)?;
    aplicar_entorno(&mut cmd, &entorno);
    // `_scratch` vive hasta después de `ejecutar`: es el HOME del comando y se
    // borra al salir.
    ejecutar(&mut cmd, timeout, cancel, || {})
}

/// El shell del sistema. Siempre bash: en Windows Git Bash, en Unix
/// `/bin/bash` o `sh`. Sin Git Bash no se sustituye el intérprete.
pub(super) fn resolver_shell() -> Result<Shell, Error> {
    #[cfg(windows)]
    {
        shell_windows().ok_or(Error::BashNoDisponible)
    }
    #[cfg(unix)]
    {
        Ok(shell_unix())
    }
}

#[cfg(windows)]
fn rutas_git(directorios: &[(String, String)]) -> Vec<PathBuf> {
    let mut rutas = Vec::new();
    for (nombre, base) in directorios {
        if base.trim().is_empty() {
            continue;
        }
        let base = PathBuf::from(base);
        let instalacion = if nombre == "LOCALAPPDATA" {
            base.join("Programs").join("Git")
        } else {
            base.join("Git")
        };
        rutas.push(instalacion.join("bin").join("bash.exe"));
    }
    rutas.push(PathBuf::from(r"C:\Program Files\Git\bin\bash.exe"));
    rutas.push(PathBuf::from(r"C:\Program Files (x86)\Git\bin\bash.exe"));
    rutas.sort();
    rutas.dedup();
    rutas
}

#[cfg(windows)]
fn shell_de_candidatos(candidatos: &[PathBuf], del_path: Option<PathBuf>) -> Option<Shell> {
    candidatos
        .iter()
        .filter(|ruta| ruta.is_file())
        .find_map(|ruta| git_bash(ruta.clone()))
        .or_else(|| del_path.and_then(git_bash))
}

#[cfg(windows)]
fn shell_windows() -> Option<Shell> {
    let directorios = ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"]
        .iter()
        .filter_map(|nombre| {
            std::env::var(nombre)
                .ok()
                .map(|valor| ((*nombre).to_owned(), valor))
        })
        .collect::<Vec<_>>();
    shell_de_candidatos(&rutas_git(&directorios), buscar_en_path("bash.exe"))
}

#[cfg(unix)]
fn shell_unix() -> Shell {
    let programa = if Path::new("/bin/bash").is_file() {
        PathBuf::from("/bin/bash")
    } else {
        PathBuf::from("sh")
    };
    Shell {
        programa,
        args: vec!["-c".to_owned()],
        clase: Clase::Unix,
    }
}

#[cfg(windows)]
fn git_bash(programa: PathBuf) -> Option<Shell> {
    // `...\Git\bin\bash.exe` → `...\Git`, solo si de verdad tiene las
    // carpetas de utilidades; un bash.exe suelto (el de WSL, por ejemplo) no
    // las inventa y no sirve para las rutas de Windows.
    let base = programa
        .parent()
        .and_then(Path::parent)
        .filter(|base| base.join("usr").join("bin").is_dir())?
        .to_path_buf();
    Some(Shell {
        programa,
        args: vec!["-c".to_owned()],
        clase: Clase::GitBash { base: Some(base) },
    })
}

/// El entorno limpio del shell: sigue usando `env_clear()` para no filtrar
/// secretos (claves API, tokens), mantiene un `HOME` temporal que se borra al
/// salir y solo reenvía rutas de configuración necesarias para git, gh, ssh,
/// cargo, pip y npm.
fn entorno_de(shell: &Shell) -> Result<(Vec<String>, Option<HomeTemporal>), Error> {
    match &shell.clase {
        #[cfg(windows)]
        Clase::GitBash { base } => {
            let sistema = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_owned());
            let scratch = HomeTemporal::crear()?;
            let mut entorno = entorno_windows(&sistema, &scratch.ruta.to_string_lossy());
            let mut carpetas: Vec<String> = Vec::new();
            if let Some(base) = base {
                carpetas.push(base.join("usr").join("bin").to_string_lossy().into_owned());
                carpetas.push(
                    base.join("mingw64")
                        .join("bin")
                        .to_string_lossy()
                        .into_owned(),
                );
                carpetas.push(base.join("bin").to_string_lossy().into_owned());
            }
            if let Ok(path_usuario) = std::env::var("PATH")
                && !path_usuario.trim().is_empty()
            {
                carpetas.push(path_usuario);
            }
            carpetas.push(format!(r"{sistema}\System32"));
            carpetas.push(sistema);
            reemplazar_path(&mut entorno, &carpetas.join(";"));
            reenviar_variables(&mut entorno, CONFIG_WINDOWS, |nombre| {
                std::env::var(nombre).ok()
            });
            entorno.push(format!("HOME={}", scratch.ruta.to_string_lossy()));
            entorno.push("PYTHONUTF8=1".to_owned());
            entorno.push("PYTHONIOENCODING=utf-8".to_owned());
            Ok((entorno, Some(scratch)))
        }
        #[cfg(unix)]
        Clase::Unix => {
            let scratch = HomeTemporal::crear()?;
            let mut entorno = entorno_unix(&scratch.ruta.to_string_lossy());
            if let Ok(path_usuario) = std::env::var("PATH")
                && !path_usuario.trim().is_empty()
            {
                for var in &mut entorno {
                    if var.starts_with("PATH=") {
                        *var = format!("PATH={path_usuario}:/usr/local/bin:/usr/bin:/bin");
                    }
                }
            }
            reenviar_variables(&mut entorno, CONFIG_UNIX, |nombre| {
                std::env::var(nombre).ok()
            });
            for nombre in ["LANG", "LC_ALL"] {
                if let Ok(valor) = std::env::var(nombre)
                    && !valor.is_empty()
                {
                    entorno.push(format!("{nombre}={valor}"));
                }
            }
            let temporal = scratch.ruta.to_string_lossy().into_owned();
            entorno.push(format!("TMPDIR={temporal}"));
            entorno.push(format!("XDG_CACHE_HOME={temporal}"));
            entorno.push("PYTHONUTF8=1".to_owned());
            Ok((entorno, Some(scratch)))
        }
    }
}

/// Cambia el `PATH=` del entorno mínimo por el del shell, sin tocar el resto.
#[cfg(windows)]
fn reemplazar_path(entorno: &mut [String], path: &str) {
    for variable in entorno.iter_mut() {
        if variable.starts_with("PATH=") {
            *variable = format!("PATH={path}");
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn git_se_busca_tambien_en_la_instalacion_por_usuario() {
        let base = tempfile::tempdir().expect("carpeta temporal");
        let local = base.path().join("AppData").join("Local");
        let rutas = super::rutas_git(&[
            (
                "LOCALAPPDATA".to_owned(),
                local.to_string_lossy().into_owned(),
            ),
            ("ProgramFiles".to_owned(), r"C:\Program Files".to_owned()),
        ]);
        assert!(
            rutas.contains(
                &local
                    .join("Programs")
                    .join("Git")
                    .join("bin")
                    .join("bash.exe")
            )
        );
        assert!(rutas.iter().any(|ruta| {
            ruta.to_string_lossy()
                .contains("Program Files\\Git\\bin\\bash.exe")
        }));
    }

    #[cfg(windows)]
    #[test]
    fn sin_bash_de_git_no_hay_fallback_a_powershell() {
        let base = tempfile::tempdir().expect("carpeta temporal");
        let ausente = base.path().join("Git").join("bin").join("bash.exe");
        assert!(super::shell_de_candidatos(std::slice::from_ref(&ausente), None).is_none());
        assert!(super::shell_de_candidatos(&[], Some(ausente)).is_none());
    }

    #[test]
    fn un_comando_vacio_no_arranca_el_shell() {
        let base = tempfile::tempdir().expect("carpeta temporal");
        let set = Set::new(super::super::Config {
            folders: vec![base.path().to_string_lossy().into_owned()],
            ..super::super::Config::default()
        })
        .expect("Set con una carpeta temporal");
        let error = run_bash(
            &set,
            serde_json::json!({"command": ""}),
            &CancelToken::new(),
        )
        .expect_err("un comando vacío devuelve error");
        assert!(matches!(error, Error::ArgumentosInvalidos), "{error:?}");
    }

    #[test]
    fn bash_respeta_la_lista_de_programas_configurados() {
        let base = tempfile::tempdir().expect("carpeta temporal");
        let raiz = base.path().to_string_lossy().into_owned();
        let set_con = |programas: &[&str]| {
            Set::new(super::super::Config {
                folders: vec![raiz.clone()],
                programs: programas
                    .iter()
                    .map(|nombre| (*nombre).to_owned())
                    .collect(),
                ..super::super::Config::default()
            })
            .expect("Set con programas")
        };

        const DENTRO: &str = "ORUS_T2_DENTRO";
        let permitido = set_con(&["echo"]);
        let salida = run_bash(
            &permitido,
            serde_json::json!({"command": format!("echo {DENTRO}")}),
            &CancelToken::new(),
        )
        .expect("un programa de la lista se ejecuta");
        assert!(salida.contains(DENTRO), "{salida}");

        const FUERA: &str = "ORUS_T2_FUERA";
        let otro = set_con(&["git"]);
        let marca_fuera = base.path().join("fuera.txt");
        let comando_fuera = format!(
            "echo {FUERA} > {}",
            marca_fuera.to_string_lossy().replace('\\', "/")
        );
        let error = run_bash(
            &otro,
            serde_json::json!({"command": comando_fuera}),
            &CancelToken::new(),
        )
        .expect_err("un programa fuera de la lista devuelve error de herramienta");
        let mensaje = error.to_string();
        assert!(mensaje.contains("echo"), "{mensaje}");
        assert!(mensaje.contains("git"), "{mensaje}");
        assert!(mensaje.contains("permitidos"), "{mensaje}");
        assert!(
            !marca_fuera.exists(),
            "el comando fuera de lista se ejecutó"
        );

        const VACIO: &str = "ORUS_T2_VACIO";
        let sin_lista = set_con(&[]);
        let salida = run_bash(
            &sin_lista,
            serde_json::json!({"command": format!("echo {VACIO}")}),
            &CancelToken::new(),
        )
        .expect("sin lista el comportamiento no cambia");
        assert!(salida.contains(VACIO), "{salida}");

        for (indice, operador) in ["&&", ";", "|"].iter().enumerate() {
            let marca = base.path().join(format!("compuesto-{indice}.txt"));
            let comando = format!(
                "echo ORUS_T2_UNO > {} {operador} git --version",
                marca.to_string_lossy().replace('\\', "/")
            );
            let error = run_bash(
                &permitido,
                serde_json::json!({"command": comando}),
                &CancelToken::new(),
            )
            .expect_err("un segmento fuera de la lista rechaza el comando compuesto");
            assert!(error.to_string().contains("git"), "{error}");
            assert!(
                !marca.exists(),
                "el comando compuesto se ejecutó: {operador}"
            );
        }

        for (indice, (bypass, razon)) in [
            ("\ngit --version", "git"),
            (" $(git --version)", "sustitución"),
            (" `git --version`", "sustitución"),
        ]
        .into_iter()
        .enumerate()
        {
            let marca = base.path().join(format!("t7-bypass-{indice}.txt"));
            let comando = format!(
                "echo ORUS_T7_{indice} > {}{bypass}",
                marca.to_string_lossy().replace('\\', "/")
            );
            let error = run_bash(
                &permitido,
                serde_json::json!({"command": comando}),
                &CancelToken::new(),
            )
            .expect_err("un salto de línea o una sustitución rechaza el comando");
            assert!(!marca.exists(), "el comando se ejecutó: {comando}");
            assert!(
                error.to_string().contains(razon),
                "el error no explica el rechazo: {error}"
            );
        }
    }

    #[test]
    fn el_entorno_reenvia_solo_rutas_de_configuracion() {
        let valores = [
            ("HOME", "/perfil"),
            ("USERPROFILE", r"C:\Users\persona"),
            ("APPDATA", r"C:\Users\persona\AppData\Roaming"),
            ("SSH_AUTH_SOCK", "/tmp/agente.sock"),
            ("GH_CONFIG_DIR", "/perfil/.config/gh"),
            ("ORUSCRIBE_TEST_SECRETO", "no-debe-pasar"),
        ];
        let mut entorno = Vec::new();
        #[cfg(unix)]
        let nombres = CONFIG_UNIX;
        #[cfg(windows)]
        let nombres = CONFIG_WINDOWS;
        super::super::comando::reenviar_variables(&mut entorno, nombres, |nombre| {
            valores
                .iter()
                .find(|(clave, _)| *clave == nombre)
                .map(|(_, valor)| (*valor).to_owned())
        });
        assert!(entorno.contains(&"SSH_AUTH_SOCK=/tmp/agente.sock".to_owned()));
        assert!(entorno.contains(&"GH_CONFIG_DIR=/perfil/.config/gh".to_owned()));
        assert!(!entorno.iter().any(|variable| {
            variable.starts_with("HOME=")
                || variable.starts_with("USERPROFILE=")
                || variable.starts_with("APPDATA=")
                || variable.contains("no-debe-pasar")
        }));
    }

    #[cfg(unix)]
    #[test]
    fn bash_usa_home_temporal_y_solo_config_del_perfil() {
        let hogar = std::env::var("HOME").expect("HOME del proceso");
        let (entorno, scratch) = entorno_de(&super::shell_unix()).expect("entorno de bash");
        let valor = |nombre: &str| {
            entorno
                .iter()
                .find_map(|variable| variable.strip_prefix(&format!("{nombre}=")))
                .map(str::to_owned)
        };
        let scratch = scratch.expect("temporal por comando");
        let temporal = scratch.ruta.to_string_lossy().into_owned();
        assert_eq!(valor("HOME").as_deref(), Some(temporal.as_str()));
        assert_eq!(valor("TMPDIR").as_deref(), Some(temporal.as_str()));
        assert_eq!(valor("XDG_CACHE_HOME").as_deref(), Some(temporal.as_str()));
        assert_ne!(valor("HOME").as_deref(), Some(hogar.as_str()));
    }

    #[cfg(windows)]
    #[test]
    fn bash_en_windows_usa_home_temporal() {
        let shell = super::Shell {
            programa: std::path::PathBuf::from("bash.exe"),
            args: vec!["-c".to_owned()],
            clase: super::Clase::GitBash { base: None },
        };
        let (entorno, scratch) = entorno_de(&shell).expect("entorno de bash");
        let scratch = scratch.expect("temporal por comando");
        let temporal = scratch.ruta.to_string_lossy().into_owned();
        assert!(entorno.contains(&format!("HOME={temporal}")));
        assert!(
            !entorno
                .iter()
                .any(|variable| variable.starts_with("USERPROFILE="))
        );
    }

    #[test]
    fn la_lista_valida_tambien_los_programas_internos_del_comando() {
        let base = tempfile::tempdir().expect("carpeta temporal");
        let set = Set::new(super::super::Config {
            folders: vec![base.path().to_string_lossy().into_owned()],
            programs: vec![
                "find".to_owned(),
                "echo".to_owned(),
                "git".to_owned(),
                "cargo".to_owned(),
                "tail".to_owned(),
                "bash".to_owned(),
                "sh".to_owned(),
            ],
            ..super::super::Config::default()
        })
        .expect("Set con programas");
        let correr = |comando: &str| {
            run_bash(
                &set,
                serde_json::json!({ "command": comando }),
                &CancelToken::new(),
            )
        };

        const INTERNO: &str = "ORUS_T5_FIND_EXEC";
        let marca = base.path().join("no-crear.txt");
        let nombre = marca.to_string_lossy().replace('\\', "/");
        let error = correr(&format!("find . -exec rm -f {nombre} \\;"))
            .expect_err("-exec lanza un programa que también se valida");
        assert!(error.to_string().contains("rm"), "{error}");
        assert!(!marca.exists(), "el programa interno se ejecutó");

        let salida =
            correr(&format!("find . -exec echo {INTERNO} \\;")).expect("un -exec permitido corre");
        assert!(salida.contains(INTERNO), "{salida}");

        for comando in [
            "bash -lc 'rm -rf /tmp/x'",
            "sh -ec 'rm -rf /tmp/x'",
            "find . -exec bash -lc 'rm -rf /tmp/x' \\;",
        ] {
            let error =
                correr(comando).expect_err("una flag corta agrupada también valida su código");
            assert!(error.to_string().contains("rm"), "{comando}: {error}");
        }

        let error = correr("awk 'BEGIN{system(\"rm -rf /tmp/x\")}'")
            .expect_err("una ejecución anidada no se puede validar");
        assert!(error.to_string().contains("anidada"), "{error}");
    }

    #[test]
    fn ampersand_seguido_de_digito_no_es_redireccion() {
        let base = tempfile::tempdir().expect("carpeta temporal");
        let set = Set::new(super::super::Config {
            folders: vec![base.path().to_string_lossy().into_owned()],
            programs: vec!["echo".to_owned()],
            ..super::super::Config::default()
        })
        .expect("Set con programas");
        let marca = base.path().join("no-crear.txt");
        let destino = marca.to_string_lossy().replace('\\', "/");
        let error = run_bash(
            &set,
            serde_json::json!({
                "command": format!("echo x &2>/dev/null touch {destino}")
            }),
            &CancelToken::new(),
        )
        .expect_err("& seguido de dígito separa comandos");
        assert!(!marca.exists(), "el segundo comando se ejecutó sin validar");
        assert!(error.to_string().contains("permitidos"), "{error}");
    }

    #[test]
    fn una_redireccion_no_corta_el_comando_en_programas() {
        let base = tempfile::tempdir().expect("carpeta temporal");
        let set = Set::new(super::super::Config {
            folders: vec![base.path().to_string_lossy().into_owned()],
            programs: vec!["echo".to_owned(), "tail".to_owned()],
            ..super::super::Config::default()
        })
        .expect("Set con programas");
        const RED: &str = "ORUS_T5_REDIRECCION";
        let salida = run_bash(
            &set,
            serde_json::json!({
                "command": format!("echo {RED} > redirected.txt 2>&1; tail -1 redirected.txt")
            }),
            &CancelToken::new(),
        )
        .expect("2>&1 es una redirección, no un separador");
        assert!(salida.contains(RED), "{salida}");
    }

    #[test]
    fn un_timeout_fuera_de_rango_no_arranca_el_shell() {
        let base = tempfile::tempdir().expect("carpeta temporal");
        let set = Set::new(super::super::Config {
            folders: vec![base.path().to_string_lossy().into_owned()],
            ..super::super::Config::default()
        })
        .expect("Set con una carpeta temporal");
        for timeout in [0_u64, 601] {
            let error = run_bash(
                &set,
                serde_json::json!({"command": "echo hola", "timeout": timeout}),
                &CancelToken::new(),
            )
            .expect_err("un timeout fuera de 1..=600 devuelve error");
            assert!(matches!(error, Error::ArgumentosInvalidos), "{error:?}");
        }
    }

    /// Un `bash.exe` que no es de Git for Windows (el de WSL, por ejemplo) no
    /// trae `usr/bin` y no puede resolver las rutas de Windows: se descarta y
    /// el resolvedor lo descarta y no hay fallback a PowerShell.
    #[cfg(windows)]
    #[test]
    fn un_bash_sin_usr_bin_no_se_usa_como_git_bash() {
        let base = tempfile::tempdir().expect("carpeta temporal");
        let bin = base.path().join("bin");
        std::fs::create_dir_all(&bin).expect("carpeta bin");
        let bash = bin.join("bash.exe");
        std::fs::write(&bash, b"").expect("bash.exe de prueba");

        assert!(
            git_bash(bash.clone()).is_none(),
            "sin usr/bin no es Git Bash"
        );
        std::fs::create_dir_all(base.path().join("usr").join("bin")).expect("carpeta usr/bin");
        assert!(git_bash(bash).is_some(), "con usr/bin sí es Git Bash");
    }

    #[test]
    fn validador_cierra_bypases_y_acepta_sintaxis_valida() {
        let base = tempfile::tempdir().expect("carpeta temporal");
        let set = Set::new(super::super::Config {
            folders: vec![base.path().to_string_lossy().into_owned()],
            programs: ["bash", "sh", "awk", "echo", "cargo", "git", "find"]
                .iter()
                .map(|nombre| (*nombre).to_owned())
                .collect(),
            ..super::super::Config::default()
        })
        .expect("Set con programas");
        for (comando, permitido) in [
            ("bash -cl 'id'", false),
            ("bash -lc 'id'", false),
            ("sh -ec 'id'", false),
            ("awk 'BEGIN { system (\"id\") }'", false),
            ("bash -c 'echo $(id)'", false),
            ("echo '$(date)'", true),
            ("RUST_BACKTRACE=1 cargo test", true),
            ("echo hola && git --version", true),
        ] {
            let resultado = validar_programas(&set, comando, base.path());
            assert_eq!(resultado.is_ok(), permitido, "{comando}: {resultado:?}");
        }
    }

    #[test]
    fn ningun_separador_deja_pasar_un_programa_fuera_de_la_lista() {
        let base = tempfile::tempdir().expect("carpeta temporal");
        let set = Set::new(super::super::Config {
            folders: vec![base.path().to_string_lossy().into_owned()],
            programs: vec!["echo".to_owned(), "git".to_owned()],
            ..super::super::Config::default()
        })
        .expect("Set con programas");

        for separador in [";", "&&", "||", "|", "\n", "\r\n"] {
            for programa in ["rm", "curl"] {
                let comando = format!("echo x{separador}{programa} --version");
                let error = run_bash(
                    &set,
                    serde_json::json!({ "command": comando }),
                    &CancelToken::new(),
                )
                .expect_err("un programa fuera de la lista tiene que rechazarse");
                assert!(
                    error.to_string().contains(programa),
                    "el rechazo no nombra {programa}: {error}"
                );
            }
            let comando = format!("echo x{separador}echo y");
            assert!(
                run_bash(
                    &set,
                    serde_json::json!({ "command": comando }),
                    &CancelToken::new()
                )
                .is_ok(),
                "un comando con programas permitidos no puede rechazarse: {comando:?}"
            );
        }

        for sustitucion in [
            "$(rm -rf /)",
            "`rm -rf /`",
            "<(sh -c x)",
            "@(Remove-Item x)",
        ] {
            let comando = format!("git --version {sustitucion}");
            let error = run_bash(
                &set,
                serde_json::json!({ "command": comando }),
                &CancelToken::new(),
            )
            .expect_err("una sustitución no validable tiene que rechazarse");
            assert!(
                error.to_string().contains("sustitución"),
                "el rechazo no explica la sustitución: {error}"
            );
        }
    }
}
