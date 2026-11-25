//! Embebe el icono de la app en el ejecutable de Windows.
//!
//! Sin este recurso, la barra de tareas, el Explorador y los accesos muestran
//! el icono genérico de exe aunque la ventana pinte el logo desde la web. En
//! otros sistemas operativos no se compila ningún recurso.

fn main() {
    let manifest =
        std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
    // Sin `..` en la ruta: rc.exe no resuelve separadores mezclados.
    let icon = manifest
        .parent()
        .and_then(std::path::Path::parent)
        .map(|raiz| raiz.join("build/windows/icon.ico"))
        .unwrap_or_else(|| manifest.join("build/windows/icon.ico"));
    println!("cargo:rerun-if-changed={}", icon.display());
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // Tiene que ser absoluta: el `.rc` se compila desde `out/`, no desde
        // la raíz del crate, y una ruta relativa queda rota ahí.
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon(icon.to_string_lossy().as_ref());
        if let Err(error) = resource.compile() {
            panic!("no se pudo embeber el icono del exe: {error}");
        }
    }
}
