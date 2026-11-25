//! La herramienta `task`: un subagente de solo lectura con contexto limpio.
//!
//! Port de `internal/subagent/subagent.go:16-104`. El subagente reutiliza el
//! [`Runner`] en [`Mode::ReadOnly`]: no escribe, no ejecuta programas y no
//! puede delegar otra tarea.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use orus_core::Message;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::policy::{Mode, Risk};
use crate::runner::{CancelToken, Runner, Usage};
use crate::types::{Provider, Tool, ToolError, ToolSpec};

/// El prompt más grande que el padre puede entregar al subagente.
/// `MaxPrompt` (`subagent.go:17-18`).
pub const MAX_PROMPT: usize = 8 << 10;

/// El tope de pasos cuando quien construye la herramienta no fija uno.
/// `DefaultSteps` (`subagent.go:19-20`).
pub const DEFAULT_STEPS: i64 = 8;

/// La descripción que ve el modelo. `Spec` (`subagent.go:32-40`).
const DESCRIPCION: &str = "Delega una exploración de solo lectura a un subagente con contexto limpio. Úsalo cuando haya que revisar varios archivos o hacer una búsqueda larga sin llenar la conversación principal. El subagente no puede escribir, ejecutar programas ni delegar otra tarea.";

/// Lo que se agrega al sistema del padre. `Run` (`subagent.go:65-68`).
const INSTRUCCION_SUBAGENTE: &str = "\n\nEres un subagente de exploración. Trabajas con contexto limpio: no ves la conversación principal. Lee lo que haga falta y devuelve un resumen con las rutas y los datos que encontraste. No cambies archivos, no ejecutes programas y no inventes resultados.";

/// La herramienta `task`.
///
/// Port de `Tool` (`subagent.go:23-30`). El padre arma `tools` con
/// [`read_tools`] y no incluye esta misma herramienta: así el subagente no se
/// delega.
pub struct Subagent {
    pub provider: Arc<dyn Provider>,
    pub model: String,
    pub system: String,
    pub tools: Vec<Arc<dyn Tool>>,
    pub max_steps: i64,
    /// Lo que gastaron las ejecuciones del subagente, hasta que el runner
    /// padre lo lee con `Tool::consumo`.
    uso: Arc<Mutex<Usage>>,
}

impl Subagent {
    /// Arma la herramienta con los valores cero de Go. El proveedor y el
    /// modelo son lo único obligatorio.
    pub fn new(provider: Arc<dyn Provider>, model: impl Into<String>) -> Self {
        Self {
            provider,
            model: model.into(),
            system: String::new(),
            tools: Vec::new(),
            max_steps: 0,
            uso: Arc::new(Mutex::new(Usage::default())),
        }
    }
}

/// Los argumentos de `task`. `Run` (`subagent.go:44-53`).
///
/// Los campos son `Option` porque `encoding/json` de Go deja el valor cero
/// cuando el campo falta o llega `null`, y solo falla con otro tipo.
#[derive(Debug, Default, Deserialize)]
struct Entrada {
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    prompt: Option<String>,
}

#[async_trait]
impl Tool for Subagent {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "task".to_owned(),
            description: DESCRIPCION.to_owned(),
            schema: json!({
                "type": "object",
                "properties": {
                    "description": {
                        "type": "string",
                        "description": "Qué hay que investigar, en una línea"
                    },
                    "prompt": {
                        "type": "string",
                        "description": "La pregunta completa para el subagente"
                    }
                },
                "required": ["prompt"]
            }),
        }
    }

    fn risk(&self) -> Option<Risk> {
        Some(Risk::Read)
    }

    async fn run(&self, args: Value) -> Result<String, ToolError> {
        // Go solo parsea cuando hay argumentos; `null` también deja la
        // estructura en cero (`subagent.go:49-53`).
        let entrada = if args.is_null() {
            Entrada::default()
        } else {
            serde_json::from_value(args)
                .map_err(|error| ToolError::Message(format!("argumentos inválidos: {error}")))?
        };
        let prompt = entrada.prompt.unwrap_or_default();
        if prompt.trim().is_empty() {
            return Err(ToolError::Message(
                "falta el prompt del subagente".to_owned(),
            ));
        }
        // Go mide bytes (`len`), no runas.
        if prompt.len() > MAX_PROMPT {
            return Err(ToolError::Message(format!(
                "el prompt del subagente pasa el tope de {MAX_PROMPT} bytes"
            )));
        }

        let pasos = if self.max_steps <= 0 {
            DEFAULT_STEPS
        } else {
            self.max_steps
        };
        let mut sistema = self.system.clone();
        sistema.push_str(INSTRUCCION_SUBAGENTE);
        let descripcion = entrada.description.as_deref().unwrap_or_default().trim();
        if !descripcion.is_empty() {
            sistema.push_str("\nObjetivo: ");
            sistema.push_str(descripcion);
        }
        let mut runner = Runner::new(Arc::clone(&self.provider), self.model.clone());
        runner.system = sistema;
        runner.tools = self.tools.clone();
        runner.mode = Some(Mode::ReadOnly);
        runner.max_steps = pasos;
        runner.uso_compartido = Some(Arc::clone(&self.uso));

        // El trait Tool no recibe contexto (D41): el token es nuevo y el runner
        // padre corta este futuro con su timeout o su cancelación.
        let (hist, resultado) = runner
            .run(
                &[Message {
                    role: "user".to_owned(),
                    content: prompt,
                    ..Message::default()
                }],
                &CancelToken::new(),
            )
            .await;
        resultado.map_err(|error| ToolError::Message(error.to_string()))?;
        for mensaje in hist.iter().rev() {
            if mensaje.role == "assistant" && !mensaje.content.trim().is_empty() {
                return Ok(mensaje.content.clone());
            }
        }
        Ok("(el subagente no devolvió texto)".to_owned())
    }

    fn consumo(&self) -> Usage {
        std::mem::take(
            &mut *self
                .uso
                .lock()
                .unwrap_or_else(|envenenado| envenenado.into_inner()),
        )
    }
}

/// Deja solo las herramientas que no cambian nada. `task` no está en la lista
/// de entrada, así que un subagente no se delega.
///
/// `ReadTools` (`subagent.go:94-104`).
pub fn read_tools(todas: &[Arc<dyn Tool>]) -> Vec<Arc<dyn Tool>> {
    todas
        .iter()
        .filter(|tool| tool.risk() == Some(Risk::Read))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use async_trait::async_trait;
    use orus_core::ToolCall;
    use serde_json::{Value, json};

    use super::*;
    use crate::runner::RunnerError;
    use crate::runner::tests::{
        Paso, ProveedorFalso, como_proveedor, respuesta_herramienta, respuesta_texto,
    };
    use crate::types::ChatReply;

    struct Herramienta {
        nombre: &'static str,
        riesgo: Option<Risk>,
    }

    #[async_trait]
    impl Tool for Herramienta {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: self.nombre.to_owned(),
                description: "prueba".to_owned(),
                schema: json!({"type": "object", "properties": {}}),
            }
        }

        fn risk(&self) -> Option<Risk> {
            self.riesgo
        }

        async fn run(&self, _args: Value) -> Result<String, ToolError> {
            Ok("contenido".to_owned())
        }
    }

    fn herramienta(nombre: &'static str, riesgo: Risk) -> Arc<dyn Tool> {
        Arc::new(Herramienta {
            nombre,
            riesgo: Some(riesgo),
        })
    }

    #[test]
    fn task_spec_and_risk() {
        let proveedor = Arc::new(ProveedorFalso::nuevo(Vec::new()));
        let task = Subagent::new(como_proveedor(&proveedor), "m");

        let spec = task.spec();
        assert_eq!(spec.name, "task");
        assert_eq!(
            spec.description,
            "Delega una exploración de solo lectura a un subagente con contexto limpio. Úsalo cuando haya que revisar varios archivos o hacer una búsqueda larga sin llenar la conversación principal. El subagente no puede escribir, ejecutar programas ni delegar otra tarea."
        );
        assert_eq!(
            spec.schema,
            json!({
                "type": "object",
                "properties": {
                    "description": {
                        "type": "string",
                        "description": "Qué hay que investigar, en una línea"
                    },
                    "prompt": {
                        "type": "string",
                        "description": "La pregunta completa para el subagente"
                    }
                },
                "required": ["prompt"]
            })
        );
        assert_eq!(task.risk(), Some(Risk::Read));
        assert_eq!(MAX_PROMPT, 8192);
        assert_eq!(DEFAULT_STEPS, 8);
    }

    #[tokio::test]
    async fn task_rejects_empty_prompt() {
        let proveedor = Arc::new(ProveedorFalso::nuevo(Vec::new()));
        let task = Subagent::new(como_proveedor(&proveedor), "m");

        for args in [json!({"prompt": "   "}), json!({}), json!({"prompt": null})] {
            let error = task
                .run(args)
                .await
                .expect_err("un prompt vacío no puede correr");
            assert_eq!(error.to_string(), "falta el prompt del subagente");
        }
        assert_eq!(proveedor.llamadas(), 0);
    }

    #[tokio::test]
    async fn task_rejects_oversized_prompt() {
        let proveedor = Arc::new(ProveedorFalso::nuevo(Vec::new()));
        let task = Subagent::new(como_proveedor(&proveedor), "m");

        let error = task
            .run(json!({"prompt": "x".repeat(MAX_PROMPT + 1)}))
            .await
            .expect_err("un prompt de más de 8 KiB no puede correr");

        assert_eq!(
            error.to_string(),
            "el prompt del subagente pasa el tope de 8192 bytes"
        );
        assert_eq!(proveedor.llamadas(), 0);
    }

    #[test]
    fn read_tools_keeps_only_read() {
        let todo = vec![
            herramienta("leer", Risk::Read),
            herramienta("escribir", Risk::Write),
            herramienta("ejecutar", Risk::Exec),
            Arc::new(Herramienta {
                nombre: "sin-riesgo",
                riesgo: None,
            }) as Arc<dyn Tool>,
        ];

        let vistas = read_tools(&todo);

        assert_eq!(vistas.len(), 1);
        assert_eq!(vistas[0].spec().name, "leer");
    }

    #[tokio::test]
    async fn task_returns_last_assistant_text() {
        const INSTRUCCION: &str = "\n\nEres un subagente de exploración. Trabajas con contexto limpio: no ves la conversación principal. Lee lo que haga falta y devuelve un resumen con las rutas y los datos que encontraste. No cambies archivos, no ejecutes programas y no inventes resultados.";

        let proveedor = Arc::new(ProveedorFalso::nuevo(vec![
            Paso::Responde(respuesta_herramienta("c1", "leer")),
            Paso::Responde(respuesta_texto("resumen final")),
        ]));
        let mut task = Subagent::new(como_proveedor(&proveedor), "modelo-hijo");
        task.system = "sistema del padre".to_owned();
        task.tools = vec![herramienta("leer", Risk::Read)];
        task.max_steps = 3;

        let salida = task
            .run(json!({
                "description": "  revisar el crate  ",
                "prompt": "¿dónde está el runner?"
            }))
            .await
            .expect("el subagente debía responder");

        assert_eq!(salida, "resumen final");
        let pedidos = proveedor.pedidos();
        assert_eq!(pedidos.len(), 2);
        assert_eq!(pedidos[0].messages[0].role, "system");
        assert_eq!(
            pedidos[0].messages[0].content,
            format!("sistema del padre{INSTRUCCION}\nObjetivo: revisar el crate")
        );
        assert_eq!(pedidos[0].messages[1].role, "user");
        assert_eq!(pedidos[0].messages[1].content, "¿dónde está el runner?");
        assert_eq!(pedidos[0].tools.len(), 1);
        assert_eq!(pedidos[0].tools[0].name, "leer");
    }

    fn respuesta_task() -> ChatReply {
        ChatReply {
            message: Message {
                role: "assistant".to_owned(),
                tool_calls: Some(vec![ToolCall {
                    id: "t1".to_owned(),
                    name: "task".to_owned(),
                    args: json!({"prompt": "busca datos"}),
                    invalid_args: false,
                }]),
                ..Message::default()
            },
            prompt_tokens: 0,
            completion_tokens: 0,
        }
    }

    /// El consumo del subagente entra en el uso del padre: el arnés lo guarda
    /// en la sesión y deriva de ahí el coste (issue #126).
    #[tokio::test]
    async fn task_suma_su_consumo_al_uso_del_padre() {
        let proveedor = Arc::new(ProveedorFalso::nuevo(vec![
            Paso::Responde(respuesta_task()),
            Paso::Responde(ChatReply {
                prompt_tokens: 100,
                completion_tokens: 20,
                ..respuesta_texto("resumen del subagente")
            }),
            Paso::Responde(respuesta_texto("respuesta final")),
        ]));
        let task = Subagent::new(como_proveedor(&proveedor), "modelo-hijo");
        let mut runner = Runner::new(como_proveedor(&proveedor), "m");
        runner.mode = Some(Mode::ReadOnly);
        runner.tools = vec![Arc::new(task)];

        let (_, resultado) = runner
            .run(
                &[Message {
                    role: "user".to_owned(),
                    content: "investiga".to_owned(),
                    ..Message::default()
                }],
                &CancelToken::new(),
            )
            .await;

        assert!(resultado.is_ok(), "Run falló: {resultado:?}");
        assert_eq!(runner.usage.prompt, 100, "el prompt del subagente falta");
        assert_eq!(runner.usage.completion, 20, "la salida del subagente falta");
        assert_eq!(
            proveedor.llamadas(),
            3,
            "dos pedidos del padre y uno del subagente"
        );

        let proveedor = Arc::new(ProveedorFalso::nuevo(vec![
            Paso::Responde(ChatReply {
                message: Message {
                    tool_calls: Some(vec![ToolCall {
                        id: "t1".to_owned(),
                        name: "leer".to_owned(),
                        args: json!({"prompt": "busca datos"}),
                        invalid_args: false,
                    }]),
                    ..Message::default()
                },
                prompt_tokens: 100,
                completion_tokens: 20,
            }),
            Paso::EsperaSiempre,
        ]));
        let task = Subagent::new(como_proveedor(&proveedor), "modelo-hijo");
        let resultado = tokio::time::timeout(
            Duration::from_millis(50),
            task.run(json!({"prompt": "investiga"})),
        )
        .await;
        assert!(resultado.is_err(), "el segundo pedido no debía terminar");
        assert_eq!(
            task.consumo(),
            Usage {
                prompt: 100,
                completion: 20
            },
            "el plazo del padre no debe perder lo ya gastado"
        );
    }

    /// Ese mismo consumo cuenta para el presupuesto del turno (issue #126).
    #[tokio::test]
    async fn task_el_consumo_agota_el_presupuesto_del_padre() {
        let proveedor = Arc::new(ProveedorFalso::nuevo(vec![
            Paso::Responde(respuesta_task()),
            Paso::Responde(ChatReply {
                prompt_tokens: 100,
                completion_tokens: 20,
                ..respuesta_texto("resumen del subagente")
            }),
            Paso::Responde(respuesta_herramienta("t2", "nada")),
        ]));
        let task = Subagent::new(como_proveedor(&proveedor), "modelo-hijo");
        let mut runner = Runner::new(como_proveedor(&proveedor), "m");
        runner.mode = Some(Mode::ReadOnly);
        runner.tools = vec![Arc::new(task)];
        runner.max_tokens = 100;

        let (_, resultado) = runner
            .run(
                &[Message {
                    role: "user".to_owned(),
                    content: "investiga".to_owned(),
                    ..Message::default()
                }],
                &CancelToken::new(),
            )
            .await;

        let Err(RunnerError::PresupuestoAgotado { max, usados }) = resultado else {
            panic!("el consumo del subagente debía agotar el presupuesto: {resultado:?}");
        };
        assert_eq!(max, 100);
        assert_eq!(usados, 120, "el presupuesto ignoró al subagente");
    }
}
