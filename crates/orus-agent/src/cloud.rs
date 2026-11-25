//! Cerebro de la organización: el asistente de chat-agent.
//!
//! Port de `internal/agent/cloud.go:1-151`. El modelo lo elige la organización
//! y sus herramientas corren del lado del servidor, así que aquí no viajan ni
//! modelo ni herramientas locales: solo la conversación. El token de la sesión
//! llega por función y nunca se guarda en el agente.

use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use orus_core::{Message, ToolCall};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::{ProviderError, SIN_RESPUESTA, respuesta_vacia};
use crate::openai::{
    MAX_CUERPO_ERR, MAX_RESPUESTA, cliente_http, enviar_con_reintentos, validar_base,
};
use crate::types::{ChatReply, ChatRequest, Provider, ToolSpec};

/// Devuelve el token de la sesión en el momento de la llamada. Vacío significa
/// sin sesión. Copia de `Token func() string` (`cloud.go:25`).
pub type TokenFn = Box<dyn Fn() -> String + Send + Sync>;

/// Habla con el asistente de la organización (`/functions/v1/chat-agent`).
///
/// Soporta dos capas complementarias para ejecutar herramientas locales Pi
/// (`read`, `write`, `edit`, `bash`) en el equipo del usuario:
/// 1. Nativa: envía `tools` en formato OpenAI y procesa `tool_calls` JSON si
///    el servidor los devuelve (con fallback automático si responde 400/422).
/// 2. Puente de arnés de escritorio (`<orus_tool>`): inyecta el contexto del
///    equipo y extrae bloques `<orus_tool>{"name":...,"args":...}</orus_tool>`
///    de la respuesta textual, permitiendo que `IA de Oruscribe` lea, cree,
///    edite archivos y ejecute comandos en el PC del usuario hoy mismo sin
///    esperar cambios en el edge function remoto.
pub struct CloudBrain {
    /// Raíz de las funciones, por ejemplo
    /// `https://<proyecto>.supabase.co/functions/v1`; `/chat-agent` se agrega
    /// aquí.
    pub base_url: String,
    /// Token de la sesión en el momento de la llamada.
    pub token: Option<TokenFn>,
    /// Organización activa: viaja como `x-org-slug` a `/chat-agent` (issue #114).
    pub org_slug: String,
    /// Carpeta base de trabajo local informada al modelo cuando hay herramientas Pi.
    pub workdir: String,
    /// Si el servidor acepta `tools` locales en `/chat-agent`. Se apaga solo
    /// si responde 400/422 a una petición con `tools`.
    soporta_tools: AtomicBool,
    /// Se enciende solo si el servidor devolvió `tool_calls` nativos en JSON.
    /// Mientras sea `false`, el historial viaja en roles clásicos `user`/`assistant`
    /// para garantizar compatibilidad con el edge function de producción.
    usa_tool_calls_nativos: AtomicBool,
}

#[derive(Serialize)]
struct PeticionCerebro {
    messages: Vec<MensajeCerebro>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<HerramientaCerebro>,
}

#[derive(Serialize)]
struct HerramientaCerebro {
    #[serde(rename = "type")]
    kind: &'static str,
    function: FuncionCerebro,
}

#[derive(Serialize)]
struct FuncionCerebro {
    name: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    description: String,
    parameters: Value,
}

#[derive(Serialize)]
struct MensajeCerebro {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tool_calls: Vec<LlamadaEnviadaCerebro>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

#[derive(Serialize)]
struct LlamadaEnviadaCerebro {
    id: String,
    #[serde(rename = "type")]
    kind: &'static str,
    function: FuncionLlamadaEnviada,
}

#[derive(Serialize)]
struct FuncionLlamadaEnviada {
    name: String,
    arguments: String,
}

#[derive(Deserialize, Default)]
struct RespuestaCerebro {
    #[serde(default)]
    content: String,
    #[serde(default)]
    tool_calls: Vec<LlamadaRecibidaCerebro>,
    #[serde(default)]
    error: Option<ErrorCerebro>,
}

#[derive(Deserialize)]
struct LlamadaRecibidaCerebro {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    args: Option<Value>,
    #[serde(default)]
    arguments: Option<Value>,
    #[serde(default)]
    function: Option<FuncionRecibidaCerebro>,
}

#[derive(Deserialize)]
struct FuncionRecibidaCerebro {
    #[serde(default)]
    name: String,
    #[serde(default)]
    arguments: Option<Value>,
}

#[derive(Deserialize)]
struct ErrorCerebro {
    #[serde(default)]
    message: String,
}

impl CloudBrain {
    /// Crea un cliente para `/chat-agent` con soporte de herramientas locales
    /// habilitado y fallback automático si el servidor aún es solo texto.
    pub fn new(base_url: String, token: Option<TokenFn>, org_slug: String) -> Self {
        Self {
            base_url,
            token,
            org_slug,
            workdir: String::new(),
            soporta_tools: AtomicBool::new(true),
            usa_tool_calls_nativos: AtomicBool::new(false),
        }
    }

    /// Informa al puente local qué carpeta base está autorizada en este equipo.
    pub fn with_workdir(mut self, workdir: String) -> Self {
        self.workdir = workdir;
        self
    }

    /// Manda la conversación al cerebro y devuelve su respuesta.
    pub async fn chat(&self, req: ChatRequest) -> Result<ChatReply, ProviderError> {
        let base = validar_base(&self.base_url)?;
        let token = match &self.token {
            Some(token) => token(),
            None => String::new(),
        };
        if token.is_empty() {
            return Err(ProviderError::local(
                "el cerebro de la organización necesita una sesión activa: inicia sesión y vuelve a intentar",
            ));
        }
        // El contrato de chat-agent es solo texto. La imagen que se está
        // enviando ahora no puede desaparecer en silencio: haría creer que el
        // modelo la vio.
        for mensaje in req.messages.iter().rev() {
            if mensaje.role != "user" {
                continue;
            }
            if mensaje
                .images
                .as_ref()
                .is_some_and(|imagenes| !imagenes.is_empty())
            {
                return Err(ProviderError::local(
                    "el cerebro de la organización todavía no recibe imágenes: usa tu propia clave o quita la imagen",
                ));
            }
            break;
        }
        let url = reqwest::Url::parse(&format!("{base}/chat-agent")).map_err(|error| {
            ProviderError::local(format!("no se pudo armar la petición: {error}"))
        })?;
        let cliente = cliente_http()?;

        let intentar_con_tools =
            !req.tools.is_empty() && self.soporta_tools.load(Ordering::Relaxed);
        let (estado, datos) = self
            .enviar_peticion(&cliente, &url, &token, &req, intentar_con_tools)
            .await?;

        // Si el backend rechaza el campo `tools` (400/422), recordamos el
        // fallback y reintentamos una vez con el contrato clásico solo texto.
        let (estado, datos) = if intentar_con_tools
            && matches!(estado.as_u16(), 400 | 422)
            && String::from_utf8_lossy(&datos)
                .to_lowercase()
                .contains("tools")
        {
            self.soporta_tools.store(false, Ordering::Relaxed);
            self.enviar_peticion(&cliente, &url, &token, &req, false)
                .await?
        } else {
            (estado, datos)
        };

        if estado.as_u16() / 100 != 2 {
            let cuerpo = cuerpo_sin_token(MAX_CUERPO_ERR, &datos, &token);
            let texto = match estado.canonical_reason() {
                Some(razon) => format!("{razon}: {cuerpo}"),
                None => cuerpo,
            };
            // Transitorio para que el runner también pueda reintentar el
            // turno, como con OpenAI y Anthropic (issue #93).
            return Err(ProviderError::http(estado.as_u16(), texto));
        }
        let salida: RespuestaCerebro = serde_json::from_slice(&datos)
            .map_err(|_| ProviderError::local("la respuesta del cerebro no se entendió"))?;
        if let Some(error) = &salida.error
            && !error.message.is_empty()
        {
            return Err(ProviderError::local(format!(
                "el cerebro contestó un error: {}",
                cuerpo_sin_token(MAX_CUERPO_ERR, error.message.as_bytes(), &token)
            )));
        }
        let nativas = parsear_tool_calls(salida.tool_calls);
        let (contenido, llamadas) = if !nativas.is_empty() {
            self.usa_tool_calls_nativos.store(true, Ordering::Relaxed);
            (salida.content, nativas)
        } else if !req.tools.is_empty() {
            // El servidor respondió 200 sin `tool_calls` nativos: dejamos de
            // mandar el array `tools` en los turnos siguientes para no mezclar
            // esquemas nativos con el puente `orus-tool` del modelo.
            self.soporta_tools.store(false, Ordering::Relaxed);
            extraer_orus_tools(&salida.content, &req.tools)
        } else {
            (limpiar_artefactos_modelo(&salida.content), Vec::new())
        };
        let respuesta = ChatReply {
            message: Message {
                role: "assistant".to_owned(),
                content: contenido,
                tool_calls: (!llamadas.is_empty()).then_some(llamadas),
                ..Message::default()
            },
            prompt_tokens: 0,
            completion_tokens: 0,
        };
        // El cerebro puede contestar `{}` o sin `content` ni `tool_calls`: no
        // es una respuesta y el turno no puede cerrarse en blanco (issue #126).
        if respuesta_vacia(&respuesta.message) {
            return Err(ProviderError::local(SIN_RESPUESTA));
        }
        Ok(respuesta)
    }

    async fn enviar_peticion(
        &self,
        cliente: &reqwest::Client,
        url: &reqwest::Url,
        token: &str,
        req: &ChatRequest,
        con_tools: bool,
    ) -> Result<(reqwest::StatusCode, Vec<u8>), ProviderError> {
        let usar_nativos = con_tools && self.usa_tool_calls_nativos.load(Ordering::Relaxed);
        let mut mensajes = if usar_nativos {
            convertir_con_tools(&req.messages)
        } else {
            convertir_para_cerebro(&req.messages)
        };
        if !req.tools.is_empty() && !usar_nativos {
            inyectar_puente_local(&mut mensajes, &req.tools, &self.workdir);
        }
        let cuerpo = PeticionCerebro {
            messages: mensajes,
            tools: if con_tools {
                convertir_specs(&req.tools)
            } else {
                Vec::new()
            },
        };
        let payload = serde_json::to_vec(&cuerpo).map_err(|error| {
            ProviderError::local(format!("no se pudo armar la petición: {error}"))
        })?;

        // El cerebro reintenta lo pasajero como sus hermanos OpenAI y
        // Anthropic: 429, 5xx y fallos de transporte (issue #93).
        let mut respuesta = enviar_con_reintentos(
            || {
                let mut peticion = cliente
                    .post(url.clone())
                    .header(CONTENT_TYPE, "application/json")
                    .header(AUTHORIZATION, format!("Bearer {token}"));
                // La organización activa elige al cerebro; sin slug el servidor
                // usará la por defecto (issue #114).
                if !self.org_slug.is_empty() {
                    peticion = peticion.header("x-org-slug", self.org_slug.as_str());
                }
                peticion.body(payload.to_vec())
            },
            None,
        )
        .await?;

        let mut datos = Vec::new();
        loop {
            match respuesta.chunk().await {
                Ok(Some(trozo)) => {
                    datos.extend_from_slice(&trozo);
                    if datos.len() > MAX_RESPUESTA {
                        return Err(ProviderError::local(
                            "la respuesta del cerebro pasó el tope de tamaño",
                        ));
                    }
                }
                Ok(None) => break,
                Err(error) => return Err(ProviderError::transporte(error)),
            }
        }
        Ok((respuesta.status(), datos))
    }
}

#[async_trait]
impl Provider for CloudBrain {
    async fn chat(&self, req: ChatRequest) -> Result<ChatReply, ProviderError> {
        CloudBrain::chat(self, req).await
    }
}

fn convertir_specs(specs: &[ToolSpec]) -> Vec<HerramientaCerebro> {
    specs
        .iter()
        .map(|spec| HerramientaCerebro {
            kind: "function",
            function: FuncionCerebro {
                name: spec.name.clone(),
                description: spec.description.clone(),
                parameters: if spec.schema.is_null() {
                    Value::Object(Map::new())
                } else {
                    spec.schema.clone()
                },
            },
        })
        .collect()
}

fn parsear_tool_calls(llamadas: Vec<LlamadaRecibidaCerebro>) -> Vec<ToolCall> {
    let mut out = Vec::with_capacity(llamadas.len());
    for (idx, item) in llamadas.into_iter().enumerate() {
        let (nombre, crudo) = match item.function {
            Some(func) => (func.name, func.arguments.or(item.args).or(item.arguments)),
            None => (item.name, item.args.or(item.arguments)),
        };
        let nombre = nombre.trim().to_owned();
        if nombre.is_empty() {
            continue;
        }
        let (args, invalid_args) = match crudo {
            Some(Value::String(texto)) => match serde_json::from_str::<Value>(&texto) {
                Ok(valor) => (valor, false),
                Err(_) => (Value::Object(Map::new()), true),
            },
            Some(Value::Null) | None => (Value::Object(Map::new()), false),
            Some(valor) => (valor, false),
        };
        let id = if item.id.trim().is_empty() {
            format!("cloud_call_{}", idx + 1)
        } else {
            item.id
        };
        out.push(ToolCall {
            id,
            name: nombre,
            args,
            invalid_args,
        });
    }
    out
}

/// Rutas reales de Descargas, Documentos y Escritorio. En Linux los nombres
/// dependen del idioma y salen de `user-dirs.dirs`; sin ese archivo valen los
/// nombres en inglés.
fn carpetas_del_usuario(home: &str, sep: &str, user_dirs: Option<&str>) -> String {
    let buscar = |clave: &str, por_defecto: &str| {
        user_dirs
            .and_then(|contenido| {
                contenido.lines().find_map(|linea| {
                    let (nombre, valor) = linea.trim().split_once('=')?;
                    (nombre == clave).then(|| valor.trim().trim_matches('"').replace("$HOME", home))
                })
            })
            .unwrap_or_else(|| format!("{home}{sep}{por_defecto}"))
    };
    format!(
        "Descargas: {}, Documentos: {}, Escritorio: {}",
        buscar("XDG_DOWNLOAD_DIR", "Downloads"),
        buscar("XDG_DOCUMENTS_DIR", "Documents"),
        buscar("XDG_DESKTOP_DIR", "Desktop"),
    )
}

/// Inyecta el puente de herramientas locales en el primer mensaje de usuario
/// usando bloques Markdown ```` ```orus-tool ```` (sin etiquetas XML `<...>`
/// que colisionan con el tokenizador DSML de modelos como DeepSeek).
fn inyectar_puente_local(mensajes: &mut [MensajeCerebro], specs: &[ToolSpec], workdir: &str) {
    let nombres: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
    if nombres.is_empty() {
        return;
    }
    let Some(primero) = mensajes.iter_mut().find(|m| m.role == "user") else {
        return;
    };
    let so = std::env::consts::OS;
    let shell_txt = if cfg!(windows) {
        "Git Bash en Windows (PATH real del usuario activo: python, node, etc. están disponibles)"
    } else {
        "bash en Linux (PATH real del usuario activo: python3, node, etc. están disponibles)"
    };
    let carpeta_txt = if workdir.is_empty() {
        "la carpeta personal del usuario".to_owned()
    } else {
        let sep = if cfg!(windows) { "\\" } else { "/" };
        let home = std::env::var(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
            .unwrap_or_else(|_| workdir.to_owned());
        let config = std::env::var("XDG_CONFIG_HOME").unwrap_or_else(|_| format!("{home}/.config"));
        let user_dirs = if cfg!(windows) {
            None
        } else {
            std::fs::read_to_string(format!("{config}/user-dirs.dirs")).ok()
        };
        format!(
            "{workdir} ({})",
            carpetas_del_usuario(&home, sep, user_dirs.as_deref())
        )
    };
    let lista = nombres.join(", ");
    let puente = format!(
        "[ARNÉS LOCAL DE ESCRITORIO ACTIVO]\n\
         Corres dentro de la app de escritorio Oruscribe Agente en el PC real del usuario.\n\
         - Sistema: {so} · Shell: {shell_txt}\n\
         - Carpeta base autorizada: {carpeta_txt}\n\
         - Herramientas locales reales disponibles: {lista}\n\
           * bash: {{\"command\":\"...\",\"cwd\":\"ruta_absoluta_opcional\",\"timeout\":segundos_opcional}} (por defecto 60 s; para pip/npm install o builds largos usa timeout 300, máximo 600)\n\
           * write: {{\"path\":\"ruta_absoluta\",\"content\":\"...\"}}\n\
           * read: {{\"path\":\"ruta_absoluta\"}}\n\
           * edit: {{\"path\":\"ruta_absoluta\",\"old_string\":\"...\",\"new_string\":\"...\"}}\n\
         REGLAS OBLIGATORIAS:\n\
         1. Para leer, crear o editar archivos locales (.pptx, .docx, .pdf, código, carpetas) o ejecutar comandos en este PC, usa EXCLUSIVAMENTE bloques Markdown `orus-tool`. PROHIBIDO llamar a herramientas de Oruscribe (como create_note o create_task) para crear archivos locales.\n\
         2. Formato EXACTO para ejecutar una herramienta local (JSON válido en una línea o escapado con \\n, sin etiquetas XML ni DSML):\n\
         ```orus-tool\n\
         {{\"tool\":\"bash\",\"args\":{{\"command\":\"...\"}}}}\n\
         ```\n\
         3. Para archivos complejos (p. ej. una presentación .pptx con python-pptx), escribe el script con `write` y ejecútalo con `bash` (o usa `bash` con `python - <<'EOF' ... EOF`), verifica el archivo en disco y luego responde al usuario en español SIN bloques `orus-tool`.\n\
         4. Dentro de `bash`, `~` y `$HOME` apuntan a una carpeta temporal que se borra al terminar el comando: usa SIEMPRE rutas absolutas, como las de arriba, para guardar archivos.\n\
         5. NUNCA cierres una respuesta diciendo \"voy a hacerlo\" o \"procedo ahora\" sin incluir el bloque ```orus-tool``` en esa misma respuesta: si la tarea aún no está verificada en disco, incluye el bloque ```orus-tool``` ahora mismo.\n\
         ---\n"
    );
    let actual = primero.content.take().unwrap_or_default();
    primero.content = Some(format!("{puente}{actual}"));
}

/// Extrae llamadas locales desde bloques Markdown ```` ```orus-tool ... ``` ````
/// (protocolo principal) y también desde etiquetas heredadas `<orus_tool>...</orus_tool>`,
/// limpiando cualquier residuo DSML o etiqueta rota del texto visible.
fn extraer_orus_tools(texto: &str, specs: &[ToolSpec]) -> (String, Vec<ToolCall>) {
    let mut llamadas = Vec::new();
    let mut indice = 0usize;

    // 1) Bloques Markdown ```orus-tool ... ``` (primario, libre de colisiones XML/DSML).
    let sin_fences = extraer_bloques_fenced(texto, specs, &mut llamadas, &mut indice);
    // 2) Compatibilidad con etiquetas <orus_tool>...</orus_tool>.
    let sin_xml = extraer_bloques_xml(&sin_fences, &mut llamadas, &mut indice);
    let limpio = limpiar_artefactos_modelo(&sin_xml);
    (limpio, llamadas)
}

fn aceptar_llamada_json(valor: &Value, llamadas: &mut Vec<ToolCall>, indice: &mut usize) -> bool {
    let Some(nombre) = valor
        .get("tool")
        .or_else(|| valor.get("name"))
        .and_then(Value::as_str)
        .map(str::trim)
    else {
        return false;
    };
    *indice += 1;
    let args = valor
        .get("args")
        .or_else(|| valor.get("arguments"))
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    let args = match args {
        Value::String(s) => serde_json::from_str::<Value>(&s).ok(),
        Value::Null => Some(Value::Object(Map::new())),
        otro => Some(otro),
    };
    llamadas.push(ToolCall {
        id: format!("orus_local_{}", *indice),
        name: nombre.to_owned(),
        invalid_args: args.is_none(),
        args: args.unwrap_or_else(|| Value::Object(Map::new())),
    });
    true
}

fn buscar_inicio_fence_tool(texto: &str) -> Option<(usize, usize)> {
    const MARCAS: [&str; 2] = ["```orus-tool", "```orus_tool"];
    MARCAS
        .iter()
        .filter_map(|m| texto.find(m).map(|pos| (pos, m.len())))
        .min_by_key(|&(pos, _)| pos)
}

/// Escapa saltos de línea o tabulaciones literales dentro de cadenas JSON
/// por si el modelo pega un script multilínea dentro de `"content"` o `"command"`.
fn reparar_json_control_chars(texto: &str) -> String {
    let mut out = String::with_capacity(texto.len() + 16);
    let mut en_cadena = false;
    let mut escape = false;
    for ch in texto.chars() {
        if en_cadena {
            if escape {
                out.push(ch);
                escape = false;
            } else if ch == '\\' {
                out.push(ch);
                escape = true;
            } else if ch == '"' {
                out.push(ch);
                en_cadena = false;
            } else if ch == '\n' {
                out.push_str("\\n");
            } else if ch == '\r' {
                out.push_str("\\r");
            } else if ch == '\t' {
                out.push_str("\\t");
            } else {
                out.push(ch);
            }
        } else {
            if ch == '"' {
                en_cadena = true;
            }
            out.push(ch);
        }
    }
    out
}

fn extraer_bloques_fenced(
    texto: &str,
    specs: &[ToolSpec],
    llamadas: &mut Vec<ToolCall>,
    indice: &mut usize,
) -> String {
    let mut limpio = String::with_capacity(texto.len());
    let mut resto = texto;

    while let Some((ini, len_marca)) = buscar_inicio_fence_tool(resto) {
        let despues = &resto[ini + len_marca..];
        let inicio_cuerpo = despues
            .find('\n')
            .map(|n| &despues[n + 1..])
            .unwrap_or(despues)
            .trim_start();

        let mut encontro = false;
        let mut cursor = inicio_cuerpo;
        loop {
            let trimmed = cursor.trim_start();
            if trimmed.is_empty() || trimmed.starts_with("```") {
                cursor = trimmed;
                break;
            }
            let mut stream = serde_json::Deserializer::from_str(trimmed).into_iter::<Value>();
            match stream.next() {
                Some(Ok(valor)) => {
                    if aceptar_llamada_json(&valor, llamadas, indice) {
                        encontro = true;
                    }
                    cursor = &trimmed[stream.byte_offset()..];
                }
                _ => {
                    let cierre = trimmed.find("\n```").or_else(|| trimmed.find("```"));
                    let bloque_raw = cierre.map(|pos| &trimmed[..pos]).unwrap_or(trimmed);
                    let reparado = reparar_json_control_chars(bloque_raw);
                    let aceptada = serde_json::from_str::<Value>(&reparado)
                        .is_ok_and(|valor| aceptar_llamada_json(&valor, llamadas, indice));
                    if !aceptada && let Some(spec) = specs.first() {
                        *indice += 1;
                        llamadas.push(ToolCall {
                            id: format!("orus_local_{}", *indice),
                            name: spec.name.clone(),
                            invalid_args: true,
                            args: Value::Object(Map::new()),
                        });
                    }
                    encontro = aceptada || !specs.is_empty();
                    cursor = cierre.map(|pos| &trimmed[pos..]).unwrap_or("");
                    break;
                }
            }
        }
        let siguiente = if let Some(tras_cierre) = cursor.trim_start().strip_prefix("```") {
            tras_cierre
        } else if let Some(fin) = cursor.find("```") {
            &cursor[fin + 3..]
        } else {
            ""
        };

        if encontro {
            limpio.push_str(&resto[..ini]);
        } else {
            let consumido = resto.len() - siguiente.len();
            limpio.push_str(&resto[..consumido]);
        }
        resto = siguiente;
    }
    limpio.push_str(resto);
    limpio
}

fn extraer_bloques_xml(texto: &str, llamadas: &mut Vec<ToolCall>, indice: &mut usize) -> String {
    const ABRIR: &str = "<orus_tool>";
    let mut limpio = String::with_capacity(texto.len());
    let mut resto = texto;

    while let Some(ini) = resto.find(ABRIR) {
        let despues_abrir = &resto[ini + ABRIR.len()..];
        let trimmed = despues_abrir.trim_start();
        let sin_fence = trimmed
            .strip_prefix("```json")
            .or_else(|| trimmed.strip_prefix("```"))
            .map(str::trim_start)
            .unwrap_or(trimmed);

        let mut iter = serde_json::Deserializer::from_str(sin_fence).into_iter::<Value>();
        let Some(Ok(valor)) = iter.next() else {
            // Si el modelo rompió el XML (p. ej. alucinación DSML), cortamos
            // desde `<orus_tool>` hasta el cierre o final para no filtrarlo al chat.
            limpio.push_str(&resto[..ini]);
            if let Some(fin_cierre) = despues_abrir.find("</")
                && let Some(gt) = despues_abrir[fin_cierre..].find('>')
            {
                resto = &despues_abrir[fin_cierre + gt + 1..];
                continue;
            }
            resto = "";
            break;
        };

        let consumido_json = iter.byte_offset();
        let tras_json = sin_fence[consumido_json..].trim_start();
        let tras_fence = tras_json
            .strip_prefix("```")
            .map(str::trim_start)
            .unwrap_or(tras_json);
        let tras_cierre = if let Some(rest_tag) = tras_fence.strip_prefix("</")
            && let Some(gt) = rest_tag.find('>')
        {
            &rest_tag[gt + 1..]
        } else {
            tras_fence
        };

        let longitud_bloque = resto.len() - tras_cierre.len();
        if aceptar_llamada_json(&valor, llamadas, indice) {
            limpio.push_str(&resto[..ini]);
        } else {
            limpio.push_str(&resto[..longitud_bloque]);
        }
        resto = tras_cierre;
    }
    limpio.push_str(resto);
    limpio
}

/// Elimina artefactos internos filtrados por modelos (como tokens `<|DSML|...>`
/// o `< | DSML | ...>` de DeepSeek) para que jamás lleguen crudos al chat.
fn limpiar_artefactos_modelo(texto: &str) -> String {
    let mut salida = texto.to_owned();
    for marcador in ["< | DSML |", "<|DSML|", "</ | DSML |", "</|DSML|"] {
        if let Some(pos) = salida.find(marcador) {
            salida.truncate(pos);
        }
    }
    salida.trim().to_owned()
}

/// Conversación estructurada con soporte para `tool_calls` y `role: "tool"`.
fn convertir_con_tools(msgs: &[Message]) -> Vec<MensajeCerebro> {
    let mut out = Vec::with_capacity(msgs.len());
    for mensaje in msgs {
        match mensaje.role.as_str() {
            "system" => out.push(MensajeCerebro {
                role: "user".to_owned(),
                content: Some(format!("(nota del programa: {})", mensaje.content)),
                tool_calls: Vec::new(),
                tool_call_id: None,
            }),
            "tool" => out.push(MensajeCerebro {
                role: "tool".to_owned(),
                content: Some(mensaje.content.clone()),
                tool_calls: Vec::new(),
                tool_call_id: (!mensaje.tool_call_id.is_empty())
                    .then(|| mensaje.tool_call_id.clone()),
            }),
            "assistant" => {
                let llamadas: Vec<LlamadaEnviadaCerebro> = mensaje
                    .tool_calls
                    .as_deref()
                    .unwrap_or(&[])
                    .iter()
                    .map(|llamada| LlamadaEnviadaCerebro {
                        id: llamada.id.clone(),
                        kind: "function",
                        function: FuncionLlamadaEnviada {
                            name: llamada.name.clone(),
                            arguments: serde_json::to_string(&llamada.args)
                                .unwrap_or_else(|_| "{}".to_owned()),
                        },
                    })
                    .collect();
                let content = if mensaje.content.is_empty() && !llamadas.is_empty() {
                    None
                } else {
                    Some(mensaje.content.clone())
                };
                out.push(MensajeCerebro {
                    role: "assistant".to_owned(),
                    content,
                    tool_calls: llamadas,
                    tool_call_id: None,
                });
            }
            "user" => {
                let mut texto = mensaje.content.clone();
                if mensaje
                    .images
                    .as_ref()
                    .is_some_and(|imagenes| !imagenes.is_empty())
                {
                    if texto.is_empty() {
                        texto = "(imagen adjunta)".to_owned();
                    } else {
                        texto.push_str(" (imagen adjunta no enviada)");
                    }
                }
                out.push(MensajeCerebro {
                    role: "user".to_owned(),
                    content: Some(texto),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                });
            }
            _ => {}
        }
    }
    out
}

/// Deja la conversación en el formato clásico que chat-agent conoce (user y
/// assistant). `convertirParaCerebro` (`cloud.go:121-148`).
fn convertir_para_cerebro(msgs: &[Message]) -> Vec<MensajeCerebro> {
    let mut out = Vec::with_capacity(msgs.len());
    for mensaje in msgs {
        match mensaje.role.as_str() {
            "system" => out.push(MensajeCerebro {
                role: "user".to_owned(),
                content: Some(format!("(nota del programa: {})", mensaje.content)),
                tool_calls: Vec::new(),
                tool_call_id: None,
            }),
            "tool" => out.push(MensajeCerebro {
                role: "user".to_owned(),
                content: Some(format!(
                    "(resultado de una herramienta local: {})\n\n[Si todavía falta crear, ejecutar o verificar el resultado local, incluye ahora el siguiente bloque ```orus-tool``` sin usar create_note/create_task. Responde sin bloques ```orus-tool``` únicamente cuando el trabajo en disco esté terminado y verificado.]",
                    mensaje.content
                )),
                tool_calls: Vec::new(),
                tool_call_id: None,
            }),
            "assistant" => {
                let mut texto = mensaje.content.clone();
                if let Some(llamadas) = &mensaje.tool_calls
                    && !llamadas.is_empty()
                {
                    #[derive(Serialize)]
                    struct BloqueHerramienta<'a> {
                        tool: &'a str,
                        args: &'a Value,
                    }
                    let resumen = llamadas
                        .iter()
                        .map(|c| {
                            let json = serde_json::to_string(&BloqueHerramienta {
                                tool: &c.name,
                                args: &c.args,
                            })
                            .unwrap_or_else(|_| "{}".to_owned());
                            format!("```orus-tool\n{json}\n```")
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    if texto.is_empty() {
                        texto = format!("(trabajo local)\n{resumen}");
                    } else {
                        texto = format!("{texto}\n{resumen}");
                    }
                }
                out.push(MensajeCerebro {
                    role: "assistant".to_owned(),
                    content: Some(texto),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                });
            }
            "user" => {
                let mut texto = mensaje.content.clone();
                if mensaje
                    .images
                    .as_ref()
                    .is_some_and(|imagenes| !imagenes.is_empty())
                {
                    if texto.is_empty() {
                        texto = "(imagen adjunta)".to_owned();
                    } else {
                        // La imagen del historial no viaja al cerebro: que no
                        // desaparezca en silencio (issue #93).
                        texto.push_str(" (imagen adjunta no enviada)");
                    }
                }
                out.push(MensajeCerebro {
                    role: "user".to_owned(),
                    content: Some(texto),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                });
            }
            _ => {}
        }
    }
    out
}

/// Recorta el cuerpo de un error a `max` bytes y borra de él el token de la
/// sesión, por si el servidor lo repite. `cuerpoSinToken` (`cloud.go:152-160`).
fn cuerpo_sin_token(max: usize, texto: &[u8], token: &str) -> String {
    let recortado = if texto.len() > max {
        let mut bytes = texto[..max].to_vec();
        bytes.extend_from_slice("…".as_bytes());
        String::from_utf8_lossy(&bytes).into_owned()
    } else {
        String::from_utf8_lossy(texto).into_owned()
    };
    if token.is_empty() {
        recortado
    } else {
        recortado.replace(token, "(token)")
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use axum::body::to_bytes;
    use axum::extract::Request;
    use axum::http::{StatusCode, header};
    use axum::response::{IntoResponse, Response};
    use orus_core::{Image, ToolCall};
    use serde_json::Value;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;
    use crate::types::ToolSpec;

    async fn servidor<H, Fut>(handler: H) -> String
    where
        H: Fn(Request) -> Fut + Clone + Send + Sync + 'static,
        Fut: std::future::Future<Output = Response> + Send + 'static,
    {
        let app = axum::Router::new().fallback(handler);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("el servidor de prueba debe escuchar");
        let addr = listener.local_addr().expect("dirección del servidor");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{addr}")
    }

    fn con_json(status: u16, cuerpo: &str) -> Response {
        (
            StatusCode::from_u16(status).expect("código HTTP válido"),
            [(header::CONTENT_TYPE, "application/json")],
            cuerpo.to_owned(),
        )
            .into_response()
    }

    fn cerebro(base: &str, token: Option<&str>) -> CloudBrain {
        CloudBrain::new(
            base.to_owned(),
            token.map(|token| {
                let token = token.to_owned();
                Box::new(move || token.clone()) as TokenFn
            }),
            String::new(),
        )
    }

    fn peticion() -> ChatRequest {
        ChatRequest {
            model: String::new(),
            messages: vec![Message {
                role: "user".to_owned(),
                content: "hola".to_owned(),
                ..Message::default()
            }],
            tools: Vec::new(),
            on_delta: None,
        }
    }

    /// `TestCloudBrainAnswersWithoutModelOrTools` (`cloud_test.go:17-79`).
    #[tokio::test]
    async fn answers_without_model_or_tools() {
        let visto = Arc::new(Mutex::new((String::new(), String::new(), String::new())));
        let visto_handler = Arc::clone(&visto);
        let servidor_url = servidor(move |peticion: Request| {
            let visto = Arc::clone(&visto_handler);
            async move {
                let (partes, cuerpo) = peticion.into_parts();
                let datos = to_bytes(cuerpo, usize::MAX).await.expect("cuerpo");
                let mut visto = visto.lock().expect("estado compartido");
                visto.0 = partes.uri.path().to_owned();
                visto.1 = partes
                    .headers
                    .get("authorization")
                    .and_then(|valor| valor.to_str().ok())
                    .unwrap_or("")
                    .to_owned();
                visto.2 = String::from_utf8_lossy(&datos).into_owned();
                con_json(200, r#"{"content":"hola de vuelta","steps":[]}"#)
            }
        })
        .await;

        let respuesta = cerebro(&servidor_url, Some("tok-de-sesion"))
            .chat(ChatRequest {
                model: "ignorado: el modelo lo elige la organización".to_owned(),
                messages: vec![
                    Message {
                        role: "system".to_owned(),
                        content: "resume la conversación".to_owned(),
                        ..Message::default()
                    },
                    Message {
                        role: "user".to_owned(),
                        content: "primera".to_owned(),
                        ..Message::default()
                    },
                    Message {
                        role: "assistant".to_owned(),
                        content: "respuesta".to_owned(),
                        ..Message::default()
                    },
                    Message {
                        role: "user".to_owned(),
                        content: "segunda".to_owned(),
                        ..Message::default()
                    },
                ],
                tools: Vec::new(),
                on_delta: None,
            })
            .await
            .expect("Chat");

        let visto = visto.lock().expect("estado compartido");
        assert_eq!(visto.0, "/chat-agent", "ruta: {}", visto.0);
        assert_eq!(
            visto.1, "Bearer tok-de-sesion",
            "autorización: {:?}",
            visto.1
        );
        let enviado: Value = serde_json::from_str(&visto.2).expect("cuerpo JSON");
        assert!(
            enviado.get("model").is_none(),
            "el modelo no debe viajar: {}",
            visto.2
        );
        assert!(
            enviado.get("tools").is_none(),
            "las herramientas no deben viajar: {}",
            visto.2
        );
        let mensajes = enviado["messages"].as_array().expect("messages");
        assert_eq!(mensajes.len(), 4, "mensajes: {}", visto.2);
        assert_eq!(mensajes[0]["role"], "user");
        assert!(
            mensajes[0]["content"]
                .as_str()
                .is_some_and(|texto| texto.contains("resume la conversación")),
            "el system debe viajar como nota de usuario: {}",
            visto.2
        );
        assert_eq!(respuesta.message.role, "assistant");
        assert_eq!(respuesta.message.content, "hola de vuelta");
        assert!(
            respuesta.message.tool_calls.is_none(),
            "el cerebro no pide herramientas locales: {:?}",
            respuesta.message.tool_calls
        );
    }

    /// El chat debe elegir la organización configurada enviando `x-org-slug`
    /// a `/chat-agent`, igual que el panel (issue #114).
    #[tokio::test]
    async fn envia_la_organizacion_configurada() {
        let slug_visto = Arc::new(Mutex::new(String::new()));
        let slug_handler = Arc::clone(&slug_visto);
        let servidor_url = servidor(move |peticion: Request| {
            let slug = Arc::clone(&slug_handler);
            async move {
                *slug.lock().expect("estado compartido") = peticion
                    .headers()
                    .get("x-org-slug")
                    .and_then(|valor| valor.to_str().ok())
                    .unwrap_or("")
                    .to_owned();
                con_json(200, r#"{"content":"listo"}"#)
            }
        })
        .await;

        let mut cerebro = cerebro(&servidor_url, Some("tok-de-sesion"));
        cerebro.org_slug = "mi-org".to_owned();
        cerebro.chat(peticion()).await.expect("Chat");

        let slug = slug_visto.lock().expect("estado compartido");
        assert_eq!(
            *slug, "mi-org",
            "la cabecera x-org-slug debe llegar: {slug}"
        );
    }

    /// `TestCloudBrainTurnsToolHistoryIntoNotes` (`cloud_test.go:84-112`).
    #[tokio::test]
    async fn turns_tool_history_into_notes() {
        let cuerpo = Arc::new(Mutex::new(String::new()));
        let cuerpo_handler = Arc::clone(&cuerpo);
        let servidor_url = servidor(move |peticion: Request| {
            let cuerpo = Arc::clone(&cuerpo_handler);
            async move {
                let (_, cuerpo_peticion) = peticion.into_parts();
                let datos = to_bytes(cuerpo_peticion, usize::MAX).await.expect("cuerpo");
                *cuerpo.lock().expect("estado compartido") =
                    String::from_utf8_lossy(&datos).into_owned();
                con_json(200, r#"{"content":"listo","steps":[]}"#)
            }
        })
        .await;

        cerebro(&servidor_url, Some("t"))
            .chat(ChatRequest {
                model: String::new(),
                messages: vec![
                    Message {
                        role: "user".to_owned(),
                        content: "busca".to_owned(),
                        ..Message::default()
                    },
                    Message {
                        role: "assistant".to_owned(),
                        tool_calls: Some(vec![ToolCall {
                            id: "1".to_owned(),
                            name: "local_read_file".to_owned(),
                            ..ToolCall::default()
                        }]),
                        ..Message::default()
                    },
                    Message {
                        role: "tool".to_owned(),
                        tool_call_id: "1".to_owned(),
                        content: "el contenido del archivo".to_owned(),
                        ..Message::default()
                    },
                    Message {
                        role: "assistant".to_owned(),
                        content: "ya".to_owned(),
                        ..Message::default()
                    },
                    Message {
                        role: "user".to_owned(),
                        content: "gracias".to_owned(),
                        ..Message::default()
                    },
                ],
                tools: Vec::new(),
                on_delta: None,
            })
            .await
            .expect("Chat");

        let cuerpo = cuerpo.lock().expect("estado compartido");
        assert!(
            cuerpo.contains("resultado de una herramienta local")
                && cuerpo.contains("el contenido del archivo"),
            "el resultado de herramienta no viaja como nota: {cuerpo}"
        );
        assert!(
            cuerpo.contains("trabajo local"),
            "la llamada sin texto debe quedar anotada: {cuerpo}"
        );
    }

    /// `TestCloudBrainErrorsHideTheToken` (`cloud_test.go:116-131`).
    #[tokio::test]
    async fn errors_hide_the_token() {
        let servidor_url = servidor(|_peticion: Request| async {
            con_json(502, r#"{"error":"gateway explotó con tok-secreto dentro"}"#)
        })
        .await;

        let error = cerebro(&servidor_url, Some("tok-secreto"))
            .chat(peticion())
            .await
            .expect_err("esperaba un error");
        let mensaje = error.to_string();
        assert!(
            !mensaje.contains("tok-secreto"),
            "el token se escapó al error: {mensaje}"
        );
        assert!(
            mensaje.contains("(token)"),
            "el token debía sustituirse por (token): {mensaje}"
        );
    }

    /// `cuerpoSinToken` (`cloud.go:152-160`) corta a 300 bytes antes de
    /// sustituir el token. El marcador de Go es `"…"` (U+2026), no un punto:
    /// el byte en `cloud.go:155` es `E2 80 A6`.
    #[tokio::test]
    async fn recorta_el_cuerpo_a_300_bytes() {
        let cuerpo = format!("tok-secreto{}", "x".repeat(350));
        let servidor_url = servidor(move |_peticion: Request| {
            let cuerpo = cuerpo.clone();
            async move { con_json(502, &cuerpo) }
        })
        .await;

        let error = cerebro(&servidor_url, Some("tok-secreto"))
            .chat(peticion())
            .await
            .expect_err("el 502 debe fallar");
        // El contrato es el recorte a 300 bytes y el tachado del token; el
        // envoltorio ahora es el error HTTP transitorio de los hermanos.
        assert_eq!(error.status, 502);
        assert_eq!(
            error.body,
            format!("Bad Gateway: (token){}…", "x".repeat(289))
        );
    }

    /// `TestCloudBrainNeedsASession` (`cloud_test.go:135-146`).
    #[tokio::test]
    async fn needs_a_session() {
        let llamadas = Arc::new(AtomicUsize::new(0));
        let llamadas_handler = Arc::clone(&llamadas);
        let servidor_url = servidor(move |_peticion: Request| {
            let llamadas = Arc::clone(&llamadas_handler);
            async move {
                llamadas.fetch_add(1, Ordering::SeqCst);
                con_json(200, r#"{"content":"no"}"#)
            }
        })
        .await;

        let error = cerebro(&servidor_url, Some(""))
            .chat(peticion())
            .await
            .expect_err("error esperado sin sesión");
        assert!(
            error.to_string().contains("sesión"),
            "error esperado sin sesión: {error}"
        );
        assert_eq!(
            llamadas.load(Ordering::SeqCst),
            0,
            "no debe haber red sin token"
        );
    }

    /// `TestCloudBrainRejectsInsecureBase` (`cloud_test.go:149-155`).
    #[tokio::test]
    async fn rejects_insecure_base() {
        let error = cerebro("http://ejemplo.com/functions/v1", Some("t"))
            .chat(peticion())
            .await
            .expect_err("una base que no es https ni loopback debe rechazarse");
        assert!(
            error.to_string().contains("https"),
            "el error debe explicar la regla de seguridad: {error}"
        );
    }

    /// El cerebro reintenta lo pasajero como sus hermanos OpenAI y Anthropic:
    /// un 429/502 pasajero no hunde el turno (issue #93).
    #[tokio::test]
    async fn reintenta_429_y_5xx() {
        let llamadas = Arc::new(AtomicUsize::new(0));
        let servidor_url = servidor({
            let llamadas = Arc::clone(&llamadas);
            move |_peticion: Request| {
                let llamadas = Arc::clone(&llamadas);
                async move {
                    match llamadas.fetch_add(1, Ordering::SeqCst) {
                        0 => con_json(429, r#"{"error":{"message":"espera"}}"#),
                        1 => con_json(502, r#"{"error":{"message":"caído"}}"#),
                        _ => con_json(200, r#"{"content":"hola de vuelta"}"#),
                    }
                }
            }
        })
        .await;

        let respuesta = cerebro(&servidor_url, Some("tok-de-sesion"))
            .chat(peticion())
            .await
            .expect("el tercer intento responde");
        assert_eq!(respuesta.message.content, "hola de vuelta");
        assert_eq!(llamadas.load(Ordering::SeqCst), 3);
    }

    /// El error final de un 5xx es transitorio: el runner también puede
    /// reintentar el turno, como con OpenAI y Anthropic (issue #93).
    #[tokio::test]
    async fn el_error_final_es_transitorio() {
        let servidor_url = servidor(|_peticion: Request| async {
            con_json(503, r#"{"error":{"message":"no disponible"}}"#)
        })
        .await;
        let error = cerebro(&servidor_url, Some("tok-de-sesion"))
            .chat(peticion())
            .await
            .expect_err("un 503 debe fallar");
        assert_eq!(error.status, 503);
        assert!(error.is_transient(), "un 503 del cerebro es transitorio");

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("el servidor de prueba debe escuchar");
        let addr = listener.local_addr().expect("dirección del servidor");
        let tarea = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("aceptar conexión");
            let mut buffer = [0u8; 4096];
            let _ = socket.read(&mut buffer).await;
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n5\r\n{\"con")
                .await
                .expect("cabecera y trozo parcial");
            socket.shutdown().await.expect("cerrar");
        });
        let error = cerebro(&format!("http://{addr}"), Some("tok-de-sesion"))
            .chat(peticion())
            .await
            .expect_err("un corte al leer el cuerpo debe fallar");
        assert!(
            error.is_transient(),
            "un corte del cuerpo debe poder reintentarse: {error}"
        );
        tarea.await.expect("el servidor termina");
    }

    /// Las imágenes del historial no desaparecen en silencio: el texto que
    /// viaja al cerebro anota que no se enviaron (issue #93).
    #[tokio::test]
    async fn anota_las_imagenes_del_historial() {
        let cuerpo = Arc::new(Mutex::new(String::new()));
        let servidor_url = servidor({
            let cuerpo = Arc::clone(&cuerpo);
            move |peticion: Request| {
                let cuerpo = Arc::clone(&cuerpo);
                async move {
                    let datos = to_bytes(peticion.into_body(), usize::MAX)
                        .await
                        .expect("cuerpo");
                    *cuerpo.lock().expect("estado compartido") =
                        String::from_utf8_lossy(&datos).into_owned();
                    con_json(200, r#"{"content":"listo"}"#)
                }
            }
        })
        .await;

        cerebro(&servidor_url, Some("t"))
            .chat(ChatRequest {
                model: String::new(),
                messages: vec![
                    Message {
                        role: "user".to_owned(),
                        content: "mira esta foto".to_owned(),
                        images: Some(vec![Image {
                            mime: "image/png".to_owned(),
                            data: "abc".to_owned(),
                            preview: String::new(),
                        }]),
                        ..Message::default()
                    },
                    Message {
                        role: "assistant".to_owned(),
                        content: "la veo".to_owned(),
                        ..Message::default()
                    },
                    Message {
                        role: "user".to_owned(),
                        content: "¿y ahora?".to_owned(),
                        ..Message::default()
                    },
                ],
                tools: Vec::new(),
                on_delta: None,
            })
            .await
            .expect("Chat");

        let cuerpo = cuerpo.lock().expect("estado compartido");
        assert!(
            cuerpo.contains("mira esta foto (imagen adjunta no enviada)"),
            "la imagen del historial quedó anotada: {cuerpo}"
        );
    }

    /// La imagen que se envía AHORA sigue siendo error: no puede anotarse
    /// como si el modelo fuera a verla.
    #[tokio::test]
    async fn la_imagen_del_mensaje_actual_sigue_siendo_error() {
        let error = cerebro("http://127.0.0.1:1", Some("t"))
            .chat(ChatRequest {
                model: String::new(),
                messages: vec![Message {
                    role: "user".to_owned(),
                    content: "mira".to_owned(),
                    images: Some(vec![Image {
                        mime: "image/png".to_owned(),
                        data: "abc".to_owned(),
                        preview: String::new(),
                    }]),
                    ..Message::default()
                }],
                tools: Vec::new(),
                on_delta: None,
            })
            .await
            .expect_err("el cerebro no recibe imágenes");
        assert!(
            error.to_string().contains("todavía no recibe imágenes"),
            "err = {error}"
        );
    }

    /// Un 200 del cerebro sin contenido no cierra el turno en blanco
    /// (issue #126).
    #[tokio::test]
    async fn rechaza_un_200_sin_contenido() {
        for cuerpo in [r#"{}"#, r#"{"content":"   "}"#] {
            let cuerpo = cuerpo.to_owned();
            let servidor_url = servidor(move |_peticion: Request| {
                let cuerpo = cuerpo.clone();
                async move { con_json(200, &cuerpo) }
            })
            .await;

            let error = cerebro(&servidor_url, Some("t"))
                .chat(peticion())
                .await
                .expect_err("un cerebro sin contenido no es una respuesta");

            assert_eq!(
                error.to_string(),
                "el proveedor no devolvió ninguna respuesta"
            );
        }
    }

    #[tokio::test]
    async fn envia_tools_locales_y_devuelve_tool_calls() {
        let cuerpos = Arc::new(Mutex::new(Vec::<Value>::new()));
        let cuerpos_handler = Arc::clone(&cuerpos);
        let turno = Arc::new(AtomicUsize::new(0));
        let servidor_url = servidor(move |peticion: Request| {
            let cuerpos = Arc::clone(&cuerpos_handler);
            let turno = Arc::clone(&turno);
            async move {
                let datos = to_bytes(peticion.into_body(), usize::MAX)
                    .await
                    .expect("cuerpo");
                let json: Value = serde_json::from_slice(&datos).expect("JSON válido");
                cuerpos.lock().expect("cuerpos").push(json);
                if turno.fetch_add(1, Ordering::SeqCst) == 0 {
                    con_json(
                        200,
                        r#"{"content":"","tool_calls":[{"id":"call_1","type":"function","function":{"name":"read","arguments":"{\"path\":\"/tmp/a.txt\"}"}},{"id":"call_2","name":"bash","args":{"command":"echo ok"}}]}"#,
                    )
                } else {
                    con_json(200, r#"{"content":"archivo leído y comando ejecutado"}"#)
                }
            }
        })
        .await;

        let brain = cerebro(&servidor_url, Some("tok"));
        let specs = vec![
            ToolSpec {
                name: "read".to_owned(),
                description: "Lee un archivo".to_owned(),
                schema: serde_json::json!({"type":"object","properties":{"path":{"type":"string"}}}),
            },
            ToolSpec {
                name: "bash".to_owned(),
                description: "Ejecuta shell".to_owned(),
                schema: serde_json::json!({"type":"object","properties":{"command":{"type":"string"}}}),
            },
        ];

        let primera = brain
            .chat(ChatRequest {
                model: String::new(),
                messages: vec![Message {
                    role: "user".to_owned(),
                    content: "lee a.txt".to_owned(),
                    ..Message::default()
                }],
                tools: specs.clone(),
                on_delta: None,
            })
            .await
            .expect("primera respuesta con tool_calls");
        let llamadas = primera
            .message
            .tool_calls
            .clone()
            .expect("debe devolver tool_calls");
        assert_eq!(llamadas.len(), 2);
        assert_eq!(llamadas[0].id, "call_1");
        assert_eq!(llamadas[0].name, "read");
        assert_eq!(llamadas[0].args["path"], "/tmp/a.txt");
        assert_eq!(llamadas[1].name, "bash");
        assert_eq!(llamadas[1].args["command"], "echo ok");

        let segunda = brain
            .chat(ChatRequest {
                model: String::new(),
                messages: vec![
                    Message {
                        role: "user".to_owned(),
                        content: "lee a.txt".to_owned(),
                        ..Message::default()
                    },
                    primera.message,
                    Message {
                        role: "tool".to_owned(),
                        tool_call_id: "call_1".to_owned(),
                        content: "hola mundo".to_owned(),
                        ..Message::default()
                    },
                ],
                tools: specs,
                on_delta: None,
            })
            .await
            .expect("segunda respuesta final");
        assert_eq!(segunda.message.content, "archivo leído y comando ejecutado");

        let enviados = cuerpos.lock().expect("cuerpos");
        assert_eq!(enviados.len(), 2);
        assert_eq!(enviados[0]["tools"].as_array().expect("tools").len(), 2);
        assert_eq!(enviados[0]["tools"][0]["function"]["name"], "read");
        assert_eq!(enviados[1]["messages"][1]["tool_calls"][0]["id"], "call_1");
        assert_eq!(enviados[1]["messages"][2]["role"], "tool");
        assert_eq!(enviados[1]["messages"][2]["tool_call_id"], "call_1");
    }

    #[tokio::test]
    async fn cae_a_solo_texto_si_el_servidor_rechaza_tools_con_400() {
        let llamadas = Arc::new(AtomicUsize::new(0));
        let servidor_url = servidor({
            let llamadas = Arc::clone(&llamadas);
            move |peticion: Request| {
                let llamadas = Arc::clone(&llamadas);
                async move {
                    llamadas.fetch_add(1, Ordering::SeqCst);
                    let datos = to_bytes(peticion.into_body(), usize::MAX)
                        .await
                        .expect("cuerpo");
                    let json: Value = serde_json::from_slice(&datos).expect("JSON");
                    if json.get("tools").is_some() {
                        con_json(400, r#"{"error":{"message":"unknown field: tools"}}"#)
                    } else {
                        con_json(200, r#"{"content":"respuesta clásica"}"#)
                    }
                }
            }
        })
        .await;

        let brain = cerebro(&servidor_url, Some("tok"));
        let req_con_tools = || ChatRequest {
            model: String::new(),
            messages: vec![Message {
                role: "user".to_owned(),
                content: "hola".to_owned(),
                ..Message::default()
            }],
            tools: vec![ToolSpec {
                name: "read".to_owned(),
                description: String::new(),
                schema: Value::Null,
            }],
            on_delta: None,
        };

        let primera = brain
            .chat(req_con_tools())
            .await
            .expect("debe caer a solo texto");
        assert_eq!(primera.message.content, "respuesta clásica");
        assert_eq!(
            llamadas.load(Ordering::SeqCst),
            2,
            "intentó tools + fallback"
        );

        let segunda = brain
            .chat(req_con_tools())
            .await
            .expect("segunda llamada directa sin tools");
        assert_eq!(segunda.message.content, "respuesta clásica");
        assert_eq!(
            llamadas.load(Ordering::SeqCst),
            3,
            "recordó que el servidor no acepta tools y no gastó un 400 extra"
        );
    }

    #[tokio::test]
    async fn extrae_bloques_orus_tool_cuando_el_servidor_responde_solo_texto() {
        let servidor_url = servidor(|_peticion: Request| async {
            con_json(
                200,
                r#"{"content":"Voy a crear el archivo ahora.\n<orus_tool>{\"name\":\"write\",\"args\":{\"path\":\"/tmp/perros.txt\",\"content\":\"hola\"}}</orus_tool>"}"#,
            )
        })
        .await;

        let brain = cerebro(&servidor_url, Some("tok")).with_workdir("/tmp".to_owned());
        let reply = brain
            .chat(ChatRequest {
                model: String::new(),
                messages: vec![Message {
                    role: "user".to_owned(),
                    content: "crea perros.txt".to_owned(),
                    ..Message::default()
                }],
                tools: vec![ToolSpec {
                    name: "write".to_owned(),
                    description: "Escribe un archivo".to_owned(),
                    schema: Value::Null,
                }],
                on_delta: None,
            })
            .await
            .expect("respuesta con <orus_tool>");

        assert_eq!(reply.message.content, "Voy a crear el archivo ahora.");
        let calls = reply.message.tool_calls.expect("debe extraer tool_call");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "write");
        assert_eq!(calls[0].args["path"], "/tmp/perros.txt");
        assert_eq!(calls[0].args["content"], "hola");

        // También tolera erratas en la etiqueta de cierre del modelo (p. ej. `</or_tool>`).
        let (texto_limpio, con_errata) = extraer_orus_tools(
            "Ahora lo leo.\n<orus_tool>{\"name\":\"read\",\"args\":{\"path\":\"/tmp/perros.txt\"}}</or_tool>",
            &[ToolSpec {
                name: "read".to_owned(),
                description: String::new(),
                schema: Value::Null,
            }],
        );
        assert_eq!(texto_limpio, "Ahora lo leo.");
        assert_eq!(con_errata.len(), 1);
        assert_eq!(con_errata[0].name, "read");

        // Protocolo primario ```orus-tool``` y limpieza de residuos DSML.
        let (limpio_fence, calls_fence) = extraer_orus_tools(
            "Reviso python.\n```orus-tool\n{\"tool\":\"bash\",\"args\":{\"command\":\"python --version\"}}\n```\n< | DSML | invoke>",
            &[ToolSpec {
                name: "bash".to_owned(),
                description: String::new(),
                schema: Value::Null,
            }],
        );
        assert_eq!(limpio_fence, "Reviso python.");
        assert_eq!(calls_fence.len(), 1);
        assert_eq!(calls_fence[0].name, "bash");
        assert_eq!(calls_fence[0].args["command"], "python --version");
    }

    #[test]
    fn las_carpetas_del_usuario_salen_de_user_dirs_o_caen_a_ingles() {
        let es = "XDG_DESKTOP_DIR=\"$HOME/Escritorio\"
XDG_DOWNLOAD_DIR=\"$HOME/Descargas\"
XDG_DOCUMENTS_DIR=\"$HOME/Documentos\"
";
        for (archivo, esperado) in [
            (
                Some(es),
                "Descargas: /home/ana/Descargas, Documentos: /home/ana/Documentos, Escritorio: /home/ana/Escritorio",
            ),
            (
                None,
                "Descargas: /home/ana/Downloads, Documentos: /home/ana/Documents, Escritorio: /home/ana/Desktop",
            ),
        ] {
            assert_eq!(carpetas_del_usuario("/home/ana", "/", archivo), esperado);
        }
    }

    #[test]
    fn un_bloque_orus_tool_roto_o_desconocido_vuelve_al_modelo_como_error() {
        let specs = [ToolSpec {
            name: "bash".to_owned(),
            description: String::new(),
            schema: Value::Null,
        }];
        for (texto, nombre, invalidos) in [
            (
                "Va.
```orus-tool
{\"tool\":\"python\",\"args\":{\"code\":\"1\"}}
```",
                "python",
                false,
            ),
            (
                "Va.
```orus-tool
{\"tool\":\"bash\",\"args\":{\"command\":\"echo \"x\" 
```",
                "bash",
                true,
            ),
        ] {
            let (limpio, llamadas) = extraer_orus_tools(texto, &specs);
            assert_eq!(limpio, "Va.", "{texto}");
            assert_eq!(llamadas.len(), 1, "{texto}");
            assert_eq!(llamadas[0].name, nombre, "{texto}");
            assert_eq!(llamadas[0].invalid_args, invalidos, "{texto}");
        }
    }
}
