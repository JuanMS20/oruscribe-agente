//! Las acciones de la app sobre el agente: configuración, vista, turno y diario.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use orus_agent::{CloudBrain, TokenFn, Tool};
use orus_config::AllowRule;
use orus_core::Message;
use orus_harness::Toolset;
use orus_tools::journal::{self, Journal};
use orus_tools::localtools::{self, Config};
use orus_tools::permissions;

use crate::Secrets;
use crate::sesion::{App, SessionError};

use super::secretos::{clave_para, guardar_clave_para};
use super::turno::AdaptadorMcp;
use super::validacion::{
    carpeta_de_instrucciones, carpetas_permitidas, host_de, pista_clave, programas_permitidos,
    sin_conexion, texto_de_voz, tope, validar_voz_url,
};
use super::vista::visibles;
use super::{AgentError, AgentInput, AgentProbe, AgentView, JournalEntryView, MAX_MODELO, Turno};

/// La costura que arma un proveedor con la base y la clave. Los tests y O6-08
/// la usan para probar sin salir a la red; sin ella, `agent_test` usa la
/// fábrica real.
pub type ProveedorFn = Arc<
    dyn Fn(&str, &str) -> Result<Arc<dyn orus_agent::Provider>, orus_agent::ProviderError>
        + Send
        + Sync,
>;

/// La lectura del llavero del sistema es bloqueante (`CredReadW` en Windows):
/// no puede correr en cada sondeo de la vista. Se cachea por identidad del
/// llavero con vigencia corta y se invalida al cambiar la clave (#95).
struct ClaveCacheada {
    secretos: Arc<dyn Secrets>,
    valor: Result<Option<String>, String>,
    leida: Instant,
}

static CACHE_CLAVE: Mutex<Option<ClaveCacheada>> = Mutex::new(None);
const VIGENCIA_CLAVE: Duration = Duration::from_secs(2);

fn clave_cacheada(secrets: &Arc<dyn Secrets>, base: &str) -> Result<Option<String>, String> {
    let mut cache = crate::sync::lock(&CACHE_CLAVE);
    if let Some(entrada) = cache.as_ref()
        && Arc::ptr_eq(&entrada.secretos, secrets)
        && entrada.leida.elapsed() < VIGENCIA_CLAVE
    {
        return entrada.valor.clone();
    }
    let valor = clave_para(secrets.as_ref(), base);
    *cache = Some(ClaveCacheada {
        secretos: Arc::clone(secrets),
        valor: valor.clone(),
        leida: Instant::now(),
    });
    valor
}

/// Un cambio de clave invalida el cache de la vista: el próximo sondeo lee
/// el llavero otra vez.
fn invalidar_clave_cacheada() {
    *crate::sync::lock(&CACHE_CLAVE) = None;
}

impl App {
    /// Valida, guarda y devuelve la vista nueva. La reserva se toma antes de validar para que un
    /// formulario no cambie el cerebro a mitad de un turno.
    pub fn agent_configure(&self, input: AgentInput) -> Result<AgentView, AgentError> {
        let agent = self.agent().ok_or_else(|| {
            AgentError::Sesion(SessionError::Tool(
                "el agente no está disponible".to_owned(),
            ))
        })?;
        let _reserva = agent.reservar()?;

        let kind = input.kind.trim();
        let kind = if kind.is_empty() { "openai" } else { kind };
        if kind != "org" && kind != "openai" {
            return Err(AgentError::Invalid(
                "el cerebro debe ser org u openai".to_owned(),
            ));
        }
        let mut base = input.base_url.trim().to_owned();
        let modelo = input.model.trim().to_owned();
        if kind == "openai" {
            base = orus_agent::validar_base(&base)
                .map_err(|error| AgentError::Invalid(error.to_string()))?;
            if modelo.is_empty() {
                return Err(AgentError::Invalid(
                    "escribe el modelo que quieres usar".to_owned(),
                ));
            }
            if modelo.chars().count() > MAX_MODELO {
                return Err(AgentError::Invalid(format!(
                    "el modelo no puede tener más de {MAX_MODELO} caracteres"
                )));
            }
            // El modo no se recorta: " ask " no es un modo válido.
            if !matches!(input.mode.as_str(), "autonomous" | "ask" | "readonly") {
                return Err(AgentError::Invalid(
                    "el modo debe ser autonomous, ask o readonly".to_owned(),
                ));
            }
        } else if !input.mode.is_empty()
            && !matches!(input.mode.as_str(), "autonomous" | "ask" | "readonly")
        {
            return Err(AgentError::Invalid(
                "el modo debe ser autonomous, ask o readonly".to_owned(),
            ));
        }
        // Carpetas y programas permitidos se validan y guardan para ambos
        // cerebros: así `IA de Oruscribe` también puede usar el arnés Pi
        // (`read`, `write`, `edit`, `bash`) cuando se autorizan carpetas.
        let carpetas = carpetas_permitidas(&input.folders)?;
        let programas = programas_permitidos(&input.programs)?;
        // Los topes del timbre son un contrato del formulario, no del
        // proveedor: se validan siempre, también con cerebro org (#127).
        if !(0..=30).contains(&input.wake_per_minute) {
            return Err(AgentError::Invalid(
                "las ejecuciones por minuto tienen que estar entre 0 y 30 (0 = valor por defecto)"
                    .to_owned(),
            ));
        }
        if !(0..=500).contains(&input.wake_per_day) {
            return Err(AgentError::Invalid(
                "las ejecuciones por día tienen que estar entre 0 y 500 (0 = valor por defecto)"
                    .to_owned(),
            ));
        }
        if !(0..=200_000).contains(&input.wake_tokens_per_day) {
            return Err(AgentError::Invalid(
                "el presupuesto de tokens por día tiene que estar entre 0 y 200000 (0 = valor por defecto)"
                    .to_owned(),
            ));
        }
        let stt_url = validar_voz_url(&input.voice_stt_url)?;
        let tts_url = validar_voz_url(&input.voice_tts_url)?;
        let stt_model = texto_de_voz(&input.voice_stt_model, "el modelo de transcripción")?;
        let tts_model = texto_de_voz(&input.voice_tts_model, "el modelo de lectura")?;
        let tts_voice = texto_de_voz(&input.voice_tts_voice, "la voz")?;

        let clave = input.api_key.trim().to_owned();
        let mut restaurar_clave: Option<(&dyn Secrets, Option<String>)> = None;
        if kind == "openai" {
            let anterior = self.settings_snapshot().agent.base_url;
            let guardada = match &self.secrets {
                Some(secrets) => secrets.get(),
                None => Ok(None),
            };
            // Un host distinto mandaría la clave anterior a otro servidor.
            if clave.is_empty()
                && matches!(&guardada, Ok(Some(valor)) if !valor.is_empty())
                && host_de(&anterior) != host_de(&base)
            {
                return Err(AgentError::Invalid(
                    "cambiaste de proveedor: escribe también la clave de ese proveedor; la anterior no se manda a otro sitio"
                        .to_owned(),
                ));
            }
            if !clave.is_empty() {
                let Some(secrets) = &self.secrets else {
                    return Err(AgentError::Save(
                        "no hay un llavero de proveedor configurado".to_owned(),
                    ));
                };
                let previa = guardada.map_err(AgentError::Save)?;
                restaurar_clave = Some((secrets.as_ref(), previa));
                guardar_clave_para(secrets.as_ref(), &base, &clave).map_err(AgentError::Save)?;
                invalidar_clave_cacheada();
            }
        }

        let kind = kind.to_owned();
        let guardado = orus_config::editar(move |settings| {
            settings.agent.kind.clone_from(&kind);
            settings.agent.voice_stt_url = stt_url;
            settings.agent.voice_stt_model = stt_model;
            settings.agent.voice_tts_url = tts_url;
            settings.agent.voice_tts_model = tts_model;
            settings.agent.voice_tts_voice = tts_voice;
            if kind == "openai" {
                settings.agent.base_url = base;
                settings.agent.model = modelo;
                settings.agent.mode = input.mode;
                settings.agent.mode_explicit = true;
                settings.agent.folders = carpetas;
                settings.agent.programs = programas;
            } else {
                // Una llamada parcial con solo `kind: "org"` (sin `mode`)
                // conserva carpetas/programas ya guardados del arnés; si llega
                // el formulario completo (`mode` presente) o listas explícitas,
                // se aplican para el arnés Pi.
                let formulario_completo = !input.mode.is_empty();
                if formulario_completo {
                    settings.agent.mode = input.mode;
                    settings.agent.mode_explicit = true;
                    settings.agent.folders = carpetas;
                    settings.agent.programs = programas;
                } else {
                    if !carpetas.is_empty() {
                        settings.agent.folders = carpetas;
                    }
                    if !programas.is_empty() {
                        settings.agent.programs = programas;
                    }
                }
            }
            settings.agent.wake_enabled = input.wake_enabled;
            settings.agent.wake_per_minute = input.wake_per_minute;
            settings.agent.wake_per_day = input.wake_per_day;
            settings.agent.wake_tokens_per_day = input.wake_tokens_per_day;
        });
        if let Err(error) = guardado {
            if let Some((secrets, anterior)) = restaurar_clave {
                let resultado = match &anterior {
                    Some(valor) => secrets.set(valor),
                    None => secrets.delete(),
                };
                invalidar_clave_cacheada();
                if let Err(fallo) = resultado {
                    let _ = secrets.delete();
                    invalidar_clave_cacheada();
                    return Err(AgentError::Save(format!(
                        "{error}; no se pudo restaurar la clave anterior: {fallo}"
                    )));
                }
            }
            return Err(AgentError::Save(error.to_string()));
        }
        // Se relee el disco para que la memoria quede igual a lo guardado.
        self.replace_settings(orus_config::load());
        self.agent_view().map_err(AgentError::Sesion)
    }

    /// Prueba el proveedor del formulario sin guardar nada. Todos los caminos son 200 salvo la validación.
    pub async fn agent_test(&self, input: AgentInput) -> Result<AgentProbe, AgentError> {
        let kind = input.kind.trim();
        let kind = if kind.is_empty() { "openai" } else { kind };
        if kind != "org" && kind != "openai" {
            return Err(AgentError::Invalid(
                "el cerebro debe ser org u openai".to_owned(),
            ));
        }
        // El mismo rechazo que `turno_para` (:420-425): un token syn_ es del
        // MCP, no del cerebro, y la prueba no debe mandarlo como Bearer.
        if kind == "org" {
            // Sin base no hay a dónde ir. Con base y sin token, el corte lo
            // pone el cerebro (`CloudBrain::chat`): su mensaje de sesión es el
            // que la persona necesita, no el de "espera a estar en línea".
            if self.cerebro_base().is_empty() {
                return Ok(AgentProbe {
                    error: "aún no hay conexión con Oruscribe: espera a estar en línea".to_owned(),
                    ..AgentProbe::default()
                });
            }
            let token_actual = self.token_de_sesion();
            if !token_actual.is_empty() && !es_jwt(&token_actual) {
                return Ok(AgentProbe {
                    error: "este token no sirve para el cerebro: inicia sesión con el navegador y vuelve a intentar".to_owned(),
                    ..AgentProbe::default()
                });
            }
        }
        let agent = self.agent().ok_or_else(|| {
            AgentError::Sesion(SessionError::Tool(
                "el agente no está disponible".to_owned(),
            ))
        })?;
        // Probar no reserva: la configuración no cuenta, solo una ejecución viva
        if agent.busy() {
            return Ok(AgentProbe {
                error: "espera a que termine la ejecución en curso y vuelve a probar".to_owned(),
                ..AgentProbe::default()
            });
        }

        let guardado = self.settings_snapshot().agent;
        let modelo = {
            let modelo = input.model.trim();
            if modelo.is_empty() {
                guardado.model.clone()
            } else {
                modelo.to_owned()
            }
        };
        let proveedor: Arc<dyn orus_agent::Provider> = if kind == "org" {
            let base = self.cerebro_base();
            if base.is_empty() {
                return Ok(AgentProbe {
                    error: "aún no hay conexión con Oruscribe: espera a estar en línea".to_owned(),
                    ..AgentProbe::default()
                });
            }
            // El token se lee en cada llamada: es de la sesión, no del cerebro.
            let app = self.clone();
            let token: TokenFn = Box::new(move || app.token_de_sesion());
            Arc::new(CloudBrain::new(
                base,
                Some(token),
                self.settings_snapshot().org_slug,
            ))
        } else {
            let base = {
                let base = input.base_url.trim();
                if base.is_empty() {
                    guardado.base_url.clone()
                } else {
                    base.to_owned()
                }
            };
            if base.is_empty() {
                return Err(AgentError::Invalid(
                    "escribe la dirección del proveedor".to_owned(),
                ));
            }
            let limpia = orus_agent::validar_base(&base)
                .map_err(|error| AgentError::Invalid(error.to_string()))?;
            if modelo.is_empty() {
                return Err(AgentError::Invalid(
                    "escribe el modelo que quieres usar".to_owned(),
                ));
            }
            if modelo.chars().count() > MAX_MODELO {
                return Err(AgentError::Invalid(format!(
                    "el modelo no puede tener más de {MAX_MODELO} caracteres"
                )));
            }
            let clave = {
                let clave = input.api_key.trim();
                if clave.is_empty() {
                    match &self.secrets {
                        Some(secrets) => match clave_para(secrets.as_ref(), &guardado.base_url) {
                            Ok(Some(clave_guardada)) => {
                                // Un host distinto mandaría la clave anterior a
                                // otro servidor.
                                if host_de(&guardado.base_url) != host_de(&limpia) {
                                    return Err(AgentError::Invalid(
                                        "cambiaste de proveedor: escribe también la clave de ese proveedor"
                                            .to_owned(),
                                    ));
                                }
                                clave_guardada
                            }
                            Ok(None) | Err(_) => {
                                return Err(AgentError::Invalid(
                                    "configura el agente antes de usarlo".to_owned(),
                                ));
                            }
                        },
                        None => {
                            return Err(AgentError::Invalid(
                                "configura el agente antes de usarlo".to_owned(),
                            ));
                        }
                    }
                } else {
                    clave.to_owned()
                }
            };
            let construido = match &self.proveedor {
                Some(fabrica) => fabrica(&limpia, &clave),
                None => orus_agent::nuevo_proveedor_por_url(&limpia, &clave, None)
                    .map(|proveedor| Arc::new(proveedor) as Arc<dyn orus_agent::Provider>),
            };
            match construido {
                Ok(proveedor) => proveedor,
                // La misma razón que vería la persona al guardar.
                Err(error) => {
                    return Ok(AgentProbe {
                        error: error.to_string(),
                        ..AgentProbe::default()
                    });
                }
            }
        };

        let peticion = orus_agent::ChatRequest {
            model: modelo,
            messages: vec![Message {
                role: "user".to_owned(),
                content: "Responde solo: ok".to_owned(),
                ..Message::default()
            }],
            tools: Vec::new(),
            on_delta: None,
        };
        let inicio = std::time::Instant::now();
        let respuesta =
            tokio::time::timeout(std::time::Duration::from_secs(20), proveedor.chat(peticion))
                .await;
        let ms = i64::try_from(inicio.elapsed().as_millis()).unwrap_or(i64::MAX);
        let respuesta = match respuesta {
            Ok(Ok(respuesta)) => respuesta,
            Ok(Err(error)) => {
                return Ok(AgentProbe {
                    ms,
                    error: error.to_string(),
                    ..AgentProbe::default()
                });
            }
            Err(_) => {
                return Ok(AgentProbe {
                    ms,
                    error: "el proveedor no respondió en 20 segundos".to_owned(),
                    ..AgentProbe::default()
                });
            }
        };
        let mut texto = respuesta.message.content.trim().to_owned();
        if texto.is_empty() {
            texto = "(respuesta vacía)".to_owned();
        }
        if texto.chars().count() > 160 {
            texto = texto.chars().take(160).collect();
        }
        Ok(AgentProbe {
            ok: true,
            ms,
            reply: texto,
            error: String::new(),
        })
    }

    /// Arma la vista desde los ajustes, el llavero, el agente y el registro de
    /// despertar.
    pub fn agent_view(&self) -> Result<AgentView, SessionError> {
        let agent = self
            .agent()
            .ok_or_else(|| SessionError::Tool("el agente no está disponible".to_owned()))?;
        let cfg = self.settings_snapshot().agent;
        let clave = match &self.secrets {
            Some(secrets) => clave_cacheada(secrets, &cfg.base_url),
            None => Ok(None),
        };
        let (key_hint, clave_presente) = match &clave {
            Ok(Some(clave)) => (pista_clave(clave), true),
            Ok(None) | Err(_) => (String::new(), false),
        };
        let mut last_error = agent.error();
        if let Err(error) = &clave
            && last_error.is_empty()
        {
            last_error.clone_from(error);
        }
        let (tokens_in, tokens_out) = agent.tokens();
        let token = self.token_de_sesion();
        let needs_login = cfg.kind == "org"
            && (token.is_empty() || !es_jwt(&token) || self.cerebro_base().is_empty());
        Ok(AgentView {
            configured: cfg.kind == "org" || (clave_presente && !cfg.model.is_empty()),
            needs_login,
            kind: cfg.kind,
            base_url: cfg.base_url,
            model: cfg.model,
            mode: cfg.mode,
            key_hint,
            running: agent.busy(),
            messages: visibles(&agent.history_vista()),
            steps: agent.steps(),
            approvals: agent.approvals().pending(),
            draft: agent.draft(),
            conv_id: agent.conversation_id().unwrap_or_default(),
            tokens_in,
            tokens_out,
            cost_usd: agent.cost_usd(),
            folders: cfg.folders,
            programs: cfg.programs,
            wake_enabled: cfg.wake_enabled,
            wake_per_minute: tope(cfg.wake_per_minute, 3),
            wake_per_day: tope(cfg.wake_per_day, 20),
            wake_tokens_per_day: tope(cfg.wake_tokens_per_day, 20_000),
            voice_stt_url: cfg.voice_stt_url,
            voice_stt_model: cfg.voice_stt_model,
            voice_tts_url: cfg.voice_tts_url,
            voice_tts_model: cfg.voice_tts_model,
            voice_tts_voice: cfg.voice_tts_voice,
            wake_log: self
                .wake_log
                .as_ref()
                .map(|log| log.entries())
                .unwrap_or_default(),
            last_error,
        })
    }

    /// Arma el turno del envío desde los ajustes, el llavero, la sesión y el
    /// agente. El borde aporta el
    /// texto y la ruta le suma las imágenes ya normalizadas; las herramientas
    /// locales, skills y subagente los resuelve el arnés desde el `Turno`. Para
    /// `kind == "org"` el proveedor ya viaja resuelto y el arnés no construye
    /// `OpenAICompat`.
    pub async fn turno_para(&self, texto: &str) -> Result<Turno, AgentError> {
        let mut turno = self.turno_para_base(texto).await?;
        if turno.cerebro {
            self.resolver_timbre(&mut turno).await?;
        }
        self.preparar_org(turno).await
    }

    pub async fn turno_para_base(&self, texto: &str) -> Result<Turno, AgentError> {
        let cfg = self.settings_snapshot().agent;
        // Si la persona no restringió carpetas específicas, montamos su carpeta
        // personal (~ / %USERPROFILE%) para que el arnés Pi (`read`, `write`,
        // `edit`, `bash`) funcione de inmediato en Documentos, Escritorio, etc.
        let folders = if cfg.folders.is_empty() {
            carpeta_inicio_usuario().into_iter().collect()
        } else {
            cfg.folders
        };
        let cwd = carpeta_de_instrucciones(&folders);
        let modo = if cfg.mode.is_empty() {
            orus_config::DEFAULT_AGENT_MODE.to_owned()
        } else {
            cfg.mode
        };
        if cfg.kind == "org" {
            // El cerebro de la organización es `CloudBrain`: su base sale del
            // timbre y el token se lee del store en cada llamada.
            let _ = self.client().await.map_err(|error| match error {
                SessionError::LoggedOut => AgentError::Sesion(SessionError::LoggedOut),
                otro => {
                    self.report_error(
                        "agent.doorbell",
                        &format!("sin cliente de Oruscribe para armar el turno: {otro}"),
                        serde_json::json!({ "kind": "client" }),
                    );
                    sin_conexion()
                }
            })?;
            let token_de_sesion = self.token_de_sesion();
            if token_de_sesion.is_empty() {
                return Err(AgentError::Sesion(SessionError::LoggedOut));
            }
            if !es_jwt(&token_de_sesion) {
                self.report_error(
                    "agent.jwt",
                    "token de sesión sin formato JWT rechazado antes de chat-agent",
                    serde_json::Value::Null,
                );
                return Err(AgentError::Invalid(
                    "este token no sirve para el cerebro: inicia sesión con el navegador y vuelve a intentar"
                        .to_owned(),
                ));
            }
            let tiene_carpetas = !folders.is_empty();
            return Ok(Turno {
                base_url: String::new(),
                modelo: cfg.model,
                modo,
                cwd,
                folders,
                cerebro: true,
                mensaje: texto.trim().to_owned(),
                imagenes: Vec::new(),
                clave: String::new(),
                proveedor: None,
                programas: cfg.programs,
                toolset: Toolset::Pi,
                max_steps: if tiene_carpetas { 40 } else { 0 },
                no_agents_md: false,
                no_skills: false,
                yes: false,
                allow_exec: false,
                es_tty: false,
                memoria: None,
                org_tools: Vec::new(),
                org_note: String::new(),
                window_tokens: cfg.window_tokens,
                cost_input_per_mtok: cfg.cost_input_per_mtok,
                cost_output_per_mtok: cfg.cost_output_per_mtok,
            });
        }
        if cfg.model.is_empty() {
            return Err(AgentError::Invalid(
                "configura el agente antes de usarlo".to_owned(),
            ));
        }
        let clave = match &self.secrets {
            Some(secrets) => match clave_para(secrets.as_ref(), &cfg.base_url) {
                Ok(Some(clave)) if !clave.is_empty() => clave,
                Ok(_) => {
                    return Err(AgentError::Invalid(
                        "configura el agente antes de usarlo".to_owned(),
                    ));
                }
                // El contrato devuelve el error crudo del llavero, que el borde pinta
                // como 500; no es "falta configurar".
                Err(error) => return Err(AgentError::Turno(error)),
            },
            None => {
                return Err(AgentError::Invalid(
                    "configura el agente antes de usarlo".to_owned(),
                ));
            }
        };
        Ok(Turno {
            base_url: cfg.base_url,
            modelo: cfg.model,
            modo,
            cwd,
            folders,
            cerebro: false,
            mensaje: texto.trim().to_owned(),
            imagenes: Vec::new(),
            clave,
            proveedor: None,
            programas: cfg.programs,
            toolset: Toolset::Pi,
            max_steps: 40,
            no_agents_md: false,
            no_skills: false,
            yes: false,
            allow_exec: false,
            es_tty: false,
            memoria: None,
            org_tools: Vec::new(),
            org_note: String::new(),
            window_tokens: cfg.window_tokens,
            cost_input_per_mtok: cfg.cost_input_per_mtok,
            cost_output_per_mtok: cfg.cost_output_per_mtok,
        })
    }

    async fn resolver_timbre(&self, turno: &mut Turno) -> Result<(), AgentError> {
        let client = self.client().await.map_err(|error| match error {
            SessionError::LoggedOut => AgentError::Sesion(SessionError::LoggedOut),
            otro => {
                self.report_error(
                    "agent.doorbell",
                    &format!("sin cliente de Oruscribe para el cerebro: {otro}"),
                    serde_json::json!({ "kind": "client" }),
                );
                sin_conexion()
            }
        })?;
        let timbre = client.doorbell().await.map_err(|error| {
            self.report_error(
                "agent.doorbell",
                &format!("get_my_doorbell falló: {error}"),
                serde_json::json!({ "kind": "doorbell" }),
            );
            sin_conexion()
        })?;
        let base = timbre.supabase_url.trim_end_matches('/');
        if base.is_empty() {
            self.report_error(
                "agent.doorbell",
                "el timbre no trae supabase_url",
                serde_json::json!({ "kind": "doorbell" }),
            );
            return Err(sin_conexion());
        }
        let base = format!("{base}/functions/v1");
        let app = self.clone();
        let token: TokenFn = Box::new(move || app.token_de_sesion());
        turno.proveedor = Some(Arc::new(
            CloudBrain::new(base.clone(), Some(token), self.settings_snapshot().org_slug)
                .with_workdir(turno.cwd.clone()),
        ));
        turno.base_url = base;
        Ok(())
    }

    async fn herramientas_mcp(&self) -> (Vec<Arc<dyn Tool>>, String) {
        match self.client().await {
            Ok(client) => match client.tool_specs().await {
                Ok(specs) => (
                    orus_harness::mcptools::build(
                        Some(Arc::new(AdaptadorMcp {
                            backend: client,
                            telemetria: self.telemetry(),
                        })),
                        specs,
                        &self.registro_lecturas,
                    ),
                    String::new(),
                ),
                Err(error) => {
                    self.report_error(
                        "mcp.specs",
                        &format!("no pude leer las herramientas de Oruscribe: {error}"),
                        serde_json::json!({ "kind": "specs" }),
                    );
                    (
                        Vec::new(),
                        format!("no pude leer las herramientas de Oruscribe: {error}"),
                    )
                }
            },
            Err(error) => (
                Vec::new(),
                format!(
                    "sin sesión de Oruscribe: el agente local sigue sin sus herramientas ({error})"
                ),
            ),
        }
    }

    pub async fn preparar_org(&self, mut turno: Turno) -> Result<Turno, AgentError> {
        if turno.cerebro {
            if turno.proveedor.is_none() {
                self.resolver_timbre(&mut turno).await?;
            }
            return Ok(turno);
        }
        let (org_tools, org_note) = self.herramientas_mcp().await;
        turno.org_tools = org_tools;
        turno.org_note = org_note;
        Ok(turno)
    }

    /// La clave del proveedor solo si el servidor de voz vive en su mismo host
    /// Dos hosts vacíos no cuentan
    /// como coincidencia: sin destino no se manda la credencial.
    pub fn clave_de_voz(&self, destino: &str) -> Option<String> {
        let proveedor = self.settings_snapshot().agent.base_url;
        let host = host_de(destino);
        if host.is_empty() || host != host_de(&proveedor) {
            return None;
        }
        match &self.secrets {
            Some(secrets) => match clave_para(secrets.as_ref(), &proveedor) {
                Ok(Some(clave)) if !clave.is_empty() => Some(clave),
                Ok(_) | Err(_) => None,
            },
            None => None,
        }
    }

    /// Las escrituras respaldadas, de la más reciente a la más vieja
    /// Sin diario, la lista es `[]`.
    pub fn journal_entries(&self) -> Result<Vec<JournalEntryView>, SessionError> {
        let diario = Journal::open(journal::default_dir())
            .map_err(|error| SessionError::Tool(error.to_string()))?;
        let entradas = diario
            .list()
            .map_err(|error| SessionError::Tool(error.to_string()))?;
        Ok(entradas
            .into_iter()
            .map(|entrada| JournalEntryView {
                id: entrada.id,
                path: entrada.path.to_string_lossy().into_owned(),
                existed: entrada.existed,
                size: entrada.size,
                when: entrada.when,
            })
            .collect())
    }

    /// Las reglas recordadas, de la más vieja a la más nueva
    pub fn rules(&self) -> Vec<AllowRule> {
        permissions::list()
    }

    /// Quita una regla; `false` dice que no existía
    pub fn revoke_rule(&self, id: &str) -> Result<bool, SessionError> {
        permissions::revoke(id).map_err(|error| SessionError::Tool(error.to_string()))
    }

    /// Deshace las últimas `n` escrituras dentro de las carpetas permitidas
    pub fn agent_undo(&self, n: i64, force: bool) -> Result<usize, SessionError> {
        let n = n.clamp(1, 50);
        let agent = self
            .agent()
            .ok_or_else(|| SessionError::Tool("el agente no está disponible".to_owned()))?;
        // La reserva es el mismo candado que `send`: deshacer mientras el
        // agente escribe pierde una entrada del diario.
        let _reserva = match agent.reservar() {
            Ok(reserva) => reserva,
            Err(AgentError::Busy) => {
                return Err(SessionError::Ocupado(AgentError::Busy.to_string()));
            }
            Err(error) => return Err(SessionError::Tool(error.to_string())),
        };
        let cfg = self.settings_snapshot().agent;
        let mut validas: Vec<String> = Vec::new();
        for folder in cfg.folders {
            let folder = folder.trim();
            if folder.is_empty() || validas.iter().any(|vista| vista == folder) {
                continue;
            }
            if Path::new(folder).is_dir() {
                validas.push(folder.to_owned());
            }
        }
        let diario = Journal::open(journal::default_dir())
            .map_err(|error| SessionError::Tool(error.to_string()))?;
        let entradas = diario
            .list()
            .map_err(|error| SessionError::Tool(error.to_string()))?;
        if validas.is_empty()
            && !entradas.is_empty()
            && let Some(inicio) = carpeta_inicio_usuario()
        {
            validas.push(inicio);
        }
        if validas.is_empty() {
            return Err(SessionError::Invalido(
                "no hay carpetas permitidas configuradas que existan; no puedo deshacer con seguridad"
                    .to_owned(),
            ));
        }
        let locales = localtools::Set::new(Config {
            folders: validas,
            ..Config::default()
        })
        .map_err(|error| SessionError::Invalido(error.to_string()))?;
        if entradas.is_empty() {
            return Err(SessionError::Invalido(
                "no hay nada que deshacer".to_owned(),
            ));
        }
        let n = n.min(entradas.len() as i64);
        let guard = |ruta: &Path| {
            locales
                .check(ruta.to_string_lossy().as_ref())
                .map_err(|error| error.to_string())
        };
        let (hechas, error) = diario.undo(n, Some(&guard), force);
        match error {
            // Un deshacer parcial no se informa como si no pasara nada: las
            // ya deshechas quedan hechas y la persona tiene que saberlo (#95).
            Some(error) if hechas > 0 => Err(SessionError::Tool(format!(
                "se deshicieron {hechas} de {n} escrituras; luego falló: {error}"
            ))),
            Some(error) => Err(SessionError::Tool(error.to_string())),
            None => Ok(hechas),
        }
    }
}

/// Carpeta personal del usuario (`%USERPROFILE%` en Windows, `$HOME` en Unix)
/// usada por defecto cuando la persona no restringió carpetas específicas,
/// habilitando el arnés Pi (`read`, `write`, `edit`, `bash`) en Documentos,
/// Escritorio y proyectos desde el primer instante.
fn carpeta_inicio_usuario() -> Option<String> {
    let candidata = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    let ruta = std::path::PathBuf::from(candidata);
    if ruta.is_absolute() && ruta.is_dir() {
        Some(ruta.to_string_lossy().into_owned())
    } else {
        None
    }
}

/// Comprueba si un token tiene forma básica de JWT (`header.payload.signature`)
/// sin espacios ni caracteres de control: los tokens opacos de MCP (`syn_...`,
/// `oru_...`, etc.) funcionan en `mcp-server` pero fallan en `chat-agent` con
/// `401 UNAUTHORIZED_INVALID_JWT_FORMAT`.
fn es_jwt(token: &str) -> bool {
    let token = token.trim();
    if token.is_empty() || token.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return false;
    }
    let mut partes = token.split('.');
    match (partes.next(), partes.next(), partes.next(), partes.next()) {
        (Some(a), Some(b), Some(c), None) => !a.is_empty() && !b.is_empty() && !c.is_empty(),
        _ => false,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use orus_auth::{Error as AuthError, Store, Token};
    use orus_config::{AgentSettings, Settings};

    use crate::agent::Secrets;
    use crate::sesion::{App, Backend, Connector, Refresh, SessionError};

    struct StoreVacio;

    impl Store for StoreVacio {
        fn save(&self, _: Token) -> Result<(), AuthError> {
            Ok(())
        }

        fn load(&self) -> Result<Token, AuthError> {
            Err(AuthError::NoSession)
        }

        fn clear(&self) -> Result<(), AuthError> {
            Ok(())
        }
    }

    struct SinRed;

    #[async_trait]
    impl Connector for SinRed {
        async fn open(&self, _: &str, _: &str, _: &str) -> Result<Arc<dyn Backend>, SessionError> {
            Err(SessionError::Connect("sin red".to_owned()))
        }
    }

    #[async_trait]
    impl Refresh for SinRed {
        async fn refresh(&self, _: &Token) -> Result<Token, SessionError> {
            Err(SessionError::RefreshRejected)
        }
    }

    struct Clave(Option<&'static str>);

    impl Secrets for Clave {
        fn get(&self) -> Result<Option<String>, String> {
            Ok(self.0.map(str::to_owned))
        }

        fn set(&self, _: &str) -> Result<(), String> {
            Ok(())
        }

        fn delete(&self) -> Result<(), String> {
            Ok(())
        }
    }

    struct ClaveRota;

    impl Secrets for ClaveRota {
        fn get(&self) -> Result<Option<String>, String> {
            Err("llavero roto".to_owned())
        }

        fn set(&self, _: &str) -> Result<(), String> {
            Err("llavero roto".to_owned())
        }

        fn delete(&self) -> Result<(), String> {
            Err("llavero roto".to_owned())
        }
    }

    fn app_con(base_url: &str, secretos: Arc<dyn Secrets>) -> App {
        App::new(
            Box::new(StoreVacio),
            Settings {
                agent: AgentSettings {
                    base_url: base_url.to_owned(),
                    ..AgentSettings::default()
                },
                ..Settings::default()
            },
            Arc::new(SinRed),
            Arc::new(SinRed),
        )
        .with_secrets(secretos)
    }

    #[test]
    fn clave_de_voz_solo_viaja_al_mismo_host() {
        let app = app_con("http://127.0.0.1:8080/v1", Arc::new(Clave(Some("clave"))));
        assert_eq!(
            app.clave_de_voz("http://127.0.0.1:8080/audio/speech"),
            Some("clave".to_owned())
        );
        assert_eq!(
            app.clave_de_voz("http://127.0.0.1:9090/audio/speech"),
            None,
            "otro puerto es otro host y no recibe la clave"
        );
        assert_eq!(
            app.clave_de_voz("https://otro.test/audio/transcriptions"),
            None
        );
    }

    #[test]
    fn clave_de_voz_no_manda_clave_con_hosts_vacios() {
        let app = app_con("", Arc::new(Clave(Some("clave"))));
        assert_eq!(app.clave_de_voz(""), None, "dos vacíos no son coincidencia");
        assert_eq!(app.clave_de_voz("http://127.0.0.1:8080/audio/speech"), None);

        let app = app_con("http://127.0.0.1:8080/v1", Arc::new(Clave(Some("clave"))));
        assert_eq!(
            app.clave_de_voz(""),
            None,
            "sin destino no hay host que comparar"
        );
    }

    #[test]
    fn clave_de_voz_sin_llavero_o_sin_clave_es_none() {
        let app = app_con("http://127.0.0.1:8080/v1", Arc::new(Clave(None)));
        assert_eq!(app.clave_de_voz("http://127.0.0.1:8080/audio/speech"), None);

        let app = app_con("http://127.0.0.1:8080/v1", Arc::new(Clave(Some(""))));
        assert_eq!(app.clave_de_voz("http://127.0.0.1:8080/audio/speech"), None);

        let app = app_con("http://127.0.0.1:8080/v1", Arc::new(ClaveRota));
        assert_eq!(
            app.clave_de_voz("http://127.0.0.1:8080/audio/speech"),
            None,
            "un llavero roto no manda una credencial a medias"
        );
    }

    use std::process::Command;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use orus_agent::CancelToken;
    use orus_harness::ErrorTurno;
    use orus_harness::sessions::Session;
    use orus_org::doorbell::Config as DoorbellConfig;

    use crate::agent::{Agent, AgentError, AgentInput, Emisor, TurnRunner, Turno};
    use crate::approvals::ApprovalQueue;
    use crate::conversations::Conversations;

    struct RunnerNunca;

    #[async_trait]
    impl TurnRunner for RunnerNunca {
        async fn run(
            &self,
            _: Turno,
            _: &mut Session,
            _: CancelToken,
            _: Emisor,
        ) -> Result<(), ErrorTurno> {
            Ok(())
        }
    }

    fn app_con_agente(base_url: &str, secretos: Arc<dyn Secrets>) -> App {
        app_con(base_url, secretos).with_agent(Arc::new(Agent::new(
            Conversations::new(),
            Arc::new(ApprovalQueue::new()),
            Arc::new(RunnerNunca),
        )))
    }

    fn app_con_clave(base_url: &str, cwd: &str) -> App {
        App::new(
            Box::new(StoreVacio),
            Settings {
                agent: AgentSettings {
                    kind: "openai".to_owned(),
                    base_url: base_url.to_owned(),
                    model: "gpt-prueba".to_owned(),
                    folders: vec![cwd.to_owned()],
                    ..AgentSettings::default()
                },
                ..Settings::default()
            },
            Arc::new(SinRed),
            Arc::new(SinRed),
        )
        .with_secrets(Arc::new(Clave(Some("clave"))))
        .with_agent(Arc::new(Agent::new(
            Conversations::new(),
            Arc::new(ApprovalQueue::new()),
            Arc::new(RunnerNunca),
        )))
    }

    #[tokio::test]
    async fn mi_clave_usa_pi_y_cuarenta_pasos() {
        let cwd = std::env::temp_dir();
        let app = app_con_clave("http://127.0.0.1:1/v1", &cwd.to_string_lossy());
        let turno = app
            .turno_para("hola")
            .await
            .expect("Mi clave no puede exigir la sesión de Oruscribe");
        assert_eq!(turno.toolset, crate::agent::Toolset::Pi);
        assert_eq!(turno.max_steps, 40);
        assert!(!turno.cerebro);
        assert!(turno.org_tools.is_empty());
    }

    /// Un llavero que cuenta sus lecturas y recuerda lo que le guardan.
    struct ClaveContada {
        lecturas: Arc<AtomicUsize>,
        valor: std::sync::Mutex<String>,
    }

    impl Secrets for ClaveContada {
        fn get(&self) -> Result<Option<String>, String> {
            self.lecturas.fetch_add(1, Ordering::SeqCst);
            Ok(Some(self.valor.lock().expect("candado").clone()))
        }

        fn set(&self, clave: &str) -> Result<(), String> {
            *self.valor.lock().expect("candado") = clave.to_owned();
            Ok(())
        }

        fn delete(&self) -> Result<(), String> {
            Ok(())
        }
    }

    /// Corre `cuerpo` en un proceso hijo con su propio `ORUSCRIBE_DATA_DIR`:
    /// los tests de un mismo binario comparten entorno. `nombre` es la ruta
    /// completa del test en este binario (p. ej. `agent::app::tests::x`).
    fn en_datos_aislados(nombre: &str, cuerpo: fn()) {
        const MARCA_HIJO: &str = "ORUSCRIBE_AGENT_APP_TEST_HIJO";
        if std::env::var_os(MARCA_HIJO).is_some() {
            cuerpo();
            return;
        }
        let base = tempfile::tempdir().expect("carpeta temporal");
        let exe = std::env::current_exe().expect("ruta del binario de tests");
        let salida = Command::new(exe)
            .args(["--exact", nombre, "--test-threads=1", "--nocapture"])
            .env(MARCA_HIJO, "1")
            .env("ORUSCRIBE_DATA_DIR", base.path().join("datos"))
            .output()
            .expect("correr el test aislado en su proceso");
        assert!(
            salida.status.success(),
            "el test aislado {nombre} falló en su proceso: {}",
            String::from_utf8_lossy(&salida.stderr)
        );
        let stdout = String::from_utf8_lossy(&salida.stdout);
        assert!(
            stdout.contains("1 passed"),
            "el hijo no ejecutó {nombre}: {stdout}"
        );
    }

    /// #95: el sondeo de la vista no puede leer el llavero del sistema en
    /// cada llamada: la pista de clave se cachea fuera del camino caliente.
    #[test]
    fn agent_view_no_lee_el_llavero_en_cada_sondeo() {
        let lecturas = Arc::new(AtomicUsize::new(0));
        let secretos = Arc::new(ClaveContada {
            lecturas: Arc::clone(&lecturas),
            valor: std::sync::Mutex::new("clave-larga-1234".to_owned()),
        });
        let app = app_con_agente("http://127.0.0.1:8080/v1", secretos);
        for _ in 0..5 {
            app.agent_view().expect("la vista sale");
        }
        assert_eq!(
            lecturas.load(Ordering::SeqCst),
            1,
            "el llavero se leyó en cada sondeo"
        );
    }

    /// Cambiar la clave invalida el cache: la vista siguiente muestra la
    /// clave nueva, no la cacheada (#95).
    #[test]
    fn cambiar_la_clave_invalida_el_cache_de_la_vista() {
        en_datos_aislados(
            "agent::app::tests::cambiar_la_clave_invalida_el_cache_de_la_vista",
            || {
                let lecturas = Arc::new(AtomicUsize::new(0));
                let secretos = Arc::new(ClaveContada {
                    lecturas: Arc::clone(&lecturas),
                    valor: std::sync::Mutex::new("clave-larga-1234".to_owned()),
                });
                let app = app_con_agente(
                    "http://127.0.0.1:8080/v1",
                    secretos.clone() as Arc<dyn Secrets>,
                );
                let vista = app.agent_view().expect("la vista sale");
                assert_eq!(vista.key_hint, "1234");
                let antes = lecturas.load(Ordering::SeqCst);

                let input = AgentInput {
                    kind: "openai".to_owned(),
                    base_url: "http://127.0.0.1:8080/v1".to_owned(),
                    model: "modelo".to_owned(),
                    mode: "ask".to_owned(),
                    api_key: "clave-nueva-5678".to_owned(),
                    ..AgentInput::default()
                };
                app.agent_configure(input).expect("configurar");

                let vista = app.agent_view().expect("la vista tras configurar");
                assert!(
                    lecturas.load(Ordering::SeqCst) > antes,
                    "la vista no volvió a leer el llavero tras cambiar la clave"
                );
                assert_eq!(vista.key_hint, "5678", "la vista muestra la clave vieja");

                let guardada = secretos.valor.lock().expect("candado").clone();
                assert!(
                    guardada.starts_with("127.0.0.1:8080\n"),
                    "la clave no quedó ligada al host: {guardada:?}"
                );
                *secretos.valor.lock().expect("candado") =
                    "127.0.0.1:9090\nclave-nueva-5678".to_owned();
                assert_eq!(
                    app.clave_de_voz("http://127.0.0.1:8080/audio/speech"),
                    None,
                    "una clave escrita para otro host no puede llegar al host guardado"
                );
            },
        );
    }

    /// #111: el interruptor y los topes del timbre se guardan también cuando
    /// el cerebro es org, no solo con openai.
    #[test]
    fn configurar_org_guarda_el_despertar() {
        en_datos_aislados(
            "agent::app::tests::configurar_org_guarda_el_despertar",
            || {
                let app = app_con_agente("http://127.0.0.1:8080/v1", Arc::new(Clave(None)));
                let entrada = AgentInput {
                    kind: "org".to_owned(),
                    wake_enabled: true,
                    wake_per_minute: 7,
                    wake_per_day: 42,
                    wake_tokens_per_day: 1234,
                    ..AgentInput::default()
                };
                app.agent_configure(entrada).expect("configurar org");

                let cfg = orus_config::load();
                assert!(cfg.agent.wake_enabled, "org no guardó wake_enabled");
                assert_eq!(
                    cfg.agent.wake_per_minute, 7,
                    "org no guardó el tope por minuto"
                );
                assert_eq!(cfg.agent.wake_per_day, 42, "org no guardó el tope por día");
                assert_eq!(
                    cfg.agent.wake_tokens_per_day, 1234,
                    "org no guardó el tope de tokens"
                );
            },
        );
    }

    /// #127: los topes del timbre son un contrato del formulario, no del
    /// proveedor. Con cerebro org un valor de 1.000.000/min se guardaba sin
    /// rechazo y el servidor lo aplicaba como límite efectivo.
    #[test]
    fn configurar_org_rechaza_topes_fuera_de_rango() {
        en_datos_aislados(
            "agent::app::tests::configurar_org_rechaza_topes_fuera_de_rango",
            || {
                let app = app_con_agente("http://127.0.0.1:8080/v1", Arc::new(Clave(None)));
                let casos = [
                    AgentInput {
                        kind: "org".to_owned(),
                        wake_per_minute: 1_000_000,
                        ..AgentInput::default()
                    },
                    AgentInput {
                        kind: "org".to_owned(),
                        wake_per_day: 501,
                        ..AgentInput::default()
                    },
                    AgentInput {
                        kind: "org".to_owned(),
                        wake_tokens_per_day: 200_001,
                        ..AgentInput::default()
                    },
                ];
                for entrada in casos {
                    let error = app
                        .agent_configure(entrada.clone())
                        .expect_err("el tope fuera de rango debe rechazarse con org");
                    assert!(
                        matches!(error, AgentError::Invalid(_)),
                        "el rechazo debe ser de validación: {error}"
                    );
                }
                let cfg = orus_config::load();
                assert_eq!(
                    cfg.agent.wake_per_minute, 0,
                    "el tope inválido no debe persistirse"
                );
                assert_eq!(
                    cfg.agent.wake_per_day, 0,
                    "el tope inválido no debe persistirse"
                );
                assert_eq!(
                    cfg.agent.wake_tokens_per_day, 0,
                    "el tope inválido no debe persistirse"
                );
            },
        );
    }

    /// #95: un deshacer parcial tiene que informar cuántas escrituras se
    /// deshicieron antes del fallo, no responder como si no pasara nada.
    #[test]
    fn deshacer_parcial_informa_lo_ya_deshecho() {
        en_datos_aislados(
            "agent::app::tests::deshacer_parcial_informa_lo_ya_deshecho",
            || {
                let datos = orus_config::data_dir();
                let permitida = datos.join("permitida");
                std::fs::create_dir_all(&permitida).expect("carpeta permitida");
                let dentro = permitida.join("dentro.txt");
                std::fs::write(&dentro, b"nuevo").expect("archivo dentro");
                let fuera = datos.join("fuera.txt");
                std::fs::write(&fuera, b"nuevo").expect("archivo fuera");
                let diario = datos.join("journal");
                std::fs::create_dir_all(&diario).expect("carpeta del diario");
                let entrada = |id: &str, ruta: &std::path::Path| {
                    serde_json::json!({
                        "id": id,
                        "path": ruta.to_string_lossy(),
                        "existed": false,
                        "backup": "",
                        "sha_after": "",
                        "size": 5,
                        "when": "2026-01-01T00:00:00Z"
                    })
                };
                // La más vieja quedó fuera de la carpeta permitida; la más nueva
                // dentro. Deshacer las dos aplica la primera y falla en la segunda.
                let lineas = format!("{}\n{}\n", entrada("1", &fuera), entrada("2", &dentro));
                std::fs::write(diario.join("journal.jsonl"), lineas).expect("diario");

                let app = App::new(
                    Box::new(StoreVacio),
                    Settings {
                        agent: AgentSettings {
                            folders: vec![permitida.to_string_lossy().into_owned()],
                            ..AgentSettings::default()
                        },
                        ..Settings::default()
                    },
                    Arc::new(SinRed),
                    Arc::new(SinRed),
                )
                .with_agent(Arc::new(Agent::new(
                    Conversations::new(),
                    Arc::new(ApprovalQueue::new()),
                    Arc::new(RunnerNunca),
                )));

                let error = app
                    .agent_undo(2, true)
                    .expect_err("la escritura fuera de la carpeta no se puede deshacer");
                let texto = error.to_string();
                assert!(
                    texto.contains("se deshicieron 1"),
                    "el deshacer parcial se informó como si no pasara nada: {texto}"
                );
                assert!(!dentro.exists(), "la escritura ya deshecha queda deshecha");
                assert!(
                    fuera.exists(),
                    "la escritura fuera de la carpeta no se toca"
                );
            },
        );
    }

    #[tokio::test]
    async fn token_opaco_no_parece_listo_y_fallo_del_timbre_no_es_nota() {
        struct ConSyn;

        impl Store for ConSyn {
            fn save(&self, _: Token) -> Result<(), AuthError> {
                Ok(())
            }

            fn load(&self) -> Result<Token, AuthError> {
                serde_json::from_value(serde_json::json!({"access_token": "syn_opaco"}))
                    .map_err(|_| AuthError::NoSession)
            }

            fn clear(&self) -> Result<(), AuthError> {
                Ok(())
            }
        }

        let mut settings = Settings::default();
        settings.agent.kind = "org".to_owned();
        let app = App::new(
            Box::new(ConSyn),
            settings,
            Arc::new(SinRed),
            Arc::new(SinRed),
        )
        .with_agent(Arc::new(Agent::new(
            Conversations::new(),
            Arc::new(ApprovalQueue::new()),
            Arc::new(RunnerNunca),
        )));
        app.set_bell_config(DoorbellConfig {
            supabase_url: "http://127.0.0.1:1".to_owned(),
            ..DoorbellConfig::default()
        });
        assert!(
            app.agent_view().expect("la vista sale").needs_login,
            "un token syn_ no puede parecer una sesión lista para el cerebro"
        );

        let local = app_con_clave(
            "http://127.0.0.1:1/v1",
            &std::env::temp_dir().to_string_lossy(),
        );
        let mut turno = local
            .turno_para("hola")
            .await
            .expect("Mi clave arma el turno");
        turno.cerebro = true;
        turno.proveedor = None;
        let error = local
            .preparar_org(turno)
            .await
            .err()
            .expect("sin timbre no hay proveedor, aunque haya nota");
        assert!(
            matches!(error, AgentError::Sesion(_)) || error.to_string().contains("conexión"),
            "el fallo del timbre se disfrazó de nota: {error}"
        );

        let agente = local.agent().expect("la app lleva el agente");
        let turno = local
            .turno_para("hola")
            .await
            .expect("Mi clave arma el turno");
        agente
            .empezar_con("hola", turno, |_| {
                Box::pin(async move { Err(AgentError::Invalid("timbre caído".to_owned())) })
            })
            .expect("el turno arranca");
        tokio::task::yield_now().await;
        assert_eq!(
            agente.error(),
            "timbre caído",
            "un fallo de preparación no llegó al estado del agente"
        );
        assert!(!agente.busy());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn un_panico_o_un_stop_durante_la_preparacion_no_dejan_el_agente_ocupado() {
        let local = app_con_clave(
            "http://127.0.0.1:1/v1",
            &std::env::temp_dir().to_string_lossy(),
        );
        let agente = local.agent().expect("la app lleva el agente");

        let turno = local.turno_para("hola").await.expect("arma el turno");
        agente
            .empezar_con("hola", turno, |_| {
                Box::pin(async move { panic!("preparación rota") })
            })
            .expect("el turno arranca");
        for _ in 0..100 {
            if !agente.busy() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(
            !agente.busy(),
            "un pánico en la preparación dejó el agente ocupado"
        );

        let turno = local.turno_para("hola").await.expect("arma el turno");
        agente
            .empezar_con("hola", turno, |_| {
                Box::pin(async move { std::future::pending().await })
            })
            .expect("el turno arranca");
        assert!(agente.busy());
        agente.cancel();
        for _ in 0..100 {
            if !agente.busy() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(!agente.busy(), "Stop no cortó la preparación");
    }
}
