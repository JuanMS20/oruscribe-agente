//! Tests de contrato de tareas, metas, periodos, actividad y equipos.
//!
//! El fake implementa [`ToolCaller`] en vez de abrir un servidor MCP: lo que se
//! prueba es el nombre de la herramienta y los argumentos que arma el port.

use std::collections::VecDeque;
use std::sync::Mutex;

use chrono::{DateTime, Duration, Utc};
use orus_org::tareas::{self};
use orus_org::{JsonObject, OrgError, Result, Status, ToolCaller};
use serde_json::{Value, json};

#[derive(Debug)]
enum Answer {
    Text(String),
    Error(String),
}

struct Fake {
    answers: Mutex<VecDeque<Answer>>,
    calls: Mutex<Vec<(String, JsonObject)>>,
}

impl Fake {
    fn texto(answers: &[&str]) -> Self {
        Self::with(
            answers
                .iter()
                .map(|answer| Answer::Text((*answer).to_owned()))
                .collect(),
        )
    }

    fn with(answers: Vec<Answer>) -> Self {
        Self {
            answers: Mutex::new(answers.into()),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<(String, JsonObject)> {
        self.calls.lock().expect("mutex de llamadas").clone()
    }
}

impl ToolCaller for Fake {
    async fn call(&self, name: &str, args: JsonObject) -> Result<String> {
        let name = name.to_owned();
        self.calls
            .lock()
            .expect("mutex de llamadas")
            .push((name.clone(), args));
        match self
            .answers
            .lock()
            .expect("mutex de respuestas")
            .pop_front()
        {
            Some(Answer::Text(text)) => Ok(text),
            Some(Answer::Error(message)) => Err(OrgError::Tool {
                tool: name,
                message,
            }),
            None => Err(OrgError::Tool {
                tool: name,
                message: "sin respuesta guionada".to_owned(),
            }),
        }
    }
}

fn assert_call(calls: &[(String, JsonObject)], index: usize, tool: &str, expected: Value) {
    let (name, args) = &calls[index];
    assert_eq!(name, tool, "herramienta de la llamada {index}");
    assert_eq!(
        Value::Object(args.clone()),
        expected,
        "argumentos de la llamada {index}"
    );
}

#[tokio::test]
async fn goals_normaliza_progreso_y_celdas() {
    // pct y progress_pct son el mismo dato con dos nombres; my_tasks llena las
    // celdas solo cuando el servidor no las manda.
    let fake = Fake::texto(&[r#"{"goals": [
            {"id": "g1", "progress_pct": 0, "pct": 60},
            {"id": "g2", "progress_pct": 40, "pct": 0},
            {"id": "g3", "mode": "auto", "my_tasks": [
                {"id": "t1", "status": "done"},
                {"id": "t2", "status": "pending"},
                {"id": "t3", "status": "done"}
            ]}
        ]}"#]);

    let goals = tareas::goals(&fake).await.unwrap();

    assert_eq!(goals[0].progress_pct, 60.0);
    assert_eq!(goals[0].percent, 60.0);
    assert_eq!(goals[1].progress_pct, 40.0);
    assert_eq!(goals[1].percent, 40.0);
    assert_eq!(goals[2].cells_total, 3);
    assert_eq!(goals[2].cells_done, 2);
    let calls = fake.calls();
    assert_eq!(calls.len(), 1);
    assert_call(&calls, 0, "list_goals", json!({}));
}

#[tokio::test]
async fn goals_cae_a_get_my_goals_si_list_goals_falla() {
    let fake = Fake::with(vec![
        Answer::Error("no existe".to_owned()),
        Answer::Text(r#"{"goals": [{"id": "g1", "pct": 25}]}"#.to_owned()),
    ]);

    let goals = tareas::goals(&fake).await.unwrap();

    assert_eq!(goals.len(), 1);
    assert_eq!(goals[0].progress_pct, 25.0);
    let calls = fake.calls();
    assert_eq!(calls.len(), 2);
    assert_call(&calls, 0, "list_goals", json!({}));
    assert_call(&calls, 1, "get_my_goals", json!({}));
}

#[tokio::test]
async fn goal_detail_rellena_del_nivel_superior() {
    let fake = Fake::texto(&[r#"{
        "goal": {"id": "g1", "needs_attention": false},
        "progress_pct": 55,
        "cells_done": 2,
        "cells_total": 5,
        "mode": "manual",
        "per_worker": [{"pct": 40, "done": 2, "name": "Ana", "total": 5, "worker_id": "u1"}],
        "needs_attention": true
    }"#]);

    let detail = tareas::goal_detail(&fake, "g1").await.unwrap();

    assert_eq!(detail.goal.progress_pct, 55.0);
    assert_eq!(detail.goal.percent, 55.0);
    assert_eq!(detail.goal.cells_done, 2);
    assert_eq!(detail.goal.cells_total, 5);
    assert_eq!(detail.goal.mode, "manual");
    assert_eq!(detail.goal.per_worker.as_ref().map(Vec::len), Some(1));
    assert!(detail.goal.needs_attention);
    let calls = fake.calls();
    assert_eq!(calls.len(), 1);
    assert_call(&calls, 0, "get_goal", json!({"goal_id": "g1"}));
}

#[tokio::test]
async fn goal_detail_no_pisa_lo_que_ya_trae_la_meta() {
    let fake = Fake::texto(&[r#"{
        "goal": {"id": "g1", "progress_pct": 70, "cells_done": 7, "cells_total": 10,
                 "mode": "auto", "per_worker": [{"pct": 1, "done": 1, "name": "B", "total": 1, "worker_id": "u2"}]},
        "progress_pct": 10,
        "cells_done": 1,
        "cells_total": 10,
        "mode": "manual",
        "needs_attention": false
    }"#]);

    let detail = tareas::goal_detail(&fake, "g1").await.unwrap();

    assert_eq!(detail.goal.progress_pct, 70.0);
    assert_eq!(detail.goal.cells_done, 7);
    assert_eq!(detail.goal.cells_total, 10);
    assert_eq!(detail.goal.mode, "auto");
    assert_eq!(
        detail
            .goal
            .per_worker
            .as_ref()
            .and_then(|rows| rows.first()),
        Some(&orus_org::WorkerProgress {
            pct: 1.0,
            done: 1,
            name: "B".to_owned(),
            total: 1,
            worker_id: "u2".to_owned(),
        })
    );
}

#[tokio::test]
async fn periods_pide_list_periods_sin_argumentos() {
    let fake = Fake::texto(&[r#"{"periods": [{"id": "p1", "name": "Sprint 1"}]}"#]);

    let periods = tareas::periods(&fake).await.unwrap();

    assert_eq!(periods.len(), 1);
    assert_eq!(periods[0].name, "Sprint 1");
    let calls = fake.calls();
    assert_eq!(calls.len(), 1);
    assert_call(&calls, 0, "list_periods", json!({}));
}

#[tokio::test]
async fn create_period_recorta_el_nombre_y_omite_opcionales_vacios() {
    let fake = Fake::texto(&[r#"{"period": {"id": "p1", "name": "Sprint 1"}}"#]);

    let period = tareas::create_period(
        &fake,
        &orus_org::PeriodInput {
            name: "  Sprint 1  ".to_owned(),
            start_date: "2026-11-01".to_owned(),
            end_date: "2026-11-15".to_owned(),
            kind: String::new(),
            team_id: "t1".to_owned(),
        },
    )
    .await
    .unwrap();

    assert_eq!(period.id, "p1");
    let calls = fake.calls();
    assert_eq!(calls.len(), 1);
    assert_call(
        &calls,
        0,
        "create_period",
        json!({"name": "Sprint 1", "start_date": "2026-11-01", "end_date": "2026-11-15", "team_id": "t1"}),
    );
}

#[tokio::test]
async fn create_period_exige_nombre_inicio_y_fin() {
    let fake = Fake::texto(&[]);

    let error = tareas::create_period(
        &fake,
        &orus_org::PeriodInput {
            name: "   ".to_owned(),
            start_date: "2026-11-01".to_owned(),
            end_date: "2026-11-15".to_owned(),
            ..orus_org::PeriodInput::default()
        },
    )
    .await
    .unwrap_err();

    assert_eq!(
        error.to_string(),
        "el periodo necesita nombre, inicio y fin"
    );
    assert!(fake.calls().is_empty());
}

#[tokio::test]
async fn activity_usa_siete_dias_y_limite_por_defecto() {
    let fake = Fake::texto(&[r#"{"events": [{"type": "task_updated"}]}"#]);

    let events = tareas::activity(&fake, None, 0).await.unwrap();

    assert_eq!(events.items.len(), 1);
    let calls = fake.calls();
    assert_eq!(calls.len(), 1);
    let args = &calls[0].1;
    assert_eq!(calls[0].0, "get_activity");
    assert_eq!(args.get("limit").and_then(Value::as_u64), Some(50));
    let since = DateTime::parse_from_rfc3339(
        args.get("since")
            .and_then(Value::as_str)
            .expect("since de actividad"),
    )
    .expect("since rfc3339")
    .with_timezone(&Utc);
    assert!(
        (since - (Utc::now() - Duration::days(7)))
            .num_seconds()
            .abs()
            <= 5
    );
}

#[tokio::test]
async fn activity_manda_since_en_utc_y_respeta_el_limite() {
    let fake = Fake::texto(&[r#"{"events": []}"#]);
    let since = DateTime::parse_from_rfc3339("2026-11-04T19:06:42-05:00")
        .unwrap()
        .with_timezone(&Utc);

    tareas::activity(&fake, Some(since), 100).await.unwrap();

    let calls = fake.calls();
    assert_call(
        &calls,
        0,
        "get_activity",
        json!({"since": "2026-11-05T00:06:42Z", "limit": 100}),
    );
}

#[tokio::test]
async fn decisions_recorta_la_busqueda_y_ajusta_el_limite() {
    let fake = Fake::texto(&[r#"{"results": [{"id": "d1"}]}"#, r#"{"results": []}"#]);

    tareas::decisions(&fake, "  meta  ", 0).await.unwrap();
    tareas::decisions(&fake, "   ", 51).await.unwrap();

    let calls = fake.calls();
    assert_eq!(calls.len(), 2);
    assert_call(
        &calls,
        0,
        "search_decisions",
        json!({"limit": 20, "query": "meta"}),
    );
    assert_call(&calls, 1, "search_decisions", json!({"limit": 20}));
}

#[tokio::test]
async fn team_status_pide_get_team_status_sin_argumentos() {
    let fake = Fake::texto(&[r#"{"teams": [{"id": "t1", "name": "Equipo"}]}"#]);

    let teams = tareas::team_status(&fake).await.unwrap();

    assert_eq!(teams.len(), 1);
    assert_eq!(teams[0].name, "Equipo");
    let calls = fake.calls();
    assert_eq!(calls.len(), 1);
    assert_call(&calls, 0, "get_team_status", json!({}));
}

#[tokio::test]
async fn create_task_arma_los_argumentos_completos() {
    let fake = Fake::texto(&[r#"{"task": {"id": "t1", "title": "Portar"}}"#]);

    let task = tareas::create_task(
        &fake,
        &orus_org::TaskInput {
            title: "  Portar  ".to_owned(),
            description: "  Desde la app  ".to_owned(),
            priority: "high".to_owned(),
            status: "pending".to_owned(),
            due_date: "2026-12-01".to_owned(),
            start_date: "2026-11-01".to_owned(),
            goal_id: "g1".to_owned(),
            subtasks: Some(vec!["a".to_owned(), "b".to_owned()]),
        },
    )
    .await
    .unwrap();

    assert_eq!(task.id, "t1");
    let calls = fake.calls();
    assert_eq!(calls.len(), 1);
    assert_call(
        &calls,
        0,
        "create_task",
        json!({
            "title": "Portar",
            "description": "Desde la app",
            "priority": "high",
            "status": "pending",
            "due_date": "2026-12-01",
            "start_date": "2026-11-01",
            "goal_id": "g1",
            "subtasks": ["a", "b"]
        }),
    );
}

#[tokio::test]
async fn create_task_exige_titulo_y_descripcion() {
    let fake = Fake::texto(&[]);

    let sin_titulo = tareas::create_task(
        &fake,
        &orus_org::TaskInput {
            title: "  ".to_owned(),
            description: "algo".to_owned(),
            ..orus_org::TaskInput::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(sin_titulo.to_string(), "la tarea necesita un título");

    let sin_descripcion = tareas::create_task(
        &fake,
        &orus_org::TaskInput {
            title: "algo".to_owned(),
            description: "  ".to_owned(),
            ..orus_org::TaskInput::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(
        sin_descripcion.to_string(),
        "la tarea necesita una descripción"
    );
    assert!(fake.calls().is_empty());
}

#[tokio::test]
async fn create_task_rechaza_una_respuesta_sin_tarea() {
    let fake = Fake::texto(&[r#"{"task": {"id": ""}}"#]);

    let error = tareas::create_task(
        &fake,
        &orus_org::TaskInput {
            title: "algo".to_owned(),
            description: "algo".to_owned(),
            ..orus_org::TaskInput::default()
        },
    )
    .await
    .unwrap_err();

    assert_eq!(error.to_string(), "create_task: respuesta sin tarea");
}

#[tokio::test]
async fn search_tasks_filtra_el_estado_valido() {
    let fake = Fake::texto(&[
        r#"{"tasks": [{"id": "t1"}]}"#,
        r#"{"tasks": [{"id": "t2"}]}"#,
    ]);

    let tasks = tareas::search_tasks(&fake, "  login  ", Status::InProgress, 0)
        .await
        .unwrap();
    assert_eq!(tasks.items[0].id, "t1");

    let tasks = tareas::search_tasks(&fake, "  ", Status::Unknown("archived".to_owned()), 51)
        .await
        .unwrap();
    assert_eq!(tasks.items[0].id, "t2");

    let calls = fake.calls();
    assert_eq!(calls.len(), 2);
    assert_call(
        &calls,
        0,
        "search_tasks",
        json!({"limit": 30, "query": "login", "status": "in_progress"}),
    );
    assert_call(&calls, 1, "search_tasks", json!({"limit": 30}));
}

#[tokio::test]
async fn send_alert_recorta_el_mensaje_y_omite_destinos_vacios() {
    let fake = Fake::texto(&["{}", "{}"]);

    tareas::send_alert(&fake, "risk", "  cuidado  ", "t1", "")
        .await
        .unwrap();

    let calls = fake.calls();
    assert_call(
        &calls,
        0,
        "send_alert",
        json!({"type": "risk", "message": "cuidado", "target_team_id": "t1"}),
    );

    let error = tareas::send_alert(&fake, "risk", "   ", "", "")
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "el aviso está vacío");
}

#[tokio::test]
async fn react_exige_mensaje_y_emoji() {
    let fake = Fake::texto(&["{}"]);

    tareas::react(&fake, "m1", "👍").await.unwrap();
    let calls = fake.calls();
    assert_call(
        &calls,
        0,
        "react_to_chat_message",
        json!({"message_id": "m1", "emoji": "👍"}),
    );

    let error = tareas::react(&fake, "", "👍").await.unwrap_err();
    assert_eq!(error.to_string(), "reacción incompleta");
}

#[tokio::test]
async fn update_context_solo_manda_campos_con_texto_y_mood_valido() {
    let fake = Fake::texto(&["{}", "{}"]);

    tareas::update_context(&fake, "  foco  ", "blocked", "   ")
        .await
        .unwrap();
    tareas::update_context(&fake, "", "on_track", "")
        .await
        .unwrap();

    let calls = fake.calls();
    assert_eq!(calls.len(), 2);
    assert_call(
        &calls,
        0,
        "update_my_context",
        json!({"current_focus": "foco", "mood_signal": "blocked"}),
    );
    assert_call(
        &calls,
        1,
        "update_my_context",
        json!({"mood_signal": "on_track"}),
    );

    // Un ánimo fuera de la lista es un error claro incluso sin foco ni notas:
    // el contrato cambió con el issue #97 (antes se descartaba en silencio).
    let error = tareas::update_context(&fake, "  ", "raro", "")
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "ánimo desconocido: \"raro\" (use on_track, at_risk o blocked)"
    );
    assert_eq!(fake.calls().len(), 2);
}

#[tokio::test]
async fn mentions_pide_get_my_mentions_sin_argumentos() {
    let fake = Fake::texto(&[r#"{"mentions": [{"id": "m1", "body": "hola"}]}"#]);

    let mentions = tareas::mentions(&fake).await.unwrap();

    assert_eq!(mentions.items.len(), 1);
    assert_eq!(mentions.items[0].body, "hola");
    let calls = fake.calls();
    assert_eq!(calls.len(), 1);
    assert_call(&calls, 0, "get_my_mentions", json!({}));
}

#[tokio::test]
async fn update_context_con_animo_invalido_no_publica_en_silencio() {
    // Con foco publicado, un ánimo fuera de la lista se descartaba sin error:
    // la persona creía publicar su ánimo y el equipo no lo veía (issue #97).
    let fake = Fake::texto(&["{}"]);

    let error = tareas::update_context(&fake, "foco", "raro", "")
        .await
        .unwrap_err();

    assert!(matches!(&error, OrgError::InvalidInput(_)), "{error}");
    assert_eq!(
        error.to_string(),
        "ánimo desconocido: \"raro\" (use on_track, at_risk o blocked)"
    );
    assert!(
        fake.calls().is_empty(),
        "no se publica a medias sin el ánimo pedido"
    );
}

#[tokio::test]
async fn update_context_recorta_el_animo() {
    let fake = Fake::texto(&["{}"]);

    tareas::update_context(&fake, "", " on_track ", "")
        .await
        .unwrap();

    let calls = fake.calls();
    assert_eq!(calls.len(), 1);
    assert_call(
        &calls,
        0,
        "update_my_context",
        json!({"mood_signal": "on_track"}),
    );
}

#[tokio::test]
async fn update_context_todo_vacio_sigue_dando_error() {
    let fake = Fake::texto(&[]);

    let error = tareas::update_context(&fake, "  ", "", "  ")
        .await
        .unwrap_err();

    assert_eq!(error.to_string(), "no hay nada que actualizar");
    assert!(fake.calls().is_empty());
}

#[tokio::test]
async fn activity_devuelve_la_paginacion_del_servidor() {
    let fake = Fake::texto(&[r#"{
        "events": [{"type": "task_updated"}],
        "count": 1, "total": 9, "has_more": true, "truncated": true, "next_offset": 1
    }"#]);

    let page = tareas::activity(&fake, None, 0).await.unwrap();
    let valor = serde_json::to_value(&page).unwrap();

    assert_eq!(
        valor["has_more"],
        json!(true),
        "faltan páginas de actividad"
    );
    assert_eq!(valor["truncated"], json!(true), "la lista vino cortada");
    assert_eq!(
        valor["next_offset"],
        json!(1),
        "dónde pedir la página siguiente"
    );
    assert_eq!(valor["count"], json!(1));
    assert_eq!(valor["total"], json!(9));
}

#[tokio::test]
async fn mentions_devuelve_la_paginacion_del_servidor() {
    let fake = Fake::texto(&[r#"{
        "mentions": [{"id": "m1", "body": "hola"}],
        "count": 1, "total": 8, "has_more": true, "truncated": false, "next_offset": 1
    }"#]);

    let page = tareas::mentions(&fake).await.unwrap();
    let valor = serde_json::to_value(&page).unwrap();

    assert_eq!(valor["has_more"], json!(true), "faltan menciones");
    assert_eq!(valor["next_offset"], json!(1));
    assert_eq!(valor["total"], json!(8));
}

#[tokio::test]
async fn search_tasks_devuelve_la_paginacion_del_servidor() {
    let fake = Fake::texto(&[r#"{
        "tasks": [{"id": "t1"}],
        "count": 1, "total": 40, "has_more": true, "truncated": true, "next_offset": 30
    }"#]);

    let page = tareas::search_tasks(&fake, "", Status::Pending, 30)
        .await
        .unwrap();
    let valor = serde_json::to_value(&page).unwrap();

    assert_eq!(valor["has_more"], json!(true), "la búsqueda está paginada");
    assert_eq!(valor["truncated"], json!(true));
    assert_eq!(valor["next_offset"], json!(30));
}

#[tokio::test]
async fn goals_no_inventa_celdas_cuando_el_servidor_dice_cero() {
    let fake = Fake::texto(&[r#"{"goals": [
        {"id": "g1", "mode": "auto", "cells_total": 0, "cells_done": 0, "my_tasks": [
            {"id": "t1", "status": "done"},
            {"id": "t2", "status": "pending"}
        ]}
    ]}"#]);

    let goals = tareas::goals(&fake).await.unwrap();

    assert_eq!(
        goals[0].cells_total, 0,
        "cero explícito no es campo ausente"
    );
    assert_eq!(goals[0].cells_done, 0, "no se inventan celdas hechas");
}

#[tokio::test]
async fn goals_no_cuenta_tareas_como_celdas_en_modo_manual() {
    let fake = Fake::texto(&[r#"{"goals": [
        {"id": "g1", "mode": "manual", "my_tasks": [
            {"id": "t1", "status": "done"},
            {"id": "t2", "status": "done"}
        ]}
    ]}"#]);

    let goals = tareas::goals(&fake).await.unwrap();

    assert_eq!(
        goals[0].cells_total, 0,
        "en modo manual las celdas no se deducen"
    );
    assert_eq!(goals[0].cells_done, 0);
}
