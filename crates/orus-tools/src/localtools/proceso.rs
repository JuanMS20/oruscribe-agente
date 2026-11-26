//! Árbol de procesos de un comando: Job Object en Windows y grupo de procesos
//! en Unix.
//!
//! Se usa desde `comando.rs`: `prepare_process` corre antes de arrancar (solo
//! Unix), `attach_tree` junta al hijo recién creado con sus descendientes,
//! `kill_tree` mata todo el árbol al vencer el timeout o al cancelar, y
//! `close` libera el Job Object después de esperar al proceso, como el
//! `defer tree.close` del contrato.

use std::process::Command;

#[cfg(windows)]
use std::mem::size_of;

#[cfg(windows)]
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
#[cfg(windows)]
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject,
};
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE};

/// El objeto de trabajo que contiene al programa y a sus hijos.
#[cfg(windows)]
pub(super) struct Arbol {
    job: HANDLE,
}

/// En Unix el grupo de procesos ya lo fija `prepare_process`: no hay objeto
/// que conservar.
#[cfg(unix)]
pub(super) struct Arbol;

/// En Windows no hace nada: el proceso tiene que existir para asignarlo al
/// objeto de trabajo.
#[cfg(windows)]
pub(super) fn prepare_process(_cmd: &mut Command) {}

/// Pone al programa en su propio grupo para poder matar a todos sus hijos de
/// una vez. `CommandExt::process_group` es la forma
/// segura de `setpgid` en la biblioteca estándar.
#[cfg(unix)]
pub(super) fn prepare_process(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
}

/// Crea el Job Object con `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` y asigna el
/// proceso. Si algo falla devuelve `None`, como el
/// error que el contrato ignora: `kill_tree` cae a `taskkill`.
#[cfg(windows)]
pub(super) fn attach_tree(pid: u32) -> Option<Arbol> {
    // SAFETY: los dos atributos van nulos; CreateJobObjectW no desreferencia
    // nada y devuelve un handle propio o NULL.
    let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if job.is_null() {
        return None;
    }
    let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    // SAFETY: `job` es un handle válido y `info` vive durante la llamada con
    // el tamaño exacto que espera la API.
    let puesto = unsafe {
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            std::ptr::from_ref(&info).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    };
    if puesto == 0 {
        // SAFETY: `job` se creó arriba y todavía no se usó en otro sitio.
        unsafe { CloseHandle(job) };
        return None;
    }
    // SAFETY: OpenProcess devuelve un handle propio o NULL; se cierra abajo.
    let proceso = unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid) };
    if proceso.is_null() {
        // SAFETY: `job` sigue siendo el único dueño del handle.
        unsafe { CloseHandle(job) };
        return None;
    }
    // SAFETY: `job` y `proceso` son handles válidos; la asignación no toma
    // posesión de ellos.
    let asignado = unsafe { AssignProcessToJobObject(job, proceso) };
    // SAFETY: el handle del proceso no se vuelve a usar después de cerrarlo.
    unsafe { CloseHandle(proceso) };
    if asignado == 0 {
        // SAFETY: `job` sigue siendo el único dueño del handle.
        unsafe { CloseHandle(job) };
        return None;
    }
    Some(Arbol { job })
}

/// En Unix no hay nada que asignar: el grupo lo puso `prepare_process`.
#[cfg(unix)]
pub(super) fn attach_tree(_pid: u32) -> Option<Arbol> {
    None
}

/// Mata el árbol completo. En Windows termina el Job Object; si no se pudo
/// asignar, pide a Windows que mate el árbol por el id
#[cfg(windows)]
pub(super) fn kill_tree(arbol: Option<&Arbol>, pid: u32) {
    if let Some(arbol) = arbol {
        // SAFETY: el handle pertenece a la Job Object creada por attach_tree y
        // sigue abierto hasta `close`.
        unsafe { TerminateJobObject(arbol.job, 1) };
        return;
    }
    let pid = pid.to_string();
    let _ = Command::new("taskkill")
        .args(["/T", "/F", "/PID", &pid])
        .status();
}

/// Mata el grupo entero, no solo al padre; el negativo es la convención de
/// Unix.
#[cfg(unix)]
pub(super) fn kill_tree(_arbol: Option<&Arbol>, pid: u32) {
    // SAFETY: `kill` con un pid negativo envía la señal a ese grupo. Si el
    // grupo ya no existe devuelve ESRCH, que se ignora como el `_ =` del contrato.
    unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
}

#[cfg(windows)]
impl Arbol {
    /// Cierra el handle. Con `KILL_ON_JOB_CLOSE`, cualquier hijo que siguiera
    /// vivo muere aquí.
    pub(super) fn close(self) {
        // SAFETY: `self` es el único dueño del handle y se consume al llamar.
        unsafe { CloseHandle(self.job) };
    }
}

#[cfg(unix)]
impl Arbol {
    pub(super) fn close(self) {}
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::path::Path;
    use std::process::{Child, Command};
    use std::time::Duration;

    /// Arranca el ayudante de `comando.rs`, que crea un hijo marcador y se
    /// queda dormido. El hijo hereda el Job Object o el grupo según el sistema.
    fn lanzar_familia(marca: &Path) -> Child {
        let mut cmd = Command::new(std::env::current_exe().expect("exe de pruebas"));
        cmd.args([
            "--exact",
            super::super::comando::NOMBRE_AYUDANTE,
            "--nocapture",
            "--",
            "mode=family",
        ])
        .arg(format!("marker={}", marca.to_string_lossy()));
        super::prepare_process(&mut cmd);
        cmd.spawn().expect("ayudante")
    }

    #[cfg(windows)]
    #[test]
    fn windows_job_object_kills_descendants() {
        let raiz = tempfile::tempdir().expect("raiz");
        let marca = raiz.path().join("hijo-vivo");
        let mut hijo = lanzar_familia(&marca);
        let arbol = super::attach_tree(hijo.id());
        assert!(
            arbol.is_some(),
            "no se pudo asignar el proceso al Job Object"
        );
        std::thread::sleep(Duration::from_millis(700));
        super::kill_tree(arbol.as_ref(), hijo.id());
        let _ = hijo.wait();
        if let Some(arbol) = arbol {
            arbol.close();
        }
        std::thread::sleep(Duration::from_secs(4));
        assert!(
            !marca.exists(),
            "el hijo siguió vivo después de TerminateJobObject"
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_process_group_kills_descendants() {
        let raiz = tempfile::tempdir().expect("raiz");
        let marca = raiz.path().join("hijo-vivo");
        let mut hijo = lanzar_familia(&marca);
        let arbol = super::attach_tree(hijo.id());
        assert!(arbol.is_none(), "Unix no usa árbol");
        std::thread::sleep(Duration::from_millis(700));
        super::kill_tree(arbol.as_ref(), hijo.id());
        let _ = hijo.wait();
        if let Some(arbol) = arbol {
            arbol.close();
        }
        std::thread::sleep(Duration::from_secs(4));
        assert!(
            !marca.exists(),
            "el hijo siguió vivo después de matar el grupo de procesos"
        );
    }
}
