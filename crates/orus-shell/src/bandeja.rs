//! Bandeja del sistema: icono, menú «Abrir/Salir» y eventos del menú.
//!
//! El icono sale del mismo PNG que sirve la ventana; se reduce a 32×32 porque
//! Windows usa ese tamaño en la bandeja. Los eventos se reenvían al bucle de
//! `tao` con [`Proxy`], que es la única forma segura de despertarlo desde otro
//! hilo (`tray-icon` documenta este patrón para usuarios de tao/winit).

use std::any::Any;
use std::error::Error;
use std::fmt;
use std::io::Cursor;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex};

use tao::event_loop::EventLoopProxy;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder, TrayIconEvent};

/// Identificador del elemento que muestra la ventana.
pub const ID_ABRIR: &str = "oruscribe.abrir";
/// Identificador del elemento que termina el proceso.
pub const ID_SALIR: &str = "oruscribe.salir";
/// Texto exacto del menú del contrato.
pub const TEXTO_ABRIR: &str = "Abrir Oruscribe Agente";
/// Texto exacto del menú del contrato.
pub const TEXTO_SALIR: &str = "Salir";
/// Tooltip de la bandeja mientras no hay avisos.
pub const TOOLTIP: &str = "Oruscribe Agente";
pub const WATCHER: &str = "org.kde.StatusNotifierWatcher";

#[derive(Debug)]
pub struct PanicoIndicador(String);

impl fmt::Display for PanicoIndicador {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "la bandeja hizo panic: {}", self.0)
    }
}

impl Error for PanicoIndicador {}

fn texto_panico(panico: Box<dyn Any + Send>) -> String {
    if let Some(texto) = panico.downcast_ref::<&str>() {
        (*texto).to_owned()
    } else if let Some(texto) = panico.downcast_ref::<String>() {
        texto.clone()
    } else {
        "sin texto".to_owned()
    }
}

fn contener<T>(operacion: impl FnOnce() -> T) -> Result<T, PanicoIndicador> {
    match catch_unwind(AssertUnwindSafe(operacion)) {
        Ok(valor) => Ok(valor),
        Err(panico) => Err(PanicoIndicador(texto_panico(panico))),
    }
}

pub fn debe_reintentar(error: &(dyn Error + 'static), intento: usize) -> bool {
    error.downcast_ref::<PanicoIndicador>().is_none() && intento == 0
}

#[cfg(not(windows))]
const ESPERA_DBUS: std::time::Duration = std::time::Duration::from_millis(1000);

#[cfg(not(windows))]
fn ejecutar_con_tiempo(mut hijo: std::process::Child) -> Option<std::process::Output> {
    let inicio = std::time::Instant::now();
    loop {
        match hijo.try_wait() {
            Ok(Some(_)) => return hijo.wait_with_output().ok(),
            Ok(None) if inicio.elapsed() >= ESPERA_DBUS => {
                let _ = hijo.kill();
                let _ = hijo.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(20)),
            Err(_) => return None,
        }
    }
}

#[cfg(not(windows))]
fn consultar_propiedad(argumentos: &[&str]) -> Option<String> {
    let (programa, resto) = argumentos.split_first()?;
    let hijo = std::process::Command::new(*programa)
        .args(resto)
        .spawn()
        .ok()?;
    let salida = ejecutar_con_tiempo(hijo)?;
    if salida.status.success() {
        Some(String::from_utf8_lossy(&salida.stdout).into_owned())
    } else {
        None
    }
}

pub fn host_registrado(respuesta: &str) -> bool {
    respuesta
        .replace(
            |caracter: char| ['<', '>', ',', '(', ')'].contains(&caracter),
            " ",
        )
        .split_whitespace()
        .rev()
        .find_map(|token| match token {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        })
        .unwrap_or(false)
}

pub fn hay_host_real() -> bool {
    #[cfg(windows)]
    {
        true
    }

    #[cfg(not(windows))]
    {
        if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none() {
            return false;
        }
        let destino = format!("--dest={WATCHER}");
        let interfaz = format!("string:{WATCHER}");
        let dbus_send = [
            "dbus-send",
            "--session",
            destino.as_str(),
            "--type=method_call",
            "--print-reply",
            "/StatusNotifierWatcher",
            "org.freedesktop.DBus.Properties.Get",
            interfaz.as_str(),
            "string:IsStatusNotifierHostRegistered",
        ];
        let gdbus = [
            "gdbus",
            "call",
            "--session",
            "--dest",
            WATCHER,
            "--object-path",
            "/StatusNotifierWatcher",
            "--method",
            "org.freedesktop.DBus.Properties.Get",
            WATCHER,
            "IsStatusNotifierHostRegistered",
        ];
        [dbus_send.as_slice(), gdbus.as_slice()]
            .into_iter()
            .find_map(consultar_propiedad)
            .is_some_and(|respuesta| host_registrado(&respuesta))
    }
}

/// Evento que despierta el bucle principal.
pub enum Aviso {
    Menu(MenuEvent),
    Bandeja(TrayIconEvent),
    Salir,
}

/// Cola compartida entre los manejadores de `tray-icon` y el bucle de `tao`.
pub type Proxy = Arc<Mutex<EventLoopProxy<Aviso>>>;

/// Mantiene vivo el icono mientras viva la app.
pub struct Bandeja {
    _icono: TrayIcon,
}

impl Bandeja {
    pub fn crear_contenida() -> Result<Self, Box<dyn Error>> {
        contener(Self::crear)?
    }

    /// Crea el icono y el menú. Sin icono no hay bandeja: el error se propaga
    /// para que `main` lo muestre en vez de arrancar a medias.
    pub fn crear() -> Result<Self, Box<dyn std::error::Error>> {
        let abrir = MenuItem::with_id(ID_ABRIR, TEXTO_ABRIR, true, None);
        let salir = MenuItem::with_id(ID_SALIR, TEXTO_SALIR, true, None);

        let menu = Menu::new();
        menu.append(&abrir)?;
        let separador = PredefinedMenuItem::separator();
        menu.append(&separador)?;
        menu.append(&salir)?;

        let icono = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip(TOOLTIP)
            .with_icon(icono()?)
            .build()?;

        Ok(Self { _icono: icono })
    }
}

/// Decodifica el logo embebido y lo reduce a un cuadrado de 32×32.
fn icono() -> Result<Icon, Box<dyn std::error::Error>> {
    let bytes = include_bytes!("../../../frontend/dist/logo-512.png");
    let mut lector = png::Decoder::new(Cursor::new(&bytes[..])).read_info()?;
    let (ancho, alto) = {
        let info = lector.info();
        (info.width, info.height)
    };
    let tamano = lector.output_buffer_size().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "el logo de la bandeja no cabe en memoria",
        )
    })?;
    let mut pixeles = vec![0u8; tamano];
    lector.next_frame(&mut pixeles)?;

    Ok(Icon::from_rgba(reducir(&pixeles, ancho, alto, 32), 32, 32)?)
}

/// Vecino más cercano: suficiente para un icono de bandeja y no arrastra una
/// dependencia de escalado.
fn reducir(origen: &[u8], ancho: u32, alto: u32, lado: u32) -> Vec<u8> {
    let mut salida = vec![0u8; (lado as usize) * (lado as usize) * 4];
    for y in 0..lado {
        for x in 0..lado {
            let origen_x = x * ancho / lado;
            let origen_y = y * alto / lado;
            let desde = ((origen_y * ancho + origen_x) as usize) * 4;
            let hasta = ((y * lado + x) as usize) * 4;
            salida[hasta..hasta + 4].copy_from_slice(&origen[desde..desde + 4]);
        }
    }
    salida
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::{contener, debe_reintentar, host_registrado};

    #[test]
    fn el_panico_de_la_bandeja_se_vuelve_error() {
        let resultado =
            contener(|| -> () { std::panic::panic_any("Failed to load ayatana-appindicator3") });
        let error = resultado.expect_err("el panic no se contuvo");
        assert!(
            error.to_string().contains("ayatana-appindicator3"),
            "el mensaje del panic se perdió: {error}"
        );
        let normal: Box<dyn std::error::Error> = Box::new(std::io::Error::other("sin icono"));
        assert!(debe_reintentar(normal.as_ref(), 0));
        assert!(!debe_reintentar(normal.as_ref(), 1));
        assert!(
            !debe_reintentar(&error, 0),
            "el Lazy de libappindicator queda envenenado: reintentar no sirve"
        );
        assert_eq!(
            contener(|| 7).unwrap_or_default(),
            7,
            "una operación normal no debe cambiar"
        );
    }

    #[test]
    fn el_host_registrado_se_lee_de_la_respuesta_dbus() {
        assert!(host_registrado("   variant       boolean true\n"));
        assert!(!host_registrado("   variant       boolean false\n"));
        assert!(host_registrado("(<true>,)\n"));
        assert!(!host_registrado("(<false>,)\n"));
        assert!(!host_registrado(
            "   string \"org.kde.StatusNotifierWatcher\"\n"
        ));
        assert!(!host_registrado(""));
    }
}
