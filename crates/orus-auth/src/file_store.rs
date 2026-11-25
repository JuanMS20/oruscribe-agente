use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[cfg(any(test, target_os = "linux"))]
use sha2::{Digest as _, Sha256};

use crate::{Error, Store, Token};

// Cuatro intentos de 25 ms: Windows niega el reemplazo mientras otro proceso
// tiene el archivo abierto (`store.go:120-136`).
const INTENTOS: usize = 4;
const ESPERA: Duration = Duration::from_millis(25);

// El RwLock es de paquete porque FileStore se copia por valor; no ordena
// procesos. App y CLI comparten session.json y necesitan un candado de archivo.
static ARCHIVO_SESION: RwLock<()> = RwLock::new(());
static TEMPORAL_SECUENCIA: AtomicU64 = AtomicU64::new(0);

// Dos segundos de espera y 30 para considerar huérfano un candado de un
// proceso que murió a mitad de guardar o borrar.
const LOCK_INTENTOS: usize = 40;
const LOCK_ESPERA: Duration = Duration::from_millis(50);
const LOCK_HUERFANO: Duration = Duration::from_secs(30);

struct CandadoSesion {
    ruta: PathBuf,
}

impl Drop for CandadoSesion {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.ruta);
    }
}

fn ruta_candado(path: &Path) -> PathBuf {
    let mut nombre = path.as_os_str().to_owned();
    nombre.push(".lock");
    PathBuf::from(nombre)
}

/// `create_new` solo lo consigue un proceso. Si el dueño murió, su candado
/// envejece (o queda con fecha futura) y puede reclamarse.
fn adquirir_candado(path: &Path) -> io::Result<CandadoSesion> {
    let ruta = ruta_candado(path);
    let mut ultimo = None;
    for intento in 0..LOCK_INTENTOS {
        match OpenOptions::new().write(true).create_new(true).open(&ruta) {
            Ok(_) => return Ok(CandadoSesion { ruta }),
            // Windows puede devolver PermissionDenied mientras otro hilo
            // está creando o soltando el mismo archivo.
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::AlreadyExists | io::ErrorKind::PermissionDenied
                ) =>
            {
                ultimo = Some(error);
                if candado_huerfano(&ruta) {
                    let _ = fs::remove_file(&ruta);
                    continue;
                }
                if intento + 1 < LOCK_INTENTOS {
                    thread::sleep(LOCK_ESPERA);
                }
            }
            Err(error) => return Err(error),
        }
    }
    Err(ultimo.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            "otro proceso está usando la sesión",
        )
    }))
}

fn candado_huerfano(ruta: &Path) -> bool {
    let Ok(meta) = fs::metadata(ruta) else {
        return false;
    };
    let Ok(modificado) = meta.modified() else {
        return false;
    };
    match modificado.duration_since(SystemTime::now()) {
        Ok(_) => true,
        Err(antes) => antes.duration() >= LOCK_HUERFANO,
    }
}

/// Respaldo en texto plano para desarrollo y tests (`store.go:85-92`).
pub struct FileStore {
    path: PathBuf,
}

impl FileStore {
    pub fn from_data_dir(data_dir: impl AsRef<Path>) -> Self {
        Self {
            path: data_dir.as_ref().join("session.json"),
        }
    }

    #[cfg(any(test, target_os = "linux"))]
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn guardar_con_datos(&self, token: &Token) -> Result<Vec<u8>, Error> {
        let datos = serde_json::to_vec(token)?;
        let _guard = write_lock();
        let _candado = adquirir_candado(&self.path)?;
        write_private(&self.path, &datos)?;
        Ok(datos)
    }
}

impl Store for FileStore {
    fn save(&self, token: Token) -> Result<(), Error> {
        self.guardar_con_datos(&token).map(|_| ())
    }

    fn load(&self) -> Result<Token, Error> {
        let _guard = read_lock();
        let data = match read_private(&self.path) {
            Ok(data) => data,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(Error::NoSession);
            }
            Err(error) => return Err(error.into()),
        };
        Ok(serde_json::from_slice(&data)?)
    }

    fn clear(&self) -> Result<(), Error> {
        let _guard = write_lock();
        let _candado = adquirir_candado(&self.path)?;
        let dir = self
            .path
            .parent()
            .filter(|dir| !dir.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let mut resto: Option<io::Error> = None;
        if let Ok(entradas) = fs::read_dir(dir) {
            for entrada in entradas.flatten() {
                let nombre = entrada.file_name();
                let Some(nombre) = nombre.to_str() else {
                    continue;
                };
                // Bajo el candado no hay escritura viva: todo temporal es
                // un resto con el token en claro, venga de quien venga.
                if nombre.starts_with("session.json.")
                    && nombre.ends_with(".tmp")
                    && let Err(error) = fs::remove_file(entrada.path())
                    && error.kind() != io::ErrorKind::NotFound
                    && resto.is_none()
                {
                    resto = Some(error);
                }
            }
        }
        match retry(
            || fs::remove_file(&self.path),
            |error| error.kind() == io::ErrorKind::NotFound,
        ) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        #[cfg(any(test, target_os = "linux"))]
        {
            let _ = fs::remove_file(ruta_marca(&self.path));
        }
        match resto {
            Some(error) => Err(error.into()),
            None => Ok(()),
        }
    }
}

/// Escribe bytes con directorio 0700 y archivo 0600, reemplazando de forma
/// atómica. Lo usan el respaldo de sesión y el de la clave del agente.
pub(crate) fn write_private(path: &Path, data: &[u8]) -> io::Result<()> {
    let dir = path.parent().filter(|dir| !dir.as_os_str().is_empty());
    if let Some(dir) = dir {
        create_private_dir(dir)?;
    }
    let prefijo = path
        .file_name()
        .map(|nombre| nombre.to_string_lossy().into_owned())
        .unwrap_or_else(|| "secreto".to_owned());
    let (tmp, mut archivo) = crear_temporal(dir.unwrap_or(Path::new(".")), &prefijo)?;
    let escrito = archivo.write_all(data).and_then(|()| archivo.sync_all());
    drop(archivo);
    if let Err(error) = escrito {
        let _ = fs::remove_file(&tmp); // no dejar el secreto a medias
        return Err(error);
    }
    if let Err(error) = retry(|| fs::rename(&tmp, path), |_| false) {
        let _ = fs::remove_file(&tmp); // el temporal trae el secreto en claro
        return Err(error);
    }
    Ok(())
}

pub(crate) fn read_private(path: &Path) -> io::Result<Vec<u8>> {
    retry(
        || fs::read(path),
        |error| error.kind() == io::ErrorKind::NotFound,
    )
}

/// Borra un respaldo bajo el mismo candado que `FileStore`, sin borrar los
/// temporales ajenos. No encontrar el archivo no es error.
#[cfg(test)]
pub(crate) fn delete_private(path: &Path) -> io::Result<()> {
    let _guard = write_lock();
    let _candado = adquirir_candado(path)?;
    match retry(
        || fs::remove_file(path),
        |error| error.kind() == io::ErrorKind::NotFound,
    ) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(any(test, target_os = "linux"))]
fn ruta_marca(path: &Path) -> PathBuf {
    let mut nombre = path.as_os_str().to_owned();
    nombre.push(".fallback");
    PathBuf::from(nombre)
}

#[cfg(any(test, target_os = "linux"))]
fn marcar_respaldo(path: &Path, datos: &[u8]) -> io::Result<()> {
    write_private(&ruta_marca(path), Sha256::digest(datos).as_slice())
}

#[cfg(test)]
pub(crate) fn borrar_marca(path: &Path) -> io::Result<()> {
    delete_private(&ruta_marca(path))
}

#[cfg(any(test, target_os = "linux"))]
pub(crate) fn guardar_respaldo_marcado(path: &Path, datos: &[u8]) -> io::Result<()> {
    let _guard = write_lock();
    let _candado = adquirir_candado(path)?;
    write_private(path, datos)?;
    marcar_respaldo(path, datos)
}

#[cfg(target_os = "linux")]
pub(crate) fn borrar_respaldo_marcado(path: &Path) -> io::Result<()> {
    let _guard = write_lock();
    let _candado = adquirir_candado(path)?;
    let datos = fs::remove_file(path);
    let marca = fs::remove_file(ruta_marca(path));
    match (datos, marca) {
        (Err(error), _) if error.kind() != io::ErrorKind::NotFound => Err(error),
        (_, Err(error)) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

#[cfg(any(test, target_os = "linux"))]
pub(crate) fn invalidar_marca(path: &Path) {
    let _guard = write_lock();
    if let Ok(_candado) = adquirir_candado(path) {
        let marca = ruta_marca(path);
        let _ = write_private(&marca, &[]);
        let _ = fs::remove_file(&marca);
    }
}

#[cfg(any(test, target_os = "linux"))]
fn marca_vigente(path: &Path, datos: &[u8]) -> bool {
    match read_private(&ruta_marca(path)) {
        Ok(marca) => marca.as_slice() == Sha256::digest(datos).as_slice(),
        Err(_) => false,
    }
}

/// Lo que respondió el llavero, separado del respaldo para poder comprobar
/// la preferencia sin depender de D-Bus.
#[cfg(any(test, target_os = "linux"))]
pub(crate) enum Llavero<T> {
    Valido(T),
    SinEntrada,
    Ausente(String),
    Fallo(String),
}

/// El llavero manda siempre: el respaldo solo se lee si no hay llavero o no
/// tiene entrada. Un fallo de transporte no deja ganar a un token viejo.
#[cfg(any(test, target_os = "linux"))]
pub(crate) fn cargar_con_llavero<T>(
    llavero: Llavero<T>,
    respaldo: impl FnOnce() -> Result<T, Error>,
) -> Result<T, Error> {
    match llavero {
        Llavero::Valido(valor) => Ok(valor),
        Llavero::SinEntrada => respaldo(),
        Llavero::Ausente(reason) => match respaldo() {
            Ok(valor) => Ok(valor),
            Err(Error::NoSession) => Err(Error::Unavailable(reason)),
            Err(error) => Err(error),
        },
        Llavero::Fallo(reason) => Err(Error::Unavailable(reason)),
    }
}

#[cfg(any(test, target_os = "linux"))]
pub(crate) fn cargar_con_llavero_marcado<T: serde::Serialize>(
    llavero: Llavero<T>,
    respaldo: impl FnOnce() -> Result<T, Error>,
    ruta: &Path,
) -> Result<T, Error> {
    match llavero {
        Llavero::Valido(valor) => Ok(valor),
        Llavero::SinEntrada => respaldo_marcado(respaldo, ruta, None),
        Llavero::Ausente(reason) => respaldo_marcado(respaldo, ruta, Some(reason)),
        Llavero::Fallo(reason) => respaldo_marcado(respaldo, ruta, Some(reason)),
    }
}

#[cfg(any(test, target_os = "linux"))]
fn respaldo_marcado<T: serde::Serialize>(
    respaldo: impl FnOnce() -> Result<T, Error>,
    ruta: &Path,
    reason: Option<String>,
) -> Result<T, Error> {
    let sin_marca = || {
        Error::Unavailable(
            reason
                .clone()
                .unwrap_or_else(|| "el respaldo no tiene una marca vigente".to_owned()),
        )
    };
    match respaldo() {
        Ok(valor) => match serde_json::to_vec(&valor) {
            Ok(datos) if marca_vigente(ruta, &datos) => Ok(valor),
            Ok(_) => Err(sin_marca()),
            Err(error) => Err(error.into()),
        },
        Err(Error::NoSession) => Err(match reason {
            Some(reason) => Error::Unavailable(reason),
            None => Error::NoSession,
        }),
        Err(error) => Err(error),
    }
}

/// Escribir en el llavero es el éxito. No poder borrar el respaldo es un
/// aviso, no un fallo: devolver Err haría creer que el login falló y dejaría
/// el token viejo disponible para la próxima lectura.
#[cfg(any(test, target_os = "linux"))]
pub(crate) fn guardar_con_llavero(
    escribir_llavero: impl FnOnce() -> Result<(), String>,
    respaldo: impl FnOnce() -> Result<Vec<u8>, Error>,
    borrar_respaldo: impl FnOnce() -> io::Result<()>,
    ruta: &Path,
    que: &str,
) -> Result<(), Error> {
    match escribir_llavero() {
        Ok(()) => {
            if let Err(error) = borrar_respaldo() {
                eprintln!(
                    "aviso: no se pudo borrar el respaldo {}; el llavero manda: {error}",
                    ruta.display()
                );
            }
            Ok(())
        }
        Err(razon) => match respaldo() {
            Ok(_) => {
                eprintln!(
                    "aviso: no se pudo usar el llavero ({razon}); {que} se guarda en {} con permisos 0600",
                    ruta.display()
                );
                Ok(())
            }
            Err(error) => {
                invalidar_marca(ruta);
                Err(Error::Unavailable(format!(
                    "llavero: {razon}; archivo: {error}"
                )))
            }
        },
    }
}

fn create_private_dir(dir: &Path) -> io::Result<()> {
    let mut builder = DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)
}

// Nombre único por proceso, secuencia y reloj: dos FileStore (CLI y app) no
// comparten temporal. `create_new` evita pisar el de otro proceso.
fn crear_temporal(dir: &Path, prefijo: &str) -> io::Result<(PathBuf, fs::File)> {
    for _ in 0..16 {
        let secuencia = TEMPORAL_SECUENCIA.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duracion| duracion.as_nanos())
            .unwrap_or_default();
        let ruta = dir.join(format!(
            "{prefijo}.{}-{secuencia}-{nanos}.tmp",
            std::process::id()
        ));
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
        "no se pudo crear un archivo temporal único",
    ))
}

fn retry<T>(
    mut operation: impl FnMut() -> io::Result<T>,
    fatal: impl Fn(&io::Error) -> bool,
) -> io::Result<T> {
    let mut last = io::Error::other("no se intentó la operación");
    for _ in 0..INTENTOS {
        match operation() {
            Ok(value) => return Ok(value),
            Err(error) if fatal(&error) => return Err(error),
            Err(error) => {
                last = error;
                thread::sleep(ESPERA);
            }
        }
    }
    Err(last)
}

fn read_lock() -> RwLockReadGuard<'static, ()> {
    ARCHIVO_SESION
        .read()
        .unwrap_or_else(PoisonError::into_inner)
}

fn write_lock() -> RwLockWriteGuard<'static, ()> {
    ARCHIVO_SESION
        .write()
        .unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::sync::mpsc;

    use super::*;

    /// #133: con ambos respaldos presentes gana el llavero, aunque el archivo
    /// tenga un token viejo en claro.
    #[test]
    fn llavero_gana_al_respaldo_al_cargar() -> Result<(), Box<dyn std::error::Error>> {
        let leyo_respaldo = Cell::new(false);
        let obtenido = cargar_con_llavero(Llavero::Valido("del llavero"), || {
            leyo_respaldo.set(true);
            Ok("del respaldo")
        })?;
        assert_eq!(obtenido, "del llavero");
        assert!(!leyo_respaldo.get(), "se leyó el respaldo viejo");
        Ok(())
    }

    #[test]
    fn sin_entrada_en_llavero_usa_el_respaldo() -> Result<(), Box<dyn std::error::Error>> {
        let obtenido = cargar_con_llavero(Llavero::<&str>::SinEntrada, || Ok("del respaldo"))?;
        assert_eq!(obtenido, "del respaldo");
        Ok(())
    }

    #[test]
    fn sin_llavero_usa_el_respaldo() -> Result<(), Box<dyn std::error::Error>> {
        let obtenido = cargar_con_llavero(Llavero::Ausente("sin bus".to_owned()), || {
            Ok("del respaldo")
        })?;
        assert_eq!(obtenido, "del respaldo");
        Ok(())
    }

    /// Sin llavero y sin archivo se conserva el motivo real del error, como
    /// espera quien arranca en una máquina sin Secret Service.
    #[test]
    fn sin_llavero_y_sin_respaldo_reporta_el_motivo() -> Result<(), Box<dyn std::error::Error>> {
        match cargar_con_llavero(Llavero::<&str>::Ausente("sin bus".to_owned()), || {
            Err(Error::NoSession)
        }) {
            Ok(_) => return Err("debía fallar".into()),
            Err(Error::Unavailable(reason)) => assert_eq!(reason, "sin bus"),
            Err(other) => return Err(format!("error inesperado: {other:?}").into()),
        }
        Ok(())
    }

    /// Un bus caído no convierte al archivo viejo en la sesión actual.
    #[test]
    fn fallo_de_llavero_no_deja_ganar_al_respaldo() -> Result<(), Box<dyn std::error::Error>> {
        let leyo_respaldo = Cell::new(false);
        match cargar_con_llavero(Llavero::Fallo("bus caído".to_owned()), || {
            leyo_respaldo.set(true);
            Ok("del respaldo")
        }) {
            Ok(_) => return Err("debía fallar".into()),
            Err(Error::Unavailable(reason)) => assert_eq!(reason, "bus caído"),
            Err(other) => return Err(format!("error inesperado: {other:?}").into()),
        }
        assert!(
            !leyo_respaldo.get(),
            "el respaldo viejo ganó al llavero caído"
        );
        Ok(())
    }

    /// #133: el token nuevo ya está en el llavero; no poder borrar el
    /// respaldo no puede convertir el guardado en un fallo.
    #[test]
    fn guardar_en_llavero_ignora_fallo_al_borrar_respaldo() -> Result<(), Box<dyn std::error::Error>>
    {
        let intento_borrado = Cell::new(false);
        let escribio_respaldo = Cell::new(false);
        let dir = tempfile::tempdir()?;
        guardar_con_llavero(
            || Ok(()),
            || -> Result<Vec<u8>, Error> {
                escribio_respaldo.set(true);
                Ok(Vec::new())
            },
            || {
                intento_borrado.set(true);
                Err(io::Error::from(io::ErrorKind::PermissionDenied))
            },
            &dir.path().join("session.json"),
            "la sesión",
        )?;
        assert!(intento_borrado.get(), "no se intentó borrar el respaldo");
        assert!(!escribio_respaldo.get(), "escribió el respaldo viejo");
        Ok(())
    }

    #[test]
    fn sin_llavero_guarda_en_el_respaldo() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let guardo_respaldo = Cell::new(false);
        guardar_con_llavero(
            || Err("sin bus".to_owned()),
            || -> Result<Vec<u8>, Error> {
                guardo_respaldo.set(true);
                Ok(Vec::new())
            },
            || Ok(()),
            &dir.path().join("session.json"),
            "la sesión",
        )?;
        assert!(guardo_respaldo.get(), "no se usó el respaldo");
        Ok(())
    }

    #[test]
    fn sin_llavero_y_sin_respaldo_falla_con_ambos_motivos() -> Result<(), Box<dyn std::error::Error>>
    {
        let dir = tempfile::tempdir()?;
        match guardar_con_llavero(
            || Err("sin llavero".to_owned()),
            || -> Result<Vec<u8>, Error> {
                Err(Error::Io(io::Error::from(io::ErrorKind::PermissionDenied)))
            },
            || Ok(()),
            &dir.path().join("session.json"),
            "la sesión",
        ) {
            Ok(()) => return Err("debía fallar".into()),
            Err(Error::Unavailable(reason)) => {
                assert!(reason.contains("sin llavero"), "{reason}");
                assert!(
                    reason.contains("Permiso denegado") || reason.contains("denied"),
                    "{reason}"
                );
            }
            Err(other) => return Err(format!("error inesperado: {other:?}").into()),
        }
        Ok(())
    }

    /// #133: `clear` no puede avanzar mientras otro proceso tiene el
    /// candado de archivo, aunque el `Mutex` local esté libre.
    #[test]
    fn clear_espera_al_candado_de_otro_proceso() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let store = FileStore::from_data_dir(dir.path());
        let candado = adquirir_candado(store.path())?;
        let (termino, espera) = mpsc::channel();

        let hilo = thread::spawn(move || {
            let resultado = store.clear();
            let _ = termino.send(());
            resultado
        });
        assert!(
            espera.recv_timeout(Duration::from_millis(250)).is_err(),
            "clear no esperó el candado de archivo"
        );
        drop(candado);
        espera.recv_timeout(Duration::from_secs(5))?;
        match hilo.join() {
            Ok(resultado) => resultado?,
            Err(_) => return Err("el hilo de clear falló".into()),
        }
        Ok(())
    }

    #[test]
    fn tabla_simetria_respaldo_bloqueado() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let ruta = dir.path().join("session.json");
        let casos = [
            (
                "llavero manda",
                Llavero::Valido("llavero"),
                Some("respaldo"),
                Some("respaldo"),
                Some("llavero"),
                false,
            ),
            (
                "sin entrada",
                Llavero::SinEntrada,
                Some("respaldo"),
                Some("respaldo"),
                Some("respaldo"),
                true,
            ),
            (
                "sin entrada marca vieja",
                Llavero::SinEntrada,
                Some("otro"),
                Some("respaldo"),
                None,
                true,
            ),
            (
                "sin entrada sin marca",
                Llavero::SinEntrada,
                None,
                Some("respaldo"),
                None,
                true,
            ),
            (
                "sin entrada sin archivo",
                Llavero::SinEntrada,
                None,
                None,
                None,
                true,
            ),
            (
                "sin llavero",
                Llavero::Ausente("sin bus".to_owned()),
                Some("respaldo"),
                Some("respaldo"),
                Some("respaldo"),
                true,
            ),
            (
                "sin llavero marca vieja",
                Llavero::Ausente("sin bus".to_owned()),
                Some("otro"),
                Some("respaldo"),
                None,
                true,
            ),
            (
                "sin llavero sin archivo",
                Llavero::Ausente("sin bus".to_owned()),
                None,
                None,
                None,
                true,
            ),
            (
                "bloqueado marcado",
                Llavero::Fallo("object locked".to_owned()),
                Some("respaldo"),
                Some("respaldo"),
                Some("respaldo"),
                true,
            ),
            (
                "bloqueado sin marca",
                Llavero::Fallo("object locked".to_owned()),
                None,
                Some("respaldo"),
                None,
                true,
            ),
            (
                "bloqueado marca vieja",
                Llavero::Fallo("object locked".to_owned()),
                Some("otro"),
                Some("respaldo"),
                None,
                true,
            ),
            (
                "bloqueado marcado sin archivo",
                Llavero::Fallo("object locked".to_owned()),
                Some("respaldo"),
                None,
                None,
                true,
            ),
        ];
        for (nombre, llavero, marca, respaldo, esperado, espera_lectura) in casos {
            match marca {
                Some(valor) => marcar_respaldo(&ruta, &serde_json::to_vec(valor)?)?,
                None => {
                    let _ = borrar_marca(&ruta);
                }
            }
            let leyo = Cell::new(false);
            let obtenido = cargar_con_llavero_marcado(
                llavero,
                || {
                    leyo.set(true);
                    respaldo.ok_or(Error::NoSession)
                },
                &ruta,
            );
            let texto = match obtenido {
                Ok(valor) => Some(valor),
                Err(Error::Unavailable(_)) | Err(Error::NoSession) => None,
                Err(error) => return Err(format!("{nombre}: {error:?}").into()),
            };
            assert_eq!(texto, esperado, "{nombre}");
            assert_eq!(leyo.get(), espera_lectura, "{nombre}: lectura del respaldo");
        }
        write_private(&ruta_marca(&ruta), &[])?;
        assert!(
            !marca_vigente(&ruta, &serde_json::to_vec("respaldo")?),
            "una marca vacía no puede validar un respaldo"
        );
        Ok(())
    }

    #[test]
    fn tabla_guardar_marca_solo_el_respaldo_vivo() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let ruta = dir.path().join("session.json");
        let datos = b"datos-de-la-sesion".to_vec();
        let casos = [
            ("llavero escribe", true, true, false),
            ("llavero cae al archivo", false, true, true),
            ("ninguno escribe", false, false, false),
        ];
        for (nombre, llavero_escribe, respaldo_escribe, espera_marca) in casos {
            let _ = borrar_marca(&ruta);
            let guardado = guardar_con_llavero(
                || {
                    if llavero_escribe {
                        Ok(())
                    } else {
                        Err("bloqueado".to_owned())
                    }
                },
                || -> Result<Vec<u8>, Error> {
                    if respaldo_escribe {
                        guardar_respaldo_marcado(&ruta, &datos)?;
                        Ok(datos.clone())
                    } else {
                        Err(Error::Io(io::Error::other("sin permiso")))
                    }
                },
                || Ok(()),
                &ruta,
                "la sesión",
            );
            assert_eq!(guardado.is_ok(), respaldo_escribe, "{nombre}");
            assert_eq!(marca_vigente(&ruta, &datos), espera_marca, "{nombre}");
        }
        Ok(())
    }
}
