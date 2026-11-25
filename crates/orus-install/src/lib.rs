//! Piezas puras del instalador: versión del binario, términos de uso,
//! comparación de versiones, rutas, el cuerpo de `/api/setup`, el update
//! completo (manifiesto, firma Ed25519, descarga y swap) y la instalación real
//! por usuario en Windows (registro, accesos y desinstalación).
//!
//! Port de `internal/update/update.go` (`Newer`, `numbers`, `validSHA`),
//! `internal/install/terms.go`, `internal/install/install_windows.go`,
//! `internal/install/install_other.go`, `internal/app/setup.go` y
//! `internal/update/{update,keys,restart_windows,restart_other}.go`. Nada aquí
//! abre una ventana; el registro y los accesos viven en [`windows`] y solo se
//! tocan en Windows.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod paths;
mod setup;
mod terms;
pub mod update;
mod version;
pub mod windows;

/// La desinstalación borra la carpeta de datos con las mismas reglas que
/// `internal/config/config.go:135-160`; el dueño es `orus-config`.
pub use orus_config::data_dir_removable;
pub use paths::{Platform, Rutas};
pub use setup::{SetupInfo, platform, setup_info, setup_json};
pub use terms::{TERMS_BODY, TERMS_TITLE, TERMS_VERSION};
pub use update::{
    ALLOWED_HOSTS, ApplyGuard, ApplyLock, DEFAULT_URL, Error as UpdateError, Keys, Manifest,
    TRUSTED_KEYS_B64, UpdateInfo, check, check_with_keys, download, rollback, swap,
};
pub use version::{newer, numbers, valid_sha, version};
