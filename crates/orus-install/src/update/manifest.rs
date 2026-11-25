//! Manifiesto de versiones (`internal/update/update.go:34-42`).

use serde::{Deserialize, Serialize};

/// Lo que sirve la URL por defecto: versión, notas y un binario por sistema.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub windows: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sha256: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub linux: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub linux_sha256: String,
}
