//! Ruta real de un objeto del disco.
//!
//! Port de `canon_windows.go:14-50`: `std::fs::canonicalize` usa
//! `GetFinalPathNameByHandle`, así que resuelve enlaces, junctions y puntos de
//! unión. Fuera de Windows equivale a `EvalSymlinks` (`canon_other.go:7`).

use std::io;
use std::path::{Path, PathBuf};

use orus_config::sin_prefijo;

/// Devuelve la ruta final, sin el prefijo `\\?\` que agrega Windows.
pub(super) fn canon(ruta: &Path) -> io::Result<PathBuf> {
    let real = std::fs::canonicalize(ruta)?;
    Ok(sin_prefijo(&real))
}
