//! Fábrica de proveedores a partir del catálogo o de una dirección escrita a
//! mano.
//!
//! Un protocolo sin adaptador es un
//! error con la razón del catálogo; nunca se degrada a `OpenAICompat` contra
//! una dirección que habla otro idioma.

use async_trait::async_trait;

use crate::anthropic::Anthropic;
use crate::catalog::{
    PROTOCOLO_ANTHROPIC_MESSAGES, PROTOCOLO_OPENAI_CHAT, ProviderPreset, buscar_por_base,
    razon_del_catalogo,
};
use crate::error::ProviderError;
use crate::openai::{OpenAICompat, cliente_http, validar_base};
use crate::types::{ChatReply, ChatRequest, Provider};

/// El proveedor que armó la fábrica. El contrato devuelve la interfaz `Provider`; el
/// enum conserva el tipo concreto para que la prueba de ruteo lo pueda mirar.
#[derive(Debug)]
pub enum Proveedor {
    OpenAI(OpenAICompat),
    Anthropic(Anthropic),
}

#[async_trait]
impl Provider for Proveedor {
    async fn chat(&self, req: ChatRequest) -> Result<ChatReply, ProviderError> {
        match self {
            Self::OpenAI(proveedor) => proveedor.chat(req).await,
            Self::Anthropic(proveedor) => proveedor.chat(req).await,
        }
    }
}

fn cliente_o_defecto(cliente: Option<reqwest::Client>) -> Result<reqwest::Client, ProviderError> {
    match cliente {
        Some(cliente) => Ok(cliente),
        None => cliente_http(),
    }
}

/// Construye el proveedor de una entrada del catálogo según su protocolo.
/// `NuevoProveedor`.
pub fn nuevo_proveedor(
    preset: &ProviderPreset,
    api_key: impl Into<String>,
    cliente: Option<reqwest::Client>,
) -> Result<Proveedor, ProviderError> {
    if preset.base_url.trim().is_empty() {
        // Entradas a medias (azure, vertex, ...): no hay dirección que validar.
        return Err(ProviderError::local(razon_del_catalogo(preset)));
    }
    let base = validar_base(&preset.base_url)?;
    let api_key = api_key.into();
    match preset.protocol.as_str() {
        PROTOCOLO_OPENAI_CHAT => Ok(Proveedor::OpenAI(OpenAICompat {
            base_url: base,
            api_key,
            client: cliente_o_defecto(cliente)?,
            retry_delay: None,
        })),
        PROTOCOLO_ANTHROPIC_MESSAGES => Ok(Proveedor::Anthropic(Anthropic {
            base_url: base,
            api_key,
            client: cliente_o_defecto(cliente)?,
            retry_delay: None,
            max_tokens: 0,
            auth_bearer: preset.auth_bearer,
        })),
        _ => Err(ProviderError::local(razon_del_catalogo(preset))),
    }
}

/// Construye un proveedor para una dirección escrita a mano. Si coincide con el
/// catálogo, usa el protocolo que el catálogo declara; si no, cae en
/// `OpenAICompat` (compatibilidad hacia atrás con direcciones propias).
/// `NuevoProveedorPorURL`.
pub fn nuevo_proveedor_por_url(
    base_url: &str,
    api_key: impl Into<String>,
    cliente: Option<reqwest::Client>,
) -> Result<Proveedor, ProviderError> {
    let normalizada = validar_base(base_url)?;
    match buscar_por_base(&normalizada) {
        Some(preset) => nuevo_proveedor(&preset, api_key, cliente),
        None => Ok(Proveedor::OpenAI(OpenAICompat {
            base_url: normalizada,
            api_key: api_key.into(),
            client: cliente_o_defecto(cliente)?,
            retry_delay: None,
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{PROTOCOLO_CONFIGURADO, buscar_preset_proveedor};

    fn preset(id: &str, base_url: &str, protocol: &str) -> ProviderPreset {
        ProviderPreset {
            id: id.to_owned(),
            base_url: base_url.to_owned(),
            protocol: protocol.to_owned(),
            ..ProviderPreset::default()
        }
    }

    #[test]
    fn enruta_por_protocolo() {
        let anthropic = nuevo_proveedor(
            &preset(
                "anthropic",
                "https://api.anthropic.com",
                PROTOCOLO_ANTHROPIC_MESSAGES,
            ),
            "sk-ant-test",
            None,
        )
        .expect("NuevoProveedor");
        assert!(
            matches!(anthropic, Proveedor::Anthropic(_)),
            "el proveedor no es Anthropic"
        );

        let openai = nuevo_proveedor(
            &preset("openai", "https://api.openai.com/v1", PROTOCOLO_OPENAI_CHAT),
            "sk-test",
            None,
        )
        .expect("NuevoProveedor");
        assert!(
            matches!(openai, Proveedor::OpenAI(_)),
            "el proveedor no es OpenAICompat"
        );
    }

    #[test]
    fn sin_direccion_usa_la_razon_del_catalogo() {
        let mut preset = preset("azure", "", PROTOCOLO_CONFIGURADO);
        preset.reason = "hay que escribir la dirección de tu recurso de Azure".to_owned();
        let error =
            nuevo_proveedor(&preset, "sk-test", None).expect_err("una base vacía debe fallar");
        assert!(
            error.to_string().contains("Azure"),
            "err = {error}, quería la razón del catálogo"
        );
    }

    #[test]
    fn rechaza_protocolo_sin_adaptador() {
        let preset =
            buscar_preset_proveedor("mistral").expect("no encontré mistral en el catálogo");
        let error = nuevo_proveedor(&preset, "sk-test", None)
            .expect_err("un protocolo sin adaptador debe fallar");
        assert!(
            error.to_string().contains("Mistral"),
            "err = {error}, quería la razón del catálogo"
        );
    }

    #[test]
    fn por_url_reconoce_el_catalogo_y_lo_escrito_a_mano() {
        let anthropic = nuevo_proveedor_por_url("https://api.anthropic.com", "sk-ant-test", None)
            .expect("NuevoProveedorPorURL");
        assert!(
            matches!(anthropic, Proveedor::Anthropic(_)),
            "api.anthropic.com no salió Anthropic"
        );

        let propio = nuevo_proveedor_por_url("https://minimax.example.com/custom", "sk-test", None)
            .expect("NuevoProveedorPorURL");
        assert!(
            matches!(propio, Proveedor::OpenAI(_)),
            "una dirección propia no salió OpenAICompat"
        );
    }
}
