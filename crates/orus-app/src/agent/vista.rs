//! Lo que la ventana pinta del agente y de su diario.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use orus_core::{Image, Message};
use serde::{Deserialize, Serialize};

use crate::approvals::AgentApproval;
use crate::wake::WakeEntry;

use super::AgentStep;

/// Una imagen que la ventana pinta. `data` y `preview` no
/// viajan vacíos. El envío la deserializa y `normalizar_imagenes` la valida
/// antes de que toque la conversación.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentImage {
    #[serde(default)]
    pub mime: String,
    /// `Data` no lleva `omitempty`: viaja también vacío cuando la
    /// miniatura lo reemplaza en la vista.
    #[serde(default)]
    pub data: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub preview: String,
}

/// Un mensaje visible de la conversación.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AgentMessage {
    pub role: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<AgentImage>>,
}

/// Lo que la ventana pinta del agente. El `null` del contrato es
/// una lista vacía acá: nunca se serializa `null` para los campos de lista.
#[derive(Debug, Clone, Serialize)]
pub struct AgentView {
    pub configured: bool,
    /// En modo org sin sesión ni timbre no hay cerebro posible: la ventana
    /// ofrece entrar en vez de dejar el chat «pensando».
    pub needs_login: bool,
    pub kind: String,
    pub base_url: String,
    pub model: String,
    pub mode: String,
    pub key_hint: String,
    pub running: bool,
    pub messages: Vec<AgentMessage>,
    pub steps: Vec<AgentStep>,
    pub approvals: Vec<AgentApproval>,
    pub draft: String,
    pub conv_id: String,
    pub tokens_in: i64,
    pub tokens_out: i64,
    #[serde(default, skip_serializing_if = "es_cero_f64")]
    pub cost_usd: f64,
    pub folders: Vec<String>,
    pub programs: Vec<String>,
    pub wake_enabled: bool,
    pub wake_per_minute: i64,
    pub wake_per_day: i64,
    pub wake_tokens_per_day: i64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub voice_stt_url: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub voice_stt_model: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub voice_tts_url: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub voice_tts_model: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub voice_tts_voice: String,
    pub wake_log: Vec<WakeEntry>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub last_error: String,
}

/// La respuesta a "¿este proveedor funciona con esta clave y este modelo?".
/// No guarda nada y nunca incluye la clave.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct AgentProbe {
    pub ok: bool,
    pub ms: i64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reply: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error: String,
}

/// Lo que la ventana ve del diario.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct JournalEntryView {
    pub id: String,
    pub path: String,
    pub existed: bool,
    pub size: i64,
    pub when: DateTime<Utc>,
}

/// Lo que la persona escribió y lo que el modelo contestó; los pasos de
/// herramientas van aparte.
pub(crate) fn visibles(historial: &[Message]) -> Vec<AgentMessage> {
    let mut salida = Vec::new();
    for mensaje in historial {
        match mensaje.role.as_str() {
            "user" => salida.push(AgentMessage {
                role: "user".to_owned(),
                content: mensaje.content.clone(),
                images: imagenes_visibles(mensaje.images.clone()),
            }),
            "assistant"
                if !mensaje.content.is_empty()
                    && mensaje.tool_calls.as_ref().is_none_or(Vec::is_empty) =>
            {
                salida.push(AgentMessage {
                    role: "assistant".to_owned(),
                    content: mensaje.content.clone(),
                    images: None,
                });
            }
            "assistant" if mensaje.tool_calls.as_ref().is_none_or(Vec::is_empty) => {
                salida.push(AgentMessage {
                    role: "assistant".to_owned(),
                    content: "(sin respuesta)".to_owned(),
                    images: None,
                });
            }
            _ => {}
        }
    }
    salida
}

const MAX_TEXTO_PASO: usize = 2000;

pub(crate) fn recortar_paso(texto: &str) -> String {
    let mut salida: String = texto.chars().take(MAX_TEXTO_PASO).collect();
    if texto.chars().count() > MAX_TEXTO_PASO {
        salida.push('…');
    }
    salida
}

/// Reconstruye los pasos `tool_call` y `tool_result` desde el historial
/// guardado en disco para que al reabrir una sesión la ventana siga mostrando
/// la trazabilidad real de herramientas.
pub(crate) fn reconstruir_pasos(historial: &[Message], at: DateTime<Utc>) -> Vec<AgentStep> {
    let mut nombres: HashMap<&str, &str> = HashMap::new();
    let mut pasos = Vec::new();
    for mensaje in historial {
        match mensaje.role.as_str() {
            "assistant" => {
                if let Some(llamadas) = &mensaje.tool_calls {
                    for llamada in llamadas {
                        if !llamada.id.is_empty() {
                            nombres.insert(llamada.id.as_str(), llamada.name.as_str());
                        }
                        let args = if llamada.args.is_null() {
                            String::new()
                        } else {
                            recortar_paso(&llamada.args.to_string())
                        };
                        pasos.push(AgentStep {
                            at,
                            kind: "tool_call".to_owned(),
                            tool: llamada.name.clone(),
                            text: args,
                        });
                    }
                }
            }
            "tool" => {
                let tool = nombres
                    .get(mensaje.tool_call_id.as_str())
                    .copied()
                    .unwrap_or("tool")
                    .to_owned();
                pasos.push(AgentStep {
                    at,
                    kind: "tool_result".to_owned(),
                    tool,
                    text: recortar_paso(mensaje.content.trim()),
                });
            }
            _ => {}
        }
    }
    pasos
}

/// La miniatura es lo que se pinta; sin ella cae al archivo completo
fn imagenes_visibles(imagenes: Option<Vec<Image>>) -> Option<Vec<AgentImage>> {
    let imagenes = imagenes.filter(|lista| !lista.is_empty())?;
    Some(
        imagenes
            .into_iter()
            .map(|imagen| {
                let preview = imagen.preview;
                let data = if preview.is_empty() {
                    imagen.data
                } else {
                    String::new()
                };
                AgentImage {
                    mime: imagen.mime,
                    data,
                    preview,
                }
            })
            .collect(),
    )
}

fn es_cero_f64(valor: &f64) -> bool {
    *valor == 0.0
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn mensaje_con(imagen: Image) -> Vec<Message> {
        vec![Message {
            role: "user".to_owned(),
            content: "mira".to_owned(),
            images: Some(vec![imagen]),
            ..Message::default()
        }]
    }

    /// La miniatura es lo que se pinta; el archivo completo se guarda para el
    /// proveedor y no se repite en cada sondeo.
    #[test]
    fn la_vista_usa_la_miniatura_y_no_repite_el_archivo() {
        let visibles = visibles(&mensaje_con(Image {
            mime: "image/png".to_owned(),
            data: "completa".to_owned(),
            preview: "data:image/png;base64,mini".to_owned(),
        }));

        assert_eq!(
            visibles[0].images,
            Some(vec![AgentImage {
                mime: "image/png".to_owned(),
                data: String::new(),
                preview: "data:image/png;base64,mini".to_owned(),
            }]),
            "con miniatura, la vista no repite el data completo"
        );
    }

    #[test]
    fn la_vista_cae_al_archivo_completo_sin_miniatura() {
        let visibles = visibles(&mensaje_con(Image {
            mime: "image/png".to_owned(),
            data: "completa".to_owned(),
            preview: String::new(),
        }));

        assert_eq!(
            visibles[0].images,
            Some(vec![AgentImage {
                mime: "image/png".to_owned(),
                data: "completa".to_owned(),
                preview: String::new(),
            }]),
            "sin miniatura, la vista conserva el archivo completo"
        );
    }
}
