//! Arranque del shell de escritorio.
//!
//! Este archivo solo elige el camino: desinstalar, instancia única, ventana,
//! bandeja y bucle de eventos. El servidor vive en [`arranque`], la ventana en
//! [`ventana`] y la bandeja en [`bandeja`] (R9). Windows y Linux comparten el
//! mismo bucle; solo cambian el cuadro de avisos y el registro.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

fn main() -> std::process::ExitCode {
    escritorio::arrancar()
}

mod escritorio {
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use orus_shell::arranque::{self, Acciones, Salir};
    use orus_shell::aviso::Severidad;
    use orus_shell::bandeja::{self, Aviso, Bandeja, Proxy, debe_reintentar};
    use orus_shell::instancia;
    use orus_shell::ventana::Ventana;
    use tao::event::{Event, WindowEvent};
    use tao::event_loop::{ControlFlow, EventLoopBuilder};
    use tray_icon::MouseButton;
    use tray_icon::MouseButtonState;
    use tray_icon::TrayIconEvent;
    use tray_icon::menu::MenuEvent;

    /// Captura panics no recuperados del shell de escritorio y envía un reporte
    /// técnico redactado (respetando el interruptor de telemetría en Ajustes).
    fn instalar_hook_panic() {
        let previo = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            previo(info);
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let payload = info
                    .payload()
                    .downcast_ref::<&str>()
                    .copied()
                    .or_else(|| info.payload().downcast_ref::<String>().map(String::as_str))
                    .unwrap_or("panic");
                let ubicacion = info
                    .location()
                    .map(|loc| format!("{}:{}:{}", loc.file(), loc.line(), loc.column()))
                    .unwrap_or_else(|| "desconocida".to_owned());
                orus_app::Telemetry::compartida().report_panic(&format!("{payload} @ {ubicacion}"));
            }));
        }));
    }

    #[cfg(windows)]
    use orus_install::windows::RegistroWindows;
    #[cfg(windows)]
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MB_ICONERROR, MB_ICONINFORMATION, MB_OK, MessageBoxW,
    };

    /// Punto de entrada real. El orden importa: `--uninstall` corre antes del
    /// mutex para que el desinstalador no dependa de que la app esté cerrada.
    pub fn arrancar() -> std::process::ExitCode {
        instalar_hook_panic();
        orus_app::Telemetry::from_env(orus_config::load().telemetry_on());
        let args: Vec<String> = std::env::args().collect();

        if args.iter().any(|arg| arg == "--version") {
            // Sonda de salud del update: contesta y sale sin abrir ventana ni
            // pedir el mutex. Si esto no contesta, el binario no sirve.
            println!("{}", env!("CARGO_PKG_VERSION"));
            return std::process::ExitCode::SUCCESS;
        }

        if let Err(error) = desinstalar(&args) {
            mensaje(
                &format!("No se pudo desinstalar: {error}"),
                Severidad::Error,
            );
            return std::process::ExitCode::FAILURE;
        }

        let instancia = match instancia::adquirir_del_sistema(&args) {
            Ok(instancia) => instancia,
            Err(ocupada) => {
                mensaje(&ocupada.mensaje(bandeja::hay_host_real()), Severidad::Info);
                return std::process::ExitCode::SUCCESS;
            }
        };

        limpiar_residuos();

        let event_loop = EventLoopBuilder::<Aviso>::with_user_event().build();
        let proxy: Proxy = Arc::new(Mutex::new(event_loop.create_proxy()));
        conectar(&proxy);

        let salir: Salir = {
            let proxy = Arc::clone(&proxy);
            Arc::new(move || enviar(&proxy, Aviso::Salir))
        };
        let acciones = Arc::new(Acciones::nuevas(salir));
        let servidor = match arranque::iniciar(acciones) {
            Ok(servidor) => servidor,
            Err(error) => {
                mensaje(
                    &format!("Oruscribe Agente no pudo arrancar: {error}"),
                    Severidad::Error,
                );
                return std::process::ExitCode::SUCCESS;
            }
        };

        let ventana = match Ventana::crear(&event_loop, &servidor.url) {
            Ok(ventana) => ventana,
            Err(error) => {
                mensaje(
                    &format!("Oruscribe Agente no pudo abrir la ventana: {error}"),
                    Severidad::Error,
                );
                return std::process::ExitCode::SUCCESS;
            }
        };

        let mut bandeja = None;
        let mut primer_error = None;
        for intento in 0..2 {
            match Bandeja::crear_contenida() {
                Ok(creada) => {
                    bandeja = Some(creada);
                    primer_error = None;
                    break;
                }
                Err(error) => {
                    if primer_error.is_none() {
                        primer_error = Some(error.to_string());
                    }
                    if !debe_reintentar(&*error, intento) {
                        break;
                    }
                }
            }
        }
        if let Some(error) = primer_error {
            let texto = format!(
                "Oruscribe Agente no pudo crear la bandeja: {error}. La app sigue abierta sin icono en la bandeja."
            );
            std::thread::spawn(move || mensaje(&texto, Severidad::Error));
        }

        #[cfg(windows)]
        if args.iter().skip(1).any(|arg| arg == "--autostart")
            && let Ok(exe) = std::env::current_exe()
            && let Err(error) = orus_install::windows::activar_autostart(&RegistroWindows, &exe)
        {
            mensaje(
                &format!("No se pudo activar el arranque con Windows: {error}"),
                Severidad::Error,
            );
        }

        let salir = |control: &mut ControlFlow| {
            orus_app::Telemetry::compartida().flush(Duration::from_secs(2));
            *control = ControlFlow::Exit;
        };

        event_loop.run(move |evento, _, control| {
            *control = ControlFlow::Wait;
            // La instancia, el runtime del servidor y la bandeja tienen que
            // vivir tanto como el bucle; el cierre de la ventana no los toca.
            let _ = (&instancia, &servidor, &bandeja);

            match evento {
                Event::WindowEvent {
                    event: WindowEvent::CloseRequested,
                    ..
                } => {
                    // Sin bandeja no hay forma de volver a abrir la ventana:
                    // esconderla dejaría la app viva y sin manera de alcanzarla.
                    if bandeja.is_none() || !bandeja::hay_host_real() {
                        salir(control);
                    } else {
                        ventana.ocultar();
                    }
                }
                Event::UserEvent(Aviso::Salir) => salir(control),
                Event::UserEvent(Aviso::Menu(evento)) => match evento.id.0.as_str() {
                    bandeja::ID_ABRIR => ventana.mostrar(),
                    bandeja::ID_SALIR => salir(control),
                    _ => {}
                },
                Event::UserEvent(Aviso::Bandeja(TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                })) => ventana.mostrar(),
                _ => {}
            }
        });
    }

    /// Atiende `--uninstall` antes de pedir el mutex.
    ///
    /// Primero se confirma que se puede desinstalar; solo después se borran
    /// la sesión y las conversaciones. Agregar o quitar programas usa este
    /// camino y no debe dejar al usuario sin datos si falla el desinstalador.
    fn desinstalar(args: &[String]) -> Result<(), String> {
        let Some(remove_data) = orus_install::windows::argumentos_desinstalacion(args) else {
            return Ok(());
        };

        let fallos = orus_shell::arranque::desinstalar_antes_de_borrar(
            remove_data,
            desinstalar_sistema,
            || {
                let mut fallos = Vec::new();
                if let Err(error) = orus_auth::new_store(orus_config::data_dir()).clear() {
                    eprintln!("no se pudo borrar la sesión: {error}");
                    fallos.push(format!("la sesión guardada ({error})"));
                }
                if remove_data && let Err(error) = orus_app::Conversations::new().clear() {
                    eprintln!("no se pudieron borrar las conversaciones: {error}");
                    fallos.push(format!("las conversaciones ({error})"));
                }
                #[cfg(not(windows))]
                if let Err(error) = arranque::borrar_datos_de_usuario(remove_data) {
                    eprintln!("no se pudo borrar la carpeta de datos: {error}");
                    fallos.push(error);
                }
                fallos
            },
        )?;
        if fallos.is_empty() {
            Ok(())
        } else {
            Err(format!("no se pudo desinstalar: {}", fallos.join("; ")))
        }
    }

    /// Lo que depende del sistema: registro y accesos en Windows. En Linux no
    /// hay desinstalador del sistema; la carpeta de datos se quita en la fase
    /// de datos, después de confirmar este paso.
    fn desinstalar_sistema(remove_data: bool) -> Result<(), String> {
        #[cfg(windows)]
        {
            if let Ok(exe) = std::env::current_exe()
                && let Err(error) =
                    orus_install::windows::desactivar_autostart(&RegistroWindows, &exe)
            {
                eprintln!("no se pudo apagar el arranque con Windows: {error}");
            }
            orus_install::windows::desinstalar(remove_data).map_err(|error| error.to_string())?;
        }

        #[cfg(not(windows))]
        {
            let _ = remove_data;
            arranque::desinstalar_sistema_linux()?;
        }

        Ok(())
    }

    /// Borra lo que dejaron una instalación o una actualización anterior.
    ///
    /// `.old` se conserva: es la copia de rescate si la versión nueva no abre.
    /// `.new` y `.nuevo` son restos de una copia atómica que no llegó a renombrarse.
    fn limpiar_residuos() {
        let Ok(exe) = std::env::current_exe() else {
            return;
        };
        for sufijo in [".new", ".nuevo"] {
            let mut ruta = exe.as_os_str().to_owned();
            ruta.push(sufijo);
            let _ = std::fs::remove_file(PathBuf::from(ruta));
        }
    }

    /// Manda los eventos de `tray-icon` al bucle de `tao`.
    fn conectar(proxy: &Proxy) {
        let menu = Arc::clone(proxy);
        MenuEvent::set_event_handler(Some(move |evento: MenuEvent| {
            enviar(&menu, Aviso::Menu(evento));
        }));

        let icono = Arc::clone(proxy);
        TrayIconEvent::set_event_handler(Some(move |evento: TrayIconEvent| {
            enviar(&icono, Aviso::Bandeja(evento));
        }));
    }

    fn enviar(proxy: &Proxy, aviso: Aviso) {
        if let Ok(proxy) = proxy.lock() {
            let _ = proxy.send_event(aviso);
        }
    }

    fn mensaje(texto: &str, icono: Severidad) {
        if matches!(icono, Severidad::Error) {
            let telemetria = orus_app::Telemetry::compartida();
            telemetria.report_text("shell.startup", "error", texto);
            telemetria.flush(Duration::from_secs(2));
        }
        #[cfg(windows)]
        {
            let bits = match icono {
                Severidad::Error => MB_ICONERROR,
                Severidad::Info => MB_ICONINFORMATION,
            };
            let texto = ancho(texto);
            let titulo = ancho("Oruscribe Agente");
            // SAFETY: los dos punteros apuntan a cadenas UTF-16 terminadas en
            // NUL que viven durante la llamada; la ventana padre va en NULL.
            unsafe {
                MessageBoxW(
                    std::ptr::null_mut(),
                    texto.as_ptr(),
                    titulo.as_ptr(),
                    MB_OK | bits,
                );
            }
        }

        #[cfg(not(windows))]
        {
            if !orus_shell::aviso::mostrar(texto, icono) {
                eprintln!("Oruscribe Agente: {texto}");
            }
        }
    }

    #[cfg(windows)]
    fn ancho(texto: &str) -> Vec<u16> {
        texto.encode_utf16().chain(std::iter::once(0)).collect()
    }
}
