#[derive(Clone, Copy)]
pub enum Severidad {
    Info,
    Error,
}

const HERRAMIENTAS: [&str; 4] = ["zenity", "kdialog", "notify-send", "xmessage"];

pub fn primera_que_funciona(
    mut funciona: impl FnMut(&'static str) -> bool,
) -> Option<&'static str> {
    HERRAMIENTAS.iter().copied().find(|nombre| funciona(nombre))
}

pub fn argumentos(herramienta: &str, texto: &str, severidad: Severidad) -> Vec<String> {
    match (herramienta, severidad) {
        ("zenity", Severidad::Error) => vec![
            "--error".into(),
            "--no-markup".into(),
            "--text".into(),
            texto.into(),
        ],
        ("zenity", Severidad::Info) => vec![
            "--info".into(),
            "--no-markup".into(),
            "--text".into(),
            texto.into(),
        ],
        ("kdialog", Severidad::Error) => vec!["--error".into(), texto.into()],
        ("kdialog", Severidad::Info) => vec!["--msgbox".into(), texto.into()],
        ("notify-send", Severidad::Error) => vec![
            "--urgency".into(),
            "critical".into(),
            "Oruscribe Agente".into(),
            texto.into(),
        ],
        ("notify-send", Severidad::Info) => vec![
            "--urgency".into(),
            "normal".into(),
            "Oruscribe Agente".into(),
            texto.into(),
        ],
        ("xmessage", _) => vec!["-center".into(), texto.into()],
        _ => Vec::new(),
    }
}

fn esta_en_path(nombre: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|rutas| {
        std::env::split_paths(&rutas).any(|directorio| directorio.join(nombre).is_file())
    })
}

pub fn mostrar(texto: &str, severidad: Severidad) -> bool {
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        return false;
    }
    let elegida = primera_que_funciona(|nombre| {
        esta_en_path(nombre)
            && std::process::Command::new(nombre)
                .args(argumentos(nombre, texto, severidad))
                .status()
                .is_ok_and(|estado| estado.success())
    });
    elegida.is_some()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::{Severidad, argumentos, primera_que_funciona};

    #[test]
    fn recorre_las_herramientas_hasta_que_una_funcione() {
        assert_eq!(primera_que_funciona(|_| false), None);
        assert_eq!(
            primera_que_funciona(|nombre| nombre == "notify-send" || nombre == "kdialog"),
            Some("kdialog")
        );
        assert_eq!(
            primera_que_funciona(|nombre| nombre != "zenity"),
            Some("kdialog"),
            "sin zenity toca la siguiente opción"
        );
        assert_eq!(
            primera_que_funciona(|nombre| {
                nombre != "zenity" && nombre != "kdialog" && nombre != "xmessage"
            }),
            Some("notify-send"),
            "si la primera instalada falla se prueba la siguiente"
        );
        assert_eq!(primera_que_funciona(|_| true), Some("zenity"));
    }

    #[test]
    fn los_argumentos_llevan_el_texto_y_la_severidad() {
        let error = argumentos("zenity", "falló el arranque", Severidad::Error);
        assert_eq!(error[0], "--error");
        assert!(error.iter().any(|arg| arg == "--no-markup"));
        assert!(error.iter().any(|arg| arg == "falló el arranque"));

        let info = argumentos("zenity", "ya abierto", Severidad::Info);
        assert_eq!(info[0], "--info");
        assert!(info.iter().any(|arg| arg == "--no-markup"));

        let critico = argumentos("notify-send", "falló", Severidad::Error);
        assert!(critico.iter().any(|arg| arg == "critical"));
        let normal = argumentos("notify-send", "aviso", Severidad::Info);
        assert!(normal.iter().any(|arg| arg == "normal"));
        assert!(argumentos("desconocida", "x", Severidad::Error).is_empty());
    }
}
