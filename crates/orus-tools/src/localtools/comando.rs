//! `runCommand`: un programa permitido, sin shell, con argumentos sueltos,
//! entorno limpio, tope de salida y timeout que mata el árbol. Port de
//! `internal/localtools/command.go:17-179`.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use orus_agent::CancelToken;
use serde::Deserialize;
use serde_json::Value;

#[cfg(windows)]
use std::mem::size_of;

#[cfg(windows)]
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{
    CREATE_SUSPENDED, OpenThread, ResumeThread, THREAD_SUSPEND_RESUME,
};
#[cfg(windows)]
use windows_sys::core::BOOL;

use super::lectura::{leer_args, texto_o_vacio};
use super::proceso::{attach_tree, kill_tree, prepare_process};
use super::{Error, Set};

pub(super) fn comprobar_lista(set: &Set, programa: &str, cwd: &Path) -> Result<(), Error> {
    let programa = programa.trim();
    if programa.is_empty() {
        return Err(Error::FaltaPrograma);
    }
    if programa.contains(['/', '\\']) {
        let real = super::canon::canon(&cwd.join(programa)).map_err(Error::ProgramaNoExiste)?;
        let real_texto = super::clave_programa(&real.to_string_lossy());
        if set
            .rutas_prog
            .iter()
            .any(|permitida| permitida == &real_texto)
        {
            return Ok(());
        }
        return Err(Error::ProgramaNoPermitido);
    }
    if set.nombres.contains(&super::clave_programa(programa)) {
        return Ok(());
    }
    Err(Error::ProgramaNoPermitido)
}

/// Tope de salida (`command.go:17`).
const MAX_SALIDA: usize = 64 << 10;

#[derive(Default, Deserialize)]
struct ArgsComando {
    #[serde(default, deserialize_with = "texto_o_vacio")]
    program: String,
    #[serde(default)]
    args: Option<Vec<String>>,
    #[serde(default, deserialize_with = "texto_o_vacio")]
    cwd: String,
}

/// `runCommand` (`command.go:22-95`).
pub(super) fn run_command(set: &Set, args: Value, cancel: &CancelToken) -> Result<String, Error> {
    run_command_con(set, args, cancel, || {})
}

/// La misma ejecución con un gancho entre el spawn y el Job Object, como el
/// `Chmod` de `escritura.rs`: las pruebas simulan un nieto creado en esa
/// ventana sin depender de una carrera de milisegundos.
fn run_command_con(
    set: &Set,
    args: Value,
    cancel: &CancelToken,
    tras_spawn: impl FnOnce(),
) -> Result<String, Error> {
    let args: ArgsComando = leer_args(args)?;
    // El cwd se valida antes que el programa: un programa relativo con
    // separadores se resuelve contra este cwd, no contra el de la app (#94).
    let cwd = if args.cwd.trim().is_empty() {
        set.roots[0].clone()
    } else {
        set.resolve(&args.cwd)?
    };
    if !std::fs::metadata(&cwd).is_ok_and(|info| info.is_dir()) {
        return Err(Error::CwdNoCarpeta);
    }
    let exe = allowed_program(set, &args.program, &cwd)?;

    let mut cmd = Command::new(&exe);
    if let Some(args) = args.args {
        cmd.args(args);
    }
    cmd.current_dir(&cwd);
    // Go deja Stdin en nil: el hijo lee del dispositivo nulo.
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    // Ni la clave del proveedor ni la carpeta de datos de la app llegan al
    // comando (`command.go:49`).
    cmd.env_clear();
    #[cfg(windows)]
    {
        let sistema = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_owned());
        let temporal = std::env::temp_dir();
        aplicar_entorno(
            &mut cmd,
            &entorno_windows(&sistema, &temporal.to_string_lossy()),
        );
    }
    #[cfg(unix)]
    // El guion bajo apaga el aviso de variable no leída: el guard debe vivir
    // hasta el final del scope para que el HOME temporal se borre al salir.
    let _home = {
        let home = HomeTemporal::crear()?;
        aplicar_entorno(&mut cmd, &entorno_unix(&home.ruta.to_string_lossy()));
        home
    };

    // En Windows el proceso se crea suspendido para asignarlo al Job Object
    // antes de que corra su primera instrucción: un nieto creado en la
    // ventana spawn→attach quedaría fuera del job y sobreviviría al timeout
    // y al cierre (#94). En Unix el grupo ya lo fija prepare_process.
    ejecutar(&mut cmd, set.timeout, cancel, tras_spawn)
}

/// El cuerpo común de `run_command` y de `bash`: arranca un `Command` ya
/// configurado con árbol de procesos, tope de salida, timeout y cancelación.
/// `tras_spawn` corre entre el arranque y la asignación al árbol; las pruebas
/// lo usan para simular un nieto creado en esa ventana.
pub(super) fn ejecutar(
    cmd: &mut Command,
    timeout: Duration,
    cancel: &CancelToken,
    tras_spawn: impl FnOnce(),
) -> Result<String, Error> {
    ejecutar_con_arbol(cmd, timeout, cancel, tras_spawn, attach_tree)
}

fn ejecutar_con_arbol(
    cmd: &mut Command,
    timeout: Duration,
    cancel: &CancelToken,
    tras_spawn: impl FnOnce(),
    attach: fn(u32) -> Option<super::proceso::Arbol>,
) -> Result<String, Error> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_SUSPENDED);
    }
    prepare_process(cmd);
    let mut hijo = cmd.spawn().map_err(Error::Ejecutar)?;
    tras_spawn();
    let arbol = attach(hijo.id());
    #[cfg(windows)]
    if arbol.is_none() {
        let _ = hijo.kill();
        let _ = hijo.wait();
        return Err(Error::Ejecutar(std::io::Error::other(
            "no se pudo asignar el proceso al árbol; el hijo se mató antes de reanudarlo",
        )));
    }
    #[cfg(windows)]
    if !reanudar_hijo(hijo.id()) {
        // Sin reanudación el proceso quedaría suspendido para siempre: se
        // mata y se reporta, en vez de colgar la herramienta hasta el plazo.
        let _ = hijo.kill();
        let _ = hijo.wait();
        cerrar(Vec::new(), arbol, hijo.id());
        return Err(Error::Ejecutar(std::io::Error::other(
            "no se pudo reanudar el proceso creado suspendido",
        )));
    }

    let acumulada = Arc::new(Mutex::new(Limitado::nuevo(MAX_SALIDA)));
    let mut lectores = Vec::new();
    if let Some(salida) = hijo.stdout.take() {
        lectores.push(drenar(salida, Arc::clone(&acumulada)));
    }
    if let Some(error) = hijo.stderr.take() {
        lectores.push(drenar(error, Arc::clone(&acumulada)));
    }

    let inicio = Instant::now();
    let estado = loop {
        match hijo.try_wait() {
            Ok(Some(estado)) => break estado,
            Ok(None) => {}
            Err(error) => {
                let _ = hijo.kill();
                let _ = hijo.wait();
                cerrar(lectores, arbol, hijo.id());
                return Err(Error::ComandoFallo {
                    error: error.to_string(),
                    salida: texto(&acumulada),
                });
            }
        }
        if cancel.esta_cancelado() {
            kill_tree(arbol.as_ref(), hijo.id());
            let _ = hijo.wait();
            cerrar(lectores, arbol, hijo.id());
            return Err(Error::Cancelado);
        }
        if inicio.elapsed() >= timeout {
            kill_tree(arbol.as_ref(), hijo.id());
            let _ = hijo.wait();
            cerrar(lectores, arbol, hijo.id());
            return Err(Error::ComandoTimeout {
                duracion: duracion_go(timeout),
                salida: texto(&acumulada),
            });
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    cerrar(lectores, arbol, hijo.id());
    let salida = texto(&acumulada);
    if !estado.success() {
        return Err(Error::ComandoFallo {
            error: estado_go(estado),
            salida,
        });
    }
    Ok(salida)
}

/// Cierra el árbol **antes** de juntar a los lectores. Si el comando dejó un
/// nieto vivo con la tubería heredada, el lector no recibe EOF y `join` no
/// vuelve nunca: el plazo del bucle ya no lo cubre (issue #55). Al cerrar el
/// Job Object (`KILL_ON_JOB_CLOSE`) o matar el grupo, el nieto muere, la
/// tubería se cierra y los lectores terminan.
fn cerrar(
    lectores: Vec<std::thread::JoinHandle<()>>,
    arbol: Option<super::proceso::Arbol>,
    pid: u32,
) {
    match arbol {
        Some(arbol) => arbol.close(),
        // Sin Job Object (o en Unix, donde el árbol es el grupo de procesos)
        // queda matar por el id, como en el camino del timeout.
        None => kill_tree(None, pid),
    }
    for lector in lectores {
        let _ = lector.join();
    }
}

/// Copia de la salida ya juntada por los dos lectores.
fn texto(acumulada: &Arc<Mutex<Limitado>>) -> String {
    match acumulada.lock() {
        Ok(limitado) => limitado.texto(),
        Err(_) => String::new(),
    }
}

/// ToolHelp vive en la feature `Win32_System_Diagnostics_ToolHelp`, que este
/// crate no habilita, y `Cargo.toml` lo trabaja otro lote: las tres funciones
/// se declaran acá contra kernel32, que windows-sys ya enlaza. Si la feature
/// se habilita, estas declaraciones se borran y se usan las de windows-sys.
#[cfg(windows)]
const TH32CS_SNAPTHREAD: u32 = 0x0000_0004;

#[cfg(windows)]
#[repr(C)]
struct THREADENTRY32 {
    dw_size: u32,
    cnt_usage: u32,
    th32_thread_id: u32,
    th32_owner_process_id: u32,
    tp_base_pri: i32,
    tp_delta_pri: i32,
    dw_flags: u32,
}

#[cfg(windows)]
#[allow(non_snake_case)]
unsafe extern "system" {
    fn CreateToolhelp32Snapshot(dwFlags: u32, th32ProcessID: u32) -> HANDLE;
    fn Thread32First(hSnapshot: HANDLE, lpte: *mut THREADENTRY32) -> BOOL;
    fn Thread32Next(hSnapshot: HANDLE, lpte: *mut THREADENTRY32) -> BOOL;
}

/// Reanuda los hilos del proceso creado suspendido, recién asignado al Job
/// Object. Un proceso creado con `CREATE_SUSPENDED` tiene un solo hilo; se
/// busca por el pid del dueño en el snapshot de hilos del sistema. Devuelve
/// si al menos un hilo se reanudó.
#[cfg(windows)]
fn reanudar_hijo(pid: u32) -> bool {
    // SAFETY: el snapshot es un handle propio que se cierra al final;
    // Thread32First/Next escriben en `entrada`, que vive durante la llamada
    // y lleva dw_size con el tamaño exacto de la estructura.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return false;
        }
        let mut entrada = THREADENTRY32 {
            dw_size: size_of::<THREADENTRY32>() as u32,
            cnt_usage: 0,
            th32_thread_id: 0,
            th32_owner_process_id: 0,
            tp_base_pri: 0,
            tp_delta_pri: 0,
            dw_flags: 0,
        };
        let mut reanudado = false;
        let mut hay = Thread32First(snapshot, &mut entrada);
        while hay != 0 {
            if entrada.th32_owner_process_id == pid {
                // SAFETY: OpenThread devuelve un handle propio o NULL;
                // ResumeThread recibe ese handle y se cierra después.
                let hilo = OpenThread(THREAD_SUSPEND_RESUME, 0, entrada.th32_thread_id);
                if !hilo.is_null() {
                    if ResumeThread(hilo) != u32::MAX {
                        reanudado = true;
                    }
                    CloseHandle(hilo);
                }
            }
            hay = Thread32Next(snapshot, &mut entrada);
        }
        CloseHandle(snapshot);
        reanudado
    }
}

/// `cmd.Env` de Go es una lista de "NOMBRE=valor"; `Command` de Rust pide
/// pares. Se parte por el primer `=`, igual que el entorno del sistema.
pub(super) fn aplicar_entorno(cmd: &mut Command, entorno: &[String]) {
    for variable in entorno {
        if let Some((nombre, valor)) = variable.split_once('=') {
            cmd.env(nombre, valor);
        }
    }
}

/// `allowedProgram` (`command.go:99-126`): un nombre de la lista se busca en el
/// PATH del sistema; una ruta se canonicaliza y se compara sin mayúsculas con
/// las rutas permitidas. Un programa relativo se resuelve contra el `cwd`
/// pedido, no contra el CWD de la app (#94).
fn allowed_program(set: &Set, programa: &str, cwd: &Path) -> Result<PathBuf, Error> {
    let programa = programa.trim();
    comprobar_lista(set, programa, cwd)?;
    if programa.contains(['/', '\\']) {
        return super::canon::canon(&cwd.join(programa)).map_err(Error::ProgramaNoExiste);
    }
    buscar_en_path(programa).ok_or_else(|| no_encontrado(programa))
}

/// `exec.LookPath`: recorre el PATH de la app (el del sistema), no el entorno
/// limpio que recibe el hijo.
pub(super) fn buscar_en_path(nombre: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|carpeta| probar_en(&carpeta, nombre))
}

#[cfg(windows)]
fn probar_en(carpeta: &Path, nombre: &str) -> Option<PathBuf> {
    let directo = carpeta.join(nombre);
    if directo.is_file() {
        return Some(directo);
    }
    if Path::new(nombre).extension().is_some() {
        return None;
    }
    let extensiones = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_owned());
    for extension in extensiones
        .split(';')
        .filter(|extension| !extension.is_empty())
    {
        let candidato = carpeta.join(format!("{nombre}{extension}"));
        if candidato.is_file() {
            return Some(candidato);
        }
    }
    None
}

#[cfg(unix)]
fn probar_en(carpeta: &Path, nombre: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let candidato = carpeta.join(nombre);
    let info = candidato.metadata().ok()?;
    if info.is_file() && info.permissions().mode() & 0o111 != 0 {
        Some(candidato)
    } else {
        None
    }
}

/// Texto de `exec.LookPath` cuando el nombre está en la lista pero no en el
/// PATH.
fn no_encontrado(nombre: &str) -> Error {
    #[cfg(windows)]
    let variable = "%PATH%";
    #[cfg(unix)]
    let variable = "$PATH";
    Error::ProgramaNoEncontrado(nombre.to_owned(), variable)
}

pub(super) fn reenviar_variables(
    entorno: &mut Vec<String>,
    nombres: &[&str],
    valor: impl Fn(&str) -> Option<String>,
) {
    for nombre in nombres {
        if let Some(valor) = valor(nombre)
            && !valor.is_empty()
            && !valor.contains(['\0', '\n', '\r'])
        {
            entorno.push(format!("{nombre}={valor}"));
        }
    }
}

/// `entornoUnix` (`command.go:128-131`): cada comando tiene su propio HOME
/// porque `/tmp` lo puede escribir cualquiera del equipo. En Windows solo la
/// usan las pruebas, como la tabla de variables de Go.
#[cfg_attr(not(unix), allow(dead_code))]
pub(super) fn entorno_unix(home: &str) -> Vec<String> {
    vec![
        "PATH=/usr/local/bin:/usr/bin:/bin".to_owned(),
        format!("HOME={home}"),
        "LANG=C.UTF-8".to_owned(),
    ]
}

/// `entornoWindows` (`command.go:133-143`): solo lo mínimo para que un programa
/// normal arranque. En Unix solo la usan las pruebas.
#[cfg_attr(not(windows), allow(dead_code))]
pub(super) fn entorno_windows(system: &str, temp: &str) -> Vec<String> {
    vec![
        format!("SystemRoot={system}"),
        format!("WINDIR={system}"),
        format!("PATH={system}\\System32;{system}"),
        "PATHEXT=.COM;.EXE;.BAT;.CMD".to_owned(),
        format!("TEMP={temp}"),
        format!("TMP={temp}"),
    ]
}

/// Carpeta temporal de escritura del comando; se borra al salir. Es el HOME
/// temporal de `bash` y `run_command`: el perfil real no se copia acá, solo se
/// reenvían rutas puntuales de configuración.
pub(super) struct HomeTemporal {
    pub(super) ruta: PathBuf,
}

impl HomeTemporal {
    pub(super) fn crear() -> Result<Self, Error> {
        let base = std::env::temp_dir();
        let pid = std::process::id();
        for intento in 0..100 {
            let ruta = base.join(format!("oruscribe-home-{pid}-{intento}"));
            match std::fs::create_dir(&ruta) {
                Ok(()) => return Ok(Self { ruta }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(Error::PrepararEntorno(error)),
            }
        }
        Err(Error::PrepararEntorno(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "no se pudo crear una carpeta HOME temporal",
        )))
    }
}

impl Drop for HomeTemporal {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.ruta);
    }
}

/// Guarda hasta `max` bytes y marca si algo quedó afuera, sin bloquear al
/// proceso que escribe (`command.go:145-179`).
struct Limitado {
    max: usize,
    datos: Vec<u8>,
    cortado: bool,
}

impl Limitado {
    fn nuevo(max: usize) -> Self {
        Self {
            max,
            datos: Vec::new(),
            cortado: false,
        }
    }

    fn escribir(&mut self, datos: &[u8]) {
        let espacio = self.max.saturating_sub(self.datos.len());
        if espacio > 0 {
            if datos.len() <= espacio {
                self.datos.extend_from_slice(datos);
            } else {
                self.datos.extend_from_slice(&datos[..espacio]);
                self.cortado = true;
            }
        } else if !datos.is_empty() {
            self.cortado = true;
        }
    }

    fn texto(&self) -> String {
        let mut salida = String::from_utf8_lossy(&self.datos).into_owned();
        if self.cortado {
            salida.push_str("\n[recortado]");
        }
        salida
    }
}

/// Un hilo por tubería; los dos escriben en el mismo colector, como en Go.
fn drenar<R: Read + Send + 'static>(
    mut origen: R,
    acumulada: Arc<Mutex<Limitado>>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut buffer = [0u8; 8192];
        loop {
            match origen.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    if let Ok(mut limitado) = acumulada.lock() {
                        limitado.escribir(&buffer[..n]);
                    }
                }
                Err(_) => break,
            }
        }
    })
}

/// `time.Duration.String()` de Go: "1m0s" para 60 s, "300ms" para 300 ms.
fn duracion_go(duracion: Duration) -> String {
    let nanos = duracion.as_nanos();
    if nanos < 1_000 {
        return format!("{nanos}ns");
    }
    if nanos < 1_000_000 {
        return format!("{}{}µs", nanos / 1_000, fraccion((nanos % 1_000) as u64, 3));
    }
    if nanos < 1_000_000_000 {
        return format!(
            "{}{}ms",
            nanos / 1_000_000,
            fraccion((nanos % 1_000_000) as u64, 6)
        );
    }
    let segundos = nanos / 1_000_000_000;
    let resto = fraccion((nanos % 1_000_000_000) as u64, 9);
    let seg = segundos % 60;
    let min = (segundos / 60) % 60;
    let horas = segundos / 3600;
    if horas > 0 {
        format!("{horas}h{min}m{seg}{resto}s")
    } else if min > 0 {
        format!("{min}m{seg}{resto}s")
    } else {
        format!("{seg}{resto}s")
    }
}

/// Parte fraccionaria de un `Duration` con los ceros de la derecha quitados.
fn fraccion(resto: u64, ancho: usize) -> String {
    if resto == 0 {
        return String::new();
    }
    let mut texto = format!("{resto:0ancho$}");
    while texto.ends_with('0') {
        texto.pop();
    }
    format!(".{texto}")
}

/// Texto del error de `cmd.Wait()`: Go imprime "exit status N" o
/// "signal: killed" (`command.go:91-93`).
fn estado_go(estado: ExitStatus) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(senal) = estado.signal() {
            let nombre = match senal {
                libc::SIGKILL => "killed",
                libc::SIGTERM => "terminated",
                libc::SIGINT => "interrupt",
                _ => return format!("signal: {senal}"),
            };
            return format!("signal: {nombre}");
        }
    }
    match estado.code() {
        Some(codigo) => format!("exit status {codigo}"),
        None => "exit status desconocido".to_owned(),
    }
}

/// Nombre del test que hace de programa externo; lo comparten los tests de
/// `proceso.rs`.
#[cfg(test)]
pub(super) const NOMBRE_AYUDANTE: &str = "localtools::comando::tests::ayudante_proceso";

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::io::Write;
    use std::path::Path;
    use std::time::{Duration, Instant};

    use serde_json::json;

    use super::super::{Config, Set};
    use super::*;

    fn conjunto(raiz: &Path, timeout: Duration) -> Arc<Set> {
        Arc::new(
            Set::new(Config {
                folders: vec![raiz.to_string_lossy().into_owned()],
                read_folders: Vec::new(),
                programs: vec![
                    std::env::current_exe()
                        .expect("exe de pruebas")
                        .to_string_lossy()
                        .into_owned(),
                ],
                timeout,
                journal: None,
            })
            .expect("Set con una carpeta temporal"),
        )
    }

    fn args_ayudante(modo: &str, extra: &[String]) -> Vec<String> {
        let mut args = vec![
            "--exact".to_owned(),
            NOMBRE_AYUDANTE.to_owned(),
            "--nocapture".to_owned(),
            "--".to_owned(),
            format!("mode={modo}"),
        ];
        args.extend(extra.iter().cloned());
        args
    }

    fn correr(
        set: &Arc<Set>,
        programa: &str,
        args: Vec<String>,
        cwd: &Path,
    ) -> Result<String, String> {
        run_command(
            set,
            json!({
                "program": programa,
                "args": args,
                "cwd": cwd.to_string_lossy(),
            }),
            &CancelToken::new(),
        )
        .map_err(|error| error.to_string())
    }

    #[test]
    fn command_does_not_inherit_app_env() {
        let raiz = tempfile::tempdir().expect("raiz");
        let set = conjunto(raiz.path(), Duration::from_secs(10));
        let exe = std::env::current_exe().expect("exe de pruebas");
        temp_env::with_var("ORUSCRIBE_TEST_SECRET", Some("sk-no-debe-salir"), || {
            let salida = correr(
                &set,
                &exe.to_string_lossy(),
                args_ayudante("env", &[]),
                raiz.path(),
            )
            .expect("correr");
            assert!(
                !salida.contains("sk-no-debe-salir"),
                "el comando vio la variable de la app: {salida}"
            );
            assert!(
                salida.contains("SECRETO="),
                "el comando no corrió: {salida}"
            );
        });
    }

    #[test]
    fn command_does_not_interpret_shell() {
        let raiz = tempfile::tempdir().expect("raiz");
        let set = conjunto(raiz.path(), Duration::from_secs(10));
        let exe = std::env::current_exe().expect("exe de pruebas");
        let literal = "hola & echo INYECTADO";
        let salida = correr(
            &set,
            &exe.to_string_lossy(),
            args_ayudante("echo", &[format!("arg={literal}")]),
            raiz.path(),
        )
        .expect("correr");
        assert!(
            salida.contains(&format!("ARG={literal}")),
            "el argumento no llegó literal: {salida}"
        );
    }

    #[test]
    fn ejecucion_no_plega_mayusculas_y_no_reanuda_sin_arbol() {
        #[cfg(not(windows))]
        {
            let base = tempfile::tempdir().expect("carpeta temporal");
            let permitido = base.path().join("tool");
            let distinto = base.path().join("TOOL");
            std::fs::write(&permitido, b"x").expect("tool");
            std::fs::write(&distinto, b"y").expect("TOOL");
            let set = Set::new(Config {
                programs: vec![permitido.to_string_lossy().into_owned()],
                ..Config {
                    folders: vec![base.path().to_string_lossy().into_owned()],
                    ..Config::default()
                }
            })
            .expect("Set con la ruta permitida");
            assert!(
                super::super::comando::comprobar_lista(
                    &set,
                    &permitido.to_string_lossy(),
                    base.path()
                )
                .is_ok()
            );
            assert!(
                super::super::comando::comprobar_lista(
                    &set,
                    &distinto.to_string_lossy(),
                    base.path()
                )
                .is_err(),
                "TOOL es otro ejecutable en un sistema sensible a mayúsculas"
            );
        }
        #[cfg(windows)]
        {
            let exe = std::env::current_exe().expect("exe de pruebas");
            let mut cmd = Command::new(exe);
            cmd.args(args_ayudante("echo", &[format!("arg={}", "hola")]));
            cmd.stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .env_clear();
            let inicio = Instant::now();
            let error = ejecutar_con_arbol(
                &mut cmd,
                Duration::from_secs(30),
                &CancelToken::new(),
                || {},
                |_| None,
            )
            .expect_err("sin Job Object el hijo suspendido se mata y se reporta");
            assert!(error.to_string().contains("árbol"), "{error}");
            assert!(
                inicio.elapsed() < Duration::from_secs(5),
                "tardó {:?} en fallar",
                inicio.elapsed()
            );
        }
    }

    #[test]
    fn command_rejects_unlisted_program() {
        let raiz = tempfile::tempdir().expect("raiz");
        let set = conjunto(raiz.path(), Duration::from_secs(10));
        let error = correr(
            &set,
            "programa-que-no-esta-permitido",
            Vec::new(),
            raiz.path(),
        )
        .expect_err("un programa fuera de la lista no corre");
        assert_eq!(error, "ese programa no está en la lista permitida");
    }

    #[test]
    fn command_rejects_outside_cwd() {
        let raiz = tempfile::tempdir().expect("raiz");
        let fuera = tempfile::tempdir().expect("fuera");
        let set = conjunto(raiz.path(), Duration::from_secs(10));
        let exe = std::env::current_exe().expect("exe de pruebas");
        let error = correr(
            &set,
            &exe.to_string_lossy(),
            args_ayudante("env", &[]),
            fuera.path(),
        )
        .expect_err("una carpeta fuera de la raíz no corre");
        assert_eq!(error, "la ruta queda fuera de las carpetas permitidas");

        let archivo = raiz.path().join("archivo.txt");
        std::fs::write(&archivo, "x").expect("archivo");
        let error = correr(
            &set,
            &exe.to_string_lossy(),
            args_ayudante("env", &[]),
            &archivo,
        )
        .expect_err("un archivo no es un directorio de trabajo");
        assert_eq!(error, "el directorio de trabajo no es una carpeta");
    }

    #[test]
    fn command_output_caps_at_64k() {
        let raiz = tempfile::tempdir().expect("raiz");
        let set = conjunto(raiz.path(), Duration::from_secs(30));
        let exe = std::env::current_exe().expect("exe de pruebas");
        let salida = correr(
            &set,
            &exe.to_string_lossy(),
            args_ayudante("grande", &[]),
            raiz.path(),
        )
        .expect("correr");
        assert_eq!(
            salida.len(),
            MAX_SALIDA + "\n[recortado]".len(),
            "la salida no quedó en 64 KiB"
        );
        assert!(
            salida.ends_with("\n[recortado]"),
            "falta la marca de recorte: {}",
            &salida[salida.len().saturating_sub(20)..]
        );
    }

    #[test]
    fn env_unix_only_home_lang_path() {
        let entorno = entorno_unix("/x/h");
        assert!(entorno.contains(&"HOME=/x/h".to_owned()));
        assert_eq!(nombres_de_variables(&entorno), ["HOME", "LANG", "PATH"]);
    }

    #[test]
    fn env_windows_only_expected_vars() {
        let entorno = entorno_windows(r"C:\Windows", r"C:\Temp");
        assert_eq!(
            nombres_de_variables(&entorno),
            ["PATH", "PATHEXT", "SystemRoot", "TEMP", "TMP", "WINDIR"]
        );
    }

    fn nombres_de_variables(entorno: &[String]) -> Vec<String> {
        let mut nombres: Vec<String> = entorno
            .iter()
            .map(|variable| {
                variable
                    .split_once('=')
                    .map(|(nombre, _)| nombre.to_owned())
                    .unwrap_or_default()
            })
            .collect();
        nombres.sort();
        nombres
    }

    #[test]
    fn hung_command_is_killed_with_children() {
        let raiz = tempfile::tempdir().expect("raiz");
        let marca = raiz.path().join("hijo-vivo");
        let set = conjunto(raiz.path(), Duration::from_millis(300));
        let exe = std::env::current_exe().expect("exe de pruebas");
        let inicio = Instant::now();
        let error = correr(
            &set,
            &exe.to_string_lossy(),
            args_ayudante("family", &[format!("marker={}", marca.to_string_lossy())]),
            raiz.path(),
        )
        .expect_err("un comando colgado termina con error");
        assert!(
            error.contains("el comando se pasó de 300ms y se cortó"),
            "{error}"
        );
        assert!(
            inicio.elapsed() < Duration::from_secs(3),
            "tardó {:?} en cortar el comando",
            inicio.elapsed()
        );
        std::thread::sleep(Duration::from_secs(4));
        assert!(
            !marca.exists(),
            "el hijo del comando siguió vivo después de matar el árbol"
        );
    }

    /// Issue #94: un nieto creado en la ventana spawn→Job Object tiene que
    /// quedar dentro del árbol. El gancho espera la confirmación de que el
    /// comando ya creó el nieto ANTES de asignar el job: sin crear el
    /// proceso suspendido, ese nieto escapa del job y sobrevive al cierre.
    #[cfg(windows)]
    #[test]
    fn nieto_de_la_ventana_spawn_muere_con_el_arbol() {
        let raiz = tempfile::tempdir().expect("raiz");
        let marca = raiz.path().join("nieto-vivo");
        let confirmacion = raiz.path().join("nieto-vivo-confirmacion");
        let set = conjunto(raiz.path(), Duration::from_secs(30));
        let exe = std::env::current_exe().expect("exe de pruebas");
        let programa = exe.to_string_lossy().into_owned();
        let cwd = raiz.path().to_string_lossy().into_owned();
        let args = args_ayudante("nieto-ya", &[format!("marker={}", marca.to_string_lossy())]);
        let (aviso, espera) = std::sync::mpsc::channel();
        let hilo = std::thread::spawn(move || {
            let resultado = run_command_con(
                &set,
                json!({"program": programa, "args": args, "cwd": cwd}),
                &CancelToken::new(),
                || {
                    // Deja que el comando cree el nieto antes de seguir:
                    // hasta 2 s. Con el proceso suspendido la confirmación
                    // no llega y se sigue tras el plazo.
                    let inicio = Instant::now();
                    while !confirmacion.exists() && inicio.elapsed() < Duration::from_secs(2) {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                },
            )
            .map_err(|error| error.to_string());
            let _ = aviso.send(resultado);
        });
        let resultado = espera
            .recv_timeout(Duration::from_secs(15))
            .expect("la herramienta no volvió en 15 s");
        hilo.join().expect("hilo del comando");
        let salida = resultado.expect("el comando terminó bien");
        assert!(salida.contains("padre-listo"), "{salida}");
        std::thread::sleep(Duration::from_secs(4));
        assert!(
            !marca.exists(),
            "el nieto creado antes del Job Object escapó del árbol y siguió vivo"
        );
    }

    /// Issue #94: un programa relativo con separadores se resuelve contra el
    /// cwd pedido, no contra el CWD de la app. La raíz es la carpeta del exe
    /// de pruebas y el programa es su nombre con un `./` delante.
    #[test]
    fn programa_relativo_se_resuelve_contra_cwd() {
        let exe = std::env::current_exe().expect("exe de pruebas");
        let carpeta = exe.parent().expect("carpeta del exe");
        let set = conjunto(carpeta, Duration::from_secs(10));
        let nombre = exe.file_name().expect("nombre del exe").to_string_lossy();
        let relativo = format!(".{}{nombre}", std::path::MAIN_SEPARATOR);
        let salida = correr(
            &set,
            &relativo,
            args_ayudante("echo", &["arg=hola".to_owned()]),
            carpeta,
        )
        .expect("el programa relativo del cwd corre");
        assert!(salida.contains("ARG=hola"), "{salida}");
    }

    /// Programa externo de los tests. Solo actúa cuando recibe `--` y un
    /// `mode=`, como `TestHelperProcess` en Go (`localtools_test.go:284-324`).
    #[test]
    fn ayudante_proceso() {
        let argumentos: Vec<String> = std::env::args().collect();
        let Some(corte) = argumentos.iter().position(|argumento| argumento == "--") else {
            return;
        };
        let mut modo = String::new();
        let mut marca = String::new();
        let mut texto = String::new();
        for argumento in &argumentos[corte + 1..] {
            if let Some(valor) = argumento.strip_prefix("mode=") {
                modo = valor.to_owned();
            } else if let Some(valor) = argumento.strip_prefix("marker=") {
                marca = valor.to_owned();
            } else if let Some(valor) = argumento.strip_prefix("arg=") {
                texto = valor.to_owned();
            }
        }
        match modo.as_str() {
            "env" => {
                print!(
                    "SECRETO={}",
                    std::env::var("ORUSCRIBE_TEST_SECRET").unwrap_or_default()
                );
            }
            "echo" => print!("ARG={texto}"),
            "grande" => print!("{}", "a".repeat(70 * 1024)),
            "family" => {
                let mut hijo = Command::new(std::env::current_exe().expect("exe de pruebas"))
                    .args([
                        "--exact",
                        NOMBRE_AYUDANTE,
                        "--nocapture",
                        "--",
                        "mode=marker",
                    ])
                    .arg(format!("marker={marca}"))
                    .spawn()
                    .expect("hijo marcador");
                std::thread::sleep(Duration::from_secs(30));
                let _ = hijo.wait();
            }
            "marker" => {
                std::thread::sleep(Duration::from_secs(3));
                std::fs::write(&marca, "vivo").expect("marca");
            }
            // Deja un nieto con la salida heredada y termina: el nieto sigue
            // con la tubería abierta, así que el lector no ve EOF.
            "nieto" => {
                // La pausa ya no es necesaria: el job se asigna antes de
                // reanudar el proceso (#94). Se conserva como margen.
                std::thread::sleep(Duration::from_millis(300));
                // El nieto tiene que quedar vivo cuando el comando termina: es
                // justo el caso que cuelga la herramienta, así que no se espera.
                #[allow(clippy::zombie_processes)]
                let _nieto = Command::new(std::env::current_exe().expect("exe de pruebas"))
                    .args([
                        "--exact",
                        NOMBRE_AYUDANTE,
                        "--nocapture",
                        "--",
                        "mode=nieto-vivo",
                    ])
                    .arg(format!("marker={marca}"))
                    .spawn()
                    .expect("nieto marcador");
                print!("padre-listo");
            }
            "nieto-vivo" => {
                std::thread::sleep(Duration::from_secs(3));
                std::fs::write(&marca, "vivo").expect("marca");
                std::thread::sleep(Duration::from_secs(60));
            }
            // Issue #94: crea el nieto apenas arranca, sin la pausa del modo
            // «nieto», y avisa con un archivo de confirmación para que el
            // gancho de la prueba sepa que el nieto ya existe.
            "nieto-ya" => {
                #[allow(clippy::zombie_processes)]
                let _nieto = Command::new(std::env::current_exe().expect("exe de pruebas"))
                    .args([
                        "--exact",
                        NOMBRE_AYUDANTE,
                        "--nocapture",
                        "--",
                        "mode=nieto-corto",
                    ])
                    .arg(format!("marker={marca}"))
                    .spawn()
                    .expect("nieto marcador");
                std::fs::write(format!("{marca}-confirmacion"), "listo").expect("confirmación");
                print!("padre-listo");
            }
            // Nieto de vida corta: escribe la marca a los 3 s y sale a los
            // 5 s, para que la prueba no deje procesos vivos si escapa.
            "nieto-corto" => {
                std::thread::sleep(Duration::from_secs(3));
                std::fs::write(&marca, "vivo").expect("marca");
                std::thread::sleep(Duration::from_secs(2));
            }
            _ => {}
        }
        let _ = std::io::stdout().flush();
    }
}
