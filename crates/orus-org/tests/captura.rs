//! Tests de contrato de la puesta al día: metas, digests y `git log`.
//!
//! Los métodos MCP usan un fake de [`ToolCaller`]. `GitLog` corre contra un
//! git falso guionado: el llamador y el parser se ejercitan siempre, sin saltos
//! silenciosos que dejen el gate en verde (#130).

use std::collections::VecDeque;
use std::sync::Mutex;

mod comun;

use chrono::{Datelike, Duration, Utc};
use orus_org::captura::{self, parse_git_log};
use orus_org::{DigestItem, DigestSource, GoalInput, JsonObject, OrgError, Result, ToolCaller};
use serde_json::{Value, json};

struct Fake {
    answers: Mutex<VecDeque<String>>,
    calls: Mutex<Vec<(String, JsonObject)>>,
}

impl Fake {
    fn texto(answers: &[&str]) -> Self {
        Self {
            answers: Mutex::new(answers.iter().map(|answer| (*answer).to_owned()).collect()),
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
        self.answers
            .lock()
            .expect("mutex de respuestas")
            .pop_front()
            .ok_or_else(|| OrgError::Tool {
                tool: name,
                message: "sin respuesta guionada".to_owned(),
            })
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

// git log separa commits con \x1e y campos con \x1f. Un parser que se coma el
// último commit o mezcle un cuerpo vacío deja la puesta al día a medias.
#[test]
fn parse_git_log_conserva_cada_commit() {
    let raw = "abc123\x1f2026-11-04T19:06:42-05:00\x1fPrimer cambio\x1fDetalle del primer cambio\n\n\x1e\
               def456\x1f2026-11-04T18:00:00-05:00\x1fSegundo cambio\x1f\x1e";
    let items = parse_git_log(raw);
    assert_eq!(items.len(), 2, "commits: {items:?}");
    assert_eq!(items[0].title, "Primer cambio");
    assert_eq!(items[0].r#ref, "abc123");
    assert_eq!(items[0].summary, "Detalle del primer cambio");
    assert_eq!(items[1].title, "Segundo cambio");
    assert_eq!(items[1].summary, "");
    assert_eq!(items[0].at, "2026-11-04T19:06:42-05:00");
    assert_eq!(items[0].state, "done");
    assert_eq!(items[0].kind, "commit");
}

#[test]
fn parse_git_log_salta_entrada_vacia() {
    assert!(parse_git_log("\n\n").is_empty());
    // Sin separadores de campo no hay commit, y un título vacío tampoco cuenta.
    assert!(parse_git_log("abc\x1f2026-11-04T19:06:42-05:00\x1f\x1e").is_empty());
    assert!(parse_git_log("abc\x1f2026-11-04T19:06:42-05:00\x1e").is_empty());
}

// El contrato corta por bytes y puede partir un rune; Rust corta en la frontera anterior
// para no producir texto inválido. En ASCII el resultado es el mismo.
#[test]
fn parse_git_log_corta_en_frontera_de_rune() {
    let title = format!("{}é", "a".repeat(299));
    let summary = format!("{}é", "b".repeat(1999));
    let raw = format!("ref\x1fat\x1f{title}\x1f{summary}\x1e");

    let items = parse_git_log(&raw);

    assert_eq!(items.len(), 1);
    assert_eq!(items[0].title, "a".repeat(299));
    assert_eq!(items[0].summary, "b".repeat(1999));
    assert!(items[0].title.len() <= 300);
    assert!(items[0].summary.len() <= 2000);
}

#[test]
fn parse_git_log_limita_a_doscientos() {
    let raw: String = (0..201)
        .map(|index| format!("r{index}\x1fat\x1ft{index}\x1f\x1e"))
        .collect();

    assert_eq!(parse_git_log(&raw).len(), 200);
}

#[test]
fn git_log_rechaza_carpeta_inexistente() {
    let base = tempfile::tempdir().unwrap();
    let missing = base.path().join("no-existe");
    let missing = missing.to_str().unwrap();

    let error = captura::git_log(missing, None).unwrap_err();

    assert_eq!(
        error.to_string(),
        format!("esa carpeta no existe: {missing}")
    );
}

#[test]
fn git_log_lee_commits_del_repo() {
    let _guarda = comun::GUARDA_PATH.lock().expect("guarda del PATH");
    comun::usar_git_falso();
    let dir = tempfile::tempdir().unwrap();
    comun::repo_con_salida(dir.path(), &comun::registros(1));

    let source = captura::git_log(
        dir.path().to_str().unwrap(),
        Some(Utc::now() - Duration::hours(1)),
    )
    .unwrap();

    assert_eq!(
        source.items.as_ref().map(Vec::len),
        Some(1),
        "items: {:?}",
        source.items
    );
    assert_eq!(source.items.as_ref().unwrap()[0].title, "t0");
    assert_eq!(
        source.name,
        dir.path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned()
    );
    let valor = serde_json::to_value(&source).unwrap();
    assert_eq!(
        valor["truncated"],
        json!(false),
        "un repo con un commit no está truncado: {valor}"
    );
}

#[test]
fn git_log_sin_commits_dice_desde_cuando() {
    let _guarda = comun::GUARDA_PATH.lock().expect("guarda del PATH");
    comun::usar_git_falso();
    let dir = tempfile::tempdir().unwrap();
    comun::repo_con_salida(dir.path(), b"");
    let since = Utc::now() + Duration::days(1);

    let error = captura::git_log(dir.path().to_str().unwrap(), Some(since)).unwrap_err();

    let fecha = format!("{} {} {}", since.day(), since.format("%b"), since.year());
    assert_eq!(
        error.to_string(),
        format!("no hay commits en esa carpeta desde {fecha}")
    );
}

#[tokio::test]
async fn create_goal_arma_los_argumentos_completos() {
    let fake = Fake::texto(&[r#"{"goal": {"id": "g1", "title": "Meta"}}"#]);

    let goal = captura::create_goal(
        &fake,
        GoalInput {
            title: "  Meta  ".to_owned(),
            description: "  detalle  ".to_owned(),
            due_date: "2026-12-01".to_owned(),
            team_id: "t1".to_owned(),
            period_id: "p1".to_owned(),
            weight: 2.5,
            is_primary: true,
            milestones: Some(vec!["m1".to_owned()]),
            workers: Some(vec!["u1".to_owned()]),
        },
    )
    .await
    .unwrap();

    assert_eq!(goal.id, "g1");
    let calls = fake.calls();
    assert_eq!(calls.len(), 1);
    assert_call(
        &calls,
        0,
        "create_goal",
        json!({
            "title": "Meta",
            "description": "detalle",
            "due_date": "2026-12-01",
            "team_id": "t1",
            "period_id": "p1",
            "weight": 2.5,
            "is_primary": true,
            "milestones": ["m1"],
            "workers": ["u1"]
        }),
    );
}

#[tokio::test]
async fn create_goal_exige_titulo() {
    let fake = Fake::texto(&[]);

    let error = captura::create_goal(
        &fake,
        GoalInput {
            title: "   ".to_owned(),
            ..GoalInput::default()
        },
    )
    .await
    .unwrap_err();

    assert_eq!(error.to_string(), "la meta necesita un título");
    assert!(fake.calls().is_empty());
}

#[tokio::test]
async fn create_goal_solo_manda_milestones_y_workers_si_vienen_los_dos() {
    let fake = Fake::texto(&[r#"{"goal": {"id": "g1"}}"#]);

    captura::create_goal(
        &fake,
        GoalInput {
            title: "Meta".to_owned(),
            milestones: Some(vec!["m1".to_owned()]),
            ..GoalInput::default()
        },
    )
    .await
    .unwrap();

    let calls = fake.calls();
    assert_call(&calls, 0, "create_goal", json!({"title": "Meta"}));
}

#[tokio::test]
async fn create_goal_cae_al_id_del_nivel_superior() {
    let fake = Fake::texto(&[r#"{"goal_id": "g9"}"#]);

    let goal = captura::create_goal(
        &fake,
        GoalInput {
            title: "  Meta  ".to_owned(),
            description: "  detalle  ".to_owned(),
            due_date: "2026-12-01".to_owned(),
            ..GoalInput::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(goal.id, "g9");
    assert_eq!(goal.title, "Meta");
    // El fallback conserva la descripción original, no la recortada que viajó.
    assert_eq!(goal.description, "  detalle  ");
    assert_eq!(goal.due_date.as_deref(), Some("2026-12-01"));
}

#[tokio::test]
async fn create_goal_acepta_id_cuando_no_hay_goal_ni_goal_id() {
    let fake = Fake::texto(&[r#"{"id": "g2"}"#]);

    let goal = captura::create_goal(
        &fake,
        GoalInput {
            title: "Meta".to_owned(),
            ..GoalInput::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(goal.id, "g2");
}

#[tokio::test]
async fn crear_sin_id_en_la_respuesta_no_es_exito() {
    let sin_goal = Fake::texto(&["{}"]);
    let error = captura::create_goal(
        &sin_goal,
        GoalInput {
            title: "Meta".to_owned(),
            ..GoalInput::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(error.to_string(), "create_goal: sin id en la respuesta");

    let sin_periodo = Fake::texto(&[r#"{"period": {"name": "Sprint"}}"#]);
    let error = orus_org::tareas::create_period(
        &sin_periodo,
        &orus_org::PeriodInput {
            name: "Sprint".to_owned(),
            start_date: "2026-11-01".to_owned(),
            end_date: "2026-11-15".to_owned(),
            ..orus_org::PeriodInput::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(error.to_string(), "create_period: sin id en la respuesta");
}

#[tokio::test]
async fn propose_digest_exige_since_y_trabajo() {
    let fake = Fake::texto(&[]);

    let sin_fecha = captura::propose_digest(&fake, "  ", &[]).await.unwrap_err();
    assert_eq!(sin_fecha.to_string(), "falta la fecha desde la que buscar");

    let sin_trabajo = captura::propose_digest(&fake, "2026-11-01", &[])
        .await
        .unwrap_err();
    assert_eq!(sin_trabajo.to_string(), "no hay trabajo que proponer");
    assert!(fake.calls().is_empty());
}

#[tokio::test]
async fn propose_digest_manda_fuentes_y_lee_el_borrador() {
    let fake = Fake::texto(&[r#"{
        "digest_id": "d1",
        "expires_at": "2026-11-12T00:00:00Z",
        "candidates": [{"id": "c1", "suggested_goal": {"id": "g1", "title": "Meta", "score": 0.8}}],
        "next_step": "revisa"
    }"#]);
    let sources = [DigestSource {
        name: "repo".to_owned(),
        items: Some(vec![DigestItem {
            kind: "commit".to_owned(),
            title: "uno".to_owned(),
            summary: String::new(),
            r#ref: "abc".to_owned(),
            at: "2026-11-04T19:06:42-05:00".to_owned(),
            state: "done".to_owned(),
        }]),
        truncated: false,
    }];

    let digest = captura::propose_digest(&fake, "2026-11-01", &sources)
        .await
        .unwrap();

    assert_eq!(digest.id, "d1");
    assert_eq!(digest.next_step, "revisa");
    let candidates = digest.candidates.as_ref().unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(
        candidates[0]
            .suggested_goal
            .as_ref()
            .map(|goal| goal.title.as_str()),
        Some("Meta")
    );
    let calls = fake.calls();
    assert_eq!(calls.len(), 1);
    assert_call(
        &calls,
        0,
        "propose_work_digest",
        json!({
            "since": "2026-11-01",
            "sources": [{
                "name": "repo",
                "items": [{
                    "kind": "commit",
                    "title": "uno",
                    "ref": "abc",
                    "at": "2026-11-04T19:06:42-05:00",
                    "state": "done"
                }],
                "truncated": false
            }]
        }),
    );
}

#[tokio::test]
async fn apply_digest_exige_seleccion_y_devuelve_el_texto() {
    let fake = Fake::texto(&["listo"]);

    let error = captura::apply_digest(&fake, "", &["c1".to_owned()])
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "no hay nada seleccionado");

    let error = captura::apply_digest(&fake, "d1", &[]).await.unwrap_err();
    assert_eq!(error.to_string(), "no hay nada seleccionado");

    let texto = captura::apply_digest(&fake, "d1", &["c1".to_owned(), "c2".to_owned()])
        .await
        .unwrap();
    assert_eq!(texto, "listo");

    let calls = fake.calls();
    assert_eq!(calls.len(), 1);
    assert_call(
        &calls,
        0,
        "apply_work_digest",
        json!({"digest_id": "d1", "accept": ["c1", "c2"]}),
    );
}
