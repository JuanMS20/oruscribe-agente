//! Diario de escrituras: respalda un archivo antes de que el agente lo pise y
//! guarda lo necesario para deshacer las últimas operaciones.
//!
//! Port de `internal/journal` (Go). El diario vive en `DataDir/journal`: un
//! archivo `journal.jsonl` con las entradas y un respaldo `.bak` por escritura.
//! Guarda las últimas [`MAX_ENTRADAS`].

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use chrono::{DateTime, Utc};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

mod error;
pub use error::Error;

const MAX_ENTRADAS: usize = 50;

// Serializa record/list/undo en este proceso: el agente puede escribir
// mientras la ventana deshace. Entre procesos distintos (CLI y app) sigue
// siendo responsabilidad de quien llama no solapar; el diario se reescribe de
// forma atómica y nunca queda a medias.
static MU: Mutex<()> = Mutex::new(());

fn lock() -> MutexGuard<'static, ()> {
    match MU.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Lo que el diario sabe de una escritura: dónde y cómo estaba y quedó el
/// archivo.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub path: PathBuf,
    #[serde(default)]
    pub existed: bool,
    /// Nombre del respaldo relativo al diario.
    #[serde(default)]
    pub backup: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sha_before: String,
    #[serde(default)]
    pub sha_after: String,
    #[serde(default)]
    pub size: i64,
    #[serde(default, skip_serializing_if = "es_cero")]
    pub mode: u32,
    #[serde(default = "when_por_defecto")]
    pub when: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pending: bool,
}

fn es_cero(valor: &u32) -> bool {
    *valor == 0
}

fn when_por_defecto() -> DateTime<Utc> {
    DateTime::<Utc>::from(std::time::UNIX_EPOCH)
}

/// Diario abierto sobre una carpeta.
#[derive(Debug, Clone)]
pub struct Journal {
    dir: PathBuf,
}

/// Decide si una ruta se puede tocar antes de deshacerla.
pub type Guard<'a> = &'a dyn Fn(&Path) -> Result<(), String>;

impl Journal {
    /// Crea la carpeta si hace falta y devuelve el diario listo para usar.
    pub fn open(dir: impl AsRef<Path>) -> Result<Self, Error> {
        let dir = dir.as_ref();
        if dir.to_string_lossy().trim().is_empty() {
            return Err(Error::EmptyDir);
        }
        crear_dir(dir)?;
        let diario = Self {
            dir: dir.to_path_buf(),
        };
        let _lock = lock();
        diario.recuperar()?;
        Ok(diario)
    }

    fn journal_path(&self) -> PathBuf {
        self.dir.join("journal.jsonl")
    }

    /// Respalda el contenido previo a una escritura y registra la operación.
    pub fn record(
        &self,
        ruta: impl AsRef<Path>,
        antes: &[u8],
        existia: bool,
        despues: &[u8],
    ) -> Result<(), Error> {
        self.registrar(ruta, antes, existia, despues, false)
            .map(|_| ())
    }

    /// Primera fase: deja el respaldo y la entrada en disco ANTES del rename.
    pub fn prepare(
        &self,
        ruta: impl AsRef<Path>,
        antes: &[u8],
        existia: bool,
        despues: &[u8],
    ) -> Result<String, Error> {
        self.registrar(ruta, antes, existia, despues, true)
    }

    fn registrar(
        &self,
        ruta: impl AsRef<Path>,
        antes: &[u8],
        existia: bool,
        despues: &[u8],
        pending: bool,
    ) -> Result<String, Error> {
        let ruta = ruta.as_ref();
        let _lock = lock();
        let id = nuevo_id()?;
        let mut entrada = Entry {
            id: id.clone(),
            path: ruta.to_path_buf(),
            existed: existia,
            backup: String::new(),
            sha_before: String::new(),
            sha_after: hash(despues),
            size: despues.len() as i64,
            mode: 0,
            when: Utc::now(),
            pending,
        };
        if existia {
            entrada.sha_before = hash(antes);
            entrada.backup = format!("{}.bak", entrada.id);
            let destino = self.dir.join(&entrada.backup);
            // El respaldo también tiene que llegar al disco: si la entrada del
            // diario sobrevive a un corte y el .bak no, undo no puede restaurar.
            escribir_atomico(&destino, antes, 0o600).map_err(|source| Error::Backup {
                path: ruta.to_path_buf(),
                source,
            })?;
            if let Ok(info) = fs::metadata(ruta) {
                entrada.mode = modo_de(&info);
            }
        }
        // Si algo falla al anexar, el respaldo se conserva: cuando `write_all`
        // entró y solo falló `sync_all`, la línea queda en el diario y borrar
        // el `.bak` dejaría el deshacer trabado en `BackupMissing` (#83, #128).
        // Un respaldo huérfano es inocuo; una entrada sin respaldo no.
        let mut linea = serde_json::to_vec(&entrada)?;
        linea.push(b'\n');
        let mut archivo = abrir_para_anexar(&self.journal_path())?;
        anexar_entrada(&mut archivo, &linea)?;
        self.recortar_con(self.leer()?)?;
        Ok(id)
    }

    /// Segunda fase: la escritura ya está en disco; la entrada deja de estar
    /// pendiente y puede deshacerse.
    pub fn commit(&self, id: &str) -> Result<(), Error> {
        let _lock = lock();
        self.recuperar()?;
        let mut entradas = self.leer()?;
        let Some(entrada) = entradas.iter_mut().find(|entrada| entrada.id == id) else {
            return Err(Error::PendingMissing(id.to_owned()));
        };
        entrada.pending = false;
        self.escribir(&entradas)
    }

    /// La escritura no llegó a aplicarse: se retira la entrada y su respaldo.
    pub fn descartar(&self, id: &str) -> Result<(), Error> {
        let _lock = lock();
        let (conservar, quitar): (Vec<_>, Vec<_>) = self
            .leer()?
            .into_iter()
            .partition(|entrada| entrada.id != id);
        if quitar.is_empty() {
            return Err(Error::PendingMissing(id.to_owned()));
        }
        for entrada in &quitar {
            if !entrada.backup.is_empty() {
                let _ = fs::remove_file(self.dir.join(&entrada.backup));
            }
        }
        self.escribir(&conservar)
    }

    /// Reconcilia las entradas pendientes de una corrida anterior: si el
    /// archivo ya tiene el contenido de la escritura se confirman; si no, se
    /// retiran. Así un corte entre el rename y el commit no pierde el deshacer.
    fn recuperar(&self) -> Result<(), Error> {
        let entradas = self.leer()?;
        let mut quedan = Vec::with_capacity(entradas.len());
        let mut cambios = false;
        for mut entrada in entradas {
            if !entrada.pending {
                quedan.push(entrada);
                continue;
            }
            cambios = true;
            let actual = match fs::read(&entrada.path) {
                Ok(datos) => Some(datos),
                Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                Err(error) => return Err(error.into()),
            };
            let aplicada = match &actual {
                Some(datos) => hash(datos) == entrada.sha_after,
                None => !entrada.existed,
            };
            if aplicada {
                entrada.pending = false;
                quedan.push(entrada);
            } else if !entrada.backup.is_empty() {
                let _ = fs::remove_file(self.dir.join(&entrada.backup));
            }
        }
        if cambios {
            self.escribir(&quedan)?;
        }
        Ok(())
    }

    /// Devuelve las entradas, de la más reciente a la más vieja. Una línea
    /// dañada se salta; perder el resto del diario por una línea corrupta no es
    /// aceptable.
    pub fn list(&self) -> Result<Vec<Entry>, Error> {
        let _lock = lock();
        self.list_sin_lock()
    }

    /// `list` sin el lock; se usa desde `record` y `undo`, que ya lo tienen.
    fn list_sin_lock(&self) -> Result<Vec<Entry>, Error> {
        self.recuperar()?;
        self.leer()
    }

    fn leer(&self) -> Result<Vec<Entry>, Error> {
        let datos = match fs::read(self.journal_path()) {
            Ok(datos) => datos,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut salida = Vec::new();
        for linea in datos.split(|byte| *byte == b'\n') {
            let texto = String::from_utf8_lossy(linea);
            let texto = texto.trim();
            if texto.is_empty() {
                continue;
            }
            let Ok(entrada) = serde_json::from_str::<Entry>(texto) else {
                continue;
            };
            if entrada.id.is_empty() || entrada.path.as_os_str().is_empty() {
                continue;
            }
            salida.push(entrada);
        }
        salida.reverse();
        Ok(salida)
    }

    /// Deja el diario en las últimas [`MAX_ENTRADAS`] y borra los respaldos de
    /// las que descarta.
    fn recortar_con(&self, entradas: Vec<Entry>) -> Result<(), Error> {
        if entradas.len() <= MAX_ENTRADAS {
            return Ok(());
        }
        let (conservar, descartar) = entradas.split_at(MAX_ENTRADAS);
        self.reescribir(conservar, descartar)
    }

    /// Sustituye el diario por las entradas dadas (orden reciente → vieja) y
    /// borra los respaldos descartados. El diario se reescribe ANTES de
    /// borrar nada: si la escritura falla, las entradas siguen listando sus
    /// respaldos y el deshacer no queda trabado (#83).
    fn reescribir(&self, conservar: &[Entry], descartar: &[Entry]) -> Result<(), Error> {
        self.escribir(conservar)?;
        for entrada in descartar {
            if !entrada.backup.is_empty() {
                let _ = fs::remove_file(self.dir.join(&entrada.backup));
            }
        }
        Ok(())
    }

    fn escribir(&self, recientes_primero: &[Entry]) -> Result<(), Error> {
        let mut datos = Vec::new();
        for entrada in recientes_primero.iter().rev() {
            serde_json::to_writer(&mut datos, entrada)?;
            datos.push(b'\n');
        }
        // La misma escritura atómica que deshacer: nombre temporal único y
        // datos llevados al disco antes del rename, para que un corte no deje
        // el diario vacío ni a medias.
        escribir_atomico(&self.journal_path(), &datos, 0o600)?;
        Ok(())
    }

    /// Deshace las últimas `n` operaciones, de la más reciente a la más vieja.
    /// `guard` decide si una ruta se puede tocar. Sin `force`, si el archivo
    /// cambió desde la escritura del agente la operación se niega entera antes
    /// de tocar nada. Devuelve cuántas quedaron hechas y el error, como el Go.
    pub fn undo(&self, n: i64, guard: Option<Guard<'_>>, force: bool) -> (usize, Option<Error>) {
        let n = if n <= 0 {
            1
        } else {
            usize::try_from(n).unwrap_or(usize::MAX)
        };
        let _lock = lock();
        let entradas = match self.list_sin_lock() {
            Ok(entradas) => entradas,
            Err(error) => return (0, Some(error)),
        };
        if entradas.is_empty() {
            return (0, Some(Error::NothingToUndo));
        }
        let n = n.min(entradas.len());
        let pedidas = &entradas[..n];
        let mut hechas: Vec<Entry> = Vec::with_capacity(n);

        // Cada operación es atómica en sí: se valida contra cómo quedó el
        // archivo tras deshacer las anteriores, y se aplica una por una. Si una
        // falla, las ya deshechas quedan hechas y el error lo dice.
        let resultado = (|| -> Result<(), Error> {
            for entrada in pedidas {
                if let Some(guard) = guard
                    && let Err(message) = guard(&entrada.path)
                {
                    return Err(Error::Guard {
                        path: entrada.path.clone(),
                        message,
                    });
                }
                let actual = match fs::read(&entrada.path) {
                    Ok(datos) => Some(datos),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                    Err(error) => return Err(error.into()),
                };
                match &actual {
                    Some(actual) => {
                        if !force && hash(actual) != entrada.sha_after {
                            return Err(Error::Changed {
                                path: entrada.path.clone(),
                            });
                        }
                    }
                    None => {
                        if entrada.existed {
                            return Err(Error::Missing {
                                path: entrada.path.clone(),
                            });
                        }
                    }
                }
                if actual.is_some() && !entrada.existed {
                    fs::remove_file(&entrada.path)?;
                } else if entrada.existed {
                    let respaldo = match fs::read(self.dir.join(&entrada.backup)) {
                        Ok(datos) => datos,
                        Err(source) => {
                            return Err(Error::BackupMissing {
                                path: entrada.path.clone(),
                                source,
                            });
                        }
                    };
                    // Un respaldo truncado o tocado no se escribe: sería pisar
                    // el archivo con basura y devolver éxito.
                    if hash(&respaldo) != entrada.sha_before {
                        return Err(Error::BackupCorrupt {
                            path: entrada.path.clone(),
                        });
                    }
                    let modo = if entrada.mode == 0 {
                        0o600
                    } else {
                        entrada.mode
                    };
                    escribir_atomico(&entrada.path, &respaldo, modo)?;
                }
                hechas.push(entrada.clone());
            }
            Ok(())
        })();

        let mut error = resultado.err();
        if let Err(consume) = self.consumir(&entradas, &hechas) {
            error = Some(match error {
                Some(undo) => Error::UndoAndConsume {
                    undo: Box::new(undo),
                    consume: Box::new(consume),
                },
                None => consume,
            });
        }
        (hechas.len(), error)
    }

    /// Quita del diario las entradas hechas y borra sus respaldos.
    fn consumir(&self, todas: &[Entry], hechas: &[Entry]) -> Result<(), Error> {
        if hechas.is_empty() {
            return Ok(());
        }
        let retiradas: HashSet<&str> = hechas.iter().map(|entrada| entrada.id.as_str()).collect();
        let quedan: Vec<Entry> = todas
            .iter()
            .filter(|entrada| !retiradas.contains(entrada.id.as_str()))
            .cloned()
            .collect();
        self.reescribir(&quedan, hechas)
    }
}

/// Carpeta del diario dentro del directorio de datos.
pub fn default_dir() -> PathBuf {
    orus_config::data_dir().join("journal")
}

fn hash(datos: &[u8]) -> String {
    hex::encode(Sha256::digest(datos))
}

fn nuevo_id() -> Result<String, Error> {
    let mut azar = [0u8; 4];
    rand::rngs::OsRng.try_fill_bytes(&mut azar)?;
    let nanos = Utc::now().timestamp_nanos_opt().unwrap_or_default();
    Ok(format!("{nanos}-{}", hex::encode(azar)))
}

fn crear_dir(dir: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)
}

fn abrir_para_anexar(ruta: &Path) -> io::Result<File> {
    let mut opciones = OpenOptions::new();
    opciones.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opciones.mode(0o600);
    }
    opciones.open(ruta)
}

/// Escribe una línea en el diario y la lleva al disco. Devuelve el primer
/// error: una entrada que no llegó al disco no se puede deshacer.
fn anexar_entrada(archivo: &mut File, linea: &[u8]) -> io::Result<()> {
    archivo.write_all(linea)?;
    archivo.sync_all()
}

fn escribir_atomico(ruta: &Path, datos: &[u8], modo: u32) -> io::Result<()> {
    let dir = ruta
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let (temporal, mut archivo) = crear_temporal(dir)?;
    let volcado = volcar_temporal(&mut archivo, datos, modo);
    drop(archivo);
    if volcado.is_ok() {
        match fs::rename(&temporal, ruta) {
            Ok(()) => return Ok(()),
            Err(error) => {
                let _ = fs::remove_file(&temporal);
                return Err(error);
            }
        }
    }
    let _ = fs::remove_file(&temporal);
    volcado
}

fn crear_temporal(dir: &Path) -> io::Result<(PathBuf, File)> {
    for _ in 0..16 {
        let mut azar = [0u8; 8];
        rand::rngs::OsRng.try_fill_bytes(&mut azar)?;
        let ruta = dir.join(format!(".oruscribe-{}.tmp", hex::encode(azar)));
        let mut opciones = OpenOptions::new();
        opciones.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opciones.mode(0o600);
        }
        match opciones.open(&ruta) {
            Ok(archivo) => return Ok((ruta, archivo)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("no se pudo crear un temporal único en {}", dir.display()),
    ))
}

/// Escribe los datos en el temporal, los lleva al disco y lo deja listo para
/// renombrar. Quien llama solo renombra si esto devolvió `Ok`.
fn volcar_temporal(archivo: &mut File, datos: &[u8], modo: u32) -> io::Result<()> {
    aplicar_modo(archivo, modo)?;
    archivo.write_all(datos)?;
    archivo.sync_all()
}

#[cfg(unix)]
fn aplicar_modo(archivo: &File, modo: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    archivo.set_permissions(fs::Permissions::from_mode(modo))
}

#[cfg(not(unix))]
fn aplicar_modo(_archivo: &File, _modo: u32) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn modo_de(info: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    info.permissions().mode() & 0o777
}

#[cfg(not(unix))]
fn modo_de(_info: &fs::Metadata) -> u32 {
    0
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests;
