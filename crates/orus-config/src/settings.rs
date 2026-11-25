use chrono::{DateTime, NaiveDate, Utc};
use serde::Serialize;

use crate::Error;
use crate::paths::data_dir;
use crate::system_locale;

/// Valores por defecto del agente: OpenRouter habla el formato de OpenAI y
/// el arnés arranca en modo autónomo (Turbo) para trabajar sobre el equipo.
pub const DEFAULT_AGENT_BASE_URL: &str = "https://openrouter.ai/api/v1";
pub const DEFAULT_AGENT_MODE: &str = "autonomous";

/// Nombre con el que la clave del proveedor vive en el llavero.
pub const AGENT_SECRET_NAME: &str = "agente-clave";

/// Idioma que habla la app. Solo se entrega en estos dos.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Locale {
    #[serde(rename = "es")]
    Spanish,
    #[serde(rename = "en")]
    English,
}

impl Locale {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Spanish => "es",
            Self::English => "en",
        }
    }

    fn from_code(code: &str) -> Option<Self> {
        match code {
            "es" => Some(Self::Spanish),
            "en" => Some(Self::English),
            _ => None,
        }
    }
}

impl std::fmt::Display for Locale {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// La porción de configuración que la persona puede cambiar, más los dos
/// valores que deja un inicio de sesión para no repetirlo al arrancar.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Settings {
    #[serde(rename = "locale")]
    pub locale: Locale,
    #[serde(rename = "client_id", skip_serializing_if = "texto_vacio")]
    pub client_id: String,
    #[serde(rename = "redirect_uri", skip_serializing_if = "texto_vacio")]
    pub redirect_uri: String,
    #[serde(rename = "org_slug", skip_serializing_if = "texto_vacio")]
    pub org_slug: String,
    /// La persona eligió la organización desde el selector: `connect` no puede
    /// reemplazarla con la que devuelva `get_me` (#117).
    #[serde(rename = "org_fijada", skip_serializing_if = "es_falso")]
    pub org_fijada: bool,
    #[serde(rename = "endpoint", skip_serializing_if = "texto_vacio")]
    pub endpoint: String,
    #[serde(rename = "terms_version", skip_serializing_if = "texto_vacio")]
    pub terms_version: String,

    // Recuerda que ya se preguntó, en el primer uso de una instalación real,
    // si la app debe abrirse al encender el equipo.
    #[serde(rename = "autostart_asked", skip_serializing_if = "es_falso")]
    pub autostart_asked: bool,

    // `None` es "sin decidir": el valor de fábrica es buscar actualizaciones.
    // `Some(false)` es un no explícito.
    #[serde(rename = "auto_update", skip_serializing_if = "Option::is_none")]
    pub auto_update: Option<bool>,

    #[serde(rename = "telemetry", skip_serializing_if = "Option::is_none")]
    pub telemetry: Option<bool>,

    // La clave del proveedor no va aquí: vive en el llavero.
    #[serde(rename = "agent")]
    pub agent: AgentSettings,
}

impl Settings {
    /// Sin una elección guardada, la búsqueda automática está encendida.
    pub fn auto_update_on(&self) -> bool {
        self.auto_update.unwrap_or(true)
    }

    /// Activa por defecto para capturar fallos técnicos redactados; la persona
    /// la apaga en Ajustes y esa elección se conserva.
    pub fn telemetry_on(&self) -> bool {
        self.telemetry.unwrap_or(true)
    }
}

/// Proveedor de IA que usa el agente.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AgentSettings {
    // "org" es el cerebro de la organización; "openai" es una clave propia
    // compatible con OpenAI. La clave no va aquí: vive en el llavero.
    #[serde(rename = "kind", skip_serializing_if = "texto_vacio")]
    pub kind: String,
    #[serde(rename = "base_url", skip_serializing_if = "texto_vacio")]
    pub base_url: String,
    #[serde(rename = "model", skip_serializing_if = "texto_vacio")]
    pub model: String,
    #[serde(rename = "mode", skip_serializing_if = "texto_vacio")]
    pub mode: String,
    /// Marca que la persona eligió explícitamente el modo en 0.8+: permite
    /// migrar de forma transparente los `settings.json` de 0.7.0 que guardaban
    /// `kind: "org", mode: "ask"` por defecto al nuevo modo Autónomo (Turbo).
    #[serde(rename = "mode_explicit", skip_serializing_if = "es_falso")]
    pub mode_explicit: bool,
    #[serde(rename = "folders", skip_serializing_if = "Vec::is_empty")]
    pub folders: Vec<String>,
    #[serde(rename = "programs", skip_serializing_if = "Vec::is_empty")]
    pub programs: Vec<String>,

    // Reglas de permiso recordadas: tool + patrón exacto.
    #[serde(rename = "allow_rules", skip_serializing_if = "Vec::is_empty")]
    pub allow_rules: Vec<AllowRule>,

    // Despertar por el timbre. Apagado por defecto.
    #[serde(rename = "wake_enabled", skip_serializing_if = "es_falso")]
    pub wake_enabled: bool,
    #[serde(rename = "wake_per_minute", skip_serializing_if = "es_cero_i64")]
    pub wake_per_minute: i64,
    #[serde(rename = "wake_per_day", skip_serializing_if = "es_cero_i64")]
    pub wake_per_day: i64,
    #[serde(rename = "wake_tokens_per_day", skip_serializing_if = "es_cero_i64")]
    pub wake_tokens_per_day: i64,

    // Voz local. Las direcciones apuntan a un servidor compatible con OpenAI.
    #[serde(rename = "voice_stt_url", skip_serializing_if = "texto_vacio")]
    pub voice_stt_url: String,
    #[serde(rename = "voice_stt_model", skip_serializing_if = "texto_vacio")]
    pub voice_stt_model: String,
    #[serde(rename = "voice_tts_url", skip_serializing_if = "texto_vacio")]
    pub voice_tts_url: String,
    #[serde(rename = "voice_tts_model", skip_serializing_if = "texto_vacio")]
    pub voice_tts_model: String,
    #[serde(rename = "voice_tts_voice", skip_serializing_if = "texto_vacio")]
    pub voice_tts_voice: String,

    // Ventana del modelo en tokens. 0 = sin declarar.
    #[serde(rename = "window_tokens", skip_serializing_if = "es_cero_i64")]
    pub window_tokens: i64,
    // Tarifas en dólares por millón de tokens. 0 en ambas = sin estimación.
    #[serde(rename = "cost_input_per_mtok", skip_serializing_if = "es_cero_f64")]
    pub cost_input_per_mtok: f64,
    #[serde(rename = "cost_output_per_mtok", skip_serializing_if = "es_cero_f64")]
    pub cost_output_per_mtok: f64,
}

impl Default for AgentSettings {
    fn default() -> Self {
        Self {
            kind: "org".to_owned(),
            base_url: DEFAULT_AGENT_BASE_URL.to_owned(),
            model: String::new(),
            mode: DEFAULT_AGENT_MODE.to_owned(),
            mode_explicit: false,
            folders: Vec::new(),
            programs: Vec::new(),
            allow_rules: Vec::new(),
            wake_enabled: false,
            wake_per_minute: 0,
            wake_per_day: 0,
            wake_tokens_per_day: 0,
            voice_stt_url: String::new(),
            voice_stt_model: String::new(),
            voice_tts_url: String::new(),
            voice_tts_model: String::new(),
            voice_tts_voice: String::new(),
            window_tokens: 0,
            cost_input_per_mtok: 0.0,
            cost_output_per_mtok: 0.0,
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            locale: system_locale(),
            client_id: String::new(),
            redirect_uri: String::new(),
            org_slug: String::new(),
            org_fijada: false,
            endpoint: String::new(),
            terms_version: String::new(),
            autostart_asked: false,
            auto_update: None,
            telemetry: None,
            agent: AgentSettings::default(),
        }
    }
}

/// Una aprobación recordada: "esta herramienta, este patrón exacto, no
/// preguntes más por ella".
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AllowRule {
    #[serde(rename = "id")]
    pub id: String,
    #[serde(rename = "tool")]
    pub tool: String,
    #[serde(rename = "pattern")]
    pub pattern: String,
    #[serde(rename = "note", skip_serializing_if = "texto_vacio")]
    pub note: String,
    #[serde(rename = "created_at")]
    pub created_at: DateTime<Utc>,
}

/// Nombre del archivo que guarda los ajustes dentro de la carpeta de datos.
pub(crate) const SETTINGS_FILE: &str = "settings.json";

/// Lee los ajustes. Un archivo ausente o roto no es error: la app tiene que
/// arrancar aunque una corrida anterior haya dejado basura. Pero el olvido
/// no es silencioso: el error de lectura queda diagnosticado en stderr, como
/// los avisos del llavero (#95).
pub fn load() -> Settings {
    let (settings, error) = load_con_diagnostico();
    if let Some(error) = error {
        eprintln!(
            "settings.json no se pudo leer; se usan valores de fábrica en solo lectura: {error}"
        );
    }
    settings
}

/// Como `load`, pero conserva el error de lectura o parseo para que el borde
/// pueda avisar en vez de adivinar por qué la app «olvidó» su configuración.
/// Un archivo ausente no es error: devuelve `None`.
fn load_con_diagnostico() -> (Settings, Option<Error>) {
    match leer_settings() {
        Ok(Some(stored)) => (apply_stored(Settings::default(), stored), None),
        Ok(None) => (Settings::default(), None),
        Err(error) => {
            let mut seguro = Settings::default();
            seguro.agent.mode = "readonly".to_owned();
            (seguro, Some(error))
        }
    }
}

/// Como `load`, pero un `settings.json` que existe y no se puede leer o
/// interpretar es un error. Lo usa `editar`: partir de los defaults con un
/// archivo roto borraría, al guardar, todo lo que la persona tenía.
pub(crate) fn load_para_editar() -> Result<Settings, Error> {
    match leer_settings()? {
        Some(stored) => Ok(apply_stored(Settings::default(), stored)),
        None => Ok(Settings::default()),
    }
}

fn leer_settings() -> Result<Option<StoredSettings>, Error> {
    let ruta = data_dir().join(SETTINGS_FILE);
    let datos = match std::fs::read(&ruta) {
        Ok(datos) => datos,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(fuente) => return Err(Error::ReadSettings { ruta, fuente }),
    };
    let stored = serde_json::from_slice::<StoredSettings>(&datos)
        .map_err(|fuente| Error::UnreadableSettings { ruta, fuente })?;
    Ok(Some(stored))
}

fn apply_stored(mut settings: Settings, stored: StoredSettings) -> Settings {
    if let Some(locale) = stored.locale.as_deref().and_then(Locale::from_code) {
        settings.locale = locale;
    }
    settings.client_id = stored.client_id.unwrap_or_default();
    settings.redirect_uri = stored.redirect_uri.unwrap_or_default();
    settings.org_slug = stored.org_slug.unwrap_or_default();
    settings.org_fijada = stored.org_fijada.unwrap_or_default();
    settings.endpoint = stored.endpoint.unwrap_or_default();
    settings.terms_version = stored.terms_version.unwrap_or_default();
    settings.autostart_asked = stored.autostart_asked.unwrap_or_default();
    settings.auto_update = stored.auto_update;
    settings.telemetry = stored.telemetry;

    let agent = stored.agent.unwrap_or_default();
    settings.agent.model = agent.model.unwrap_or_default();
    if let Some(base_url) = agent.base_url.filter(|value| !value.is_empty()) {
        settings.agent.base_url = base_url;
    }
    settings.agent.kind = match agent.kind.as_deref() {
        Some("org") => "org".to_owned(),
        Some("openai") => "openai".to_owned(),
        // Ajustes escritos antes del kind: quien ya tenía modelo propio sigue
        // en su clave.
        _ if !settings.agent.model.is_empty() => "openai".to_owned(),
        _ => "org".to_owned(),
    };
    settings.agent.mode_explicit = agent.mode_explicit.unwrap_or(false);
    if let Some(mode) = agent.mode.map(|value| {
        if matches!(value.as_str(), "autonomous" | "ask" | "readonly") {
            value
        } else {
            "ask".to_owned()
        }
    }) {
        // En 0.7.0 la UI ocultaba el selector de modo para `kind == "org"` y
        // grababa `"ask"` como valor por defecto. Al subir a 0.8+, esos
        // perfiles pasan automáticamente a `"autonomous"` (Turbo), salvo que
        // la persona elija otro modo explícitamente (`mode_explicit == true`).
        if !settings.agent.mode_explicit && settings.agent.kind == "org" && mode == "ask" {
            settings.agent.mode = DEFAULT_AGENT_MODE.to_owned();
        } else {
            settings.agent.mode = mode;
        }
    }
    settings.agent.folders = agent.folders.unwrap_or_default();
    settings.agent.programs = agent.programs.unwrap_or_default();
    for rule in agent.allow_rules.unwrap_or_default() {
        let tool = rule.tool.unwrap_or_default().trim().to_owned();
        let pattern = rule.pattern.unwrap_or_default().trim().to_owned();
        let Some(id) = rule.id.filter(|id| !id.is_empty()) else {
            continue;
        };
        if tool.is_empty() || pattern.is_empty() {
            continue;
        }
        settings.agent.allow_rules.push(AllowRule {
            id,
            tool,
            pattern,
            note: rule.note.unwrap_or_default(),
            created_at: rule.created_at.unwrap_or_else(go_zero_time),
        });
    }
    settings.agent.wake_enabled = agent.wake_enabled.unwrap_or_default();
    settings.agent.wake_per_minute = agent.wake_per_minute.unwrap_or_default();
    settings.agent.wake_per_day = agent.wake_per_day.unwrap_or_default();
    settings.agent.wake_tokens_per_day = agent.wake_tokens_per_day.unwrap_or_default();
    settings.agent.voice_stt_url = agent.voice_stt_url.unwrap_or_default();
    settings.agent.voice_stt_model = agent.voice_stt_model.unwrap_or_default();
    settings.agent.voice_tts_url = agent.voice_tts_url.unwrap_or_default();
    settings.agent.voice_tts_model = agent.voice_tts_model.unwrap_or_default();
    settings.agent.voice_tts_voice = agent.voice_tts_voice.unwrap_or_default();
    settings.agent.window_tokens = agent
        .window_tokens
        .filter(|value| *value > 0)
        .unwrap_or_default();
    settings.agent.cost_input_per_mtok = agent
        .cost_input_per_mtok
        .filter(|value| *value > 0.0)
        .unwrap_or_default();
    settings.agent.cost_output_per_mtok = agent
        .cost_output_per_mtok
        .filter(|value| *value > 0.0)
        .unwrap_or_default();
    settings
}

fn texto_vacio(value: &str) -> bool {
    value.is_empty()
}

fn es_falso(value: &bool) -> bool {
    !*value
}

fn es_cero_i64(value: &i64) -> bool {
    *value == 0
}

fn es_cero_f64(value: &f64) -> bool {
    *value == 0.0
}

// El cero de time.Time en Go se escribe "0001-01-01T00:00:00Z".
fn go_zero_time() -> DateTime<Utc> {
    let naive = NaiveDate::from_ymd_opt(1, 1, 1).and_then(|date| date.and_hms_opt(0, 0, 0));
    match naive {
        Some(naive) => DateTime::from_naive_utc_and_offset(naive, Utc),
        None => DateTime::UNIX_EPOCH,
    }
}

// Espejo tolerante de Settings: en Go un campo ausente queda en su cero y una
// locale desconocida no invalida el resto del archivo. Los Option distinguen
// "ausente" de un valor vacío sin usar serde(default).
#[derive(Default, serde::Deserialize)]
struct StoredSettings {
    locale: Option<String>,
    client_id: Option<String>,
    redirect_uri: Option<String>,
    org_slug: Option<String>,
    org_fijada: Option<bool>,
    endpoint: Option<String>,
    terms_version: Option<String>,
    autostart_asked: Option<bool>,
    auto_update: Option<bool>,
    telemetry: Option<bool>,
    agent: Option<StoredAgentSettings>,
}

#[derive(Default, serde::Deserialize)]
struct StoredAgentSettings {
    kind: Option<String>,
    base_url: Option<String>,
    model: Option<String>,
    mode: Option<String>,
    mode_explicit: Option<bool>,
    folders: Option<Vec<String>>,
    programs: Option<Vec<String>>,
    allow_rules: Option<Vec<StoredAllowRule>>,
    wake_enabled: Option<bool>,
    wake_per_minute: Option<i64>,
    wake_per_day: Option<i64>,
    wake_tokens_per_day: Option<i64>,
    voice_stt_url: Option<String>,
    voice_stt_model: Option<String>,
    voice_tts_url: Option<String>,
    voice_tts_model: Option<String>,
    voice_tts_voice: Option<String>,
    window_tokens: Option<i64>,
    cost_input_per_mtok: Option<f64>,
    cost_output_per_mtok: Option<f64>,
}

#[derive(Default, serde::Deserialize)]
struct StoredAllowRule {
    id: Option<String>,
    tool: Option<String>,
    pattern: Option<String>,
    note: Option<String>,
    created_at: Option<DateTime<Utc>>,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use serial_test::serial;

    use super::*;

    fn en_datos(cuerpo: impl FnOnce()) {
        let dir = tempfile::tempdir().expect("carpeta temporal");
        let path = dir.path().to_str().expect("ruta UTF-8").to_owned();
        temp_env::with_var("ORUSCRIBE_DATA_DIR", Some(path.as_str()), cuerpo);
    }

    /// #95: el diagnóstico distingue «no hay archivo» de «el archivo está
    /// roto», y el arranque tolerante se conserva.
    #[test]
    #[serial]
    fn load_con_diagnostico_distingue_ausente_de_roto() {
        en_datos(|| {
            let (settings, error) = load_con_diagnostico();
            assert!(error.is_none(), "sin archivo no hay error");
            assert_eq!(settings.agent.mode, DEFAULT_AGENT_MODE);

            std::fs::write(data_dir().join(SETTINGS_FILE), r#"{"client_id":7}"#)
                .expect("escribir settings roto");
            let (settings, error) = load_con_diagnostico();
            assert!(
                matches!(error, Some(Error::UnreadableSettings { .. })),
                "el archivo roto tiene que quedar diagnosticado: {error:?}"
            );
            assert_eq!(
                settings.agent.mode, "readonly",
                "un archivo ilegible arranca en solo lectura, no en autónomo"
            );
        });
    }

    /// Un archivo legible no deja diagnóstico.
    #[test]
    #[serial]
    fn load_con_diagnostico_con_archivo_legible_no_deja_error() {
        en_datos(|| {
            std::fs::write(
                data_dir().join(SETTINGS_FILE),
                r#"{"locale":"en","org_slug":"mi-org"}"#,
            )
            .expect("escribir settings");
            let (settings, error) = load_con_diagnostico();
            assert!(error.is_none());
            assert_eq!(settings.org_slug, "mi-org");
            assert_eq!(settings.locale, Locale::English);
        });
    }

    /// #117: la elección hecha en el selector sobrevive al reinicio; un
    /// `settings.json` viejo sin la clave arranca sin org fijada.
    #[test]
    #[serial]
    fn org_fijada_se_lee_y_un_archivo_viejo_no_la_fija() {
        en_datos(|| {
            std::fs::write(
                data_dir().join(SETTINGS_FILE),
                r#"{"org_slug":"talenai","org_fijada":true}"#,
            )
            .expect("escribir settings");
            let settings = load();
            assert_eq!(settings.org_slug, "talenai");
            assert!(settings.org_fijada, "la elección tiene que conservarse");

            std::fs::write(
                data_dir().join(SETTINGS_FILE),
                r#"{"org_slug":"oruscribe"}"#,
            )
            .expect("escribir settings viejo");
            assert!(
                !load().org_fijada,
                "un archivo sin la clave no fija la organización"
            );
        });
    }
}
