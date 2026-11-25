use serde::{Deserialize, Serialize};

/// Un evento del turno. Es el contrato con el cliente: una línea JSON por
/// evento.
///
/// Reproduce el struct `output` de `internal/harness/run.go:642-658`, no una
/// versión "más limpia". Tres cosas que no se tocan:
/// - `args` viaja como texto JSON, no como objeto (`run.go:652`).
/// - `tool_result` no lleva `ok`: el éxito o el fallo van dentro de `text`.
/// - `denied` reemplaza a `tool_result`; una llamada negada no emite resultado.
///
/// Los campos opcionales son los que el Go omite con `omitempty`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// Arranca el turno. `session` es el id que el cliente puede reanudar.
    Start {
        #[serde(default, skip_serializing_if = "texto_vacio")]
        session: String,
        #[serde(default, skip_serializing_if = "texto_vacio")]
        model: String,
        #[serde(default, skip_serializing_if = "texto_vacio")]
        cwd: String,
        #[serde(default, skip_serializing_if = "texto_vacio")]
        mode: String,
        #[serde(default, skip_serializing_if = "org_ausente")]
        org: Option<bool>,
        #[serde(default, skip_serializing_if = "nota_ausente")]
        org_note: Option<String>,
    },
    /// Un trozo de texto del modelo, tal como llegó por streaming.
    Delta {
        #[serde(default, skip_serializing_if = "texto_vacio")]
        text: String,
    },
    /// El modelo pidió una herramienta. `args` es el JSON serializado.
    ToolCall {
        #[serde(default, skip_serializing_if = "texto_vacio")]
        tool: String,
        #[serde(default, skip_serializing_if = "texto_vacio")]
        args: String,
    },
    /// La herramienta terminó. El resultado, o el error, va en `text`.
    ToolResult {
        #[serde(default, skip_serializing_if = "texto_vacio")]
        tool: String,
        #[serde(default, skip_serializing_if = "texto_vacio")]
        text: String,
    },
    /// La política negó la herramienta. No hay `tool_result` para esta llamada.
    Denied {
        #[serde(default, skip_serializing_if = "texto_vacio")]
        tool: String,
    },
    /// El texto completo de la respuesta, cuando el modelo no pidió herramientas.
    Answer {
        #[serde(default, skip_serializing_if = "texto_vacio")]
        text: String,
    },
    /// Consumo del turno. Se emite siempre, antes de `done` o de `error`.
    Usage {
        #[serde(default, skip_serializing_if = "texto_vacio")]
        session: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt_tokens: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        completion_tokens: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cost_usd: Option<f64>,
    },
    /// El turno terminó bien, después de guardar la sesión.
    Done {
        #[serde(default, skip_serializing_if = "texto_vacio")]
        session: String,
    },
    /// El turno terminó mal. Hoy `exit` siempre es 1.
    Error {
        #[serde(default, skip_serializing_if = "texto_vacio")]
        message: String,
        #[serde(default, skip_serializing_if = "es_cero")]
        exit: i32,
    },
}

/// El Go usa `omitempty` en todos los campos de texto (`run.go:642-658`).
fn texto_vacio(texto: &str) -> bool {
    texto.is_empty()
}

/// `org` es un `bool` con `omitempty`: solo viaja cuando es verdadero.
fn org_ausente(org: &Option<bool>) -> bool {
    !matches!(org, Some(true))
}

/// `org_note` es un `string` con `omitempty`.
fn nota_ausente(nota: &Option<String>) -> bool {
    nota.as_deref().is_none_or(str::is_empty)
}

/// `exit` es un `int` con `omitempty`: el cero no viaja.
fn es_cero(n: &i32) -> bool {
    *n == 0
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn los_campos_vacios_no_viajan() {
        let start = Event::Start {
            session: "s".into(),
            model: "m".into(),
            cwd: "c".into(),
            mode: "ask".into(),
            org: Some(false),
            org_note: Some(String::new()),
        };
        let valor = serde_json::to_value(&start).expect("start serializa");
        assert!(valor.get("org").is_none(), "org falso no viaja: {valor}");
        assert!(
            valor.get("org_note").is_none(),
            "nota vacía no viaja: {valor}"
        );

        let answer = Event::Answer {
            text: String::new(),
        };
        let valor = serde_json::to_value(&answer).expect("answer serializa");
        assert!(
            valor.get("text").is_none(),
            "respuesta vacía no lleva text: {valor}"
        );

        let error = Event::Error {
            message: "falló".into(),
            exit: 0,
        };
        let valor = serde_json::to_value(&error).expect("error serializa");
        assert!(valor.get("exit").is_none(), "exit cero no viaja: {valor}");
    }
}
