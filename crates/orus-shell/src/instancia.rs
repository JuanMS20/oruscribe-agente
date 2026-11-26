//! Instancia única del agente en Windows.
//!
//! Un mutex con nombre en el espacio
//! `Local\` mantiene dos copias del programa lejos una de otra. El handle se
//! guarda durante toda la vida del proceso y Windows lo libera cuando el
//! proceso termina, sin importar cómo termine.
//!
//! Una copia lanzada por una actualización (`--relaunch`) espera a que la
//! anterior suelte el mutex; las demás se rinden enseguida. En Unix el mismo
//! contrato se cumple con un archivo de bloqueo en la carpeta de datos.

use std::time::Duration;

/// Nombre exacto del contrato.
pub const MUTEX: &str = r"Local\OruscribeAgente";

/// Tope de espera de una copia relanzada.
pub const ESPERA_RELANZAMIENTO: Duration = Duration::from_secs(15);

/// Reintento entre intentos.
pub const REINTENTO: Duration = Duration::from_millis(200);

/// La copia anterior todavía tiene el mutex.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstanciaOcupada;

impl InstanciaOcupada {
    pub fn mensaje(&self, con_bandeja: bool) -> String {
        if con_bandeja {
            "Oruscribe Agente ya está abierto.\n\nBúscalo en la bandeja del sistema, junto al reloj."
                .to_owned()
        } else {
            "Oruscribe Agente ya está abierto.\n\nSu ventana sigue abierta en el escritorio. \
             Este escritorio no muestra icono en la bandeja: ciérrala desde la propia app antes de volver a abrirla."
                .to_owned()
        }
    }
}

impl std::fmt::Display for InstanciaOcupada {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.mensaje(true))
    }
}

impl std::error::Error for InstanciaOcupada {}

/// Handle que mantiene viva la instancia. Se cierra solo al hacer `drop`.
#[must_use = "el handle mantiene la instancia única mientras viva"]
pub struct InstanciaUnica {
    #[cfg(windows)]
    handle: windows_sys::Win32::Foundation::HANDLE,
    /// Mantiene el `flock` de Unix mientras viva; al cerrarse se libera solo.
    #[cfg(not(windows))]
    _archivo: std::fs::File,
}

#[cfg(windows)]
impl Drop for InstanciaUnica {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            // SAFETY: el handle lo devolvió CreateMutexW y no se cerró antes.
            unsafe { windows_sys::Win32::Foundation::CloseHandle(self.handle) };
            self.handle = std::ptr::null_mut();
        }
    }
}

// En Unix el candado es el `flock`, no la existencia del archivo: el kernel lo
// libera al cerrar el descriptor, aunque el proceso muera sin desenrollar.
// El archivo puede quedar en disco; no bloquea al siguiente arranque.

/// Pide el mutex `nombre`.
///
/// `relanzar` habilita la espera de [`ESPERA_RELANZAMIENTO`]; sin él, una
/// segunda copia falla en el primer intento. Si Windows no deja crear el
/// mutex, devuelve [`InstanciaOcupada`]: arrancar sin candado deja dos copias
/// escribiendo los mismos datos.
pub fn adquirir(
    nombre: &str,
    relanzar: bool,
    espera: Duration,
    reintento: Duration,
) -> Result<InstanciaUnica, InstanciaOcupada> {
    #[cfg(windows)]
    {
        use std::ptr;
        use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError, SetLastError};
        use windows_sys::Win32::System::Threading::CreateMutexW;

        // El contrato no puede convertir un nombre con NUL y deja arrancar; aquí se
        // corta igual antes de llamar a Windows.
        if nombre.contains('\0') {
            return Ok(InstanciaUnica {
                handle: ptr::null_mut(),
            });
        }

        let nombre: Vec<u16> = nombre.encode_utf16().chain(std::iter::once(0)).collect();
        let limite = std::time::Instant::now() + espera;
        loop {
            // Windows no limpia el error del hilo: sin esto, un
            // ERROR_ALREADY_EXISTS de una llamada anterior hace pasar un
            // mutex recién creado por «ya hay otra instancia».
            // SAFETY: SetLastError solo escribe el error del hilo actual.
            unsafe { SetLastError(0) };
            // SAFETY: nombre termina en NUL y vive hasta que CreateMutexW
            // retorna; no se retiene el puntero.
            let handle = unsafe { CreateMutexW(ptr::null(), 0, nombre.as_ptr()) };
            // SAFETY: GetLastError lee el error del hilo inmediatamente después
            // de CreateMutexW.
            let error = unsafe { GetLastError() };
            if !handle.is_null() && error != ERROR_ALREADY_EXISTS {
                return Ok(InstanciaUnica { handle });
            }
            if !handle.is_null() {
                // SAFETY: handle lo devolvió CreateMutexW y todavía no se cerró.
                unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
            }
            // Un handle nulo es un fallo de Windows, no una instancia creada:
            // sin candado la app no puede arrancar. Se trata como ocupada
            // porque el motivo típico es que el nombre ya lo tomó otra copia.
            if !relanzar || std::time::Instant::now() >= limite {
                return Err(InstanciaOcupada);
            }
            std::thread::sleep(reintento);
        }
    }

    #[cfg(not(windows))]
    {
        use std::fs::OpenOptions;
        use std::io::ErrorKind;
        use std::os::unix::io::AsRawFd;

        let _ = nombre;
        let directorio = orus_config::data_dir();
        let _ = std::fs::create_dir_all(&directorio);
        let ruta = directorio.join("orus-shell.lock");
        // El archivo puede existir de una ejecución anterior: `flock` decide si
        // hay una instancia viva, no la existencia del archivo. Así un `kill -9`
        // o un cierre de sesión no deja la app bloqueada para siempre.
        let archivo = match OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            // El contenido del candado no importa: `flock` decide. No se
            // trunca el archivo de una ejecución anterior.
            .truncate(false)
            .open(&ruta)
        {
            Ok(archivo) => archivo,
            // Un error de permisos o de disco deja la app sin candado: igual
            // que el mutex que Windows no deja crear, se reporta como ocupada
            // en vez de arrancar una segunda copia escribiendo los mismos
            // datos.
            Err(_) => return Err(InstanciaOcupada),
        };
        let limite = std::time::Instant::now() + espera;
        loop {
            // SAFETY: el descriptor sale de `archivo`, que vive hasta el final.
            let tomado = unsafe { libc::flock(archivo.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if tomado == 0 {
                return Ok(InstanciaUnica { _archivo: archivo });
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != ErrorKind::WouldBlock {
                // Un fallo raro del sistema también deja la app sin candado:
                // se reporta, no se traga en silencio.
                return Err(InstanciaOcupada);
            }
            if !relanzar || std::time::Instant::now() >= limite {
                return Err(InstanciaOcupada);
            }
            std::thread::sleep(reintento);
        }
    }
}

/// Igual que [`adquirir`], leyendo `--relaunch` de la línea de comandos.
///
/// `args` es la lista completa; el primer elemento es el ejecutable y se
/// ignora.
pub fn adquirir_del_sistema(args: &[String]) -> Result<InstanciaUnica, InstanciaOcupada> {
    let relanzar = args.iter().skip(1).any(|arg| arg == "--relaunch");
    adquirir(MUTEX, relanzar, ESPERA_RELANZAMIENTO, REINTENTO)
}

#[cfg(test)]
mod constantes {
    use super::*;

    #[test]
    fn coinciden_con_el_contrato() {
        assert_eq!(MUTEX, r"Local\OruscribeAgente");
        assert_eq!(ESPERA_RELANZAMIENTO, std::time::Duration::from_secs(15));
        assert_eq!(REINTENTO, std::time::Duration::from_millis(200));
    }

    // Hallazgo 5: el aviso incluye la segunda línea.
    #[test]
    fn el_texto_de_segunda_instancia_es_el_esperado() {
        assert_eq!(
            InstanciaOcupada.to_string(),
            "Oruscribe Agente ya está abierto.\n\nBúscalo en la bandeja del sistema, junto al reloj."
        );
    }

    #[test]
    fn sin_icono_el_aviso_no_manda_a_la_bandeja() {
        let texto = InstanciaOcupada.mensaje(false);
        assert!(
            texto.contains("ya está abierto"),
            "falta el motivo: {texto}"
        );
        assert!(
            !texto.contains("junto al reloj"),
            "promete un icono que no hay: {texto}"
        );
        assert!(
            texto.contains("ventana"),
            "no da una vía de vuelta: {texto}"
        );
    }
}

#[cfg(all(test, windows))]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use super::*;

    fn nombre_unico(sufijo: &str) -> String {
        let unico = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|tiempo| tiempo.as_nanos())
            .unwrap_or(0);
        format!(
            r"Local\OruscribeTest-{}-{unico}-{sufijo}",
            std::process::id()
        )
    }

    // El flujo normal sigue devolviendo la instancia, y al soltarla entra la
    // siguiente.
    #[test]
    fn el_flujo_normal_devuelve_la_instancia() {
        let nombre = nombre_unico("normal");
        let primera = adquirir(&nombre, false, ESPERA_RELANZAMIENTO, REINTENTO)
            .expect("la primera copia no obtuvo el mutex");
        drop(primera);
        let segunda = adquirir(&nombre, false, ESPERA_RELANZAMIENTO, REINTENTO)
            .expect("al soltar el mutex entra la siguiente");
        drop(segunda);
    }

    #[test]
    fn la_segunda_copia_del_mismo_nombre_no_entra() {
        let nombre = nombre_unico("segunda");
        let primera = adquirir(&nombre, false, ESPERA_RELANZAMIENTO, REINTENTO)
            .expect("la primera copia no obtuvo el mutex");
        let segunda = adquirir(&nombre, false, Duration::from_millis(10), REINTENTO);
        assert_eq!(segunda.err(), Some(InstanciaOcupada));
        drop(primera);
    }

    // La primera copia entra, la
    // segunda sin `--relaunch` no, y una relanzada espera a que la primera
    // suelte el mutex.
    #[test]
    fn el_relanzamiento_espera_a_la_copia_vieja() {
        let unico = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|tiempo| tiempo.as_nanos())
            .unwrap_or(0);
        let nombre = format!(r"Local\OruscribeTest-{}-{unico}", std::process::id());

        let primera = adquirir(&nombre, false, ESPERA_RELANZAMIENTO, REINTENTO)
            .expect("la primera copia no obtuvo el mutex");
        assert!(
            adquirir(&nombre, false, ESPERA_RELANZAMIENTO, REINTENTO).is_err(),
            "una segunda copia sin --relaunch entró"
        );

        let espera = std::thread::spawn(move || {
            let inicio = Instant::now();
            let entro = adquirir(&nombre, true, ESPERA_RELANZAMIENTO, REINTENTO).is_ok();
            (entro, inicio.elapsed())
        });
        std::thread::sleep(Duration::from_millis(500));
        drop(primera);

        let (entro, tomo) = espera.join().expect("el hilo del relanzamiento murió");
        assert!(
            entro,
            "el relanzamiento no entró después de soltar el mutex"
        );
        assert!(
            tomo < Duration::from_secs(5),
            "el relanzamiento tardó {tomo:?}, quiero menos de 5 s"
        );
    }
}

#[cfg(all(test, not(windows)))]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::time::{Duration, Instant};

    use super::*;
    use serial_test::serial;
    use temp_env::with_var;

    fn en_datos<T>(f: impl FnOnce() -> T) -> T {
        let dir = tempfile::tempdir().expect("carpeta temporal");
        let ruta = dir.path().to_str().expect("ruta utf8");
        with_var("ORUSCRIBE_DATA_DIR", Some(ruta), f)
    }

    // El fallo que esto cubre: un cierre sin `Drop` dejaba el archivo y la app
    // no volvía a abrir nunca. Con `flock`, un archivo viejo no bloquea.
    #[test]
    #[serial]
    fn un_candado_viejo_no_bloquea() {
        en_datos(|| {
            let dir = orus_config::data_dir();
            std::fs::write(dir.join("orus-shell.lock"), b"viejo").expect("candado viejo");
            let instancia = adquirir(
                "prueba",
                false,
                Duration::from_millis(10),
                Duration::from_millis(1),
            )
            .expect("un archivo viejo no debe bloquear");
            drop(instancia);
        });
    }

    // Un fallo al abrir el candado (permisos, disco, un directorio en su
    // lugar) no puede dejar la app arrancando sin instancia única: se
    // reporta como ocupada, igual que el mutex que Windows no deja crear.
    #[test]
    #[serial]
    fn un_fallo_al_abrir_el_candado_no_arranca_sin_el() {
        en_datos(|| {
            let dir = orus_config::data_dir();
            // Un directorio donde va el archivo: `open` falla con EISDIR.
            std::fs::create_dir_all(dir.join("orus-shell.lock")).expect("obstáculo");
            let resultado = adquirir(
                "prueba",
                false,
                Duration::from_millis(10),
                Duration::from_millis(1),
            );
            assert_eq!(
                resultado.err(),
                Some(InstanciaOcupada),
                "un fallo de open tiene que reportarse, no tragarse"
            );
        });
    }

    #[test]
    #[serial]
    fn la_segunda_copia_no_entra_hasta_soltar_la_primera() {
        en_datos(|| {
            let primera = adquirir(
                "prueba",
                false,
                Duration::from_millis(10),
                Duration::from_millis(1),
            )
            .expect("la primera copia entra");
            assert!(
                adquirir(
                    "prueba",
                    false,
                    Duration::from_millis(10),
                    Duration::from_millis(1)
                )
                .is_err(),
                "la segunda copia entró sin --relaunch"
            );
            drop(primera);
            let segunda = adquirir(
                "prueba",
                false,
                Duration::from_millis(10),
                Duration::from_millis(1),
            )
            .expect("al soltar el flock entra la siguiente");
            drop(segunda);
        });
    }

    #[test]
    #[serial]
    fn el_relanzamiento_espera_a_la_copia_vieja() {
        en_datos(|| {
            let primera = adquirir(
                "prueba",
                false,
                Duration::from_secs(5),
                Duration::from_millis(10),
            )
            .expect("la primera copia entra");
            let espera = std::thread::spawn(|| {
                let inicio = Instant::now();
                let entro = adquirir(
                    "prueba",
                    true,
                    Duration::from_secs(5),
                    Duration::from_millis(10),
                )
                .is_ok();
                (entro, inicio.elapsed())
            });
            std::thread::sleep(Duration::from_millis(200));
            drop(primera);

            let (entro, tomo) = espera.join().expect("el hilo del relanzamiento murió");
            assert!(
                entro,
                "el relanzamiento no entró después de soltar el flock"
            );
            assert!(
                tomo < Duration::from_secs(5),
                "el relanzamiento tardó {tomo:?}, quiero menos de 5 s"
            );
        });
    }
}
