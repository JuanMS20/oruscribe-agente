use std::collections::HashMap;
use std::io::{self, BufRead};
use std::sync::Arc;

use orus_agent::{ApproveFn, HERRAMIENTA_PROPOSE_PLAN, PlanApproveFn, Risk, ToolError};
use orus_core::ToolCall;
use orus_tools::permissions;
use serde_json::Value;

use super::ApproveRiskFn;
use crate::compactar::compactar;

/// La acción que muestra el prompt según el riesgo.
fn accion(riesgo: Option<Risk>) -> &'static str {
    match riesgo {
        Some(Risk::Exec) => "ejecutar un programa",
        Some(Risk::Write) => "escribir un archivo",
        _ => "usar una herramienta",
    }
}

/// El prompt exacto de una llamada. El plan se lee entero
/// porque quien aprueba necesita ver todos los pasos.
pub(super) fn texto_prompt_llamada(nombre: &str, args: &Value, riesgo: Option<Risk>) -> String {
    if nombre == HERRAMIENTA_PROPOSE_PLAN {
        let plan = args
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| args.to_string());
        return format!("\nEl agente quiere ejecutar este plan:\n{plan}\n¿Permitir? [s/N] ");
    }
    format!(
        "\nEl agente quiere {}:\n  {nombre} {}\n¿Permitir? [s/N] ",
        accion(riesgo),
        args
    )
}

/// Acepta el sí después de recortar y pasar a minúsculas.
pub(super) fn interpretar_respuesta(linea: &str) -> bool {
    matches!(
        linea.trim().to_lowercase().as_str(),
        "s" | "si" | "sí" | "y" | "yes"
    )
}

/// Lee una línea de la terminal. Un EOF cuenta como respuesta vacía, igual
/// que.
fn leer_respuesta() -> std::result::Result<bool, ToolError> {
    let mut linea = String::new();
    io::stdin()
        .lock()
        .read_line(&mut linea)
        .map_err(|error| ToolError::Message(error.to_string()))?;
    Ok(interpretar_respuesta(&linea))
}

/// Arma la aprobación de herramientas: primero las
/// banderas, después la terminal; sin terminal se rechaza. Solo la usan los
/// tests de la política; el turno arma la variante con costura.
#[cfg(test)]
pub(super) fn aprobador(
    riesgos: Arc<HashMap<String, Option<Risk>>>,
    yes: bool,
    allow_exec: bool,
    es_tty: bool,
) -> ApproveFn {
    aprobador_con_externo(None, riesgos, yes, allow_exec, es_tty)
}

/// Como `aprobador`, con una costura externa que contesta con el riesgo real
/// de la llamada. Su respuesta manda aunque `es_tty` sea false (O5-07b).
pub(super) fn aprobador_con_externo(
    externo: Option<ApproveRiskFn>,
    riesgos: Arc<HashMap<String, Option<Risk>>>,
    yes: bool,
    allow_exec: bool,
    es_tty: bool,
) -> ApproveFn {
    Arc::new(move |llamada: &ToolCall| {
        let riesgo = riesgos.get(&llamada.name).copied().flatten();
        if riesgo == Some(Risk::Write) && yes {
            return Ok(true);
        }
        if riesgo == Some(Risk::Exec) && allow_exec {
            return Ok(true);
        }
        if let Some(externo) = &externo {
            return externo(llamada, riesgo);
        }
        if !es_tty {
            return Ok(false);
        }
        eprint!(
            "{}",
            texto_prompt_llamada(&llamada.name, &llamada.args, riesgo)
        );
        if !leer_respuesta()? {
            return Ok(false);
        }
        tras_aprobar(&llamada.name, &llamada.args, riesgo, es_tty);
        Ok(true)
    })
}

/// Arma la aprobación del plan: `--yes` lo aprueba; sin
/// terminal y sin `--yes` se rechaza. Tras el sí ofrece recordar la regla.
/// Solo la usan los tests; el turno arma la variante con costura.
#[cfg(test)]
pub(super) fn aprobador_plan(yes: bool, es_tty: bool) -> PlanApproveFn {
    aprobador_plan_con_externo(None, yes, es_tty)
}

/// Como `aprobador_plan`, con la costura de la ventana. El plan viaja como
/// un `ToolCall` de `propose_plan` con riesgo de escritura (O5-07b).
pub(super) fn aprobador_plan_con_externo(
    externo: Option<ApproveRiskFn>,
    yes: bool,
    es_tty: bool,
) -> PlanApproveFn {
    Arc::new(move |plan: &str| {
        if yes {
            return Ok(true);
        }
        if let Some(externo) = &externo {
            let llamada = ToolCall {
                name: HERRAMIENTA_PROPOSE_PLAN.to_owned(),
                args: Value::String(plan.to_owned()),
                ..ToolCall::default()
            };
            return externo(&llamada, Some(Risk::Write));
        }
        if !es_tty {
            return Ok(false);
        }
        eprint!("\nEl agente quiere ejecutar este plan:\n{plan}\n¿Permitir? [s/N] ");
        if !leer_respuesta()? {
            return Ok(false);
        }
        let args = Value::String(plan.to_owned());
        tras_aprobar(HERRAMIENTA_PROPOSE_PLAN, &args, Some(Risk::Write), es_tty);
        Ok(true)
    })
}

/// Tras aprobar, ofrece recordar la regla. Un fallo al
/// guardarla se avisa y el turno sigue.
fn tras_aprobar(nombre: &str, args: &Value, riesgo: Option<Risk>, es_tty: bool) {
    if !es_tty {
        return;
    }
    let Some(riesgo) = riesgo else {
        return;
    };
    let Some(patron) = permissions::eligible(nombre, args, riesgo) else {
        return;
    };
    if permissions::check(nombre, args, riesgo) {
        return;
    }
    eprint!(
        "¿recordar esta regla para {nombre} ({})? [s/N] ",
        compactar(&patron, 120)
    );
    let Ok(recordar) = leer_respuesta() else {
        return;
    };
    if !recordar {
        return;
    }
    match permissions::remember(nombre, &patron, "") {
        Ok(regla) => eprintln!(
            "regla recordada ({}). Se ve con: oruscribe-agent rules list; se quita con: oruscribe-agent rules revoke {}",
            regla.id, regla.id
        ),
        Err(error) => eprintln!("no se pudo recordar la regla: {error}"),
    }
}
