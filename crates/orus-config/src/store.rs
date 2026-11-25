use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};

use crate::settings::{SETTINGS_FILE, load_para_editar};
use crate::{Error, Settings, data_dir};

// Serializa los guardados dentro de este proceso. El temporal es único por
// guardado para que la ventana y el CLI no se pisen el archivo a medias.
static SAVE_MUTEX: Mutex<()> = Mutex::new(());
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn lock_saves() -> MutexGuard<'static, ()> {
    match SAVE_MUTEX.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Escribe los ajustes de forma atómica: un corte a mitad de escritura no
/// puede dejar un archivo a medias que Load después ignore.
pub fn save(settings: Settings) -> Result<(), Error> {
    let _guard = lock_saves();
    save_unlocked(&settings)
}

/// Aplica `mutate` sobre los ajustes frescos de disco y guarda el resultado,
/// todo bajo el candado de guardado: dos editores simultáneos no pueden
/// leer-viejo-y-escribir-nuevo y perderse el cambio del otro.
///
/// Si `settings.json` existe pero no se puede interpretar, devuelve error y no
/// escribe: guardar los defaults encima borraría lo que la persona tenía.
pub fn editar(mutate: impl FnOnce(&mut Settings)) -> Result<(), Error> {
    let _guard = lock_saves();
    let mut settings = load_para_editar()?;
    mutate(&mut settings);
    save_unlocked(&settings)
}

fn save_unlocked(settings: &Settings) -> Result<(), Error> {
    let dir = data_dir();
    create_private_dir(&dir)?;
    let data = serde_json::to_vec_pretty(settings)?;
    write_atomic(&dir, &data)
}

fn create_private_dir(dir: &Path) -> Result<(), Error> {
    let mut builder = DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)?;
    Ok(())
}

fn write_atomic(dir: &Path, data: &[u8]) -> Result<(), Error> {
    let (temp_path, mut file) = create_temp(dir)?;

    let written = (|| -> std::io::Result<()> {
        file.write_all(data)?;
        // Sin fsync, un corte de energía puede dejar el temporal vacío y el
        // rename lo pondría encima del archivo bueno.
        file.sync_all()
    })();
    if let Err(error) = written {
        drop(file);
        let _ = fs::remove_file(&temp_path);
        return Err(error.into());
    }
    drop(file);

    if let Err(error) = fs::rename(&temp_path, dir.join(SETTINGS_FILE)) {
        let _ = fs::remove_file(&temp_path);
        return Err(error.into());
    }
    Ok(())
}

fn create_temp(dir: &Path) -> Result<(PathBuf, File), Error> {
    for _ in 0..16 {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let name = format!("settings-{}-{sequence}-{nanos}.tmp", std::process::id());
        let path = dir.join(name);

        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }

        match options.open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Err(Error::TempFile(dir.to_path_buf()))
}
