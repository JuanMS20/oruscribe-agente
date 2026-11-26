//! Cliente MCP de una organización.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::fmt;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use reqwest::header::{HeaderName, HeaderValue};
use rmcp::ServiceExt;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ClientCapabilities, ClientConfig, ContentBlock,
    Implementation, PaginatedRequestParams,
};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use crate::error::{OrgError, Result};
use crate::url_segura;

/// El servidor MCP de producción.
pub const DEFAULT_ENDPOINT: &str =
    "https://cfamprdygpngrjtaxzes.supabase.co/functions/v1/mcp-server";

/// Tiempo máximo de una petición, igual que el `http.Client` del contrato.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(45);

const MAX_PAGAS_TOOLS: usize = 10;
const MAX_HERRAMIENTAS: usize = 500;

/// Tope duro global de llamadas MCP salientes por cliente para impedir
/// bucles o tormentas que quemen la cuota de la organización.
pub const MAX_LLAMADAS_POR_MINUTO: usize = 60;
pub const VENTANA_MINUTO: Duration = Duration::from_secs(60);
pub const MAX_LLAMADAS_POR_HORA: usize = 400;
pub const VENTANA_HORA: Duration = Duration::from_secs(3600);
const CACHE_TOOL_SPECS_TTL: Duration = Duration::from_secs(600);

/// Foto instantánea del consumo MCP del cliente (sin tocar la red).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct McpQuota {
    pub used_minute: usize,
    pub limit_minute: usize,
    pub used_hour: usize,
    pub limit_hour: usize,
}

/// Regulador deslizante global (por minuto y por hora) sobre todas las
/// llamadas MCP del cliente.
struct PresupuestoMcp {
    max_minuto: usize,
    ventana_minuto: Duration,
    max_hora: usize,
    ventana_hora: Duration,
    marcas: VecDeque<Instant>,
}

impl PresupuestoMcp {
    fn nuevo(
        max_minuto: usize,
        ventana_minuto: Duration,
        max_hora: usize,
        ventana_hora: Duration,
    ) -> Self {
        Self {
            max_minuto,
            ventana_minuto,
            max_hora,
            ventana_hora,
            marcas: VecDeque::new(),
        }
    }

    fn por_defecto() -> Self {
        Self::nuevo(
            MAX_LLAMADAS_POR_MINUTO,
            VENTANA_MINUTO,
            MAX_LLAMADAS_POR_HORA,
            VENTANA_HORA,
        )
    }

    fn podar(&mut self, ahora: Instant) {
        while let Some(&primera) = self.marcas.front() {
            if ahora.duration_since(primera) >= self.ventana_hora {
                self.marcas.pop_front();
            } else {
                break;
            }
        }
    }

    fn en_minuto(&self, ahora: Instant) -> usize {
        self.marcas
            .iter()
            .rev()
            .take_while(|&&marca| ahora.duration_since(marca) < self.ventana_minuto)
            .count()
    }

    fn foto(&mut self, ahora: Instant) -> McpQuota {
        self.podar(ahora);
        McpQuota {
            used_minute: self.en_minuto(ahora),
            limit_minute: self.max_minuto,
            used_hour: self.marcas.len(),
            limit_hour: self.max_hora,
        }
    }

    fn permitir(&mut self, ahora: Instant) -> Result<()> {
        self.podar(ahora);
        if self.marcas.len() >= self.max_hora {
            return Err(OrgError::RateLimited {
                limit: self.max_hora,
                window_secs: self.ventana_hora.as_secs(),
            });
        }
        if self.en_minuto(ahora) >= self.max_minuto {
            return Err(OrgError::RateLimited {
                limit: self.max_minuto,
                window_secs: self.ventana_minuto.as_secs(),
            });
        }
        self.marcas.push_back(ahora);
        Ok(())
    }
}

struct CacheSpecs {
    obtenido: Instant,
    specs: Vec<ToolSpec>,
}

/// Argumentos de una herramienta MCP.
pub type JsonObject = Map<String, Value>;

/// Una herramienta que ofrece el servidor, con el JSON Schema de sus argumentos.
///
/// `Schema` es un [`Value`], así que se conserva la estructura
/// pero no el orden de las claves.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub schema: Value,
}

/// Una sesión MCP abierta contra Oruscribe.
pub struct Client {
    service: RunningService<RoleClient, ClientConfig>,
    presupuesto: Mutex<PresupuestoMcp>,
    specs_cache: Mutex<Option<CacheSpecs>>,
}

impl fmt::Debug for Client {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Nunca se imprime el token: ni siquiera se guarda en el cliente.
        formatter
            .debug_struct("Client")
            .field("closed", &self.service.is_closed())
            .finish()
    }
}

impl Client {
    /// Abre una sesión MCP. El llamador es dueño del [`Client::close`].
    ///
    /// Un endpoint vacío usa [`DEFAULT_ENDPOINT`]. Un token vacío falla antes
    /// de tocar la red.
    pub async fn open(endpoint: &str, token: &str, org_slug: &str) -> Result<Self> {
        let endpoint = if endpoint.is_empty() {
            DEFAULT_ENDPOINT
        } else {
            endpoint
        };
        if token.is_empty() {
            return Err(OrgError::IncompleteConfig);
        }
        let url = reqwest::Url::parse(endpoint)
            .map_err(|_| OrgError::InvalidEndpoint(endpoint.to_owned()))?;
        if url.host_str().is_none_or(str::is_empty) {
            return Err(OrgError::InvalidEndpoint(endpoint.to_owned()));
        }
        if !url_segura(&url) {
            return Err(OrgError::InsecureEndpoint(endpoint.to_owned()));
        }

        // El contrato solo reagrega Authorization en redirects al mismo host. Denegar
        // todo redirect cumple el mismo objetivo de seguridad: el token nunca
        // cambia de origen.
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(OrgError::HttpClient)?;

        let mut custom_headers = HashMap::new();
        if !org_slug.is_empty() {
            let value = HeaderValue::from_str(org_slug)
                .map_err(|_| OrgError::InvalidOrgSlug(org_slug.to_owned()))?;
            custom_headers.insert(HeaderName::from_static("x-org-slug"), value);
        }

        // `auth_header` recibe el token sin prefijo: rmcp agrega "Bearer ".
        let config = StreamableHttpClientTransportConfig::with_uri(endpoint.to_owned())
            .auth_header(token.to_owned())
            .custom_headers(custom_headers)
            .reinit_on_expired_session(true);
        let transport = StreamableHttpClientTransport::with_client(http, config);

        let client_info = ClientConfig::new(
            ClientCapabilities::default(),
            Implementation::new("oruscribe-agent", "0.1.0"),
        );
        let service = tokio::time::timeout(REQUEST_TIMEOUT, client_info.serve(transport))
            .await
            .map_err(|_| OrgError::Timeout {
                tool: "initialize".to_owned(),
                plazo: REQUEST_TIMEOUT.as_secs(),
            })?
            .map_err(|error| OrgError::Connect(Box::new(error)))?;

        Ok(Self {
            service,
            presupuesto: Mutex::new(PresupuestoMcp::por_defecto()),
            specs_cache: Mutex::new(None),
        })
    }

    fn cobrar_cupo(&self) -> Result<()> {
        let mut guardia = match self.presupuesto.lock() {
            Ok(g) => g,
            Err(e) => e.into_inner(),
        };
        guardia.permitir(Instant::now())
    }

    /// Devuelve el consumo MCP actual en memoria (último minuto y última hora).
    pub fn mcp_quota(&self) -> McpQuota {
        let mut guardia = match self.presupuesto.lock() {
            Ok(g) => g,
            Err(e) => e.into_inner(),
        };
        guardia.foto(Instant::now())
    }

    /// Cierra la sesión MCP.
    pub async fn close(&mut self) -> Result<()> {
        self.service
            .close()
            .await
            .map(|_| ())
            .map_err(|error| OrgError::Close(error.to_string()))
    }

    /// Lista todas las herramientas, siguiendo las páginas del servidor.
    ///
    /// `list_all_tools` de `rmcp` pide la página siguiente con el cursor y
    /// conserva el orden en que llegan. Una sesión cerrada se trata como
    /// [`OrgError::NotConnected`], igual que el cliente nil del contrato
    pub async fn tool_specs(&self) -> Result<Vec<ToolSpec>> {
        if self.service.is_closed() {
            return Err(OrgError::NotConnected);
        }
        {
            let guardia = match self.specs_cache.lock() {
                Ok(g) => g,
                Err(e) => e.into_inner(),
            };
            if let Some(cache) = guardia.as_ref()
                && cache.obtenido.elapsed() < CACHE_TOOL_SPECS_TTL
            {
                return Ok(cache.specs.clone());
            }
        }
        let inicio = Instant::now();
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        let mut vistos = BTreeSet::new();
        loop {
            self.cobrar_cupo()?;
            let restante = REQUEST_TIMEOUT.saturating_sub(inicio.elapsed());
            if restante.is_zero() {
                return Err(OrgError::Timeout {
                    tool: "tools/list".to_owned(),
                    plazo: REQUEST_TIMEOUT.as_secs(),
                });
            }
            let pagina = tokio::time::timeout(
                restante,
                self.service.list_tools(Some(
                    PaginatedRequestParams::default().with_cursor(cursor.clone()),
                )),
            )
            .await
            .map_err(|_| OrgError::Timeout {
                tool: "tools/list".to_owned(),
                plazo: REQUEST_TIMEOUT.as_secs(),
            })?
            .map_err(|source| OrgError::Call {
                tool: "tools/list".to_owned(),
                source: Box::new(source),
            })?;
            tools.extend(pagina.tools);
            if tools.len() > MAX_HERRAMIENTAS {
                return Err(OrgError::LimitExceeded(format!(
                    "tools/list superó las {MAX_HERRAMIENTAS} herramientas"
                )));
            }
            match pagina.next_cursor {
                None => break,
                Some(siguiente) => {
                    if vistos.len() >= MAX_PAGAS_TOOLS
                        || siguiente == cursor.as_deref().unwrap_or_default()
                        || !vistos.insert(siguiente.clone())
                    {
                        return Err(OrgError::LimitExceeded(
                            "tools/list superó el tope de páginas o repitió un cursor".to_owned(),
                        ));
                    }
                    cursor = Some(siguiente);
                }
            }
        }
        let specs: Vec<ToolSpec> = tools
            .into_iter()
            .map(|tool| ToolSpec {
                name: tool.name.into_owned(),
                description: tool
                    .description
                    .map(|text| text.into_owned())
                    .unwrap_or_default(),
                schema: Value::Object(std::sync::Arc::unwrap_or_clone(tool.input_schema)),
            })
            .collect();
        let mut guardia = match self.specs_cache.lock() {
            Ok(g) => g,
            Err(e) => e.into_inner(),
        };
        *guardia = Some(CacheSpecs {
            obtenido: Instant::now(),
            specs: specs.clone(),
        });
        Ok(specs)
    }

    /// Ejecuta una herramienta y devuelve su texto.
    ///
    /// Un resultado marcado como error se convierte en [`OrgError::Tool`].
    pub async fn call(&self, name: &str, args: JsonObject) -> Result<String> {
        self.cobrar_cupo()?;
        let params = CallToolRequestParams::new(name.to_owned()).with_arguments(args);
        let result = tokio::time::timeout(REQUEST_TIMEOUT, self.service.call_tool(params))
            .await
            .map_err(|_| OrgError::Timeout {
                tool: name.to_owned(),
                plazo: REQUEST_TIMEOUT.as_secs(),
            })?
            .map_err(|source| OrgError::Call {
                tool: name.to_owned(),
                source: Box::new(source),
            })?;
        let text = text_of(&result);
        if result.is_error.unwrap_or(false) {
            let message = if text.is_empty() {
                "sin detalle".to_owned()
            } else {
                text
            };
            return Err(OrgError::Tool {
                tool: name.to_owned(),
                message,
            });
        }
        Ok(text)
    }

    /// Ejecuta una herramienta y decodifica su texto como JSON.
    pub async fn call_json<T: DeserializeOwned>(&self, name: &str, args: JsonObject) -> Result<T> {
        let text = self.call(name, args).await?;
        decode_json(name, &text)
    }
}

/// Decodifica el texto de una herramienta conservando su nombre en el error.
pub(crate) fn decode_json<T: DeserializeOwned>(tool: &str, text: &str) -> Result<T> {
    serde_json::from_str(text).map_err(|source| OrgError::InvalidJson {
        tool: tool.to_owned(),
        source,
    })
}

/// Junta los bloques de texto de un resultado; el resto se ignora.
fn text_of(result: &CallToolResult) -> String {
    let mut text = String::new();
    for block in &result.content {
        if let ContentBlock::Text(value) = block {
            text.push_str(&value.text);
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_joins_text_and_ignores_images() {
        let result = CallToolResult::success(vec![
            ContentBlock::text("uno "),
            ContentBlock::image("aGk=", "image/png"),
            ContentBlock::text("dos"),
        ]);
        assert_eq!(text_of(&result), "uno dos");

        let empty = CallToolResult::success(Vec::new());
        assert_eq!(text_of(&empty), "");
    }

    #[test]
    fn presupuesto_mcp_corta_por_minuto_y_recupera_al_vencer() {
        let mut p =
            PresupuestoMcp::nuevo(3, Duration::from_secs(60), 10, Duration::from_secs(3600));
        let t0 = Instant::now();
        assert!(p.permitir(t0).is_ok());
        assert!(p.permitir(t0 + Duration::from_secs(1)).is_ok());
        assert!(p.permitir(t0 + Duration::from_secs(2)).is_ok());
        assert_eq!(
            p.foto(t0 + Duration::from_secs(3)),
            McpQuota {
                used_minute: 3,
                limit_minute: 3,
                used_hour: 3,
                limit_hour: 10,
            }
        );
        assert!(matches!(
            p.permitir(t0 + Duration::from_secs(3)),
            Err(OrgError::RateLimited {
                limit: 3,
                window_secs: 60
            })
        ));
        assert!(p.permitir(t0 + Duration::from_secs(61)).is_ok());
        // t0+2s (59s atrás) y t0+61s siguen en el último minuto -> 2; a los 63s queda solo 1.
        assert_eq!(p.foto(t0 + Duration::from_secs(61)).used_minute, 2);
        assert_eq!(p.foto(t0 + Duration::from_secs(63)).used_minute, 1);
        assert_eq!(p.foto(t0 + Duration::from_secs(61)).used_hour, 4);
    }

    #[test]
    fn presupuesto_mcp_corta_por_hora_aunque_cambie_el_minuto() {
        let mut p = PresupuestoMcp::nuevo(2, Duration::from_secs(60), 3, Duration::from_secs(3600));
        let t0 = Instant::now();
        assert!(p.permitir(t0).is_ok());
        assert!(p.permitir(t0 + Duration::from_secs(70)).is_ok());
        assert!(p.permitir(t0 + Duration::from_secs(140)).is_ok());
        assert!(matches!(
            p.permitir(t0 + Duration::from_secs(210)),
            Err(OrgError::RateLimited {
                limit: 3,
                window_secs: 3600
            })
        ));
        assert!(p.permitir(t0 + Duration::from_secs(3601)).is_ok());
    }
}
