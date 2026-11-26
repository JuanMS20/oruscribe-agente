//! Escritura de archivos: temporal en el mismo directorio, modo conservado,
//! rename y registro en el diario.

use std::fs::{File, Metadata, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use orus_agent::CancelToken;
use serde::Deserialize;
use serde_json::Value;

use super::lectura::{leer_args, texto_o_vacio};
use super::{Error, Set};

/// 1 MiB por escritura.
const MAX_CONTENIDO: usize = 1 << 20;
/// 16 MiB de respaldo previo.
const MAX_RESPALDO: u64 = 16 << 20;

/// Cómo se aplica el modo al temporal. Se pasa por parámetro para que las
/// pruebas simulen un chmod que falla sin tocar un global, como
type Chmod = fn(&File, u32) -> io::Result<()>;

/// Cómo se sincroniza el temporal antes del rename. Misma costura que
/// `Chmod`: las pruebas simulan un sync que falla sin tocar un global.
type Sincronizar = fn(&File) -> io::Result<()>;

#[derive(Default, Deserialize)]
struct ArgsEscritura {
    #[serde(default, deserialize_with = "texto_o_vacio")]
    path: String,
    #[serde(default, deserialize_with = "texto_o_vacio")]
    content: String,
}

/// Los datos de una escritura: ruta real, contenido nuevo, respaldo previo,
/// si existía y el modo que se aplica al temporal.
pub(super) struct Escritura<'a> {
    pub real: &'a Path,
    pub datos: &'a [u8],
    pub antes: &'a [u8],
    pub existia: bool,
    pub modo: u32,
}

pub(super) fn write_file(set: &Set, args: Value, cancel: &CancelToken) -> Result<String, Error> {
    write_file_con(
        set,
        args,
        cancel,
        chmod_temporal,
        sincronizar_temporal,
        |_| {},
    )
}

/// La misma escritura con el chmod, el sync y la costura de carrera inyectables.
fn write_file_con(
    set: &Set,
    args: Value,
    cancel: &CancelToken,
    chmod: Chmod,
    sync: Sincronizar,
    tras_validar: impl Fn(&Path),
) -> Result<String, Error> {
    let args: ArgsEscritura = leer_args(args)?;
    if args.content.len() > MAX_CONTENIDO {
        return Err(Error::ContenidoGrande((MAX_CONTENIDO >> 20) as u64));
    }
    let real = set.resolve(&args.path).map_err(|error| match error {
        Error::SinResolver(causa) if causa.kind() == std::io::ErrorKind::NotFound => {
            Error::CarpetaDestinoNoExiste
        }
        otro => otro,
    })?;
    let dir = real.parent().ok_or(Error::CarpetaDestinoNoExiste)?;
    set.resolve(&dir.to_string_lossy())?;
    match std::fs::metadata(dir) {
        Ok(info) if info.is_dir() => {}
        _ => return Err(Error::CarpetaDestinoNoExiste),
    }
    let mut modo = 0o600;
    let mut existia = false;
    if let Ok(actual) = std::fs::metadata(&real) {
        if actual.is_dir() {
            return Err(Error::EsCarpeta);
        }
        modo = modo_de(&actual);
        existia = true;
    }
    let antes = leer_antes(&real)?.unwrap_or_default();
    guardar_con(
        set,
        cancel,
        Escritura {
            real: &real,
            datos: args.content.as_bytes(),
            antes: &antes,
            existia,
            modo,
        },
        chmod,
        sync,
        tras_validar,
    )?;
    Ok(format!(
        "escrito {} ({} bytes)",
        real.display(),
        args.content.len()
    ))
}

/// Un archivo inexistente no se respalda; una carpeta o uno de más de 16 MiB se rechazan.
pub(super) fn leer_antes(real: &Path) -> Result<Option<Vec<u8>>, Error> {
    let info = match std::fs::metadata(real) {
        Ok(info) => info,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if info.is_dir() {
        return Err(Error::EsCarpeta);
    }
    if info.len() > MAX_RESPALDO {
        return Err(Error::RespaldoGrande(MAX_RESPALDO >> 20));
    }
    Ok(Some(std::fs::read(real)?))
}

/// `guardar`: respalda en el diario si hay, escribe
/// en un temporal del mismo directorio y renombra. El diario se registra
/// después del rename.
pub(super) fn guardar(
    set: &Set,
    cancel: &CancelToken,
    escritura: Escritura<'_>,
) -> Result<(), Error> {
    guardar_con(
        set,
        cancel,
        escritura,
        chmod_temporal,
        sincronizar_temporal,
        |_| {},
    )
}

fn guardar_con(
    set: &Set,
    cancel: &CancelToken,
    escritura: Escritura<'_>,
    chmod: Chmod,
    sync: Sincronizar,
    tras_validar: impl Fn(&Path),
) -> Result<(), Error> {
    let Escritura {
        real,
        datos,
        antes,
        existia,
        modo,
    } = escritura;
    if cancel.esta_cancelado() {
        return Err(Error::Cancelado);
    }
    let dir = real.parent().ok_or(Error::CarpetaDestinoNoExiste)?;
    let diario = set.journal.as_ref();
    tras_validar(real);
    confirmar_padre(dir)?;
    let actual = leer_antes(real)?;
    if actual.is_some() != existia || actual.as_deref().unwrap_or_default() != antes {
        return Err(Error::ArchivoCambio);
    }
    if std::fs::symlink_metadata(real).is_ok_and(|info| info.file_type().is_symlink()) {
        return Err(Error::RutaCambio);
    }
    let (temporal, mut archivo) = crear_temporal(dir)?;
    let pendiente = match diario {
        Some(journal) => match journal.prepare(real, &actual.unwrap_or_default(), existia, datos) {
            Ok(id) => Some(id),
            Err(error) => {
                let _ = std::fs::remove_file(&temporal);
                return Err(Error::RegistroDiario(error));
            }
        },
        None => None,
    };
    let volcado = copiar_acl(real, &archivo)
        .and_then(|()| chmod(&archivo, modo))
        .and_then(|()| archivo.write_all(datos))
        .and_then(|()| sync(&archivo));
    drop(archivo);
    if let Err(error) = volcado {
        let _ = std::fs::remove_file(&temporal);
        if let (Some(journal), Some(id)) = (diario, pendiente)
            && let Err(error) = journal.descartar(&id)
        {
            return Err(Error::RegistroDiario(error));
        }
        return Err(error.into());
    }
    confirmar_padre(dir)?;
    if let Err(error) = std::fs::rename(&temporal, real) {
        let _ = std::fs::remove_file(&temporal);
        if let (Some(journal), Some(id)) = (diario, pendiente)
            && let Err(error) = journal.descartar(&id)
        {
            return Err(Error::RegistroDiario(error));
        }
        return Err(error.into());
    }
    if let Err(error) = confirmar_destino(real, dir) {
        let _ = std::fs::remove_file(real);
        if let (Some(journal), Some(id)) = (diario, pendiente)
            && let Err(error) = journal.descartar(&id)
        {
            return Err(Error::RegistroDiario(error));
        }
        return Err(error);
    }
    if let (Some(journal), Some(id)) = (diario, pendiente)
        && let Err(error) = journal.commit(&id)
    {
        return Err(Error::RegistroDiario(error));
    }
    Ok(())
}

fn confirmar_padre(dir: &Path) -> Result<(), Error> {
    if matches!(super::canon::canon(dir), Ok(real) if real.as_path() == dir) {
        Ok(())
    } else {
        Err(Error::RutaCambio)
    }
}

fn confirmar_destino(real: &Path, dir: &Path) -> Result<(), Error> {
    confirmar_padre(dir)?;
    if matches!(super::canon::canon(real), Ok(canonico) if canonico.as_path() == real) {
        Ok(())
    } else {
        Err(Error::RutaCambio)
    }
}

#[cfg(not(windows))]
fn copiar_acl(_original: &Path, _temporal: &File) -> io::Result<()> {
    Ok(())
}

#[cfg(windows)]
fn copiar_acl(original: &Path, temporal: &File) -> io::Result<()> {
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::Foundation::{ERROR_SUCCESS, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        GetSecurityInfo, SE_FILE_OBJECT, SetSecurityInfo,
    };
    use windows_sys::Win32::Security::{ACL, DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR};

    let archivo = match File::open(original) {
        Ok(archivo) => archivo,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    let leido = unsafe {
        GetSecurityInfo(
            archivo.as_raw_handle(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut dacl,
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    let copiado = if leido == ERROR_SUCCESS {
        let puesto = unsafe {
            SetSecurityInfo(
                temporal.as_raw_handle(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                dacl,
                std::ptr::null_mut(),
            )
        };
        if puesto == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(puesto as i32))
        }
    } else {
        Err(io::Error::from_raw_os_error(leido as i32))
    };
    if !descriptor.is_null() {
        unsafe {
            LocalFree(descriptor);
        }
    }
    copiado
}

/// El sync del temporal antes del rename, como `volcar_temporal` del diario:
/// sin fsync un corte de energía puede dejar el temporal vacío y el rename
/// lo pondría encima del archivo bueno.
fn sincronizar_temporal(archivo: &File) -> io::Result<()> {
    archivo.sync_all()
}

/// `os.CreateTemp(dir, ".oruscribe-*.tmp")`: el contador más el id del proceso
/// bastan para no pisar un temporal de otra corrida; `create_new` lo garantiza.
static SECUENCIA: AtomicU64 = AtomicU64::new(0);

fn crear_temporal(dir: &Path) -> Result<(PathBuf, File), Error> {
    for _ in 0..100 {
        let n = SECUENCIA.fetch_add(1, Ordering::Relaxed);
        let ruta = dir.join(format!(".oruscribe-{}-{n}.tmp", std::process::id()));
        let mut opciones = OpenOptions::new();
        opciones.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opciones.mode(0o600);
            opciones.custom_flags(libc::O_NOFOLLOW);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            opciones.custom_flags(
                windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT,
            );
            opciones.access_mode(
                windows_sys::Win32::Foundation::GENERIC_WRITE
                    | windows_sys::Win32::Storage::FileSystem::WRITE_DAC
                    | windows_sys::Win32::Storage::FileSystem::READ_CONTROL,
            );
        }
        match opciones.open(&ruta) {
            Ok(archivo) => return Ok((ruta, archivo)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no se pudo crear un temporal único",
    )
    .into())
}

/// El chmod aplicado al temporal. En Windows solo importa el bit de escritura.
#[cfg(unix)]
pub(super) fn chmod_temporal(archivo: &File, modo: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    archivo.set_permissions(std::fs::Permissions::from_mode(modo))
}

#[cfg(windows)]
pub(super) fn chmod_temporal(archivo: &File, modo: u32) -> io::Result<()> {
    let mut permisos = archivo.metadata()?.permissions();
    permisos.set_readonly(modo & 0o200 == 0);
    archivo.set_permissions(permisos)
}

#[cfg(not(any(unix, windows)))]
pub(super) fn chmod_temporal(_archivo: &File, _modo: u32) -> io::Result<()> {
    Ok(())
}

/// `Mode.Perm` del contrato: los bits de permiso en Unix, 0666/0444 en Windows.
#[cfg(unix)]
pub(super) fn modo_de(info: &Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    info.permissions().mode() & 0o777
}

#[cfg(windows)]
pub(super) fn modo_de(info: &Metadata) -> u32 {
    if info.permissions().readonly() {
        0o444
    } else {
        0o666
    }
}

#[cfg(not(any(unix, windows)))]
pub(super) fn modo_de(_info: &Metadata) -> u32 {
    0o600
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::Arc;

    use orus_agent::Risk;
    use serde_json::json;

    use super::*;
    use crate::journal::Journal;
    use crate::localtools::Config;

    fn conjunto(carpeta: &Path) -> Arc<Set> {
        Arc::new(
            Set::new(Config {
                folders: vec![carpeta.to_string_lossy().into_owned()],
                ..Config::default()
            })
            .expect("Set con una carpeta temporal"),
        )
    }

    async fn correr(set: &Arc<Set>, args: Value) -> Result<String, String> {
        let herramientas = set.tools();
        let herramienta = herramientas
            .iter()
            .find(|herramienta| herramienta.spec().name == "local_write_file")
            .expect("local_write_file registrada");
        assert_eq!(herramienta.risk(), Some(Risk::Write));
        herramienta
            .run(args)
            .await
            .map_err(|error| error.to_string())
    }

    fn temporales(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .expect("leer la carpeta")
            .filter_map(Result::ok)
            .filter_map(|entrada| {
                let nombre = entrada.file_name();
                let nombre = nombre.to_string_lossy();
                (nombre.starts_with(".oruscribe-") && nombre.ends_with(".tmp"))
                    .then(|| nombre.into_owned())
            })
            .collect()
    }

    fn chmod_roto(_archivo: &File, _modo: u32) -> io::Result<()> {
        Err(io::Error::other("chmod no soportado"))
    }

    fn sync_roto(_archivo: &File) -> io::Result<()> {
        Err(io::Error::other("sync no soportado"))
    }

    #[tokio::test]
    async fn write_file_writes_inside_and_rejects_outside() {
        let raiz = tempfile::tempdir().expect("raiz");
        let fuera = tempfile::tempdir().expect("fuera");
        let set = conjunto(raiz.path());
        let nuevo = raiz.path().join("nuevo.txt");

        let salida = correr(
            &set,
            json!({"path": nuevo.to_string_lossy(), "content": "hola"}),
        )
        .await
        .expect("escribir dentro");
        // Misma canonización que la herramienta: el TEMP del runner puede
        // llegar en su forma corta 8.3.
        let real = super::super::canon::canon(&nuevo).expect("ruta real");
        assert_eq!(salida, format!("escrito {} (4 bytes)", real.display()));
        assert_eq!(
            std::fs::read_to_string(&nuevo).expect("leer"),
            "hola",
            "el contenido quedó distinto"
        );

        let afuera = fuera.path().join("x.txt");
        let error = correr(
            &set,
            json!({"path": afuera.to_string_lossy(), "content": "x"}),
        )
        .await
        .expect_err("escribir afuera falla");
        assert_eq!(error, "la ruta queda fuera de las carpetas permitidas");

        let carpeta = raiz.path().join("carpeta");
        std::fs::create_dir(&carpeta).expect("carpeta");
        let error = correr(
            &set,
            json!({"path": carpeta.to_string_lossy(), "content": "x"}),
        )
        .await
        .expect_err("una carpeta no se escribe");
        assert_eq!(error, "esa ruta es una carpeta");

        let error = correr(&set, json!({"path": null, "content": "x"}))
            .await
            .expect_err("path null es el valor cero");
        assert_eq!(error, "falta la ruta");

        let error = correr(&set, json!({"path": nuevo.to_string_lossy(), "content": 1}))
            .await
            .expect_err("content que no es texto");
        assert_eq!(error, "argumentos inválidos");

        let grande = "a".repeat(MAX_CONTENIDO + 1);
        let error = correr(
            &set,
            json!({"path": nuevo.to_string_lossy(), "content": grande}),
        )
        .await
        .expect_err("el contenido pasa el tope");
        assert_eq!(error, "el contenido pasa de 1 MiB");
        assert_eq!(
            std::fs::read_to_string(&nuevo).expect("leer"),
            "hola",
            "un contenido rechazado no debe tocar el archivo"
        );
    }

    #[tokio::test]
    async fn write_file_rejects_missing_parent() {
        let raiz = tempfile::tempdir().expect("raiz");
        let set = conjunto(raiz.path());
        let ruta = raiz.path().join("sub").join("x.txt");

        let error = correr(
            &set,
            json!({"path": ruta.to_string_lossy(), "content": "x"}),
        )
        .await
        .expect_err("la carpeta no existe");
        assert_eq!(error, "la carpeta donde va el archivo no existe");
        assert!(!ruta.exists(), "no debe crear carpetas");

        // El texto propio de la escritura aparece cuando el padre resuelve pero
        // no es una carpeta.
        let archivo = raiz.path().join("nota.txt");
        std::fs::write(&archivo, "x").expect("archivo");
        let ruta = archivo.join("x.txt");
        let error = correr(
            &set,
            json!({"path": ruta.to_string_lossy(), "content": "x"}),
        )
        .await
        .expect_err("el padre es un archivo");
        assert_eq!(error, "la carpeta donde va el archivo no existe");
    }

    #[tokio::test]
    async fn failed_write_leaves_original_and_no_temp() {
        let raiz = tempfile::tempdir().expect("raiz");
        let ruta = raiz.path().join("nota.txt");
        std::fs::write(&ruta, "contenido original").expect("original");
        let set = conjunto(raiz.path());

        let error = write_file_con(
            &set,
            json!({"path": ruta.to_string_lossy(), "content": "contenido nuevo"}),
            &CancelToken::new(),
            chmod_roto,
            sincronizar_temporal,
            |_| {},
        )
        .expect_err("el chmod falló");
        assert!(error.to_string().contains("chmod no soportado"), "{error}");
        assert_eq!(
            std::fs::read_to_string(&ruta).expect("leer"),
            "contenido original",
            "el original no debe cambiar"
        );
        assert!(
            temporales(raiz.path()).is_empty(),
            "quedaron temporales: {:?}",
            temporales(raiz.path())
        );
    }

    /// Issue #94: el temporal se sincroniza antes del rename. Un sync que
    /// falla deja el original intacto y sin temporales, como el chmod roto:
    /// sin fsync un corte de energía puede poner un temporal vacío encima
    /// del archivo bueno.
    #[tokio::test]
    async fn write_sincroniza_el_temporal_antes_del_rename() {
        let raiz = tempfile::tempdir().expect("raiz");
        let ruta = raiz.path().join("nota.txt");
        std::fs::write(&ruta, "contenido original").expect("original");
        let set = conjunto(raiz.path());
        let error = write_file_con(
            &set,
            json!({"path": ruta.to_string_lossy(), "content": "contenido nuevo"}),
            &CancelToken::new(),
            chmod_temporal,
            sync_roto,
            |_| {},
        )
        .expect_err("el sync falló");
        assert!(error.to_string().contains("sync no soportado"), "{error}");
        assert_eq!(
            std::fs::read_to_string(&ruta).expect("leer"),
            "contenido original",
            "el original no debe cambiar"
        );
        assert!(
            temporales(raiz.path()).is_empty(),
            "quedaron temporales: {:?}",
            temporales(raiz.path())
        );
    }

    #[tokio::test]
    async fn escritura_verifica_preimage_destino_y_diario() {
        let raiz = tempfile::tempdir().expect("raiz");
        let fuera = tempfile::tempdir().expect("fuera");
        let carpeta_diario = tempfile::tempdir().expect("diario");
        let journal = Journal::open(carpeta_diario.path()).expect("abrir el diario");
        let set = Arc::new(
            Set::new(Config {
                folders: vec![raiz.path().to_string_lossy().into_owned()],
                journal: Some(journal.clone()),
                ..Config::default()
            })
            .expect("Set con diario"),
        );

        let ruta = raiz.path().join("nota.txt");
        std::fs::write(&ruta, "original").expect("original");
        let cambio: fn(&Path) =
            |real| std::fs::write(real, "cambio de la persona").expect("cambio");
        let error = write_file_con(
            &set,
            json!({"path": ruta.to_string_lossy(), "content": "del agente"}),
            &CancelToken::new(),
            chmod_temporal,
            sincronizar_temporal,
            cambio,
        )
        .expect_err("un cambio concurrente no se pisa");
        assert!(matches!(error, Error::ArchivoCambio), "{error:?}");
        assert_eq!(
            std::fs::read_to_string(&ruta).expect("leer"),
            "cambio de la persona"
        );
        assert!(
            journal.list().expect("diario").is_empty(),
            "no se respalda una escritura fallida"
        );

        std::fs::create_dir(raiz.path().join("sub")).expect("sub");
        #[cfg(windows)]
        let creada = {
            let prueba = raiz.path().join("prueba-junction");
            let ok = std::process::Command::new("cmd")
                .args(["/c", "mklink", "/J"])
                .arg(&prueba)
                .arg(fuera.path())
                .output()
                .is_ok_and(|salida| salida.status.success());
            if ok {
                let _ = std::process::Command::new("cmd")
                    .args(["/c", "rmdir"])
                    .arg(&prueba)
                    .status();
            }
            ok
        };
        #[cfg(not(windows))]
        let creada = {
            let prueba = raiz.path().join("prueba-enlace");
            let ok = std::os::unix::fs::symlink(fuera.path(), &prueba).is_ok();
            if ok {
                let _ = std::fs::remove_file(&prueba);
            }
            ok
        };
        if creada {
            let destino = raiz.path().join("sub").join("nuevo.txt");
            let swap = |real: &Path| {
                let padreal = real.parent().expect("padre");
                let temporal = padreal.with_file_name("sub-real");
                std::fs::rename(padreal, &temporal).expect("mover la carpeta validada");
                #[cfg(windows)]
                std::process::Command::new("cmd")
                    .args(["/c", "mklink", "/J"])
                    .arg(padreal)
                    .arg(fuera.path())
                    .status()
                    .expect("crear junction");
                #[cfg(not(windows))]
                std::os::unix::fs::symlink(fuera.path(), padreal).expect("crear enlace");
            };
            let error = write_file_con(
                &set,
                json!({"path": destino.to_string_lossy(), "content": "x"}),
                &CancelToken::new(),
                chmod_temporal,
                sincronizar_temporal,
                swap,
            )
            .expect_err("el padre cambió después de validarse");
            assert!(matches!(error, Error::RutaCambio), "{error:?}");
            assert!(
                !fuera.path().join("nuevo.txt").exists(),
                "la escritura salió de la raíz"
            );
        }

        correr(
            &set,
            json!({"path": ruta.to_string_lossy(), "content": "escrito"}),
        )
        .await
        .expect("escribir con diario");
        let entradas = journal.list().expect("listar el diario");
        assert_eq!(entradas.len(), 1, "{entradas:?}");
        assert!(
            !entradas[0].pending,
            "la entrada quedó pendiente tras el rename"
        );

        let id = journal
            .prepare(&ruta, b"cambio de la persona", true, b"escrito")
            .expect("preparar una entrada huérfana");
        let entradas = journal.list().expect("recuperar al listar");
        assert!(
            entradas.iter().any(|e| e.id == id && !e.pending),
            "un corte tras el rename no se recupera"
        );
        let id = journal
            .prepare(&ruta, b"escrito", true, b"otro contenido")
            .expect("preparar una escritura no aplicada");
        let entradas = journal.list().expect("recuperar al listar");
        assert!(
            !entradas.iter().any(|e| e.id == id),
            "una escritura no aplicada no debe quedar en el diario"
        );

        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;

            use windows_sys::Win32::Foundation::{ERROR_SUCCESS, LocalFree};
            use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_FILE_OBJECT};
            use windows_sys::Win32::Security::{
                ACL, DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
            };

            let acl_de = |ruta: &Path| -> Vec<u8> {
                let archivo = File::open(ruta).expect("abrir para leer la DACL");
                let mut dacl: *mut ACL = std::ptr::null_mut();
                let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
                let estado = unsafe {
                    GetSecurityInfo(
                        archivo.as_raw_handle(),
                        SE_FILE_OBJECT,
                        DACL_SECURITY_INFORMATION,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        &mut dacl,
                        std::ptr::null_mut(),
                        &mut descriptor,
                    )
                };
                if estado != ERROR_SUCCESS {
                    return Vec::new();
                }
                let bytes = if dacl.is_null() {
                    Vec::new()
                } else {
                    unsafe {
                        std::slice::from_raw_parts(dacl.cast::<u8>(), (*dacl).AclSize as usize)
                            .to_vec()
                    }
                };
                if !descriptor.is_null() {
                    unsafe {
                        LocalFree(descriptor);
                    }
                }
                bytes
            };
            let con_acl = raiz.path().join("con-acl.txt");
            std::fs::write(&con_acl, "privado").expect("archivo con DACL");
            let puesto = std::process::Command::new("icacls")
                .arg(&con_acl)
                .args(["/grant", "*S-1-1-0:(R)"])
                .status()
                .expect("icacls");
            if puesto.success() {
                let antes_acl = acl_de(&con_acl);
                assert!(!antes_acl.is_empty(), "icacls no fijó una DACL explícita");
                correr(
                    &set,
                    json!({"path": con_acl.to_string_lossy(), "content": "reescrito"}),
                )
                .await
                .expect("reescribir con DACL");
                assert_eq!(
                    acl_de(&con_acl),
                    antes_acl,
                    "la DACL del archivo cambió al reemplazarlo"
                );
            }
        }
    }

    #[tokio::test]
    async fn write_after_rename_records_journal() {
        let raiz = tempfile::tempdir().expect("raiz");
        let carpeta_diario = tempfile::tempdir().expect("diario");
        let journal = Journal::open(carpeta_diario.path()).expect("abrir el diario");
        let set = Arc::new(
            Set::new(Config {
                folders: vec![raiz.path().to_string_lossy().into_owned()],
                journal: Some(journal.clone()),
                ..Config::default()
            })
            .expect("Set con diario"),
        );

        let nuevo = raiz.path().join("nuevo.txt");
        correr(
            &set,
            json!({"path": nuevo.to_string_lossy(), "content": "nuevo"}),
        )
        .await
        .expect("escribir un archivo nuevo");
        let entradas = journal.list().expect("listar el diario");
        assert_eq!(entradas.len(), 1, "debe quedar una entrada");
        assert!(!entradas[0].existed, "el archivo no existía");
        assert_eq!(entradas[0].size, 5);
        assert_eq!(
            entradas[0].path,
            set.resolve(&nuevo.to_string_lossy()).expect("resolver")
        );
        let (hechas, error) = journal.undo(1, None, false);
        assert_eq!(hechas, 1);
        assert!(error.is_none(), "{error:?}");
        assert!(!nuevo.exists(), "deshacer un archivo nuevo lo borra");

        let nota = raiz.path().join("nota.txt");
        std::fs::write(&nota, "original").expect("original");
        correr(
            &set,
            json!({"path": nota.to_string_lossy(), "content": "reescrito"}),
        )
        .await
        .expect("sobrescribir");
        let entradas = journal.list().expect("listar el diario");
        assert_eq!(entradas.len(), 1, "debe quedar una entrada");
        assert!(entradas[0].existed, "el archivo existía");
        assert_eq!(entradas[0].size, 9);
        let (hechas, error) = journal.undo(1, None, false);
        assert_eq!(hechas, 1);
        assert!(error.is_none(), "{error:?}");
        assert_eq!(
            std::fs::read_to_string(&nota).expect("leer"),
            "original",
            "deshacer restaura el contenido previo"
        );
    }

    #[tokio::test]
    async fn write_over_16mib_refuses_backup() {
        let raiz = tempfile::tempdir().expect("raiz");
        let grande = raiz.path().join("grande.bin");
        let tamano = MAX_RESPALDO as usize + 1;
        std::fs::write(&grande, vec![b'a'; tamano]).expect("archivo grande");
        let set = conjunto(raiz.path());

        let error = correr(
            &set,
            json!({"path": grande.to_string_lossy(), "content": "x"}),
        )
        .await
        .expect_err("no se respalda un archivo de más de 16 MiB");
        assert_eq!(
            error,
            "el archivo pesa más de 16 MiB y no se puede respaldar para deshacer"
        );
        assert_eq!(
            std::fs::metadata(&grande).expect("metadata").len(),
            tamano as u64,
            "el original no debe cambiar"
        );
        assert!(
            temporales(raiz.path()).is_empty(),
            "quedaron temporales: {:?}",
            temporales(raiz.path())
        );
    }
}
