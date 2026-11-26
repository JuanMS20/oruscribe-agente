//! Lo que el borde ya resolvió para un turno.

use std::sync::Arc;

use orus_agent::{Provider, Tool};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Toolset {
    #[default]
    Full,
    Pi,
}

/// Lo que el borde ya resolvió para un turno. Es propiedad, no préstamo: el
/// turno corre en otra tarea. La clave ya viene resuelta y el `cwd` ya es
/// absoluto; resolverlos es trabajo del borde.
pub struct Turno {
    pub base_url: String,
    pub modelo: String,
    pub modo: String,
    pub cwd: String,
    /// Todas las carpetas permitidas, en el orden de los ajustes. `cwd` sigue
    /// siendo la primera o el hogar para instrucciones y skills.
    pub folders: Vec<String>,
    /// El cerebro de la organización: no arma herramientas locales, ni lee
    /// instrucciones o skills; su prompt y sus herramientas vienen del servidor
    pub cerebro: bool,
    pub mensaje: String,
    /// Las imágenes ya normalizadas que la persona adjuntó al mensaje. Vacío
    /// es un turno de solo texto.
    pub imagenes: Vec<orus_core::Image>,
    pub clave: String,
    /// Proveedor ya resuelto por el borde; el cerebro de la organización llega
    /// así. Sin él, el arnés construye `OpenAICompat`.
    pub proveedor: Option<Arc<dyn Provider>>,
    /// Programas permitidos para `local_run_command`, en el orden en que
    /// llegaron por `--program`.
    pub programas: Vec<String>,
    pub toolset: Toolset,
    pub max_steps: i64,
    /// `--no-agents-md`: no leer el `AGENTS.md` global; los del proyecto
    /// siguen
    pub no_agents_md: bool,
    /// `--no-skills`: no cargar skills ni ofrecer la herramienta `skill`.
    pub no_skills: bool,
    /// `--yes`: aprueba escrituras sin preguntar.
    pub yes: bool,
    /// `--allow-exec`: aprueba ejecuciones sin preguntar.
    pub allow_exec: bool,
    /// La entrada es una terminal. Sin ella, lo que la política deja en
    /// `AskUser` se rechaza.
    pub es_tty: bool,
    /// Herramienta de memoria ya abierta por el borde. La CLI la construye
    /// porque `orus-memory` depende de este crate por `sessions` (D72).
    pub memoria: Option<Arc<dyn Tool>>,
    /// Herramientas de la organización, antes que las locales.
    pub org_tools: Vec<Arc<dyn Tool>>,
    /// Aviso de la organización; vacío si no hay nada que avisar.
    pub org_note: String,
    /// Ventana declarada en ajustes; `0` = usar la derivada del modelo.
    pub window_tokens: i64,
    /// Tarifa de entrada por millón de tokens; la app estima el costo del turno.
    pub cost_input_per_mtok: f64,
    /// Tarifa de salida por millón de tokens; la app estima el costo del turno.
    pub cost_output_per_mtok: f64,
}
