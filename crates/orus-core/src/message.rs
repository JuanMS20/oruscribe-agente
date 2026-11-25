use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Una imagen que viaja con un mensaje de la persona.
///
/// Copia de `internal/agent/types.go:11-16`. El struct Go no tiene tags JSON:
/// las claves en disco son los nombres de los campos, en mayúscula.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Image {
    /// Tipo de la imagen (`image/png`, `image/jpeg`...).
    #[serde(rename = "MIME")]
    pub mime: String,
    /// Contenido en base64, sin el prefijo `data:`.
    #[serde(rename = "Data")]
    pub data: String,
    /// Miniatura para la ventana. No viaja al proveedor.
    #[serde(rename = "Preview")]
    pub preview: String,
}

/// Una petición del modelo para correr una herramienta.
///
/// Copia de `internal/agent/types.go:28-36`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ToolCall {
    #[serde(rename = "ID")]
    pub id: String,
    #[serde(rename = "Name")]
    pub name: String,
    /// Argumentos arbitrarios. En Go es `json.RawMessage`; acá es un valor
    /// JSON, así que se conserva la estructura pero no el orden de las claves.
    #[serde(rename = "Args")]
    pub args: Value,
    /// Marca interna: el JSON de `Args` no se pudo leer. No viaja al JSON.
    #[serde(skip)]
    pub invalid_args: bool,
}

impl ToolCall {
    /// Arma una llamada a partir de los argumentos crudos de un proveedor.
    ///
    /// Un argumento vacío es la herramienta sin argumentos; uno que no es JSON
    /// queda en `{}` y se marca en [`invalid_args`](Self::invalid_args). Sin
    /// id, el resultado no tendría a quién responder y se usa `call_{indice}`.
    pub fn con_argumentos(id: &str, name: &str, argumentos: &str, indice: usize) -> Self {
        let recortado = argumentos.trim();
        let (args, invalid_args) = if recortado.is_empty() {
            (json!({}), false)
        } else {
            match serde_json::from_str(recortado) {
                Ok(args) => (args, false),
                Err(_) => (json!({}), true),
            }
        };
        Self {
            id: id_de_respaldo(id, indice),
            name: name.to_owned(),
            args,
            invalid_args,
        }
    }

    /// Arma una llamada cuando el proveedor ya entrega los argumentos como JSON.
    ///
    /// `null` es la herramienta sin argumentos; el resto se conserva tal cual.
    pub fn desde_valor(id: &str, name: &str, args: Value, indice: usize) -> Self {
        Self {
            id: id_de_respaldo(id, indice),
            name: name.to_owned(),
            args: if args.is_null() { json!({}) } else { args },
            invalid_args: false,
        }
    }
}

fn id_de_respaldo(id: &str, indice: usize) -> String {
    if id.is_empty() {
        format!("call_{indice}")
    } else {
        id.to_owned()
    }
}

/// Un mensaje del chat, con el formato de OpenAI.
///
/// Copia de `internal/agent/types.go:19-25`. El formato en disco es contrato
/// (`internal/sessions/sessions.go:33-36`): no se renombran las claves.
///
/// `Images` y `ToolCalls` son `Option<Vec<_>>` porque Go distingue una slice
/// nil (`null`) de una vacía (`[]`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Message {
    /// system, user, assistant o tool.
    #[serde(rename = "Role")]
    pub role: String,
    #[serde(rename = "Content")]
    pub content: String,
    /// Solo en mensajes user: lo que la persona adjuntó.
    #[serde(rename = "Images")]
    pub images: Option<Vec<Image>>,
    /// Solo en mensajes assistant que piden herramientas.
    #[serde(rename = "ToolCalls")]
    pub tool_calls: Option<Vec<ToolCall>>,
    /// Solo en mensajes tool: a qué llamada responden.
    #[serde(rename = "ToolCallID")]
    pub tool_call_id: String,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Deserializa la línea tal como la escribió Go y exige que vuelva a salir
    /// byte a byte igual. Es el contrato de `internal/sessions/sessions.go:33-36`.
    fn ida_y_vuelta(esperada: &str) -> Message {
        let mensaje: Message =
            serde_json::from_str(esperada).expect("el JSON de Go debe deserializar");
        let texto = serde_json::to_string(&mensaje).expect("el mensaje debe serializar");
        assert_eq!(texto, esperada);
        mensaje
    }

    #[test]
    fn mensaje_de_usuario_con_texto() {
        let mensaje = ida_y_vuelta(
            r#"{"Role":"user","Content":"hola","Images":null,"ToolCalls":null,"ToolCallID":""}"#,
        );
        assert_eq!(mensaje.role, "user");
        assert_eq!(mensaje.content, "hola");
        assert_eq!(mensaje.images, None);
        assert_eq!(mensaje.tool_calls, None);
        assert_eq!(mensaje.tool_call_id, "");
    }

    #[test]
    fn mensaje_de_assistant_con_llamada() {
        let mensaje = ida_y_vuelta(
            r#"{"Role":"assistant","Content":"","Images":null,"ToolCalls":[{"ID":"x","Name":"leer","Args":{"a":1}}],"ToolCallID":""}"#,
        );
        assert_eq!(mensaje.images, None);
        assert_eq!(mensaje.tool_calls.as_deref().map(<[_]>::len), Some(1));
    }

    #[test]
    fn mensaje_de_usuario_con_lista_vacia() {
        let mensaje = ida_y_vuelta(
            r#"{"Role":"user","Content":"","Images":[],"ToolCalls":null,"ToolCallID":""}"#,
        );
        assert_eq!(mensaje.images, Some(Vec::new()));
        assert_eq!(mensaje.tool_calls, None);
    }

    #[test]
    fn mensaje_de_usuario_con_imagen() {
        let mensaje = ida_y_vuelta(
            r#"{"Role":"user","Content":"","Images":[{"MIME":"image/png","Data":"abc","Preview":"p"}],"ToolCalls":null,"ToolCallID":""}"#,
        );
        let imagenes = mensaje.images.expect("la imagen debe llegar");
        assert_eq!(
            imagenes,
            vec![Image {
                mime: "image/png".into(),
                data: "abc".into(),
                preview: "p".into(),
            }]
        );
    }

    #[test]
    fn mensaje_de_tool_con_su_llamada() {
        let mensaje = ida_y_vuelta(
            r#"{"Role":"tool","Content":"listo","Images":null,"ToolCalls":null,"ToolCallID":"x"}"#,
        );
        assert_eq!(mensaje.tool_call_id, "x");
        assert_eq!(mensaje.images, None);
        assert_eq!(mensaje.tool_calls, None);
    }

    #[test]
    fn args_objeto_se_lee_como_valor() {
        let mensaje: Message = serde_json::from_str(
            r#"{"Role":"assistant","Content":"","Images":null,"ToolCalls":[{"ID":"x","Name":"leer","Args":{"a":1}}],"ToolCallID":""}"#,
        )
        .expect("Args objeto debe deserializar");
        let llamadas = mensaje.tool_calls.expect("la llamada debe llegar");
        assert_eq!(llamadas[0].args, json!({"a": 1}));
        assert!(llamadas[0].args.is_object());
        assert!(!llamadas[0].invalid_args);
    }

    #[test]
    fn id_vacio_usa_call_con_el_indice() {
        let llamada = ToolCall::con_argumentos("", "leer", "{}", 3);
        assert_eq!(llamada.id, "call_3");
    }

    #[test]
    fn id_no_vacio_se_conserva() {
        let llamada = ToolCall::con_argumentos("call_x", "leer", "{}", 0);
        assert_eq!(llamada.id, "call_x");
    }

    #[test]
    fn argumentos_json_validos_se_parsean() {
        let llamada = ToolCall::con_argumentos("x", "leer", r#"{"a":1}"#, 0);
        assert_eq!(llamada.args, json!({"a": 1}));
        assert!(!llamada.invalid_args);
    }

    #[test]
    fn argumentos_invalidos_quedan_vacios_y_marcados() {
        let llamada = ToolCall::con_argumentos("x", "leer", "{roto", 0);
        assert_eq!(llamada.args, json!({}));
        assert!(llamada.invalid_args);
    }

    #[test]
    fn argumentos_vacios_quedan_vacios_y_sin_marca() {
        let llamada = ToolCall::con_argumentos("x", "leer", "", 0);
        assert_eq!(llamada.args, json!({}));
        assert!(!llamada.invalid_args);
    }

    #[test]
    fn argumentos_solo_espacios_quedan_vacios_y_sin_marca() {
        let llamada = ToolCall::con_argumentos("x", "leer", "   ", 0);
        assert_eq!(llamada.args, json!({}));
        assert!(!llamada.invalid_args);
    }

    #[test]
    fn desde_valor_con_null_usa_objeto_vacio() {
        let llamada = ToolCall::desde_valor("", "leer", Value::Null, 2);
        assert_eq!(llamada.id, "call_2");
        assert_eq!(llamada.args, json!({}));
        assert!(!llamada.invalid_args);
    }

    #[test]
    fn desde_valor_con_objeto_se_conserva() {
        let llamada = ToolCall::desde_valor("x", "leer", json!({"a": 1}), 0);
        assert_eq!(llamada.args, json!({"a": 1}));
        assert!(!llamada.invalid_args);
    }

    #[test]
    fn la_marca_interna_no_viaja_al_json() {
        let llamada = ToolCall {
            id: "x".into(),
            name: "leer".into(),
            args: json!({"a": 1}),
            invalid_args: true,
        };
        let valor = serde_json::to_value(&llamada).expect("la llamada debe serializar");
        assert!(valor.get("invalid_args").is_none());
        assert!(valor.get("invalidArgs").is_none());
        assert_eq!(valor, json!({"ID": "x", "Name": "leer", "Args": {"a": 1}}));
    }
}
