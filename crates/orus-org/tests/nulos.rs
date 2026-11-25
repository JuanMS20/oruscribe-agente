//! El servidor manda `null` donde el tipo declaraba un valor: la respuesta
//! entera no puede rechazarse por eso.

#[test]
fn la_meta_con_pct_nulo_se_lee() {
    let meta: orus_org::types::Goal = serde_json::from_str(
        r#"{"id":"g","title":"t","pct":null,"weight":null,"progress_pct":null}"#,
    )
    .expect("la meta con pct nulo debe leerse");

    assert_eq!(
        (meta.percent, meta.weight, meta.progress_pct),
        (0.0, 0.0, 0.0),
        "los decimales nulos se leen como cero"
    );
}

#[test]
fn la_decision_sin_motivo_se_lee() {
    let decision: orus_org::types::Decision = serde_json::from_str(
        r#"{"id":"d","title":"t","rationale":null,"context":null,"decision":null}"#,
    )
    .expect("la decisión con rationale nulo debe leerse");

    assert_eq!(decision.rationale, "");
    assert_eq!(decision.context, "");
    assert_eq!(decision.decision, "");
}

#[test]
fn el_evento_con_team_nulo_se_lee() {
    let evento: orus_org::types::ActivityEvent = serde_json::from_str(
        r#"{"type":"goal_created","timestamp":"2026-11-10T02:38:55Z","team_id":null,"task_id":null,"goal_id":null}"#,
    )
    .expect("el evento con team_id nulo debe leerse");

    assert_eq!(evento.team_id, "");
    assert_eq!(evento.task_id, "");
}

#[test]
fn el_detalle_de_meta_con_nulos_se_lee() {
    // get_goal devuelve los mismos agregados que list_goals: el null que #56
    // corrigió en Goal también llega al detalle y no puede tumbar la pantalla
    // (issue #97).
    let detalle: orus_org::types::GoalDetail = serde_json::from_str(
        r#"{"goal":{"id":"g","title":"t","cells_done":null,"cells_total":null},"progress_pct":null,"cells_done":null,"cells_total":null}"#,
    )
    .expect("el detalle con nulos debe leerse");
    assert_eq!(
        (
            detalle.progress_pct,
            detalle.cells_done,
            detalle.cells_total
        ),
        (0.0, 0, 0),
        "los agregados nulos del detalle se leen como cero"
    );
    assert_eq!(
        (detalle.goal.cells_done, detalle.goal.cells_total),
        (0, 0),
        "los contadores nulos de la meta embebida se leen como cero"
    );
}

#[test]
fn el_avance_por_persona_con_contadores_nulos_se_lee() {
    let avance: orus_org::types::WorkerProgress =
        serde_json::from_str(r#"{"pct":null,"done":null,"total":null}"#)
            .expect("el avance con contadores nulos debe leerse");
    assert_eq!(
        (avance.pct, avance.done, avance.total),
        (0.0, 0, 0),
        "los contadores nulos se leen como cero"
    );
}
