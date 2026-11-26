//! Catálogo de proveedores.
//!
//! El catálogo es la fuente de
//! verdad: dice a dónde ir y qué protocolo habla cada proveedor. `supported` se
//! calcula del protocolo, no se escribe en la entrada, así que agregar un
//! adaptador enciende la lista entera sin tocar el catálogo.
//!
//! Los nombres, direcciones y protocolos salen del catálogo público de pi-ai
//! (MIT, Mario Zechner); ver `THIRD_PARTY_NOTICES.md`.

use serde::{Deserialize, Serialize};

/// Protocolos que este arnés habla hoy.
pub const PROTOCOLO_OPENAI_CHAT: &str = "openai-completions";
pub const PROTOCOLO_ANTHROPIC_MESSAGES: &str = "anthropic-messages";
pub const PROTOCOLO_OPENAI_RESPONSES: &str = "openai-responses";
pub const PROTOCOLO_GOOGLE_GENERATIVE_AI: &str = "google-generative-ai";
pub const PROTOCOLO_GOOGLE_VERTEX: &str = "google-vertex";
pub const PROTOCOLO_MISTRAL_CONVERSATIONS: &str = "mistral-conversations";
pub const PROTOCOLO_BEDROCK_CONVERSE: &str = "bedrock-converse-stream";
pub const PROTOCOLO_OPENAI_CODEX: &str = "openai-codex-responses";
pub const PROTOCOLO_PI_MESSAGES: &str = "pi-messages";
pub const PROTOCOLO_TYPESAFE_SYSTEM_ONE: &str = "typesafe-system-one";
pub const PROTOCOLO_COPILOT: &str = "github-copilot";
pub const PROTOCOLO_CONFIGURADO: &str = "configurable";

/// Lo que este arnés sabe hablar. `implementados`.
pub(crate) fn esta_implementado(protocolo: &str) -> bool {
    protocolo == PROTOCOLO_OPENAI_CHAT || protocolo == PROTOCOLO_ANTHROPIC_MESSAGES
}

/// Una entrada del catálogo de proveedores.
///
/// Los nombres de las claves
/// JSON son contrato y conservan sus tags.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ProviderPreset {
    /// Identificador estable, el que guardan los ajustes.
    pub id: String,
    /// Lo que la persona lee en el selector.
    pub name: String,
    /// Raíz del proveedor, sin diagonal final.
    #[serde(rename = "base_url")]
    pub base_url: String,
    /// Idioma que habla el proveedor.
    pub protocol: String,
    /// Dice si este arnés lo habla.
    pub supported: bool,
    /// El proveedor anthropic-compatible espera `Authorization: Bearer`.
    #[serde(rename = "auth_bearer", default, skip_serializing_if = "es_falso")]
    pub auth_bearer: bool,
    /// Por qué una entrada no está disponible, en palabras normales.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
}

fn es_falso(valor: &bool) -> bool {
    !*valor
}

struct Entrada {
    id: &'static str,
    name: &'static str,
    base_url: &'static str,
    protocol: &'static str,
    auth_bearer: bool,
    reason: &'static str,
}

const fn entrada(
    id: &'static str,
    name: &'static str,
    base_url: &'static str,
    protocol: &'static str,
) -> Entrada {
    Entrada {
        id,
        name,
        base_url,
        protocol,
        auth_bearer: false,
        reason: "",
    }
}

const fn entrada_bearer(
    id: &'static str,
    name: &'static str,
    base_url: &'static str,
    protocol: &'static str,
) -> Entrada {
    Entrada {
        id,
        name,
        base_url,
        protocol,
        auth_bearer: true,
        reason: "",
    }
}

const fn entrada_sin_adaptador(
    id: &'static str,
    name: &'static str,
    base_url: &'static str,
    protocol: &'static str,
    reason: &'static str,
) -> Entrada {
    Entrada {
        id,
        name,
        base_url,
        protocol,
        auth_bearer: false,
        reason,
    }
}

/// La fuente de verdad, sin calcular y con `supported` en cero.
/// `catalogo`. Es una tabla de datos: una entrada por
/// línea se lee y se compara contra el contrato mejor que el envoltorio de rustfmt.
#[rustfmt::skip]
const CATALOGO: &[Entrada] = &[
    entrada("ant-ling", "Ant Ling", "https://api.ant-ling.com/v1", PROTOCOLO_OPENAI_CHAT),
    entrada("baseten", "Baseten", "https://inference.baseten.co/v1", PROTOCOLO_OPENAI_CHAT),
    entrada("cerebras", "Cerebras", "https://api.cerebras.ai/v1", PROTOCOLO_OPENAI_CHAT),
    entrada("deepseek", "DeepSeek", "https://api.deepseek.com", PROTOCOLO_OPENAI_CHAT),
    entrada("fireworks", "Fireworks", "https://api.fireworks.ai/inference/v1", PROTOCOLO_OPENAI_CHAT),
    entrada("groq", "Groq", "https://api.groq.com/openai/v1", PROTOCOLO_OPENAI_CHAT),
    entrada("huggingface", "Hugging Face", "https://router.huggingface.co/v1", PROTOCOLO_OPENAI_CHAT),
    entrada("moonshotai", "Moonshot AI", "https://api.moonshot.ai/v1", PROTOCOLO_OPENAI_CHAT),
    entrada("moonshotai-cn", "Moonshot AI (China)", "https://api.moonshot.cn/v1", PROTOCOLO_OPENAI_CHAT),
    entrada("nvidia", "NVIDIA", "https://integrate.api.nvidia.com/v1", PROTOCOLO_OPENAI_CHAT),
    entrada("openai", "OpenAI", "https://api.openai.com/v1", PROTOCOLO_OPENAI_CHAT),
    entrada("opencode", "OpenCode Zen", "https://opencode.ai/inference/openai/v1", PROTOCOLO_OPENAI_CHAT),
    entrada("opencode-go", "OpenCode Go", "https://opencode.ai/inference/go/openai/v1", PROTOCOLO_OPENAI_CHAT),
    entrada("openrouter", "OpenRouter", "https://openrouter.ai/api/v1", PROTOCOLO_OPENAI_CHAT),
    entrada("qwen-token-plan", "Qwen Token Plan", "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1", PROTOCOLO_OPENAI_CHAT),
    entrada("qwen-token-plan-cn", "Qwen Token Plan (China)", "https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1", PROTOCOLO_OPENAI_CHAT),
    entrada("qwen-token-plan-individual", "Qwen Token Plan (individual)", "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1", PROTOCOLO_OPENAI_CHAT),
    entrada("qwen-token-plan-individual-cn", "Qwen Token Plan (individual, China)", "https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1", PROTOCOLO_OPENAI_CHAT),
    entrada("together", "Together", "https://api.together.ai/v1", PROTOCOLO_OPENAI_CHAT),
    entrada("xiaomi", "Xiaomi MiMo", "https://api.xiaomimimo.com/v1", PROTOCOLO_OPENAI_CHAT),
    entrada("xiaomi-token-plan-ams", "Xiaomi Token Plan (Ámsterdam)", "https://token-plan-ams.xiaomimimo.com/v1", PROTOCOLO_OPENAI_CHAT),
    entrada("xiaomi-token-plan-cn", "Xiaomi Token Plan (China)", "https://token-plan-cn.xiaomimimo.com/v1", PROTOCOLO_OPENAI_CHAT),
    entrada("xiaomi-token-plan-sgp", "Xiaomi Token Plan (Singapur)", "https://token-plan-sgp.xiaomimimo.com/v1", PROTOCOLO_OPENAI_CHAT),
    entrada("xai", "xAI (Grok)", "https://api.x.ai/v1", PROTOCOLO_OPENAI_CHAT),
    entrada("zai", "Z.AI", "https://api.z.ai/api/coding/paas/v4", PROTOCOLO_OPENAI_CHAT),
    entrada("zai-coding-cn", "Z.AI Coding (China)", "https://open.bigmodel.cn/api/coding/paas/v4", PROTOCOLO_OPENAI_CHAT),
    entrada("anthropic", "Anthropic (Claude)", "https://api.anthropic.com", PROTOCOLO_ANTHROPIC_MESSAGES),
    entrada("kimi-coding", "Kimi For Coding", "https://api.kimi.com/coding/v1", PROTOCOLO_OPENAI_CHAT),
    entrada_bearer("minimax", "MiniMax", "https://api.minimax.io/anthropic", PROTOCOLO_ANTHROPIC_MESSAGES),
    entrada_bearer("minimax-cn", "MiniMax (China)", "https://api.minimaxi.com/anthropic", PROTOCOLO_ANTHROPIC_MESSAGES),
    entrada_bearer("vercel-ai-gateway", "Vercel AI Gateway", "https://ai-gateway.vercel.sh", PROTOCOLO_ANTHROPIC_MESSAGES),
    entrada_sin_adaptador("google", "Google Gemini", "https://generativelanguage.googleapis.com/v1beta", PROTOCOLO_GOOGLE_GENERATIVE_AI, "este arnés no habla todavía la API de Gemini"),
    entrada_sin_adaptador("mistral", "Mistral", "https://api.mistral.ai", PROTOCOLO_MISTRAL_CONVERSATIONS, "este arnés no habla todavía la API de conversaciones de Mistral"),
    entrada_sin_adaptador("meta", "Meta", "https://api.meta.ai/v1", PROTOCOLO_OPENAI_RESPONSES, "este arnés no habla todavía la API de respuestas, que es la que usa Meta"),
    entrada_sin_adaptador("openai-codex", "OpenAI Codex", "https://chatgpt.com/backend-api", PROTOCOLO_OPENAI_CODEX, "este arnés no habla todavía el protocolo de Codex"),
    entrada_sin_adaptador("radius", "Radius", "https://radius.pi.dev/v1", PROTOCOLO_PI_MESSAGES, "este arnés no habla todavía este proveedor"),
    entrada_sin_adaptador("typesafe", "TypeSafe", "https://api.typesafe.ai/v1/", PROTOCOLO_TYPESAFE_SYSTEM_ONE, "este arnés no habla todavía este proveedor"),
    entrada_sin_adaptador("amazon-bedrock", "Amazon Bedrock", "https://bedrock-runtime.us-east-1.amazonaws.com", PROTOCOLO_BEDROCK_CONVERSE, "este arnés no habla todavía la API de Bedrock de Amazon"),
    entrada_sin_adaptador("github-copilot", "GitHub Copilot", "https://api.individual.githubcopilot.com", PROTOCOLO_COPILOT, "pide cabeceras de editor que este arnés no manda"),
    entrada_sin_adaptador("azure", "Azure OpenAI", "", PROTOCOLO_CONFIGURADO, "hay que escribir la dirección de tu recurso de Azure"),
    entrada_sin_adaptador("google-vertex", "Google Vertex", "", PROTOCOLO_CONFIGURADO, "hay que escribir tu región y tu proyecto de Google Cloud"),
    entrada_sin_adaptador("cloudflare-ai-gateway", "Cloudflare AI Gateway", "", PROTOCOLO_CONFIGURADO, "hay que escribir tu cuenta y tu puerta de Cloudflare"),
    entrada_sin_adaptador("cloudflare-workers-ai", "Cloudflare Workers AI", "", PROTOCOLO_CONFIGURADO, "hay que escribir tu cuenta de Cloudflare"),
];

/// Decide `supported` a partir del protocolo y explica lo que no se puede usar.
fn con_resultado(entrada: &Entrada) -> ProviderPreset {
    let supported = esta_implementado(entrada.protocol);
    ProviderPreset {
        id: entrada.id.to_owned(),
        name: entrada.name.to_owned(),
        base_url: entrada.base_url.to_owned(),
        protocol: entrada.protocol.to_owned(),
        supported,
        auth_bearer: entrada.auth_bearer,
        reason: if !supported && entrada.reason.is_empty() {
            format!("este arnés todavía no habla {}", entrada.protocol)
        } else {
            entrada.reason.to_owned()
        },
    }
}

/// El catálogo completo, ordenado por nombre.
pub fn presets_de_proveedores() -> Vec<ProviderPreset> {
    let mut out: Vec<ProviderPreset> = CATALOGO.iter().map(con_resultado).collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Busca una entrada por su id.
pub fn buscar_preset_proveedor(id: &str) -> Option<ProviderPreset> {
    CATALOGO
        .iter()
        .find(|entrada| entrada.id == id)
        .map(con_resultado)
}

/// Busca por dirección ya validada y sin diagonal final.
pub(crate) fn buscar_por_base(normalizada: &str) -> Option<ProviderPreset> {
    CATALOGO
        .iter()
        .find(|entrada| entrada.base_url.trim_end_matches('/') == normalizada)
        .map(con_resultado)
}

/// Motivo de no disponibilidad en palabras normales.
pub(crate) fn razon_del_catalogo(preset: &ProviderPreset) -> String {
    let razon = preset.reason.trim();
    if razon.is_empty() {
        format!("este arnés todavía no habla {}", preset.protocol)
    } else {
        razon.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::{Arc, Mutex};

    use axum::body::to_bytes;
    use axum::extract::Request;
    use axum::http::{StatusCode, header};
    use axum::response::{IntoResponse, Response};
    use orus_core::Message;

    use super::*;
    use crate::factory::nuevo_proveedor;
    use crate::openai::validar_base;
    use crate::types::{ChatRequest, Provider as _};

    const RESPUESTA_CHAT: &str = r#"{"choices":[{"message":{"role":"assistant","content":"ok"}}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#;
    const RESPUESTA_ANTHROPIC: &str =
        r#"{"content":[{"type":"text","text":"ok"}],"usage":{"input_tokens":1,"output_tokens":1}}"#;

    #[derive(Default)]
    struct Visto {
        ruta: String,
        autorizacion: String,
        x_api_key: String,
        version: String,
        cuerpo: String,
    }

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

    fn con_json(cuerpo: &str) -> Response {
        (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            cuerpo.to_owned(),
        )
            .into_response()
    }

    fn cabecera(headers: &axum::http::HeaderMap, nombre: &str) -> String {
        headers
            .get(nombre)
            .and_then(|valor| valor.to_str().ok())
            .unwrap_or("")
            .to_owned()
    }

    /// El catálogo tiene que ser usable tal cual: ids únicos, nombres y
    /// direcciones que pasan la misma validación que usa quien configura.
    #[test]
    fn integridad() {
        let mut vistos = HashSet::new();
        for p in CATALOGO.iter().map(con_resultado) {
            assert!(!p.id.is_empty(), "entrada sin id: {p:?}");
            assert!(!p.name.is_empty(), "entrada {:?} sin nombre", p.id);
            assert!(vistos.insert(p.id.clone()), "id repetido: {:?}", p.id);
            assert!(!p.protocol.is_empty(), "entrada {:?} sin protocolo", p.id);
            let implementado = esta_implementado(&p.protocol);
            if implementado {
                assert!(
                    !p.base_url.is_empty(),
                    "entrada disponible {:?} sin dirección",
                    p.id
                );
            }
            if !p.base_url.is_empty() {
                validar_base(&p.base_url).unwrap_or_else(|error| {
                    panic!("entrada {:?} con dirección que no valida: {error}", p.id)
                });
            }
            if !implementado {
                assert!(
                    !p.reason.is_empty(),
                    "entrada {:?} no está disponible y no dice por qué",
                    p.id
                );
            }
            for slug in [
                PROTOCOLO_OPENAI_CHAT,
                PROTOCOLO_ANTHROPIC_MESSAGES,
                PROTOCOLO_OPENAI_RESPONSES,
                PROTOCOLO_GOOGLE_GENERATIVE_AI,
                PROTOCOLO_GOOGLE_VERTEX,
                PROTOCOLO_MISTRAL_CONVERSATIONS,
                PROTOCOLO_BEDROCK_CONVERSE,
                PROTOCOLO_OPENAI_CODEX,
                PROTOCOLO_PI_MESSAGES,
                PROTOCOLO_TYPESAFE_SYSTEM_ONE,
                PROTOCOLO_CONFIGURADO,
            ] {
                assert!(
                    !p.reason.contains(slug),
                    "entrada {:?}: el motivo {:?} suelta el nombre del protocolo",
                    p.id,
                    p.reason
                );
            }
        }
    }

    /// `supported` se calcula del protocolo, no de la entrada.
    #[test]
    fn supported_sigue_al_protocolo() {
        for p in presets_de_proveedores() {
            let quiere = esta_implementado(&p.protocol);
            assert_eq!(
                p.supported, quiere,
                "{:?}: supported={}, el protocolo {:?} quiere {}",
                p.id, p.supported, p.protocol, quiere
            );
        }
    }

    #[test]
    fn busca_por_id() {
        let p = buscar_preset_proveedor("opencode-go").expect("no encontré opencode-go");
        assert_eq!(
            p.base_url, "https://opencode.ai/inference/go/openai/v1",
            "base = {:?}, quería la de OpenCode Go",
            p.base_url
        );
        assert!(p.supported, "opencode-go no sale disponible: {}", p.reason);
        assert!(
            buscar_preset_proveedor("no-existe").is_none(),
            "un id que no existe no debe encontrarse"
        );
    }

    #[test]
    fn coincide_con_pi() {
        let esperado = [
            ("kimi-coding", "https://api.kimi.com/coding/v1"),
            ("opencode", "https://opencode.ai/inference/openai/v1"),
            ("opencode-go", "https://opencode.ai/inference/go/openai/v1"),
            (
                "qwen-token-plan-individual-cn",
                "https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1",
            ),
        ];
        for (id, base) in esperado {
            let p =
                buscar_preset_proveedor(id).unwrap_or_else(|| panic!("falta {id} en el catálogo"));
            assert_eq!(
                p.base_url, base,
                "{id}: base = {:?}, quería {base:?}",
                p.base_url
            );
            assert!(p.supported, "{id} no sale disponible: {}", p.reason);
        }
    }

    /// Cada proveedor disponible arma la petición en su ruta, con su clave y
    /// sin rutas ni parámetros de más.
    /// Acá la dirección del catálogo se cambia por
    /// el servidor loopback, porque reqwest no permite sustituir el transporte.
    #[tokio::test]
    async fn soportado_arma_la_peticion() {
        for p in presets_de_proveedores().into_iter().filter(|p| p.supported) {
            let anthropic = p.protocol == PROTOCOLO_ANTHROPIC_MESSAGES;
            let (respuesta, ruta) = if anthropic {
                (RESPUESTA_ANTHROPIC, "/v1/messages")
            } else {
                (RESPUESTA_CHAT, "/chat/completions")
            };
            let visto = Arc::new(Mutex::new(Visto::default()));
            let visto_handler = Arc::clone(&visto);
            let servidor_url = servidor(move |peticion: Request| {
                let visto = Arc::clone(&visto_handler);
                async move {
                    let (partes, cuerpo) = peticion.into_parts();
                    let datos = to_bytes(cuerpo, usize::MAX).await.expect("cuerpo");
                    let mut visto = visto.lock().expect("estado compartido");
                    visto.ruta = partes.uri.path().to_owned();
                    visto.autorizacion = cabecera(&partes.headers, "authorization");
                    visto.x_api_key = cabecera(&partes.headers, "x-api-key");
                    visto.version = cabecera(&partes.headers, "anthropic-version");
                    visto.cuerpo = String::from_utf8_lossy(&datos).into_owned();
                    con_json(respuesta)
                }
            })
            .await;

            let preset = ProviderPreset {
                base_url: servidor_url,
                ..p.clone()
            };
            let proveedor = nuevo_proveedor(&preset, "clave-de-prueba", None)
                .unwrap_or_else(|error| panic!("{}: NuevoProveedor: {error}", p.id));
            proveedor
                .chat(ChatRequest {
                    model: "modelo".to_owned(),
                    messages: vec![Message {
                        role: "user".to_owned(),
                        content: "hola".to_owned(),
                        ..Message::default()
                    }],
                    tools: Vec::new(),
                    on_delta: None,
                })
                .await
                .unwrap_or_else(|error| panic!("{}: Chat: {error}", p.id));

            let visto = visto.lock().expect("estado compartido");
            assert_eq!(visto.ruta, ruta, "{}: url = {:?}", p.id, visto.ruta);
            assert!(
                visto.cuerpo.contains(r#""model":"modelo""#),
                "{}: el cuerpo no lleva el modelo: {}",
                p.id,
                visto.cuerpo
            );
            if anthropic {
                if p.auth_bearer {
                    assert_eq!(
                        visto.autorizacion, "Bearer clave-de-prueba",
                        "{}: Authorization = {:?}",
                        p.id, visto.autorizacion
                    );
                } else {
                    assert_eq!(
                        visto.x_api_key, "clave-de-prueba",
                        "{}: x-api-key = {:?}",
                        p.id, visto.x_api_key
                    );
                }
                assert!(
                    !visto.version.is_empty(),
                    "{}: falta la cabecera anthropic-version",
                    p.id
                );
                assert!(
                    visto.cuerpo.contains(r#""max_tokens""#),
                    "{}: el cuerpo no lleva max_tokens: {}",
                    p.id,
                    visto.cuerpo
                );
            } else {
                assert_eq!(
                    visto.autorizacion, "Bearer clave-de-prueba",
                    "{}: Authorization = {:?}",
                    p.id, visto.autorizacion
                );
            }
        }
    }

    /// OpenAI y xAI hablan chat completions en la dirección del catálogo.
    #[test]
    fn ofrece_openai_y_xai() {
        for id in ["openai", "xai"] {
            let p =
                buscar_preset_proveedor(id).unwrap_or_else(|| panic!("falta {id} en el catálogo"));
            assert_eq!(
                p.protocol, PROTOCOLO_OPENAI_CHAT,
                "{id} habla {:?}, quería {PROTOCOLO_OPENAI_CHAT:?}",
                p.protocol
            );
            assert!(p.supported, "{id} no sale disponible: {}", p.reason);
        }
    }

    /// La lista que pinta la ventana va ordenada y sin perder entradas.
    #[test]
    fn ordenado() {
        let lista = presets_de_proveedores();
        assert_eq!(
            lista.len(),
            CATALOGO.len(),
            "la lista tiene {} entradas, el catálogo {}",
            lista.len(),
            CATALOGO.len()
        );
        for i in 1..lista.len() {
            assert!(
                lista[i - 1].name <= lista[i].name,
                "{:?} viene antes que {:?}",
                lista[i - 1].name,
                lista[i].name
            );
        }
    }

    /// Una entrada soportada manda la clave a https, nunca a una dirección sin
    /// esquema.
    #[test]
    fn soportados_mandan_llave_a_sitio_seguro() {
        for p in presets_de_proveedores().into_iter().filter(|p| p.supported) {
            assert!(
                p.base_url.starts_with("https://"),
                "{:?}: {:?} no es https, y ahí viaja la clave",
                p.id,
                p.base_url
            );
        }
    }
}
