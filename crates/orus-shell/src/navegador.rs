//! Política de navegación del WebView y apertura de enlaces externos.
//!
//! La ventana solo puede quedarse en el origen del servidor local. Cualquier
//! enlace http(s) sale al navegador del sistema; `javascript:`, `file:` y los
//! demás esquemas se descartan. Así un enlace pegado en el chat no puede
//! reemplazar la app por una página externa ni arrastrar la cookie local.

use std::process::{Command, Stdio};
#[cfg(target_os = "linux")]
use std::time::Duration;
#[cfg(windows)]
use std::{
    ffi::{OsStr, OsString},
    os::windows::ffi::OsStrExt as _,
    path::PathBuf,
    ptr,
};

/// Abre una dirección http(s) en el navegador del sistema.
///
/// Devuelve `false` si el esquema no es seguro o si no se pudo lanzar el
/// navegador. Nunca pasa la dirección por un shell:
/// - En Windows usa `ShellExecuteW("open", url)` (soporta URLs largas OAuth y
///   navegadores AppX/Edge en VMs limpias) con respaldo en `explorer.exe` y
///   `rundll32.exe`.
/// - En Linux prueba `$BROWSER`, `xdg-open`, `gio open` y navegadores
///   instalados, detectando salidas fallidas inmediatas (comunes con `xdg-open`
///   en Arch/Hyprland/Wayland).
pub fn abrir(url: &str) -> bool {
    let url = url.trim();
    if !es_externa_segura(url) {
        return false;
    }

    #[cfg(windows)]
    {
        abrir_windows(url)
    }

    #[cfg(target_os = "linux")]
    {
        abrir_linux(url)
    }

    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = url;
        false
    }
}

#[cfg(windows)]
fn abrir_windows(url: &str) -> bool {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let verbo = wide_nul("open");
    let destino = wide_nul(url);
    let resultado = unsafe {
        ShellExecuteW(
            ptr::null_mut(),
            verbo.as_ptr(),
            destino.as_ptr(),
            ptr::null(),
            ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    if (resultado as isize) > 32 {
        return true;
    }

    // Respaldo 1: `explorer.exe` directo desde `%SystemRoot%`.
    let explorer = ruta_explorer();
    if Command::new(explorer)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .is_ok()
    {
        return true;
    }

    // Respaldo 2: `rundll32.exe` en System32.
    Command::new(ruta_rundll32())
        .args(["url.dll,FileProtocolHandler", url])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .is_ok()
}

#[cfg(windows)]
fn wide_nul(texto: &str) -> Vec<u16> {
    OsString::from(texto)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

#[cfg(windows)]
fn ruta_explorer() -> PathBuf {
    let raiz = std::env::var_os("SystemRoot")
        .filter(|valor| !valor.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    raiz.join("explorer.exe")
}

#[cfg(target_os = "linux")]
fn abrir_linux(url: &str) -> bool {
    if let Ok(env_browser) = std::env::var("BROWSER") {
        for candidato in parsear_env_browser(&env_browser) {
            if lanzar_linux(candidato, &[url]) {
                return true;
            }
        }
    }
    if lanzar_linux("xdg-open", &[url]) {
        return true;
    }
    if lanzar_linux("gio", &["open", url]) {
        return true;
    }
    for navegador in [
        "sensible-browser",
        "firefox",
        "chromium",
        "google-chrome",
        "google-chrome-stable",
        "brave",
        "brave-browser",
        "zen-browser",
        "vivaldi",
        "librewolf",
        "epiphany",
    ] {
        if lanzar_linux(navegador, &[url]) {
            return true;
        }
    }
    false
}

/// Lanza un abridor en Linux y comprueba brevemente si falló al instante
/// (p. ej. `xdg-open` devolviendo código 3 o 4 en Hyprland/Arch sin handler).
#[cfg(target_os = "linux")]
fn lanzar_linux(programa: &str, args: &[&str]) -> bool {
    let Ok(mut hijo) = Command::new(programa)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    for _ in 0..5 {
        match hijo.try_wait() {
            Ok(Some(estado)) => return estado.success(),
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => return false,
        }
    }
    std::thread::spawn(move || {
        let _ = hijo.wait();
    });
    true
}

#[cfg(any(target_os = "linux", test))]
fn parsear_env_browser(valor: &str) -> Vec<&str> {
    valor
        .split(':')
        .map(str::trim)
        .filter(|item| !item.is_empty() && !item.chars().any(char::is_control))
        .collect()
}

/// Ruta absoluta del `rundll32.exe` del sistema.
///
/// `Command::new("rundll32.exe")` busca primero junto a la app y en el CWD:
/// un ejecutable plantado ahí se abriría al pulsar un enlace del chat. La
/// ruta de System32 cierra esa puerta, igual que `ruta_cmd` en orus-install.
#[cfg(windows)]
fn ruta_rundll32() -> PathBuf {
    ruta_rundll32_en(std::env::var_os("SystemRoot").as_deref())
}

#[cfg(windows)]
fn ruta_rundll32_en(raiz: Option<&OsStr>) -> PathBuf {
    let raiz = raiz
        .filter(|valor| !valor.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    raiz.join("System32").join("rundll32.exe")
}

#[cfg(all(test, windows))]
mod tests_ruta {
    use std::ffi::OsStr;
    use std::path::PathBuf;

    use super::ruta_rundll32_en;

    // Un `rundll32.exe` plantado junto a la app o en el CWD no puede abrir un
    // enlace del chat: se usa la ruta absoluta de System32.
    #[test]
    fn rundll32_va_por_la_ruta_absoluta_de_system32() {
        let esperada = PathBuf::from(r"C:\Windows\System32").join("rundll32.exe");
        assert_eq!(ruta_rundll32_en(Some(OsStr::new(r"C:\Windows"))), esperada);
        assert_eq!(ruta_rundll32_en(None), esperada);
        assert_eq!(ruta_rundll32_en(Some(OsStr::new(""))), esperada);
    }
}

/// Solo http y https; el resto de esquemas no se abren fuera de la app.
pub fn es_externa_segura(url: &str) -> bool {
    let url = url.trim();
    (url.starts_with("https://") || url.starts_with("http://"))
        && !url.chars().any(char::is_control)
}

/// La navegación dentro de la ventana se queda en el mismo origen que sirvió
/// la app. `about:blank` es el lienzo interno del WebView y sí se permite.
pub fn navegacion_permitida(base: &str, destino: &str) -> bool {
    if destino == "about:blank" {
        return true;
    }
    match (origen(base), origen(destino)) {
        (Some(base), Some(destino)) => base == destino,
        _ => false,
    }
}

/// Esquema, host y puerto; el camino y la query no cuentan para el origen.
fn origen(url: &str) -> Option<(String, String, u16)> {
    let uri: axum::http::Uri = url.parse().ok()?;
    let esquema = uri.scheme_str()?.to_ascii_lowercase();
    let host = uri.host()?.to_ascii_lowercase();
    let puerto = uri.port_u16().unwrap_or(match esquema.as_str() {
        "http" => 80,
        "https" => 443,
        _ => return None,
    });
    Some((esquema, host, puerto))
}

#[cfg(test)]
mod tests {
    use super::{es_externa_segura, navegacion_permitida};

    #[test]
    fn permite_el_mismo_origen_y_su_query() {
        let base = "http://127.0.0.1:49152/?token=abc";
        assert!(navegacion_permitida(base, "http://127.0.0.1:49152/"));
        assert!(navegacion_permitida(
            base,
            "http://127.0.0.1:49152/app.css?v=1"
        ));
        assert!(navegacion_permitida(base, "about:blank"));
    }

    #[test]
    fn bloquea_otro_puerto_otro_host_y_otro_esquema() {
        let base = "http://127.0.0.1:49152/";
        assert!(!navegacion_permitida(base, "http://127.0.0.1:49153/"));
        assert!(!navegacion_permitida(base, "http://localhost:49152/"));
        assert!(!navegacion_permitida(base, "https://127.0.0.1:49152/"));
        assert!(!navegacion_permitida(base, "https://oruscribe.com/"));
        assert!(!navegacion_permitida(base, "file:///C:/Windows/win.ini"));
        assert!(!navegacion_permitida(base, "javascript:alert(1)"));
    }

    #[test]
    fn solo_abre_http_y_https() {
        assert!(es_externa_segura("https://oruscribe.com/"));
        assert!(es_externa_segura("http://127.0.0.1:8080/"));
        assert!(!es_externa_segura("file:///C:/Windows/win.ini"));
        assert!(!es_externa_segura("javascript:alert(1)"));
        assert!(!es_externa_segura("https://oruscribe.com/\ninject"));
    }

    #[test]
    fn parsea_variable_browser_con_varios_candidatos() {
        assert_eq!(
            super::parsear_env_browser("firefox:chromium: :brave\n"),
            vec!["firefox", "chromium", "brave"]
        );
    }
}
