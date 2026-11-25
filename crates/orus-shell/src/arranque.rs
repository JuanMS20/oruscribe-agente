//! Servidor local del shell y acciones que el panel pide por HTTP.
//!
//! `orus-server::api` es el dueño de la elección de store y del router; aquí
//! solo se le inyectan las acciones de instalación/actualización, se ata a un
//! puerto efímero de loopback y se protege `/api/*` con un token por arranque.
//! El token entra por la URL inicial y se cambia por una cookie `HttpOnly`;
//! sin ella, la API responde 401. No hay puerto fijo ni API abierta a la red.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use orus_api::Update as _;
use orus_api::{Fase, Progreso};

/// Lo que hace el borde cuando una acción pide terminar el proceso.
pub type Salir = Arc<dyn Fn() + Send + Sync>;

/// El desinstalador del borde: verifica que se puede desinstalar y hace el
/// trabajo. Se inyecta para que los tests fallen el desinstalador sin tocar
/// el registro ni la carpeta temporal reales (#81).
type Desinstalador = Arc<dyn Fn(bool) -> Result<(), String> + Send + Sync>;

/// Secuencia compartida por `--uninstall` y `POST /api/setup`: primero se
/// confirma que la desinstalación arrancó; solo después se borran sesión y
/// conversaciones. Si falla, los datos de la persona siguen intactos.
///
/// El llamador decide cómo borrar y devuelve los fallos para darlos al usuario.
pub fn desinstalar_antes_de_borrar(
    remove_data: bool,
    desinstalar: impl FnOnce(bool) -> Result<(), String>,
    borrar: impl FnOnce() -> Vec<String>,
) -> Result<Vec<String>, String> {
    desinstalar(remove_data)?;
    Ok(borrar())
}

/// Acciones reales de `/api/setup` y `/api/update`.
///
/// Vive en el shell y no en `orus-install` porque instalar, desinstalar y
/// actualizar terminan el proceso; `orus-install` se queda en las piezas puras.
pub struct Acciones {
    salir: Salir,
    version: String,
    http: reqwest::Client,
    http_chequeo: reqwest::Client,
    actualizando: orus_install::ApplyLock,
    desinstalar: Desinstalador,
    /// Avance que lee `GET /api/update/progress`. El candado se toma por
    /// fase, nunca durante la descarga entera.
    progreso: Arc<Mutex<Progreso>>,
}

impl Acciones {
    pub fn nuevas(salir: Salir) -> Self {
        Self {
            salir,
            version: env!("CARGO_PKG_VERSION").to_owned(),
            http: cliente_de_descarga(),
            http_chequeo: cliente_de_chequeo(),
            actualizando: orus_install::ApplyLock::new(),
            progreso: Arc::new(Mutex::new(Progreso::default())),
            desinstalar: Arc::new(|remove_data| {
                #[cfg(windows)]
                {
                    if let Ok(exe) = std::env::current_exe()
                        && let Err(error) = orus_install::windows::desactivar_autostart(
                            &orus_install::windows::RegistroWindows,
                            &exe,
                        )
                    {
                        // Un Run que apunta a un exe borrado no rompe nada, pero
                        // deja basura en el registro; se avisa junto con el resto.
                        eprintln!("no se pudo quitar el autostart: {error}");
                    }
                    orus_install::windows::desinstalar(remove_data)
                        .map_err(|error| error.to_string())?;
                }
                #[cfg(not(windows))]
                {
                    borrar_datos_de_usuario(remove_data)?;
                }
                Ok(())
            }),
        }
    }

    /// Deja el avance en `fase`, con los contadores en cero.
    fn marcar(&self, fase: Fase) {
        self.guardar(Progreso::nueva(fase));
    }

    /// Fase `error` con el texto que se le muestra a la persona.
    fn marcar_fallo(&self, mensaje: &str) {
        self.guardar(Progreso::fallo(mensaje));
    }

    /// El callback de la descarga: bytes recibidos y total si el servidor lo
    /// mandó. El candado se toma y se suelta acá; nunca se sostiene durante
    /// la descarga.
    fn marcar_descarga(&self, recibidos: u64, total: Option<u64>) {
        let mut avance = self.candado();
        avance.phase = Fase::Downloading.as_str().to_owned();
        avance.received = recibidos;
        avance.total = total.unwrap_or(0);
    }

    fn guardar(&self, nuevo: Progreso) {
        *self.candado() = nuevo;
    }

    /// El candado del progreso, sin dejar que un pánico ajeno lo envenene
    /// para siempre: el último valor escrito sigue siendo válido.
    fn candado(&self) -> std::sync::MutexGuard<'_, Progreso> {
        self.progreso
            .lock()
            .unwrap_or_else(|envenenado| envenenado.into_inner())
    }

    /// Programa el cierre del proceso para que la respuesta HTTP salga antes.
    fn salir_en(&self, milisegundos: u64) {
        let salir = Arc::clone(&self.salir);
        let _ = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(milisegundos));
            salir();
        });
    }

    /// Fases de una actualización, sin el candado de "una a la vez": eso lo
    /// hace [`orus_api::Update::apply_update`].
    async fn aplicar_actualizacion(&self) -> Result<(), String> {
        self.marcar(Fase::Checking);
        let info = self.check_update().await?;
        if !info.available {
            return Err("no hay una versión nueva para descargar".to_owned());
        }
        let target = std::env::current_exe().map_err(|error| error.to_string())?;

        #[cfg(windows)]
        {
            let nuevo = con_sufijo(&target, ".new");
            // Reintentar el apply no deja restos de un intento anterior.
            let _ = std::fs::remove_file(&nuevo);
            let resultado = self.aplicar_en_windows(&target, &info, &nuevo).await;
            if resultado.is_err() {
                let _ = std::fs::remove_file(&nuevo);
            }
            resultado
        }

        #[cfg(not(windows))]
        {
            let paquete = con_sufijo(&target, ".new.pkg");
            let nuevo = con_sufijo(&target, ".new");
            // Reintentar el apply no deja restos de un intento anterior.
            let _ = std::fs::remove_file(&paquete);
            let _ = std::fs::remove_file(&nuevo);
            let resultado = self
                .aplicar_en_linux(&target, &info, &paquete, &nuevo)
                .await;
            if resultado.is_err() {
                let _ = std::fs::remove_file(&paquete);
                let _ = std::fs::remove_file(&nuevo);
            }
            resultado
        }
    }

    /// Windows: el binario publicado ya es el ejecutable. El orden es el de
    /// siempre: descarga, swap, `--version`, rollback si no arranca, restart.
    #[cfg(windows)]
    async fn aplicar_en_windows(
        &self,
        target: &Path,
        info: &orus_install::UpdateInfo,
        nuevo: &Path,
    ) -> Result<(), String> {
        self.marcar(Fase::Downloading);
        orus_install::update::download_con(
            &self.http,
            &info.url,
            &info.sha256,
            nuevo,
            &mut |recibidos, total| self.marcar_descarga(recibidos, total),
        )
        .await
        .map_err(|error| error.to_string())?;

        self.marcar(Fase::Applying);
        orus_install::swap(nuevo, target).map_err(|error| error.to_string())?;

        // La copia nueva tiene que contestar `--version`; si no contesta, se
        // vuelve a la anterior antes de relanzar. Un update que deja la app sin
        // abrir es peor que no actualizar.
        self.marcar(Fase::Verifying);
        if !orus_install::update::responde_version(target) {
            orus_install::rollback(target).map_err(|error| {
                format!("la versión nueva no arrancó y no se pudo volver a la anterior: {error}")
            })?;
            return Err("la versión nueva no arrancó: se restauró la anterior".to_owned());
        }

        self.marcar(Fase::Restarting);
        orus_install::update::restart(target).map_err(|error| error.to_string())?;

        // Si la clave del registro desapareció (instalación manual), no se
        // aborta: el binario ya está aplicado y el .old sigue de rescate.
        registrar_version_aplicada(&orus_install::windows::RegistroWindows, info);

        self.salir_en(1200);
        Ok(())
    }

    /// Linux: el paquete publicado es un `.tar.gz`. Se baja a `.new.pkg`, se
    /// extrae a `.new` y se comprueba que esa copia conteste `--version`
    /// **antes** de pisar el binario en uso. Sin `.old`, un swap sin
    /// verificación no tiene vuelta atrás.
    #[cfg(not(windows))]
    async fn aplicar_en_linux(
        &self,
        target: &Path,
        info: &orus_install::UpdateInfo,
        paquete: &Path,
        nuevo: &Path,
    ) -> Result<(), String> {
        let carpeta = target
            .parent()
            .filter(|padre| !padre.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        if !carpeta_escribible(carpeta) {
            return Err(format!(
                "la carpeta del ejecutable no se puede escribir: {}. En Linux descarga el paquete o el tar.gz de la release y ejecuta ./install.sh desde la carpeta descomprimida; si el binario está en /usr o /opt, usa el gestor de paquetes",
                carpeta_visible(carpeta)
            ));
        }

        self.marcar(Fase::Downloading);
        orus_install::update::download_con(
            &self.http,
            &info.url,
            &info.sha256,
            paquete,
            &mut |recibidos, total| self.marcar_descarga(recibidos, total),
        )
        .await
        .map_err(|error| error.to_string())?;

        self.marcar(Fase::Applying);
        orus_install::update::paquete::extraer_binario(paquete, nuevo)
            .map_err(|error| error.to_string())?;

        self.marcar(Fase::Verifying);
        if !orus_install::update::responde_version(nuevo) {
            return Err("la versión nueva no arrancó: se conserva la anterior".to_owned());
        }

        self.marcar(Fase::Applying);
        orus_install::swap(nuevo, target).map_err(|error| error.to_string())?;
        let _ = std::fs::remove_file(paquete);

        self.marcar(Fase::Restarting);
        orus_install::update::restart(target).map_err(|error| error.to_string())?;
        self.salir_en(1200);
        Ok(())
    }
}

/// Cliente del manifiesto con el tope de 30 s de Go (`internal/app/app.go:226`).
///
/// La descarga usa otro cliente sin timeout total (`internal/update/update.go:63`):
/// un paquete grande en una red lenta no debe cortarse a los 30 s.
fn cliente_de_chequeo() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// Cliente de descarga con plazo sin datos: un servidor que acepta la
/// conexión y calla (o se cuelga a mitad del paquete) corta a los 60 s en vez
/// de dejar el candado de actualización tomado para siempre. El plazo corre
/// por lectura, no sobre el total: mientras lleguen fragmentos, un paquete
/// grande en una red lenta sigue bajando (`internal/update/update.go:63`).
fn cliente_de_descarga() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(60))
        .read_timeout(Duration::from_secs(60))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

impl orus_api::Setup for Acciones {
    fn setup_info(&self) -> Result<orus_install::SetupInfo, String> {
        let exe = std::env::current_exe().map_err(|error| error.to_string())?;
        let aceptados = orus_config::load().terms_version;
        Ok(orus_install::setup_info(
            &orus_install::Rutas::system(),
            &exe,
            &aceptados,
        ))
    }

    fn install(&self, autostart: bool, terms_version: &str) -> Result<(), String> {
        #[cfg(not(windows))]
        {
            let _ = autostart;
            if terms_version != orus_install::TERMS_VERSION {
                return Err(
                    "los términos cambiaron: vuelve a leerlos y acepta la versión vigente"
                        .to_owned(),
                );
            }
            Err(
                "la instalación desde la app solo está disponible en Windows; en Linux usa el paquete o el tar.gz".to_owned(),
            )
        }

        #[cfg(windows)]
        {
            if terms_version != orus_install::TERMS_VERSION {
                return Err(
                    "los términos cambiaron: vuelve a leerlos y acepta la versión vigente"
                        .to_owned(),
                );
            }
            let exe = std::env::current_exe().map_err(|error| error.to_string())?;
            orus_install::windows::instalar_sistema(&exe).map_err(|error| error.to_string())?;
            if let Err(error) =
                orus_config::editar(|settings| settings.terms_version = terms_version.to_owned())
            {
                return Err(format!(
                    "se instaló, pero no se pudo guardar la aceptación: {error}"
                ));
            }

            // La copia instalada espera el mutex con `--relaunch`; el
            // `--autostart` viaja en la línea de comandos para que el registro
            // apunte a esa copia y no al binario portátil que está por salir.
            // Se lanza antes de responder: si falla, el error vuelve a la
            // ventana en vez de perderse en un hilo (#113).
            let destino = orus_install::Rutas::system().exe();
            lanzar_copia_instalada(&destino, autostart, &lanzar_instalada)?;
            // La respuesta HTTP sale antes de este cierre; si el proceso
            // siguiera vivo, la copia instalada no podría tomar el mutex.
            self.salir_en(1200);
            Ok(())
        }
    }

    fn uninstall(&self, remove_data: bool) -> Result<(), String> {
        // #81 y #132: el CLI y esta ruta HTTP comparten la misma secuencia.
        // Si el desinstalador falla, sesión y conversaciones siguen intactas.
        let fallos = desinstalar_antes_de_borrar(
            remove_data,
            |remove_data| (self.desinstalar)(remove_data),
            || {
                let mut fallos: Vec<String> = Vec::new();
                if let Err(error) = orus_auth::new_store(orus_config::data_dir()).clear() {
                    fallos.push(format!("la sesión guardada ({error})"));
                }
                if remove_data && let Err(error) = orus_app::Conversations::new().clear() {
                    fallos.push(format!("las conversaciones ({error})"));
                }
                fallos
            },
        )?;
        if !fallos.is_empty() {
            // Los datos que no se pudieron borrar se reportan y se aborta.
            // En Windows el `.cmd` ya está lanzado, pero espera ~3 s antes
            // del primer borrado: este error avisa de lo que quedó vivo.
            return Err(format!(
                "no se desinstaló: no se pudo borrar {}. Cierra la app y vuelve a intentar",
                fallos.join(" y ")
            ));
        }

        // La respuesta HTTP sale antes de este cierre; si el proceso siguiera
        // vivo, el `.cmd` no podría borrar el ejecutable.
        self.salir_en(1200);
        Ok(())
    }
}

#[async_trait::async_trait]
impl orus_api::Update for Acciones {
    async fn check_update(&self) -> Result<orus_install::UpdateInfo, String> {
        let url = std::env::var("ORUSCRIBE_UPDATE_URL")
            .ok()
            .filter(|url| !url.is_empty())
            .unwrap_or_else(|| orus_install::DEFAULT_URL.to_owned());
        orus_install::check(&self.http_chequeo, &url, &self.version)
            .await
            .map_err(|error| error.to_string())
    }

    async fn apply_update(&self) -> Result<(), String> {
        // El candado se pide antes de tocar el progreso: un segundo clic no
        // puede pisar la fase de la actualización que ya está corriendo.
        let Some(_guard) = self.actualizando.try_acquire() else {
            return Err("ya hay una actualización en curso".to_owned());
        };
        match self.aplicar_actualizacion().await {
            Ok(()) => {
                self.marcar(Fase::Idle);
                Ok(())
            }
            Err(error) => {
                self.marcar_fallo(&error);
                orus_app::Telemetry::compartida().report_text("shell.update", "error", &error);
                Err(error)
            }
        }
    }

    fn progreso(&self) -> Progreso {
        self.candado().clone()
    }
}

/// `<exe><sufijo>`, sin tocar el archivo original.
fn con_sufijo(target: &Path, sufijo: &str) -> PathBuf {
    let mut ruta = target.as_os_str().to_owned();
    ruta.push(sufijo);
    PathBuf::from(ruta)
}

/// ¿Se puede crear un archivo en `carpeta`? Se prueba con uno y se borra en
/// el acto: los permisos del modo no dicen nada con ACL, montajes de solo
/// lectura ni carpetas de otro dueño.
#[cfg(not(windows))]
fn carpeta_escribible(carpeta: &Path) -> bool {
    let prueba = carpeta.join(format!(".oruscribe-escritura-{}", std::process::id()));
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&prueba)
    {
        Ok(_) => {
            let _ = std::fs::remove_file(&prueba);
            true
        }
        Err(_) => false,
    }
}

/// La carpeta tal como se le muestra a la persona: si cuelga del perfil, con
/// `~` en vez de la ruta completa de la casa.
#[cfg(not(windows))]
fn carpeta_visible(carpeta: &Path) -> String {
    if let Some(casa) = std::env::var_os("HOME").map(PathBuf::from)
        && let Ok(resto) = carpeta.strip_prefix(&casa)
    {
        return format!("~/{}", resto.display());
    }
    carpeta.display().to_string()
}

/// Escribe en Agregar o quitar programas la versión que se acaba de aplicar:
/// la del manifiesto (`info.latest`), no la del binario que corre
/// (`self.version`, la vieja). Si la clave desapareció (instalación manual),
/// no se aborta: el binario ya está aplicado y el `.old` sigue de rescate.
#[cfg(windows)]
fn registrar_version_aplicada(
    registro: &dyn orus_install::windows::Registro,
    info: &orus_install::UpdateInfo,
) {
    if let Err(error) = orus_install::windows::actualizar_version(registro, &info.latest) {
        eprintln!("no se pudo actualizar DisplayVersion: {error}");
    }
}

#[cfg(not(windows))]
pub fn desinstalar_sistema_linux() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    if exe.starts_with("/usr/") || exe.starts_with("/opt/") {
        return Err(
            "esta copia la instaló el gestor de paquetes: desinstálala con apt, dnf o pacman en vez de --uninstall".to_owned(),
        );
    }
    let prefijo = std::env::var_os("ORUSCRIBE_PREFIX")
        .filter(|valor| !valor.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|valor| !valor.is_empty())
                .map(|casa| PathBuf::from(casa).join(".local"))
        });
    let Some(prefijo) = prefijo else {
        return Ok(());
    };
    quitar_instalacion_linux(&exe, &prefijo)
}

#[cfg(not(windows))]
fn quitar_instalacion_linux(exe: &Path, prefijo: &Path) -> Result<(), String> {
    let instalado = prefijo.join("bin").join("oruscribe-agente");
    let mismo = std::fs::canonicalize(exe)
        .ok()
        .zip(std::fs::canonicalize(&instalado).ok())
        .is_some_and(|(actual, destino)| actual == destino);
    if !mismo {
        return Ok(());
    }
    let rutas = [
        instalado,
        prefijo.join("share/applications/oruscribe-agente.desktop"),
        prefijo
            .join("share/icons/hicolor/512x512/apps")
            .join("oruscribe-agente.png"),
    ];
    let mut fallos = Vec::new();
    for ruta in rutas {
        if ruta.exists()
            && let Err(error) = std::fs::remove_file(&ruta)
        {
            fallos.push(format!("{} ({error})", ruta.display()));
        }
    }
    if fallos.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "no se pudo quitar la instalación: {}",
            fallos.join("; ")
        ))
    }
}

/// `--remove-data` sí borra la carpeta de datos, y un fallo se reporta:
/// responder éxito dejaba los datos en disco tras pedir borrarlos.
#[cfg(not(windows))]
pub fn borrar_datos_de_usuario(remove_data: bool) -> Result<(), String> {
    if remove_data && orus_install::data_dir_removable() {
        std::fs::remove_dir_all(orus_config::data_dir()).map_err(|error| {
            format!(
                "no se pudo borrar la carpeta de datos: {error}. Cierra la app y vuelve a intentar"
            )
        })?;
    }
    Ok(())
}

impl orus_api::Quit for Acciones {
    fn quit(&self) -> Result<(), String> {
        // La respuesta HTTP 200 sale antes de este cierre; 600 ms dan tiempo a
        // que el cliente la reciba. Es el mismo `Aviso::Salir` de la bandeja.
        self.salir_en(600);
        Ok(())
    }
}

/// Arranca el servidor en un puerto efímero y devuelve la URL con el token.
pub struct Servidor {
    pub url: String,
    _runtime: tokio::runtime::Runtime,
}

/// El arranque automático real: la entrada `Run` del usuario apunta al exe.
#[cfg(windows)]
struct AutostartWindows {
    exe: PathBuf,
}

#[cfg(windows)]
impl orus_api::Autostart for AutostartWindows {
    fn activo(&self) -> bool {
        orus_install::windows::autostart_activo(&orus_install::windows::RegistroWindows, &self.exe)
            .unwrap_or(false)
    }

    fn cambiar(&self, encendido: bool) -> Result<(), String> {
        let registro = orus_install::windows::RegistroWindows;
        if encendido {
            orus_install::windows::activar_autostart(&registro, &self.exe)
        } else {
            orus_install::windows::desactivar_autostart(&registro, &self.exe)
        }
        .map_err(|error| error.to_string())
    }
}

/// Monta el router real de `orus-server` y lo sirve en `127.0.0.1:0`.
pub fn iniciar(acciones: Arc<Acciones>) -> Result<Servidor, Box<dyn std::error::Error>> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    let url = runtime.block_on(async {
        let api = orus_server::api_con_apertura(
            None,
            Some(Arc::new(|url: &str| {
                if !crate::navegador::abrir(url) {
                    orus_app::Telemetry::compartida().report_text(
                        "shell.browser",
                        "error",
                        "no se pudo abrir el navegador del sistema para iniciar sesión",
                    );
                }
            })),
        )?;
        // El binario publicado abría sin selector y el borde respondía 400:
        // ahora el diálogo nativo llega a la app instalada.
        #[cfg(windows)]
        let api = api.with_pick_folder(Arc::new(crate::selector_carpeta::SelectorCarpeta));
        #[cfg(windows)]
        let api = match std::env::current_exe() {
            Ok(exe) => api.with_autostart(Arc::new(AutostartWindows { exe })),
            Err(_) => api,
        };
        let api = api
            .with_setup(acciones.clone())
            .with_update(acciones.clone())
            .with_quit(acciones.clone());
        // `App::new` deja `connecting` si hay token en disco; el `Boot()` de Go
        // era quien arrancaba la restauración. Sin esto, la ventana se queda en
        // el splash para siempre.
        api.boot();
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
        let direccion = listener.local_addr()?;
        let guardia = orus_server::Guardia::nuevo(direccion.to_string());
        let token = guardia.token().to_owned();
        let router = orus_server::proteger(orus_server::router(&api), guardia);
        let cierre = acciones;
        tokio::spawn(async move {
            if let Err(error) = axum::serve(listener, router).await {
                eprintln!("el servidor local se detuvo: {error}");
                let telemetria = orus_app::Telemetry::compartida();
                telemetria.report_text(
                    "shell.server",
                    "fatal",
                    &format!("el servidor local se detuvo: {error}"),
                );
                telemetria.flush(Duration::from_secs(2));
                // Sin API la ventana queda muda: se cierra en vez de dejar una
                // app que parece abierta y no responde a nada.
                let _ = orus_api::Quit::quit(cierre.as_ref());
            }
        });
        Ok::<String, Box<dyn std::error::Error>>(format!("http://{direccion}/?token={token}"))
    })?;

    Ok(Servidor {
        url,
        _runtime: runtime,
    })
}

/// Lanza la copia instalada y devuelve el fallo a quien pidió instalar (#113).
/// El lanzador se inyecta para comprobar el camino de error sin tocar el
/// registro ni un ejecutable real.
///
/// Solo la llama el camino de instalación de Windows y su prueba; en Linux
/// fuera de los tests no existe, así que no queda código muerto que silenciar.
#[cfg(any(windows, test))]
fn lanzar_copia_instalada(
    destino: &Path,
    autostart: bool,
    lanzar: &dyn Fn(&Path, bool) -> std::io::Result<()>,
) -> Result<(), String> {
    lanzar(destino, autostart)
        .map_err(|error| format!("no se pudo abrir la copia instalada: {error}"))
}

/// Arranca la copia instalada con el mismo contrato que `start_windows.go`:
/// `--relaunch` siempre y `--autostart` si se pidió en la instalación.
#[cfg(windows)]
fn lanzar_instalada(exe: &Path, autostart: bool) -> std::io::Result<()> {
    use std::process::{Command, Stdio};

    let mut comando = Command::new(exe);
    comando.arg("--relaunch");
    if autostart {
        comando.arg("--autostart");
    }
    comando
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        comando.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }

    comando.spawn().map(|_| ())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::path::Path;
    use std::sync::Arc;

    use orus_api::{Fase, Progreso, Setup as _, Update as _};
    use orus_auth::Store as _;
    use serial_test::serial;

    #[cfg(not(windows))]
    use super::quitar_instalacion_linux;
    use super::{Acciones, lanzar_copia_instalada};

    /// Datos de prueba: sesión en archivo y una conversación, sin llavero.
    fn con_datos_de_prueba<T>(f: impl FnOnce(&Path) -> T) -> T {
        let datos = tempfile::tempdir().expect("carpeta temporal");
        temp_env::with_vars(
            [
                ("ORUSCRIBE_DATA_DIR", Some(datos.path().as_os_str())),
                (
                    "ORUSCRIBE_ALLOW_FILE_SECRETS",
                    Some(std::ffi::OsStr::new("1")),
                ),
            ],
            || f(datos.path()),
        )
    }

    fn guardar_sesion_y_conversacion(ruta: &Path) {
        let vencido: orus_auth::Token = serde_json::from_value(serde_json::json!({
            "access_token": "token-de-prueba",
            "expiry": "2020-01-01T00:00:00Z"
        }))
        .expect("token de prueba");
        orus_auth::FileStore::from_data_dir(ruta)
            .save(vencido)
            .expect("guardar la sesión");
        let conversaciones = ruta.join("conversations");
        std::fs::create_dir_all(&conversaciones).expect("carpeta de conversaciones");
        std::fs::write(conversaciones.join("c-1.json"), b"{}").expect("conversación");
    }

    // #132: el CLI y `/api/setup` comparten la secuencia. Si el desinstalador
    // falla, no se tocan la sesión ni las conversaciones.
    #[test]
    fn la_secuencia_no_borra_datos_si_falla_la_desinstalacion() {
        let orden = Arc::new(std::sync::Mutex::new(Vec::new()));

        let registro = Arc::clone(&orden);
        let borrado = Arc::clone(&orden);
        let error = super::desinstalar_antes_de_borrar(
            false,
            |_remove_data| {
                registro.lock().expect("orden").push("desinstalar");
                Err("sin permisos".to_owned())
            },
            || {
                borrado.lock().expect("orden").push("borrar");
                Vec::new()
            },
        )
        .expect_err("el desinstalador falla");
        assert!(error.contains("sin permisos"), "{error}");
        assert_eq!(*orden.lock().expect("orden"), vec!["desinstalar"]);

        let registro = Arc::clone(&orden);
        let borrado = Arc::clone(&orden);
        let fallos = super::desinstalar_antes_de_borrar(
            true,
            move |remove_data| {
                assert!(remove_data);
                registro.lock().expect("orden").push("desinstalar");
                Ok(())
            },
            move || {
                borrado.lock().expect("orden").push("borrar");
                vec!["la sesión".to_owned()]
            },
        )
        .expect("desinstalar");
        assert_eq!(fallos, vec!["la sesión"]);
        assert_eq!(
            *orden.lock().expect("orden"),
            vec!["desinstalar", "desinstalar", "borrar"]
        );
    }

    // #81: si el desinstalador no puede hacer su trabajo, la sesión y las
    // conversaciones tienen que seguir intactas.
    #[test]
    #[serial]
    fn desinstalar_que_falla_no_borra_sesion_ni_conversaciones() {
        con_datos_de_prueba(|ruta| {
            guardar_sesion_y_conversacion(ruta);
            let acciones = Acciones {
                desinstalar: Arc::new(|_remove_data| Err("no se puede desinstalar".to_owned())),
                ..Acciones::nuevas(Arc::new(|| {}))
            };

            let error = acciones
                .uninstall(false)
                .expect_err("el desinstalador falla");

            assert!(error.contains("no se puede desinstalar"), "{error}");
            assert!(
                ruta.join("session.json").exists(),
                "la sesión se borró aunque falló el desinstalador"
            );
            assert!(
                ruta.join("conversations").join("c-1.json").exists(),
                "las conversaciones se borraron aunque falló el desinstalador"
            );
        });
    }

    #[test]
    #[serial]
    fn desinstalar_que_funciona_borra_sesion_y_conserva_conversaciones() {
        con_datos_de_prueba(|ruta| {
            guardar_sesion_y_conversacion(ruta);
            let llamado = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let llamado_clon = Arc::clone(&llamado);
            let acciones = Acciones {
                desinstalar: Arc::new(move |remove_data| {
                    assert!(!remove_data, "esta prueba no pide borrar datos");
                    llamado_clon.store(true, std::sync::atomic::Ordering::SeqCst);
                    Ok(())
                }),
                ..Acciones::nuevas(Arc::new(|| {}))
            };

            acciones.uninstall(false).expect("desinstalar");

            assert!(
                llamado.load(std::sync::atomic::Ordering::SeqCst),
                "el desinstalador no se llamó"
            );
            assert!(!ruta.join("session.json").exists(), "la sesión quedó");
            assert!(
                ruta.join("conversations").join("c-1.json").exists(),
                "las conversaciones se borraron aunque se pidó conservar los datos"
            );

            let conversacion = ruta.join("conversations").join("c-1.json");
            std::fs::write(&conversacion, b"{}").expect("conversación para el segundo caso");
            let acciones = Acciones {
                desinstalar: Arc::new(|remove_data| {
                    assert!(remove_data, "esta prueba pide borrar datos");
                    Ok(())
                }),
                ..Acciones::nuevas(Arc::new(|| {}))
            };
            acciones.uninstall(true).expect("desinstalar con datos");
            assert!(
                !conversacion.exists(),
                "las conversaciones no se borraron con remove_data"
            );
        });
    }

    // El candado de actualización se libera aunque la actualización falle:
    // si no, el piloto ve «ya hay una actualización en curso» para siempre.
    #[test]
    #[serial]
    fn el_candado_se_libera_cuando_falla_la_actualizacion() {
        temp_env::with_var(
            "ORUSCRIBE_UPDATE_URL",
            Some("http://127.0.0.1:1/latest.json"),
            || {
                let acciones = Acciones::nuevas(Arc::new(|| {}));
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("runtime de prueba");

                let resultado = runtime.block_on(acciones.apply_update());

                assert!(
                    resultado.is_err(),
                    "sin servidor de versiones la actualización tiene que fallar"
                );
                assert!(
                    acciones.actualizando.try_acquire().is_some(),
                    "el candado quedó tomado después del fallo"
                );
                // #159: el fallo también se ve en `/api/update/progress`.
                let avance = acciones.progreso();
                assert_eq!(
                    avance.phase, "error",
                    "una actualización que falla tiene que dejar la fase en error"
                );
                assert_eq!(
                    avance.error,
                    resultado.expect_err("la actualización falla"),
                    "el texto de la fase error no es el que se devolvió"
                );
            },
        );
    }

    /// Un servidor que contesta una sola petición y cierra.
    fn servidor_de_un_cuerpo(cuerpo: Vec<u8>) -> (String, std::thread::JoinHandle<()>) {
        use std::io::{Read as _, Write as _};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("puerto local");
        let direccion = listener.local_addr().expect("dirección local");
        let hilo = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut peticion = [0u8; 2048];
                let _ = stream.read(&mut peticion);
                let cabecera = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    cuerpo.len()
                );
                let _ = stream.write_all(cabecera.as_bytes());
                let _ = stream.write_all(&cuerpo);
                let _ = stream.flush();
            }
        });
        (format!("http://{direccion}/paquete"), hilo)
    }

    /// #159: el avance que lee `/api/update/progress` sale con los bytes que
    /// reporta la descarga (el mismo callback que usa `apply_update`) y
    /// vuelve a reposo o a error. La huella falsa corta al final, que es lo
    /// que la fase `error` tiene que mostrar.
    #[test]
    fn el_progreso_refleja_los_bytes_de_la_descarga() {
        let cuerpo = vec![b'x'; 48 << 10];
        let (url, hilo) = servidor_de_un_cuerpo(cuerpo.clone());
        let acciones = Acciones::nuevas(Arc::new(|| {}));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime de prueba");
        let dir = tempfile::tempdir().expect("carpeta temporal");
        let destino = dir.path().join("paquete");
        let sha = "0".repeat(64);

        let error = runtime
            .block_on(orus_install::update::download_con(
                &acciones.http,
                &url,
                &sha,
                &destino,
                &mut |recibidos, total| acciones.marcar_descarga(recibidos, total),
            ))
            .expect_err("la huella no coincide");
        hilo.join().expect("cerrar el servidor");

        let avance = acciones.progreso();
        assert_eq!(avance.phase, "downloading");
        assert_eq!(
            avance.received,
            cuerpo.len() as u64,
            "el callback no llegó a los bytes descargados"
        );
        assert_eq!(
            avance.total,
            cuerpo.len() as u64,
            "no se reportó el Content-Length"
        );

        acciones.marcar_fallo(&error.to_string());
        let avance = acciones.progreso();
        assert_eq!(avance.phase, "error");
        assert_eq!(avance.error, error.to_string());

        acciones.marcar(Fase::Idle);
        assert_eq!(
            acciones.progreso(),
            Progreso::default(),
            "al terminar bien el avance vuelve a reposo"
        );
    }

    // La versión que va al registro es la del manifiesto recién aplicado
    // (`info.latest`), no la del binario que corre (`self.version`).
    #[cfg(windows)]
    #[test]
    fn la_version_registrada_es_la_aplicada_no_la_que_corre() {
        use orus_install::windows::{CLAVE_DESINSTALACION, Valor};

        let registro = RegistroFalso::default();
        let info = orus_install::UpdateInfo {
            available: true,
            current: env!("CARGO_PKG_VERSION").to_owned(),
            latest: "9.9.9".to_owned(),
            ..orus_install::UpdateInfo::default()
        };

        super::registrar_version_aplicada(&registro, &info);

        let escrituras = registro.escrituras.lock().expect("candado del registro");
        assert_eq!(escrituras.len(), 1, "solo se escribe DisplayVersion");
        let (clave, nombre, valor) = &escrituras[0];
        assert_eq!(clave, CLAVE_DESINSTALACION);
        assert_eq!(nombre, "DisplayVersion");
        assert_eq!(valor, &Valor::Texto("9.9.9".to_owned()));
        assert_ne!(
            valor,
            &Valor::Texto(env!("CARGO_PKG_VERSION").to_owned()),
            "la versión escrita no puede ser la vieja del binario que corre"
        );
    }

    #[cfg(windows)]
    #[derive(Default)]
    struct RegistroFalso {
        escrituras: std::sync::Mutex<Vec<(String, String, orus_install::windows::Valor)>>,
    }

    #[cfg(windows)]
    impl orus_install::windows::Registro for RegistroFalso {
        fn crear(&self, _clave: &str) -> Result<(), orus_install::windows::Error> {
            Ok(())
        }
        fn escribir(
            &self,
            clave: &str,
            nombre: &str,
            valor: orus_install::windows::Valor,
        ) -> Result<(), orus_install::windows::Error> {
            self.escrituras.lock().expect("candado").push((
                clave.to_owned(),
                nombre.to_owned(),
                valor,
            ));
            Ok(())
        }
        fn leer_texto(
            &self,
            _clave: &str,
            _nombre: &str,
        ) -> Result<Option<String>, orus_install::windows::Error> {
            Ok(None)
        }
        fn borrar_valor(
            &self,
            _clave: &str,
            _nombre: &str,
        ) -> Result<(), orus_install::windows::Error> {
            Ok(())
        }
        fn borrar(&self, _clave: &str) -> Result<(), orus_install::windows::Error> {
            Ok(())
        }
        fn subclaves(&self, _clave: &str) -> Result<Vec<String>, orus_install::windows::Error> {
            Ok(Vec::new())
        }
        fn valores(
            &self,
            _clave: &str,
        ) -> Result<Vec<(String, String)>, orus_install::windows::Error> {
            Ok(Vec::new())
        }
    }

    // remove-data en Linux: si borrar la carpeta falla, se reporta; responder
    // éxito dejaba los datos en disco después de pedir borrarlos.
    #[cfg(not(windows))]
    #[test]
    #[serial]
    fn remove_data_en_linux_reporta_el_fallo() {
        let config = tempfile::tempdir().expect("carpeta temporal");
        temp_env::with_vars(
            [
                ("ORUSCRIBE_DATA_DIR", None),
                ("XDG_CONFIG_HOME", Some(config.path().as_os_str())),
            ],
            || {
                // Un archivo donde debería estar la carpeta de datos:
                // `remove_dir_all` falla incluso para root.
                let obstaculo = config.path().join("Oruscribe");
                std::fs::write(&obstaculo, b"no soy carpeta").expect("obstáculo");
                assert!(
                    orus_install::data_dir_removable(),
                    "la carpeta de prueba tiene que ser borrable"
                );

                let error = super::borrar_datos_de_usuario(true)
                    .expect_err("borrar un archivo como carpeta tiene que fallar");

                assert!(error.contains("carpeta de datos"), "{error}");
                assert!(obstaculo.exists(), "el obstáculo no se tocó");
            },
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn desinstalar_linux_quita_binario_lanzador_e_icono_del_prefijo() {
        let prefijo = tempfile::tempdir().expect("prefijo temporal");
        let binario = prefijo.path().join("bin/oruscribe-agente");
        let lanzador = prefijo
            .path()
            .join("share/applications/oruscribe-agente.desktop");
        let icono = prefijo
            .path()
            .join("share/icons/hicolor/512x512/apps/oruscribe-agente.png");
        for ruta in [&binario, &lanzador, &icono] {
            std::fs::create_dir_all(ruta.parent().expect("carpeta")).expect("carpeta");
            std::fs::write(ruta, b"instalado").expect("archivo instalado");
        }

        let fuera = tempfile::tempdir().expect("copia portátil");
        let portatil = fuera.path().join("oruscribe-agente");
        std::fs::write(&portatil, b"portatil").expect("copia portátil");
        quitar_instalacion_linux(&portatil, prefijo.path()).expect("no toca la copia portátil");
        assert!(binario.exists(), "se borró una instalación ajena");

        quitar_instalacion_linux(&binario, prefijo.path()).expect("desinstalar");
        assert!(!binario.exists(), "el binario sigue instalado");
        assert!(!lanzador.exists(), "el lanzador sigue en el menú");
        assert!(!icono.exists(), "el icono sigue instalado");
    }

    /// #113: si no se puede abrir la copia instalada, «Instalar» tiene que
    /// devolver el error. Antes se imprimía en un hilo y la ventana creía que
    /// había terminado bien.
    #[test]
    fn el_fallo_al_abrir_la_copia_instalada_se_propaga() {
        let error = lanzar_copia_instalada(Path::new("no-existe.exe"), false, &|_, _| {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no existe",
            ))
        })
        .expect_err("el fallo de lanzar vuelve a quien pidió instalar");

        assert!(
            error.contains("no se pudo abrir la copia instalada"),
            "{error}"
        );
        assert!(error.contains("no existe"), "{error}");
    }

    /// #159: el flujo de Linux de punta a punta —descargar, extraer, verificar
    /// y swap— sin tocar el proceso: el binario nuevo es un guion que contesta
    /// `--version`. Es el camino que reemplaza el ejecutable de la persona.
    #[cfg(not(windows))]
    #[tokio::test]
    async fn linux_aplica_el_paquete_y_deja_el_binario_nuevo() {
        use std::os::unix::fs::PermissionsExt as _;
        use std::path::PathBuf;

        use sha2::Digest as _;

        let dir = tempfile::tempdir().expect("carpeta temporal");

        // El paquete real: gzip + tar con el binario en la raíz.
        let paquete = dir.path().join("paquete.tar.gz");
        let guion: &[u8] = b"#!/bin/sh\necho 0.6.11\n";
        {
            let gz = flate2::write::GzEncoder::new(
                std::fs::File::create(&paquete).expect("crear paquete"),
                flate2::Compression::default(),
            );
            let mut constructor = tar::Builder::new(gz);
            let mut cabecera = tar::Header::new_gnu();
            cabecera.set_mode(0o755);
            cabecera.set_size(guion.len() as u64);
            cabecera.set_cksum();
            constructor
                .append_data(&mut cabecera, "oruscribe-agente", guion)
                .expect("entrada del tar");
            constructor
                .into_inner()
                .expect("cerrar tar")
                .finish()
                .expect("cerrar gzip");
        }
        let bytes = std::fs::read(&paquete).expect("leer paquete");
        let sha = hex::encode(sha2::Sha256::digest(&bytes));
        let (url, hilo) = servidor_de_un_cuerpo(bytes);

        // Copia "instalada" en una carpeta escribible, como ~/.local/bin.
        let carpeta = dir.path().join("bin");
        std::fs::create_dir_all(&carpeta).expect("carpeta bin");
        let target = carpeta.join("oruscribe-agente");
        std::fs::write(&target, b"#!/bin/sh\necho 0.6.10\n").expect("binario viejo");
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))
            .expect("permisos del binario viejo");

        let mut nombre = target.as_os_str().to_owned();
        nombre.push(".new");
        let nuevo = PathBuf::from(nombre);

        let acciones = Acciones::nuevas(Arc::new(|| {}));
        let info = orus_install::UpdateInfo {
            configured: true,
            available: true,
            current: "0.6.10".to_owned(),
            latest: "0.6.11".to_owned(),
            notes: String::new(),
            url,
            sha256: sha,
        };

        acciones
            .aplicar_en_linux(&target, &info, &paquete, &nuevo)
            .await
            .expect("el paquete se aplica");
        hilo.join().expect("hilo del servidor");

        let salida = std::process::Command::new(&target)
            .arg("--version")
            .output()
            .expect("el binario nuevo tiene que correr");
        assert_eq!(
            String::from_utf8_lossy(&salida.stdout).trim(),
            "0.6.11",
            "el destino no quedó con la versión nueva"
        );
        assert_eq!(acciones.progreso().phase, "restarting");
    }
}
