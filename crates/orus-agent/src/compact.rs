//! Compactación de la conversación: resume lo viejo cuando ya no cabe.
//!
//! `largo` y.
//! Con ventana declarada dispara al 80 % del último pedido; sin ventana usa el
//! presupuesto de caracteres. Nunca deja un mensaje `tool` sin la llamada que
//! lo pidió.

use orus_core::Message;

use crate::runner::CancelToken;
use crate::types::{ChatRequest, Provider};

/// Presupuesto de caracteres cuando no hay ventana ni `max_chars`.
const PRESUPUESTO_CHARS: usize = 24_000;

/// Turnos que sobreviven a la compactación.
const TURNOS_QUE_SE_QUEDAN: usize = 12;

/// Lo que se le pide al modelo para resumir.
const PROMPT_RESUMEN: &str = "Resume la conversación anterior en pocas líneas. Conserva decisiones, nombres, tareas y datos que hagan falta para seguir hablando. No agregues instrucciones nuevas.";

/// Aviso cuando no hubo resumen y solo se recortó.
const MARCA_OMISION: &str = "[se omitieron los turnos más viejos porque no cupieron]";

/// Prefijo del resumen que entra al historial.
const PREFIJO_RESUMEN: &str = "Resumen de lo anterior:\n";

/// Lo que la compactación necesita del runner.
pub(crate) struct Peticion<'a> {
    pub(crate) provider: &'a dyn Provider,
    pub(crate) model: &'a str,
    pub(crate) window_tokens: i64,
    pub(crate) max_chars: i64,
    pub(crate) ultimo_prompt: i64,
    pub(crate) cancel: &'a CancelToken,
}

/// El historial que queda y el consumo del resumen, si lo hubo.
pub(crate) struct Compactacion {
    pub(crate) msgs: Vec<Message>,
    pub(crate) prompt_tokens: i64,
    pub(crate) completion_tokens: i64,
    /// Solo si el historial cambió: resumen u omisión.
    pub(crate) compacto: bool,
}

/// Compacta `msgs` si la conversación ya no cabe. Nunca parte un grupo de
/// llamada y resultado. `compactar`.
pub(crate) async fn compactar(peticion: Peticion<'_>, msgs: Vec<Message>) -> Compactacion {
    if !hace_falta(&peticion, &msgs) {
        return Compactacion::intacta(msgs);
    }
    let corte = corte_seguro(&msgs, msgs.len().saturating_sub(TURNOS_QUE_SE_QUEDAN));
    if corte == 0 || corte >= msgs.len() {
        return Compactacion::intacta(msgs);
    }
    let solo_resumen_previo = msgs[..corte]
        .iter()
        .all(|m| m.role == "system" && m.content.starts_with(PREFIJO_RESUMEN));
    if solo_resumen_previo {
        return Compactacion::intacta(msgs);
    }

    let mut peticion_resumen = Vec::with_capacity(corte + 1);
    peticion_resumen.push(Message {
        role: "system".to_owned(),
        content: PROMPT_RESUMEN.to_owned(),
        ..Message::default()
    });
    peticion_resumen.extend_from_slice(&msgs[..corte]);

    let respuesta = tokio::select! {
        resultado = peticion.provider.chat(ChatRequest {
            model: peticion.model.to_owned(),
            messages: peticion_resumen,
            tools: Vec::new(),
            on_delta: None,
        }) => resultado,
        // La cancelación no es una decisión de responder: devolver el
        // historial original evita que se guarde recortado (issue #126).
        () = peticion.cancel.esperar() => return Compactacion::intacta(msgs),
    };

    match respuesta {
        Ok(respuesta) if !respuesta.message.content.trim().is_empty() => {
            let mut resultado = Vec::with_capacity(msgs.len() - corte + 1);
            resultado.push(Message {
                role: "system".to_owned(),
                content: format!("{PREFIJO_RESUMEN}{}", respuesta.message.content),
                ..Message::default()
            });
            resultado.extend_from_slice(&msgs[corte..]);
            Compactacion {
                msgs: resultado,
                prompt_tokens: respuesta.prompt_tokens,
                completion_tokens: respuesta.completion_tokens,
                compacto: true,
            }
        }
        _ => Compactacion::con_omision(msgs, corte),
    }
}

/// Dice si la conversación pasó el umbral. `compactar`.
/// El presupuesto se mira aunque haya 13 mensajes o menos: dos resultados de
/// herramienta grandes lo superan sin pasar de 13 mensajes (issue #93). Con
/// 12 o menos no hay turnos viejos que cortar y `compactar` deja el historial
/// intacto.
fn hace_falta(peticion: &Peticion<'_>, msgs: &[Message]) -> bool {
    if peticion.window_tokens > 0 {
        // El contrato trunca el umbral a entero: `int(0.8*float64(WindowTokens))`.
        let umbral = (0.8 * peticion.window_tokens as f64) as i64;
        return peticion.ultimo_prompt >= umbral
            || (peticion.ultimo_prompt == 0 && largo(msgs) > presupuesto(peticion.max_chars));
    }
    largo(msgs) > presupuesto(peticion.max_chars)
}

/// El tope por caracteres: `max_chars` si se declaró, si no el de siempre.
fn presupuesto(max_chars: i64) -> usize {
    if max_chars > 0 {
        usize::try_from(max_chars).unwrap_or(usize::MAX)
    } else {
        PRESUPUESTO_CHARS
    }
}

/// Estima el tamaño de la conversación: bytes de `Content` más los argumentos
/// de las llamadas. `largo`.
fn largo(msgs: &[Message]) -> usize {
    msgs.iter()
        .map(|mensaje| {
            mensaje.content.len()
                + mensaje
                    .tool_calls
                    .iter()
                    .flatten()
                    .map(|llamada| llamada.args.to_string().len())
                    .sum::<usize>()
        })
        .sum()
}

/// Mueve el corte para que no parta un grupo de llamada y resultado.
fn corte_seguro(msgs: &[Message], corte: usize) -> usize {
    let mut corte = corte;
    while corte < msgs.len() && msgs[corte].role == "tool" {
        corte += 1;
    }
    corte
}

impl Compactacion {
    fn intacta(msgs: Vec<Message>) -> Self {
        Self {
            msgs,
            prompt_tokens: 0,
            completion_tokens: 0,
            compacto: false,
        }
    }

    /// Avisa que se omitieron los turnos viejos y conserva el resto.
    fn con_omision(msgs: Vec<Message>, corte: usize) -> Self {
        let mut resultado = Vec::with_capacity(msgs.len().saturating_sub(corte) + 1);
        resultado.push(Message {
            role: "system".to_owned(),
            content: MARCA_OMISION.to_owned(),
            ..Message::default()
        });
        resultado.extend_from_slice(&msgs[corte..]);
        Self {
            msgs: resultado,
            prompt_tokens: 0,
            completion_tokens: 0,
            compacto: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use orus_core::{Message, ToolCall};

    use crate::error::ProviderError;
    use crate::runner::{CancelToken, Runner};
    use crate::types::{ChatReply, ChatRequest, Provider};

    use super::PROMPT_RESUMEN;

    #[tokio::test]
    async fn no_vuelve_a_resumir_un_resumen_previo() {
        let proveedor = Arc::new(PedidosFalsos::default());
        let mut runner = Runner::new(como_proveedor(&proveedor), "m");
        runner.max_chars = 400;

        let mut historial = vec![mensaje("system", "Resumen de lo anterior:\nlo que pasó")];
        historial.extend(historial_largo(6));
        assert_eq!(historial.len(), 13);

        let (_, resultado) = runner.run(&historial, &CancelToken::new()).await;
        assert!(resultado.is_ok(), "Run falló: {resultado:?}");
        let pedidos = proveedor.pedidos();
        assert_eq!(
            pedidos.len(),
            1,
            "resumir el resumen gasta una petición por paso"
        );
        assert!(!pide_resumen(&pedidos[0]));
    }

    /// Proveedor falso que contesta el resumen cuando ve su mensaje `system`
    /// exacto y guarda cada petición.
    #[derive(Default)]
    struct PedidosFalsos {
        pedidos: Mutex<Vec<Vec<Message>>>,
        falla_resumen: bool,
    }

    impl PedidosFalsos {
        fn que_falla_el_resumen() -> Self {
            Self {
                falla_resumen: true,
                ..Self::default()
            }
        }

        fn pedidos(&self) -> Vec<Vec<Message>> {
            self.pedidos
                .lock()
                .expect("los pedidos del proveedor falso")
                .clone()
        }
    }

    fn pide_resumen(msgs: &[Message]) -> bool {
        msgs.iter()
            .any(|m| m.role == "system" && m.content == PROMPT_RESUMEN)
    }

    fn como_proveedor(proveedor: &Arc<PedidosFalsos>) -> Arc<dyn Provider> {
        proveedor.clone()
    }

    fn mensaje(role: &str, content: &str) -> Message {
        Message {
            role: role.to_owned(),
            content: content.to_owned(),
            ..Message::default()
        }
    }

    fn respuesta(texto: &str, prompt: i64, completion: i64) -> ChatReply {
        ChatReply {
            message: mensaje("assistant", texto),
            prompt_tokens: prompt,
            completion_tokens: completion,
        }
    }

    #[async_trait]
    impl Provider for PedidosFalsos {
        async fn chat(&self, req: ChatRequest) -> Result<ChatReply, ProviderError> {
            let es_resumen = pide_resumen(&req.messages);
            self.pedidos
                .lock()
                .expect("los pedidos del proveedor falso")
                .push(req.messages);
            if !es_resumen {
                return Ok(respuesta("listo", 50, 5));
            }
            if self.falla_resumen {
                return Err(ProviderError::local("el resumen falló"));
            }
            Ok(respuesta("resumen del principio", 100, 10))
        }
    }

    fn historial_largo(pares: usize) -> Vec<Message> {
        let mut msgs = Vec::with_capacity(pares * 2);
        for _ in 0..pares {
            msgs.push(mensaje("user", &"pregunta ".repeat(5)));
            msgs.push(mensaje("assistant", &"respuesta ".repeat(5)));
        }
        msgs
    }

    /// Un grupo de herramienta justo donde suele caer el corte.
    fn grupo_de_herramienta() -> [Message; 2] {
        let mut llamada = mensaje("assistant", "");
        llamada.tool_calls = Some(vec![ToolCall {
            id: "c1".to_owned(),
            name: "x".to_owned(),
            args: serde_json::json!({}),
            invalid_args: false,
        }]);
        let mut resultado = mensaje("tool", "resultado");
        resultado.tool_call_id = "c1".to_owned();
        [llamada, resultado]
    }

    /// Ningún `tool` sin la llamada que lo pidió.
    fn sin_huerfanos(msgs: &[Message]) {
        let mut pedidas = HashSet::new();
        for m in msgs {
            for llamada in m.tool_calls.iter().flatten() {
                pedidas.insert(llamada.id.as_str());
            }
            if m.role == "tool" {
                assert!(
                    pedidas.contains(m.tool_call_id.as_str()),
                    "mensaje tool {:?} sin su llamada",
                    m.tool_call_id
                );
            }
        }
    }

    #[tokio::test]
    async fn compacta_la_conversacion_larga_con_un_resumen() {
        let proveedor = Arc::new(PedidosFalsos::default());
        let mut runner = Runner::new(como_proveedor(&proveedor), "m");
        runner.max_chars = 400;

        let (_, resultado) = runner.run(&historial_largo(10), &CancelToken::new()).await;

        assert!(resultado.is_ok(), "Run falló: {resultado:?}");
        let pedidos = proveedor.pedidos();
        assert_eq!(pedidos.len(), 2, "quería una de resumen y una de respuesta");
        assert!(
            pide_resumen(&pedidos[0]),
            "la primera petición no pidió el resumen"
        );
        let segunda = &pedidos[1];
        assert!(
            segunda
                .iter()
                .any(|m| m.role == "system" && m.content.contains("resumen del principio")),
            "la respuesta no recibió el resumen: {segunda:?}"
        );
        sin_huerfanos(segunda);
        assert_eq!(
            (runner.usage.prompt, runner.usage.completion),
            (150, 15),
            "consumo del resumen y de la respuesta"
        );
    }

    #[tokio::test]
    async fn no_compacta_cuando_la_conversacion_cabe() {
        let proveedor = Arc::new(PedidosFalsos::default());
        let mut runner = Runner::new(como_proveedor(&proveedor), "m");
        runner.max_chars = 100_000;

        let (_, resultado) = runner.run(&historial_largo(3), &CancelToken::new()).await;

        assert!(resultado.is_ok(), "Run falló: {resultado:?}");
        let pedidos = proveedor.pedidos();
        assert_eq!(pedidos.len(), 1, "pidió un resumen sin necesidad");
        assert!(!pide_resumen(&pedidos[0]), "no debía pedir el resumen");
    }

    #[tokio::test]
    async fn si_el_resumen_falla_se_recorta_sin_perder_la_llamada_de_la_herramienta() {
        let proveedor = Arc::new(PedidosFalsos::que_falla_el_resumen());
        let mut runner = Runner::new(como_proveedor(&proveedor), "m");
        runner.max_chars = 400;

        let mut historial = historial_largo(10);
        historial.extend(grupo_de_herramienta());
        historial.extend(historial_largo(4));

        let (_, resultado) = runner.run(&historial, &CancelToken::new()).await;

        assert!(resultado.is_ok(), "Run falló: {resultado:?}");
        let pedidos = proveedor.pedidos();
        assert_eq!(pedidos.len(), 2, "quería el resumen fallido y la respuesta");
        sin_huerfanos(&pedidos[1]);
    }

    /// Con 13 mensajes el presupuesto también se mira: dos lecturas grandes
    /// superan el presupuesto sin pasar de 13 mensajes (issue #93). El corte
    /// deja 12 turnos, así que el mensaje más viejo se resume.
    #[tokio::test]
    async fn compacta_con_trece_mensajes_si_el_presupuesto_no_alcanza() {
        let proveedor = Arc::new(PedidosFalsos::default());
        let mut runner = Runner::new(como_proveedor(&proveedor), "m");
        runner.max_chars = 400;

        let mut historial = historial_largo(6);
        historial.push(mensaje("user", &"z".repeat(50)));
        assert_eq!(historial.len(), 13);

        let (_, resultado) = runner.run(&historial, &CancelToken::new()).await;
        assert!(resultado.is_ok(), "Run falló: {resultado:?}");
        let pedidos = proveedor.pedidos();
        assert_eq!(
            pedidos.len(),
            2,
            "con 13 mensajes sobre presupuesto debía pedir el resumen"
        );
        assert!(
            pide_resumen(&pedidos[0]),
            "la primera petición no pidió el resumen"
        );
    }

    /// Con 12 mensajes o menos no hay turnos viejos que cortar: el corte cae
    /// en 0 y la compactación deja el historial intacto aunque el presupuesto
    /// se haya pasado (issue #93, residual documentado).
    #[tokio::test]
    async fn con_doce_mensajes_no_hay_que_cortar() {
        let proveedor = Arc::new(PedidosFalsos::default());
        let mut runner = Runner::new(como_proveedor(&proveedor), "m");
        runner.max_chars = 400;

        let historial = historial_largo(6);
        assert_eq!(historial.len(), 12);

        let (_, resultado) = runner.run(&historial, &CancelToken::new()).await;
        assert!(resultado.is_ok(), "Run falló: {resultado:?}");
        let pedidos = proveedor.pedidos();
        assert_eq!(pedidos.len(), 1, "no hay nada viejo que resumir");
        assert!(!pide_resumen(&pedidos[0]));
    }

    #[tokio::test]
    async fn compacta_antes_del_primer_pedido_con_ventana_y_medicion_cero() {
        let proveedor = Arc::new(PedidosFalsos::default());
        let mut runner = Runner::new(como_proveedor(&proveedor), "m");
        runner.window_tokens = 128_000;
        let historial: Vec<Message> = (0..14)
            .map(|indice| {
                mensaje(
                    if indice % 2 == 0 { "user" } else { "assistant" },
                    &"x".repeat(2000),
                )
            })
            .collect();

        let (_, resultado) = runner.run(&historial, &CancelToken::new()).await;

        assert!(resultado.is_ok(), "Run falló: {resultado:?}");
        let pedidos = proveedor.pedidos();
        assert_eq!(pedidos.len(), 2);
        assert!(
            pide_resumen(&pedidos[0]),
            "el primer pedido debía compactar el historial largo"
        );
        assert!(
            pedidos[1].len() < historial.len(),
            "el primer pedido normal recibió el historial sin recortar"
        );
    }
}
