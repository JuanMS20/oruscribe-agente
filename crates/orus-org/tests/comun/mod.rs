//! Infraestructura compartida de los tests de captura con git falso.
//!
//! El git falso es un ejecutable real que se compila con rustc al correr los
//! tests (rustc existe en cualquier máquina que corre cargo). Graba los
//! argumentos que recibe y reproduce la salida guionada en la carpeta que
//! recibe con `-C`: cada test tiene su propia carpeta y no hay carreras de
//! entorno. Cambiar el PATH afecta a todo el proceso de test, así que los
//! tests de estos binarios se serializan con [`GUARDA_PATH`].
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};

/// Serializa los tests que cambian el PATH del proceso de test.
pub static GUARDA_PATH: Mutex<()> = Mutex::new(());

const FUENTE_GIT_FALSO: &str = r#"
use std::io::{Read, Write};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dir = args
        .windows(2)
        .find(|par| par[0] == "-C")
        .map(|par| par[1].clone())
        .unwrap_or_default();
    let base = std::path::Path::new(&dir);
    let _ = std::fs::write(base.join(".fake-git-args"), args.join("\n"));
    let modo = std::fs::read_to_string(base.join(".fake-git-modo")).unwrap_or_default();
    match modo.trim() {
        "colgado" => {
            for _ in 0..40 {
                if let Ok(mut archivo) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(base.join(".fake-git-latido"))
                {
                    let _ = writeln!(archivo, "vivo");
                }
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        }
        "fallo" => {
            eprintln!("fallo guionado");
            std::process::exit(3);
        }
        _ => {
            let mut datos = Vec::new();
            if let Ok(mut archivo) = std::fs::File::open(base.join(".fake-git-salida")) {
                let _ = archivo.read_to_end(&mut datos);
            }
            let _ = std::io::stdout().write_all(&datos);
        }
    }
}
"#;

/// Compila una sola vez el git falso y devuelve su carpeta, que hay que
/// anteponer al PATH. La carpeta vive hasta que termina el proceso de test.
pub fn carpeta_git_falso() -> &'static Path {
    static CARPETA: OnceLock<PathBuf> = OnceLock::new();
    CARPETA.get_or_init(|| {
        let dir = tempfile::tempdir()
            .expect("sin carpeta temporal para el git falso")
            .keep();
        let fuente = dir.join("fake_git.rs");
        std::fs::write(&fuente, FUENTE_GIT_FALSO).expect("no se pudo escribir el git falso");
        let exe = dir.join(if cfg!(windows) { "git.exe" } else { "git" });
        let salida = Command::new("rustc")
            .arg("-O")
            .arg("--edition=2021")
            .arg(&fuente)
            .arg("-o")
            .arg(&exe)
            .output()
            .expect("rustc no está disponible para compilar el git falso");
        assert!(
            salida.status.success(),
            "no se pudo compilar el git falso: {}",
            String::from_utf8_lossy(&salida.stderr)
        );
        dir
    })
}

/// Antepone la carpeta del git falso al PATH de este proceso de test.
///
/// En edition 2024 `set_var` es `unsafe`: los tests de este binario se
/// serializan con [`GUARDA_PATH`] y ningún otro hilo del proceso lee el PATH
/// mientras tanto; el cambio no sale de este proceso de test.
pub fn usar_git_falso() {
    let dir = carpeta_git_falso();
    let original = std::env::var_os("PATH").unwrap_or_default();
    let ya_puesto = std::env::split_paths(&original)
        .next()
        .is_some_and(|primero| primero == dir);
    if !ya_puesto {
        let nuevo = std::env::join_paths(
            std::iter::once(dir.to_path_buf()).chain(std::env::split_paths(&original)),
        )
        .expect("PATH inválido");
        unsafe { std::env::set_var("PATH", nuevo) };
    }
}

/// Deja en la carpeta la salida que emitirá el git falso.
pub fn repo_con_salida(dir: &Path, salida: &[u8]) {
    std::fs::write(dir.join(".fake-git-salida"), salida).expect("no se pudo guionar la salida");
}

/// Elige el comportamiento del git falso: salida guionada, "colgado" o "fallo".
pub fn repo_con_modo(dir: &Path, modo: &str) {
    std::fs::write(dir.join(".fake-git-modo"), modo).expect("no se pudo guionar el modo");
}

/// Los argumentos que el git falso recibió, uno por línea.
pub fn argumentos(dir: &Path) -> String {
    std::fs::read_to_string(dir.join(".fake-git-args"))
        .expect("el git falso no grabó sus argumentos")
}

/// Cuántas líneas de latido escribió el git falso (una por segundo vivo).
pub fn latidos(dir: &Path) -> usize {
    std::fs::read_to_string(dir.join(".fake-git-latido"))
        .map(|texto| texto.lines().count())
        .unwrap_or(0)
}

/// `n` registros con la forma de `git log`: campos con \x1f, commits con \x1e.
pub fn registros(n: usize) -> Vec<u8> {
    let mut raw = String::new();
    for indice in 0..n {
        raw.push_str(&format!(
            "r{indice}\x1f2026-11-18T10:00:00Z\x1ft{indice}\x1f\x1e"
        ));
    }
    raw.into_bytes()
}
