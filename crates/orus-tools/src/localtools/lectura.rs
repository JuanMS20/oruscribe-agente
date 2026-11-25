//! Herramientas de lectura y el registro de las herramientas locales como
//! `agent::Tool`. Port de
//! `internal/localtools/localtools.go:99-127,199-216,315-454`.

use std::collections::HashSet;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use orus_agent::{CancelToken, Risk, Tool};
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};

use super::tool::HerramientaLocal;
use super::{Error, Set};

const MAX_LEER: usize = 1 << 20;
const MAX_LISTA: usize = 500;
const MAX_BUSQUEDA: usize = 50;
const MAX_LINEA_BUSQUEDA: usize = 200;

/// Abre un archivo para leerlo como texto. En Windows `File::open` de una
/// carpeta falla con "acceso denegado"; Go la abre y el llamador la rechaza.
/// Se comprueba antes para dar el mismo texto que Go.
fn abrir_texto(real: &Path) -> Result<File, Error> {
    if let Ok(info) = std::fs::metadata(real) {
        if info.is_dir() {
            return Err(Error::EsCarpeta);
        }
        if !info.is_file() {
            return Err(Error::Binario);
        }
    }
    Ok(File::open(real)?)
}

/// `leerTexto` (`localtools.go:281-313`): 1 MiB, sin NUL y UTF-8 válido, con
/// los mismos textos que `read_file`.
pub(super) fn leer_texto(real: &Path) -> Result<String, Error> {
    leer_texto_abierto(abrir_texto(real)?)
}

/// Tramos comunes de `readFile` y `leerTexto` (`localtools.go:315-351`).
fn leer_texto_abierto(archivo: File) -> Result<String, Error> {
    let info = archivo.metadata()?;
    if info.is_dir() {
        return Err(Error::EsCarpeta);
    }
    if info.len() > MAX_LEER as u64 {
        return Err(Error::ArchivoGrande((MAX_LEER >> 20) as u64));
    }
    let mut datos = Vec::new();
    archivo.take(MAX_LEER as u64 + 1).read_to_end(&mut datos)?;
    if datos.len() > MAX_LEER {
        return Err(Error::ArchivoGrande((MAX_LEER >> 20) as u64));
    }
    if datos.contains(&0) {
        return Err(Error::Binario);
    }
    String::from_utf8(datos).map_err(|_| Error::NoUtf8)
}

/// Arma las herramientas locales, como `Set.Tools` en Go
/// (`localtools.go:101-110`). Go devuelve herramientas que apuntan al `Set`;
/// acá comparten su dueño con `Arc`.
impl Set {
    pub fn tools(self: &Arc<Self>) -> Vec<Arc<dyn Tool>> {
        vec![
            Arc::new(HerramientaLocal::nueva(
                Arc::clone(self),
                "local_list_dir",
                Risk::Read,
                "Lista una carpeta permitida.",
                json!({
                    "type": "object",
                    "properties": {"path": {"type": "string"}},
                    "required": ["path"]
                }),
                list_dir_sin_cancelar,
            )),
            Arc::new(HerramientaLocal::nueva(
                Arc::clone(self),
                "local_read_file",
                Risk::Read,
                "Lee un archivo de texto de una carpeta permitida, hasta 1 MiB.",
                esquema_lectura(),
                read_file_sin_cancelar,
            )),
            Arc::new(HerramientaLocal::nueva(
                Arc::clone(self),
                "local_search_text",
                Risk::Read,
                "Busca un texto en los archivos de una carpeta permitida.",
                json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string"},
                        "text": {"type": "string"},
                        "limit": {"type": "integer"}
                    },
                    "required": ["path", "text"]
                }),
                search_text_con_cancelar,
            )),
            Arc::new(HerramientaLocal::nueva(
                Arc::clone(self),
                "local_write_file",
                Risk::Write,
                "Crea o reemplaza un archivo de una carpeta permitida.",
                esquema_escritura(),
                super::escritura::write_file,
            )),
            Arc::new(HerramientaLocal::nueva(
                Arc::clone(self),
                "local_edit_file",
                Risk::Write,
                "Reemplaza una porción exacta de un archivo (old_string → new_string). Falla sin escribir si no la encuentra; con varias coincidencias exige replace_all.",
                esquema_edicion(),
                super::edicion::edit_file,
            )),
            Arc::new(HerramientaLocal::nueva(
                Arc::clone(self),
                "local_run_command",
                Risk::Exec,
                "Ejecuta un programa permitido, sin shell, en una carpeta permitida.",
                json!({
                    "type": "object",
                    "properties": {
                        "program": {"type": "string"},
                        "args": {"type": "array", "items": {"type": "string"}},
                        "cwd": {"type": "string"}
                    },
                    "required": ["program"]
                }),
                super::comando::run_command,
            )),
        ]
    }
}

/// Esquemas compartidos entre el registro del CLI y `tools_pi`: una sola
/// fuente para que ambos no se separen (contrato T1a).
pub(super) fn esquema_lectura() -> Value {
    json!({
        "type": "object",
        "properties": {"path": {"type": "string"}},
        "required": ["path"]
    })
}

pub(super) fn esquema_escritura() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": {"type": "string"},
            "content": {"type": "string"}
        },
        "required": ["path", "content"]
    })
}

pub(super) fn esquema_edicion() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": {"type": "string"},
            "old_string": {"type": "string"},
            "new_string": {"type": "string"},
            "replace_all": {"type": "boolean"}
        },
        "required": ["path", "old_string", "new_string"]
    })
}

fn list_dir_sin_cancelar(set: &Set, args: Value, cancel: &CancelToken) -> Result<String, Error> {
    set.list_dir(args, cancel)
}

pub(super) fn read_file_sin_cancelar(
    set: &Set,
    args: Value,
    _cancel: &CancelToken,
) -> Result<String, Error> {
    set.read_file(args)
}

fn search_text_con_cancelar(set: &Set, args: Value, cancel: &CancelToken) -> Result<String, Error> {
    set.search_text(args, cancel)
}

/// Los argumentos llegan ya decodificados como JSON. Un `null` es el cero de
/// Go; un tipo equivocado es "argumentos inválidos".
pub(super) fn leer_args<T: serde::de::DeserializeOwned + Default>(
    valor: Value,
) -> Result<T, Error> {
    if valor.is_null() {
        return Ok(T::default());
    }
    serde_json::from_value(valor).map_err(|_| Error::ArgumentosInvalidos)
}

/// `encoding/json` deja el cero cuando el campo vale `null`; serde no.
pub(super) fn texto_o_vacio<'de, D: Deserializer<'de>>(
    deserializador: D,
) -> Result<String, D::Error> {
    Ok(Option::<String>::deserialize(deserializador)?.unwrap_or_default())
}

fn entero_o_cero<'de, D: Deserializer<'de>>(deserializador: D) -> Result<i64, D::Error> {
    Ok(Option::<i64>::deserialize(deserializador)?.unwrap_or_default())
}

#[derive(Default, Deserialize)]
struct ArgsRuta {
    #[serde(default, deserialize_with = "texto_o_vacio")]
    path: String,
}

#[derive(Default, Deserialize)]
struct ArgsBusqueda {
    #[serde(default, deserialize_with = "texto_o_vacio")]
    path: String,
    #[serde(default, deserialize_with = "texto_o_vacio")]
    text: String,
    #[serde(default, deserialize_with = "entero_o_cero")]
    limit: i64,
}

impl Set {
    /// `abrir` (`localtools.go:199-216`): valida, abre y vuelve a validar. Si
    /// la ruta cambió entre la comprobación y la apertura, se rechaza. Es una
    /// lectura: acepta también las carpetas de solo lectura.
    fn abrir(&self, ruta: &str) -> Result<File, Error> {
        let real = self.resolve_lectura(ruta)?;
        let archivo = abrir_texto(&real)?;
        let otra = self.resolve_lectura(ruta).map_err(|_| Error::RutaCambio)?;
        if otra != real {
            return Err(Error::RutaCambio);
        }
        Ok(archivo)
    }

    /// `readFile` (`localtools.go:315-351`): hasta 1 MiB, sin NUL y UTF-8
    /// válido.
    fn read_file(&self, args: Value) -> Result<String, Error> {
        let args: ArgsRuta = leer_args(args)?;
        let archivo = self.abrir(&args.path)?;
        leer_texto_abierto(archivo)
    }

    /// `listDir` (`localtools.go:353-385`): carpetas con `/`, archivos con su
    /// tamaño, y corte a 500 entradas.
    fn list_dir(&self, args: Value, cancel: &CancelToken) -> Result<String, Error> {
        let args: ArgsRuta = leer_args(args)?;
        let real = self.resolve_lectura(&args.path)?;
        let mut mejores = std::collections::BinaryHeap::with_capacity(MAX_LISTA);
        let mut cortado = false;
        for entrada in std::fs::read_dir(&real)? {
            if cancel.esta_cancelado() {
                return Err(Error::Cancelado);
            }
            let entrada = entrada?;
            let candidato = (entrada.file_name(), entrada.path());
            if mejores.len() < MAX_LISTA {
                mejores.push(candidato);
                continue;
            }
            cortado = true;
            if mejores.peek().is_some_and(|peor| candidato.0 < peor.0) {
                mejores.pop();
                mejores.push(candidato);
            }
        }
        let mut entradas = mejores.into_vec();
        entradas.sort_by(|a, b| a.0.cmp(&b.0));
        let mut salida = String::new();
        for (nombre, ruta) in &entradas {
            let nombre = nombre.to_string_lossy();
            if std::fs::symlink_metadata(ruta).is_ok_and(|tipo| tipo.is_dir()) {
                salida.push_str(&format!("{nombre}/\n"));
                continue;
            }
            match std::fs::metadata(ruta) {
                Ok(info) => salida.push_str(&format!("{nombre}  {} bytes\n", info.len())),
                Err(_) => salida.push_str(&format!("{nombre}\n")),
            }
        }
        if cortado {
            salida.push_str(&format!(
                "[se muestran las primeras {MAX_LISTA} entradas]\n"
            ));
        }
        Ok(salida)
    }

    /// `searchText` (`localtools.go:387-454`): recorre sin seguir enlaces, salta
    /// fuera, binarios y archivos de más de 1 MiB, y corta en seco al llegar al
    /// límite. Un error de lectura se propaga como error: «sin coincidencias»
    /// solo significa que no hubo coincidencias (#79).
    fn search_text(&self, args: Value, cancel: &CancelToken) -> Result<String, Error> {
        let args: ArgsBusqueda = leer_args(args)?;
        if args.text.trim().is_empty() {
            return Err(Error::FaltaTexto);
        }
        let limite = if args.limit <= 0 || args.limit > MAX_BUSQUEDA as i64 {
            MAX_BUSQUEDA
        } else {
            args.limit as usize
        };
        let real = self.resolve_lectura(&args.path)?;
        let mut busqueda = Busqueda {
            aguja: args.text.to_lowercase(),
            limite,
            cancel,
            visitados: HashSet::new(),
            salida: String::new(),
            encontrados: 0,
        };
        self.buscar_en(&real, &mut busqueda)?;
        if busqueda.encontrados == 0 {
            return Ok("sin coincidencias".to_owned());
        }
        Ok(busqueda.salida)
    }

    fn buscar_en(&self, ruta: &Path, busqueda: &mut Busqueda<'_>) -> Result<(), Error> {
        if busqueda.cancel.esta_cancelado() {
            return Err(Error::Cancelado);
        }
        // Un error de verdad se propaga: «sin coincidencias» solo puede
        // significar que no hubo coincidencias, no que no se pudo buscar
        // (#79). Lo que desapareció en medio del recorrido se salta.
        let tipo = match std::fs::symlink_metadata(ruta) {
            Ok(info) => info.file_type(),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
                ) =>
            {
                return Ok(());
            }
            Err(source) => {
                return Err(Error::BusquedaFallida {
                    path: ruta.to_path_buf(),
                    source,
                });
            }
        };
        if !tipo.is_dir() {
            return self.buscar_archivo(ruta, busqueda);
        }
        let Ok(real) = self.resolve_lectura(&ruta.to_string_lossy()) else {
            return Ok(());
        };
        // Un enlace o junction que vuelve a una carpeta ya vista no se recorre.
        if !busqueda.visitados.insert(real) {
            return Ok(());
        }
        let entradas = match std::fs::read_dir(ruta) {
            Ok(entradas) => entradas,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
                ) =>
            {
                return Ok(());
            }
            Err(source) => {
                return Err(Error::BusquedaFallida {
                    path: ruta.to_path_buf(),
                    source,
                });
            }
        };
        for entrada in entradas {
            if busqueda.cancel.esta_cancelado() {
                return Err(Error::Cancelado);
            }
            let entrada = entrada.map_err(|source| Error::BusquedaFallida {
                path: ruta.to_path_buf(),
                source,
            })?;
            let hijo = entrada.path();
            let tipo = entrada
                .file_type()
                .map_err(|source| Error::BusquedaFallida {
                    path: hijo.clone(),
                    source,
                })?;
            if tipo.is_dir() && !tipo.is_symlink() {
                self.buscar_en(&hijo, busqueda)?;
            } else {
                self.buscar_archivo(&hijo, busqueda)?;
            }
            if busqueda.encontrados >= busqueda.limite {
                return Ok(());
            }
        }
        Ok(())
    }

    fn buscar_archivo(&self, ruta: &Path, busqueda: &mut Busqueda<'_>) -> Result<(), Error> {
        // Salir de la raíz por un enlace es contención, no error: se salta.
        let Ok(seguro) = self.resolve_lectura(&ruta.to_string_lossy()) else {
            return Ok(());
        };
        let tipo = match std::fs::symlink_metadata(&seguro) {
            Ok(info) => info.file_type(),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
                ) =>
            {
                return Ok(());
            }
            Err(source) => {
                return Err(Error::BusquedaFallida {
                    path: seguro,
                    source,
                });
            }
        };
        if !tipo.is_file() {
            return Ok(());
        }
        let mut opciones = std::fs::OpenOptions::new();
        opciones.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opciones.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let archivo = match opciones.open(&seguro) {
            Ok(archivo) => archivo,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
                ) =>
            {
                return Ok(());
            }
            Err(source) => {
                return Err(Error::BusquedaFallida {
                    path: seguro,
                    source,
                });
            }
        };
        let info = archivo.metadata()?;
        if !info.is_file() || info.len() > MAX_LEER as u64 {
            return Ok(());
        }
        let mut datos = Vec::new();
        match archivo.take(MAX_LEER as u64 + 1).read_to_end(&mut datos) {
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
                ) =>
            {
                return Ok(());
            }
            Err(source) => {
                return Err(Error::BusquedaFallida {
                    path: seguro,
                    source,
                });
            }
        }
        if datos.len() > MAX_LEER || datos.contains(&0) {
            return Ok(());
        }
        let texto = String::from_utf8_lossy(&datos);
        for (i, linea) in texto.split('\n').enumerate() {
            if !linea.to_lowercase().contains(&busqueda.aguja) {
                continue;
            }
            busqueda.salida.push_str(&format!(
                "{}:{}: {}\n",
                ruta.display(),
                i + 1,
                recortar_linea(linea).trim()
            ));
            busqueda.encontrados += 1;
            if busqueda.encontrados >= busqueda.limite {
                return Ok(());
            }
        }
        Ok(())
    }
}

struct Busqueda<'a> {
    aguja: String,
    limite: usize,
    cancel: &'a CancelToken,
    visitados: HashSet<PathBuf>,
    salida: String,
    encontrados: usize,
}

/// Recorta a 200 runas y agrega "…", como `localtools.go:434-436`.
fn recortar_linea(linea: &str) -> String {
    if linea.chars().count() <= MAX_LINEA_BUSQUEDA {
        return linea.to_owned();
    }
    let mut recortada: String = linea.chars().take(MAX_LINEA_BUSQUEDA).collect();
    recortada.push('…');
    recortada
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn conjunto(carpeta: &Path) -> Arc<Set> {
        Arc::new(
            Set::new(super::super::Config {
                folders: vec![carpeta.to_string_lossy().into_owned()],
                ..super::super::Config::default()
            })
            .expect("Set con una carpeta temporal"),
        )
    }

    async fn correr(set: &Arc<Set>, nombre: &str, args: Value) -> Result<String, String> {
        let herramientas = set.tools();
        let herramienta = herramientas
            .iter()
            .find(|herramienta| herramienta.spec().name == nombre)
            .expect("herramienta registrada");
        herramienta
            .run(args)
            .await
            .map_err(|error| error.to_string())
    }

    #[tokio::test]
    async fn read_file_reads_inside_and_rejects_dotdot() {
        let raiz = tempfile::tempdir().expect("raiz");
        let fuera = tempfile::tempdir().expect("fuera");
        std::fs::write(raiz.path().join("dentro.txt"), "hola").expect("escribir");
        std::fs::write(fuera.path().join("secreto.txt"), "no").expect("escribir");
        let set = conjunto(raiz.path());

        let dentro = raiz.path().join("dentro.txt");
        assert_eq!(
            correr(
                &set,
                "local_read_file",
                json!({"path": dentro.to_string_lossy()})
            )
            .await,
            Ok("hola".to_owned())
        );
        let sin_ruta = correr(&set, "local_read_file", json!({"path": null}))
            .await
            .expect_err("path null es el cero de Go");
        assert_eq!(sin_ruta, "falta la ruta");
        let secreto = fuera.path().join("secreto.txt");
        assert!(
            correr(
                &set,
                "local_read_file",
                json!({"path": secreto.to_string_lossy()})
            )
            .await
            .is_err()
        );
        let escape = raiz
            .path()
            .join("..")
            .join(fuera.path().file_name().expect("nombre"))
            .join("secreto.txt");
        assert!(
            correr(
                &set,
                "local_read_file",
                json!({"path": escape.to_string_lossy()})
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn read_file_rejects_big_binary_non_utf8() {
        let raiz = tempfile::tempdir().expect("raiz");
        let grande = raiz.path().join("grande.txt");
        std::fs::write(&grande, vec![b'a'; MAX_LEER + 10]).expect("grande");
        let binario = raiz.path().join("binario.dat");
        std::fs::write(&binario, b"abc\0def").expect("binario");
        let invalido = raiz.path().join("invalido.txt");
        std::fs::write(&invalido, [0x61, 0x62, 0xff]).expect("invalido");
        let carpeta = raiz.path().join("carpeta");
        std::fs::create_dir(&carpeta).expect("carpeta");
        let set = conjunto(raiz.path());

        for (ruta, fragmento) in [
            (&grande, "más de 1 MiB"),
            (&binario, "binario"),
            (&invalido, "UTF-8"),
            (&carpeta, "es una carpeta"),
        ] {
            let error = correr(
                &set,
                "local_read_file",
                json!({"path": ruta.to_string_lossy()}),
            )
            .await
            .expect_err("debía fallar");
            assert!(error.contains(fragmento), "{error} no dice {fragmento}");
        }
    }

    #[tokio::test]
    async fn list_dir_lists_without_leaving_root() {
        let raiz = tempfile::tempdir().expect("raiz");
        std::fs::write(raiz.path().join("a.txt"), "x").expect("a");
        std::fs::create_dir(raiz.path().join("sub")).expect("sub");
        let set = conjunto(raiz.path());

        let salida = correr(
            &set,
            "local_list_dir",
            json!({"path": raiz.path().to_string_lossy()}),
        )
        .await
        .expect("listar");
        assert!(salida.contains("a.txt  1 bytes"), "{salida}");
        assert!(salida.contains("sub/"), "{salida}");
        let afuera = raiz.path().parent().expect("padre");
        assert!(
            correr(
                &set,
                "local_list_dir",
                json!({"path": afuera.to_string_lossy()})
            )
            .await
            .is_err()
        );

        for i in 0..=MAX_LISTA {
            std::fs::write(raiz.path().join(format!("n{i:04}.txt")), "").expect("entrada");
        }
        let salida = correr(
            &set,
            "local_list_dir",
            json!({"path": raiz.path().to_string_lossy()}),
        )
        .await
        .expect("listar");
        assert!(
            salida.contains("[se muestran las primeras 500 entradas]"),
            "{salida}"
        );
        assert!(!salida.contains("sub/"), "la entrada 502 no debe aparecer");
    }

    #[test]
    fn listado_acotado_y_busqueda_omite_fifos() {
        let raiz = tempfile::tempdir().expect("raiz");
        for i in 0..MAX_LISTA + 2 {
            std::fs::write(raiz.path().join(format!("n{i:04}.txt")), "").expect("entrada");
        }
        let set = conjunto(raiz.path());
        let args = json!({"path": raiz.path().to_string_lossy()});
        let salida = set
            .list_dir(args.clone(), &CancelToken::new())
            .expect("listar");
        assert!(
            salida.contains("[se muestran las primeras 500 entradas]"),
            "{salida}"
        );
        assert_eq!(
            salida
                .lines()
                .filter(|linea| linea.starts_with("n0"))
                .count(),
            MAX_LISTA,
            "solo 500 entradas: {salida}"
        );
        let cancel = CancelToken::new();
        cancel.cancelar();
        assert!(matches!(set.list_dir(args, &cancel), Err(Error::Cancelado)));

        #[cfg(unix)]
        {
            use std::ffi::CString;
            use std::io::Write;
            use std::sync::mpsc;
            use std::thread;
            use std::time::Duration;

            let fifo = raiz.path().join("pipe");
            let nombre = CString::new(fifo.to_string_lossy().as_bytes()).expect("nombre FIFO");
            assert_eq!(unsafe { libc::mkfifo(nombre.as_ptr(), 0o600) }, 0, "mkfifo");
            let set_hilo = Arc::clone(&set);
            let (aviso, recibo) = mpsc::channel();
            let args = json!({"path": raiz.path().to_string_lossy(), "text": "aguja"});
            let _buscador = thread::spawn(move || {
                let _ = aviso.send(set_hilo.search_text(args, &CancelToken::new()));
            });
            match recibo.recv_timeout(Duration::from_secs(3)) {
                Ok(Ok(salida)) => assert_eq!(salida, "sin coincidencias", "{salida}"),
                Ok(Err(error)) => panic!("la búsqueda falló: {error}"),
                Err(_) => {
                    let mut escritor = std::fs::OpenOptions::new()
                        .write(true)
                        .open(&fifo)
                        .expect("desbloquear el FIFO");
                    let _ = escritor.write_all(b"aguja\n");
                    panic!("la búsqueda se colgó en la FIFO");
                }
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn la_busqueda_salta_las_carpetas_sin_permiso_y_conserva_lo_encontrado() {
        use std::os::unix::fs::PermissionsExt;

        let raiz = tempfile::tempdir().expect("raiz");
        std::fs::write(raiz.path().join("a.txt"), "una aguja").expect("a");
        let privada = raiz.path().join("privada");
        std::fs::create_dir(&privada).expect("carpeta");
        std::fs::write(privada.join("b.txt"), "otra aguja").expect("b");
        std::fs::set_permissions(&privada, std::fs::Permissions::from_mode(0o000))
            .expect("quitar permisos");
        let set = conjunto(raiz.path());
        let resultado = set.search_text(
            json!({"path": raiz.path().to_string_lossy(), "text": "aguja"}),
            &CancelToken::new(),
        );
        std::fs::set_permissions(&privada, std::fs::Permissions::from_mode(0o700))
            .expect("devolver permisos");
        let salida = resultado.expect("una carpeta sin permiso no aborta la búsqueda");
        assert!(salida.contains("a.txt"), "{salida}");
    }

    #[tokio::test]
    async fn search_text_finds_and_does_not_follow_links() {
        let raiz = tempfile::tempdir().expect("raiz");
        let fuera = tempfile::tempdir().expect("fuera");
        std::fs::write(raiz.path().join("nota.txt"), "una aguja en el pajar").expect("nota");
        std::fs::write(raiz.path().join("dos.txt"), "aguja uno\naguja dos\n").expect("dos");
        std::fs::write(
            raiz.path().join("larga.txt"),
            format!("{}aguja", "a".repeat(250)),
        )
        .expect("larga");
        std::fs::write(raiz.path().join("bin.dat"), b"aguja\0").expect("bin");
        std::fs::write(fuera.path().join("secreto.txt"), "aguja de afuera").expect("secreto");
        let enlace = raiz.path().join("enlace");
        #[cfg(windows)]
        let creado = std::os::windows::fs::symlink_dir(fuera.path(), &enlace).is_ok()
            || std::process::Command::new("cmd")
                .args(["/c", "mklink", "/J"])
                .arg(&enlace)
                .arg(fuera.path())
                .output()
                .is_ok_and(|salida| salida.status.success());
        #[cfg(not(windows))]
        let creado = std::os::unix::fs::symlink(fuera.path(), &enlace).is_ok();
        if !creado {
            // Sin enlace no hay rama de contención que comprobar: el caso se
            // salta explícito en vez de quedar verde sin ejercitarla (#94).
            eprintln!("sin enlaces ni junctions en este equipo: caso de contención saltado");
            return;
        }
        let set = conjunto(raiz.path());

        let salida = correr(
            &set,
            "local_search_text",
            json!({"path": raiz.path().to_string_lossy(), "text": "AGUJA"}),
        )
        .await
        .expect("buscar");
        assert!(
            salida.contains("nota.txt:1: una aguja en el pajar"),
            "{salida}"
        );
        assert!(
            !salida.contains("de afuera") && !salida.contains("secreto.txt"),
            "la búsqueda salió de la carpeta: {salida}"
        );
        assert!(!salida.contains("bin.dat"), "el binario se buscó: {salida}");
        let larga = salida
            .lines()
            .find(|linea| linea.contains("larga.txt"))
            .expect("la línea larga se mostró");
        let (_, texto) = larga.split_once("larga.txt:1: ").expect("prefijo");
        assert_eq!(texto.chars().count(), MAX_LINEA_BUSQUEDA + 1);
        assert!(texto.ends_with('…'), "sin marca de recorte: {texto}");

        let limitada = correr(
            &set,
            "local_search_text",
            json!({"path": raiz.path().to_string_lossy(), "text": "aguja", "limit": 1}),
        )
        .await
        .expect("búsqueda limitada");
        assert_eq!(limitada.lines().count(), 1, "{limitada}");

        let vacio = correr(
            &set,
            "local_search_text",
            json!({"path": raiz.path().to_string_lossy(), "text": "  "}),
        )
        .await
        .expect_err("texto vacío");
        assert_eq!(vacio, "falta el texto a buscar");

        let nada = correr(
            &set,
            "local_search_text",
            json!({"path": raiz.path().to_string_lossy(), "text": "no-existe"}),
        )
        .await
        .expect("sin coincidencias");
        assert_eq!(nada, "sin coincidencias");
    }

    #[test]
    fn search_text_cancelled_returns_error() {
        let raiz = tempfile::tempdir().expect("raiz");
        std::fs::write(raiz.path().join("a.txt"), "aguja").expect("escribir");
        let set = conjunto(raiz.path());
        let cancel = CancelToken::new();
        cancel.cancelar();

        let error = set
            .search_text(
                json!({"path": raiz.path().to_string_lossy(), "text": "aguja"}),
                &cancel,
            )
            .expect_err("una búsqueda cancelada falla");
        assert!(matches!(error, Error::Cancelado), "{error}");
    }

    /// Issue #79: un error de lectura no puede responder «sin coincidencias»:
    /// el modelo y la persona creerían que el texto no está.
    #[test]
    fn search_text_con_error_de_lectura_no_es_sin_coincidencias() {
        let raiz = tempfile::tempdir().expect("raiz");
        let bloqueado = raiz.path().join("bloqueado.txt");
        std::fs::write(&bloqueado, "aguja escondida").expect("escribir");
        // Windows: abrir sin compartir bloquea fs::read. Unix: chmod 000
        // (como root no bloquea: el caso se salta explícito).
        #[cfg(windows)]
        let _candado = {
            use std::os::windows::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .read(true)
                .share_mode(0)
                .open(&bloqueado)
                .expect("candado del archivo")
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if unsafe { libc::geteuid() } == 0 {
                eprintln!("corriendo como root: chmod no bloquea, caso saltado");
                return;
            }
            std::fs::set_permissions(&bloqueado, std::fs::Permissions::from_mode(0o000))
                .expect("chmod");
        }
        let set = conjunto(raiz.path());
        let error = set
            .search_text(
                json!({"path": raiz.path().to_string_lossy(), "text": "aguja"}),
                &CancelToken::new(),
            )
            .expect_err("una búsqueda que no pudo leer no puede decir «sin coincidencias»");
        let texto = error.to_string();
        assert_ne!(texto, "sin coincidencias");
        assert!(
            texto.contains("no se pudo buscar"),
            "el error dice qué pasó: {texto}"
        );
        assert!(
            texto.contains("bloqueado.txt"),
            "el error nombra el archivo: {texto}"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bloqueado, std::fs::Permissions::from_mode(0o600))
                .expect("restaurar permisos");
        }
    }

    /// Issue #79: una raíz que resuelve pero no se puede listar da error, no
    /// «sin coincidencias».
    #[cfg(unix)]
    #[test]
    fn search_text_con_raiz_ilegible_da_error() {
        use std::os::unix::fs::PermissionsExt;
        if unsafe { libc::geteuid() } == 0 {
            eprintln!("corriendo como root: chmod no bloquea, caso saltado");
            return;
        }
        let raiz = tempfile::tempdir().expect("raiz");
        std::fs::write(raiz.path().join("a.txt"), "aguja").expect("escribir");
        let set = conjunto(raiz.path());
        std::fs::set_permissions(raiz.path(), std::fs::Permissions::from_mode(0o000))
            .expect("chmod");
        let resultado = set.search_text(
            json!({"path": raiz.path().to_string_lossy(), "text": "aguja"}),
            &CancelToken::new(),
        );
        std::fs::set_permissions(raiz.path(), std::fs::Permissions::from_mode(0o700))
            .expect("restaurar permisos");
        let error = resultado.expect_err("una raíz ilegible no puede decir «sin coincidencias»");
        assert!(error.to_string().contains("no se pudo buscar"), "{error}");
    }

    #[test]
    fn tools_register_read_risk_and_schema() {
        let raiz = tempfile::tempdir().expect("raiz");
        let set = conjunto(raiz.path());
        let herramientas = set.tools();
        let resumen: Vec<(String, Option<Risk>, String)> = herramientas
            .iter()
            .map(|herramienta| {
                let spec = herramienta.spec();
                (spec.name, herramienta.risk(), spec.description)
            })
            .collect();
        assert_eq!(
            resumen,
            vec![
                (
                    "local_list_dir".to_owned(),
                    Some(Risk::Read),
                    "Lista una carpeta permitida.".to_owned()
                ),
                (
                    "local_read_file".to_owned(),
                    Some(Risk::Read),
                    "Lee un archivo de texto de una carpeta permitida, hasta 1 MiB.".to_owned()
                ),
                (
                    "local_search_text".to_owned(),
                    Some(Risk::Read),
                    "Busca un texto en los archivos de una carpeta permitida.".to_owned()
                ),
                (
                    "local_write_file".to_owned(),
                    Some(Risk::Write),
                    "Crea o reemplaza un archivo de una carpeta permitida.".to_owned()
                ),
                (
                    "local_edit_file".to_owned(),
                    Some(Risk::Write),
                    "Reemplaza una porción exacta de un archivo (old_string → new_string). Falla sin escribir si no la encuentra; con varias coincidencias exige replace_all.".to_owned()
                ),
                (
                    "local_run_command".to_owned(),
                    Some(Risk::Exec),
                    "Ejecuta un programa permitido, sin shell, en una carpeta permitida.".to_owned()
                ),
            ]
        );
        let schemas: Vec<Value> = herramientas
            .iter()
            .map(|herramienta| herramienta.spec().schema)
            .collect();
        assert_eq!(
            schemas[0],
            json!({"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]})
        );
        assert_eq!(schemas[1], schemas[0]);
        assert_eq!(
            schemas[2],
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "text": {"type": "string"},
                    "limit": {"type": "integer"}
                },
                "required": ["path", "text"]
            })
        );
        assert_eq!(
            schemas[3],
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "content": {"type": "string"}
                },
                "required": ["path", "content"]
            })
        );
        assert_eq!(
            schemas[4],
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "old_string": {"type": "string"},
                    "new_string": {"type": "string"},
                    "replace_all": {"type": "boolean"}
                },
                "required": ["path", "old_string", "new_string"]
            })
        );
        assert_eq!(
            schemas[5],
            json!({
                "type": "object",
                "properties": {
                    "program": {"type": "string"},
                    "args": {"type": "array", "items": {"type": "string"}},
                    "cwd": {"type": "string"}
                },
                "required": ["program"]
            })
        );
    }

    #[tokio::test]
    async fn read_roots_leen_pero_no_escriben_ni_deshacen() {
        let raiz = tempfile::tempdir().expect("raiz");
        let skill = tempfile::tempdir().expect("skill");
        std::fs::create_dir(skill.path().join("references")).expect("references");
        let referencia = skill.path().join("references").join("x.md");
        std::fs::write(&referencia, "de la skill").expect("x");
        let set = Arc::new(
            Set::new(super::super::Config {
                folders: vec![raiz.path().to_string_lossy().into_owned()],
                read_folders: vec![skill.path().to_string_lossy().into_owned()],
                ..super::super::Config::default()
            })
            .expect("Set con una carpeta de solo lectura"),
        );

        assert_eq!(
            correr(
                &set,
                "local_read_file",
                json!({"path": referencia.to_string_lossy()})
            )
            .await,
            Ok("de la skill".to_owned())
        );
        let listado = correr(
            &set,
            "local_list_dir",
            json!({"path": skill.path().join("references").to_string_lossy()}),
        )
        .await
        .expect("listar la carpeta de la skill");
        assert!(listado.contains("x.md"), "{listado}");
        let busqueda = correr(
            &set,
            "local_search_text",
            json!({"path": skill.path().to_string_lossy(), "text": "skill"}),
        )
        .await
        .expect("buscar en la carpeta de la skill");
        assert!(busqueda.contains("x.md:1: de la skill"), "{busqueda}");

        let nueva = skill.path().join("references").join("y.md");
        let error = correr(
            &set,
            "local_write_file",
            json!({"path": nueva.to_string_lossy(), "content": "x"}),
        )
        .await
        .expect_err("la carpeta de la skill no se escribe");
        assert_eq!(error, "la ruta queda fuera de las carpetas permitidas");
        assert!(!nueva.exists(), "la escritura no debía llegar al disco");
        // El deshacer pregunta por `check`: la carpeta de la skill no es suya.
        assert!(matches!(
            set.check(&referencia.to_string_lossy()),
            Err(Error::Fuera)
        ));

        // `..` sigue rechazándose, incluso saliendo de una raíz de lectura.
        let escape = skill.path().join("..").join("..").join("afuera.txt");
        assert!(
            correr(
                &set,
                "local_read_file",
                json!({"path": escape.to_string_lossy()})
            )
            .await
            .is_err()
        );
    }
}
