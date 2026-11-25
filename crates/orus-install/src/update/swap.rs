//! Cambio del ejecutable en uso y relanzamiento
//! (`internal/update/restart_windows.go:14-32`,
//! `internal/update/restart_other.go:12-26`).

use std::path::Path;
use std::process::{Command, Stdio};

#[cfg(windows)]
/// Windows no deja sobrescribir un programa en ejecución, pero sí renombrarlo:
/// el update va por dos renames y conserva `target.old` (`restart_windows.go:11-25`).
pub fn swap(downloaded: &Path, target: &Path) -> std::io::Result<()> {
    intercambiar(downloaded, target, |origen, destino| {
        std::fs::rename(origen, destino)
    })
}

pub(super) fn intercambiar(
    downloaded: &Path,
    target: &Path,
    renombrar: impl Fn(&Path, &Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let mut old = target.as_os_str().to_owned();
    old.push(".old");
    let old = std::path::PathBuf::from(old);
    let _ = std::fs::remove_file(&old);
    renombrar(target, &old)?;
    if let Err(error) = renombrar(downloaded, target) {
        return match renombrar(&old, target) {
            Ok(()) => Err(error),
            Err(restauracion) => Err(std::io::Error::new(
                error.kind(),
                format!(
                    "no se pudo colocar la versión nueva ({error}) ni restaurar la anterior ({restauracion}); la copia anterior sigue en {}",
                    old.display()
                ),
            )),
        };
    }
    Ok(())
}

#[cfg(not(windows))]
/// Linux y macOS dejan reemplazar un programa en ejecución renombrando encima.
pub fn swap(downloaded: &Path, target: &Path) -> std::io::Result<()> {
    std::fs::rename(downloaded, target)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(target, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

#[cfg(windows)]
/// Devuelve `target.old` a su lugar cuando la copia nueva no arranca: la
/// nueva queda como `target.failed` para poder mirarla, no se borra.
pub fn rollback(target: &Path) -> std::io::Result<()> {
    let mut old = target.as_os_str().to_owned();
    old.push(".old");
    let old = std::path::PathBuf::from(old);
    if !old.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no hay copia anterior para restaurar",
        ));
    }
    let mut failed = target.as_os_str().to_owned();
    failed.push(".failed");
    let failed = std::path::PathBuf::from(failed);
    let _ = std::fs::remove_file(&failed);
    std::fs::rename(target, &failed)?;
    std::fs::rename(&old, target)
}

#[cfg(not(windows))]
/// En Linux y macOS el swap pisa el binario en uso y no deja `.old`.
pub fn rollback(_target: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "no hay copia anterior para restaurar",
    ))
}

#[cfg(windows)]
/// Arranca la copia nueva desacoplada de esta consola y en su propio grupo:
/// `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP` (`restart_windows.go:27-32`).
pub fn restart(target: &Path) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;

    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    Command::new(target)
        .arg("--relaunch")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
        .spawn()
        .map(|_| ())
}

#[cfg(not(windows))]
/// Arranca la copia nueva con `--relaunch` para esperar a que el proceso viejo
/// libere el candado `flock` de instancia única sin abortar el reinicio.
pub fn restart(target: &Path) -> std::io::Result<()> {
    Command::new(target)
        .arg("--relaunch")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
}

/// ¿La copia recién aplicada contesta `--version`?
///
/// Un binario truncado, de otra arquitectura o roto falla aquí. Windows lo
/// consulta después del swap (y vuelve atrás con [`rollback`]); Linux lo
/// consulta sobre el `.new` **antes** de pisar el binario en uso, porque no
/// tiene `.old` para volver.
pub fn responde_version(exe: &Path) -> bool {
    const ESPERA: u64 = 100;
    const PASO: std::time::Duration = std::time::Duration::from_millis(50);

    let mut hijo = match Command::new(exe)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(hijo) => hijo,
        Err(_) => return false,
    };
    for _ in 0..ESPERA {
        match hijo.try_wait() {
            Ok(Some(estado)) => return estado.success(),
            Ok(None) => std::thread::sleep(PASO),
            Err(_) => return false,
        }
    }
    let _ = hijo.kill();
    false
}
