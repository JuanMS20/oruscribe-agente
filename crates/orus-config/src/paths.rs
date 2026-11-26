use std::env;
use std::ffi::{OsStr, OsString};
use std::path::{Component, Path, PathBuf};

const APP_DIR: &str = "Oruscribe";

fn env_non_empty(key: &str) -> Option<OsString> {
    env::var_os(key).filter(|value| !value.is_empty())
}

/// Carpeta que guarda ajustes, la sesión y, más adelante, notas.
/// `ORUSCRIBE_DATA_DIR` existe para que las pruebas y el desarrollo nunca
/// toquen el perfil real.
pub fn data_dir() -> PathBuf {
    if let Some(value) = env_non_empty("ORUSCRIBE_DATA_DIR") {
        return PathBuf::from(value);
    }
    if let Some(value) = env_non_empty("LOCALAPPDATA") {
        return PathBuf::from(value).join(APP_DIR);
    }
    if let Some(value) = system_config_dir() {
        return value.join(APP_DIR);
    }
    env::temp_dir().join(APP_DIR)
}

#[cfg(windows)]
fn system_config_dir() -> Option<PathBuf> {
    env_non_empty("APPDATA").map(PathBuf::from)
}

#[cfg(all(not(windows), target_os = "macos"))]
fn system_config_dir() -> Option<PathBuf> {
    home_dir().map(|home| home.join("Library").join("Application Support"))
}

#[cfg(all(not(windows), not(target_os = "macos")))]
fn system_config_dir() -> Option<PathBuf> {
    if let Some(value) = env_non_empty("XDG_CONFIG_HOME") {
        let path = PathBuf::from(value);
        if path.is_absolute() {
            return Some(path);
        }
    }
    home_dir().map(|home| home.join(".config"))
}

#[cfg(windows)]
fn home_dir() -> Option<PathBuf> {
    if let Some(value) = env_non_empty("USERPROFILE") {
        return Some(PathBuf::from(value));
    }
    let drive = env_non_empty("HOMEDRIVE")?;
    let path = env_non_empty("HOMEPATH")?;
    let mut full = drive;
    full.push(path);
    Some(PathBuf::from(full))
}

#[cfg(not(windows))]
fn home_dir() -> Option<PathBuf> {
    env_non_empty("HOME").map(PathBuf::from)
}

/// Dice si la desinstalación puede borrar la carpeta de datos.
/// `ORUSCRIBE_DATA_DIR` apunta a una carpeta propia: desinstalar no la toca.
/// La ruta por defecto tiene que llamarse Oruscribe y no puede ser el hogar ni
/// una carpeta padre.
pub fn data_dir_removable() -> bool {
    if env_non_empty("ORUSCRIBE_DATA_DIR").is_some() {
        return false;
    }

    let dir = data_dir();
    if dir.as_os_str().is_empty() || !dir.is_absolute() {
        return false;
    }

    let clean = clean_path(&dir);
    let Some(name) = clean.file_name() else {
        return false;
    };
    if !name.to_string_lossy().eq_ignore_ascii_case(APP_DIR) {
        return false;
    }

    if let Some(home) = home_dir() {
        let home = clean_path(&home);
        let clean_lower = clean.to_string_lossy().to_lowercase();
        let home_lower = home.to_string_lossy().to_lowercase();
        if clean_lower == home_lower {
            return false;
        }
        let parent_prefix = format!("{clean_lower}{}", std::path::MAIN_SEPARATOR);
        if home_lower.starts_with(&parent_prefix) {
            return false;
        }
    }

    true
}

// Limpieza léxica: quita `.`, resuelve `..` sin tocar el
// disco y conserva el prefijo y la raíz de Windows.
pub fn clean_path(path: &Path) -> PathBuf {
    let mut prefix = None;
    let mut rooted = false;
    let mut parts: Vec<OsString> = Vec::new();

    for component in path.components() {
        match component {
            Component::Prefix(value) => prefix = Some(value.as_os_str().to_owned()),
            Component::RootDir => rooted = true,
            Component::CurDir => {}
            Component::ParentDir => {
                let can_pop = parts
                    .last()
                    .is_some_and(|part| part.as_os_str() != OsStr::new(".."));
                if can_pop {
                    parts.pop();
                } else if !rooted {
                    parts.push(OsString::from(".."));
                }
            }
            Component::Normal(value) => parts.push(value.to_owned()),
        }
    }

    let mut cleaned = PathBuf::new();
    if let Some(prefix) = prefix {
        cleaned.push(prefix);
    }
    if rooted {
        cleaned.push(Component::RootDir.as_os_str());
    }
    for part in parts {
        cleaned.push(part);
    }
    cleaned
}

/// Quita el prefijo `\\?\` que `canonicalize` agrega en Windows.
pub fn sin_prefijo(ruta: &Path) -> PathBuf {
    let texto = ruta.to_string_lossy();
    if let Some(resto) = texto.strip_prefix("\\\\?\\UNC\\") {
        return PathBuf::from(format!("\\\\{resto}"));
    }
    if let Some(resto) = texto.strip_prefix("\\\\?\\") {
        return PathBuf::from(resto);
    }
    ruta.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_path_quita_punto_y_resuelve_padre_sin_tocar_el_disco() {
        let ruta = PathBuf::from("no-existe")
            .join(".")
            .join("hijo")
            .join("..")
            .join("final");
        assert_eq!(clean_path(&ruta), PathBuf::from("no-existe").join("final"));
    }

    #[test]
    fn clean_path_conserva_la_raiz() {
        #[cfg(windows)]
        assert_eq!(
            clean_path(Path::new(r"C:\x\..\..\y")),
            PathBuf::from(r"C:\y")
        );
        #[cfg(not(windows))]
        assert_eq!(clean_path(Path::new("/x/../../y")), PathBuf::from("/y"));
    }

    #[test]
    fn clean_path_conserva_los_padres_sin_raiz() {
        assert_eq!(
            clean_path(Path::new("a/../../b")),
            PathBuf::from("..").join("b")
        );
    }

    #[test]
    fn sin_prefijo_quita_el_prefijo_de_windows() {
        assert_eq!(sin_prefijo(Path::new(r"\\?\C:\x")), PathBuf::from(r"C:\x"));
    }

    #[test]
    fn sin_prefijo_convierte_unc() {
        assert_eq!(
            sin_prefijo(Path::new(r"\\?\UNC\server\share\x")),
            PathBuf::from(r"\\server\share\x")
        );
    }
}
