//! Tests de contrato de los tipos y de la apertura de sesión.

use chrono::{DateTime, Utc};
use orus_org::{
    Client, DEFAULT_ENDPOINT, Goal, Me, Message, OrgError, Room, Status, Task, TaskFiles,
    TaskInput, parse_status,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

fn round_trip<T>(json: &str) -> T
where
    T: DeserializeOwned + Serialize + PartialEq + std::fmt::Debug,
{
    let first: T = serde_json::from_str(json).unwrap();
    let encoded = serde_json::to_string(&first).unwrap();
    let second: T = serde_json::from_str(&encoded).unwrap();
    assert_eq!(first, second, "el tipo no sobrevive un round-trip");
    first
}

#[test]
fn status_serializes_with_wire_names() {
    let cases = [
        (Status::Pending, "\"pending\""),
        (Status::InProgress, "\"in_progress\""),
        (Status::Done, "\"done\""),
        (Status::Blocked, "\"blocked\""),
    ];
    for (status, wire) in cases {
        assert_eq!(serde_json::to_string(&status).unwrap(), wire);
        assert_eq!(serde_json::from_str::<Status>(wire).unwrap(), status);
    }

    let unknown: Status = serde_json::from_str("\"archived\"").unwrap();
    assert_eq!(unknown, Status::Unknown("archived".to_owned()));
    assert!(!unknown.valid());
    assert!(!unknown.open());
    assert_eq!(serde_json::to_string(&unknown).unwrap(), "\"archived\"");
}

#[test]
fn task_keeps_unknown_status_instead_of_failing() {
    // Go decodifica Status como string y no valida al leer: un valor nuevo del
    // servidor no debe romper la lista de tareas.
    let task: Task = serde_json::from_str(r#"{"id": "t1", "status": "archived"}"#).unwrap();
    assert_eq!(task.status, Status::Unknown("archived".to_owned()));
    assert!(!task.status.valid());
}

#[test]
fn parse_status_accepts_known_values_and_rejects_others() {
    let cases = [
        ("pending", Status::Pending, true),
        ("in_progress", Status::InProgress, true),
        ("done", Status::Done, false),
        ("blocked", Status::Blocked, true),
    ];
    for (wire, expected, open) in cases {
        let status = parse_status(wire).unwrap();
        assert_eq!(status, expected);
        assert!(status.valid());
        assert_eq!(status.open(), open);
        assert_eq!(status.as_str(), wire);
    }

    let error = parse_status("archived").unwrap_err();
    assert!(matches!(error, OrgError::UnknownStatus(ref value) if value == "archived"));
    assert_eq!(error.to_string(), "estado desconocido: \"archived\"");
}

#[test]
fn task_round_trip_keeps_tags_and_nulls() {
    let task: Task = round_trip(
        r#"{
            "id": "t1",
            "title": "Portar tipos",
            "description": "desde Go",
            "status": "in_progress",
            "priority": "high",
            "goal_id": "g1",
            "start_date": "2026-01-01",
            "due_date": "2026-01-10",
            "updated_at": "2026-01-05T10:00:00Z",
            "created_at": "2026-01-01T09:00:00Z",
            "assigned_to": "u1",
            "parent_task_id": null,
            "tags": ["rust", "port"],
            "points": 3,
            "subtasks": [
                {"id": "s1", "title": "leer", "status": "done"}
            ]
        }"#,
    );

    assert_eq!(task.status, Status::InProgress);
    assert_eq!(task.goal_id.as_deref(), Some("g1"));
    assert_eq!(task.parent_task_id, None);
    assert_eq!(task.points, Some(3));
    assert_eq!(
        task.tags.as_deref(),
        Some(["rust".to_owned(), "port".to_owned()].as_slice())
    );
    assert_eq!(task.subtasks.as_ref().map(Vec::len), Some(1));
    assert_eq!(
        task.updated_at,
        "2026-01-05T10:00:00Z".parse::<DateTime<Utc>>().unwrap()
    );
}

#[test]
fn goal_round_trip_keeps_pct_and_nested_lists() {
    let goal: Goal = round_trip(
        r#"{
            "id": "g1",
            "team_id": "tm1",
            "title": "Migrar",
            "description": "a Rust",
            "due_date": null,
            "weight": 1.5,
            "is_active": true,
            "created_at": "2026-01-01T00:00:00Z",
            "period_id": null,
            "created_by": "u1",
            "is_primary": true,
            "progress_pct": 42.5,
            "cells_done": 17,
            "cells_total": 40,
            "mode": "count",
            "per_worker": [
                {"pct": 50.0, "done": 5, "name": "Ana", "total": 10, "worker_id": "u2"}
            ],
            "needs_attention": false,
            "pct": 42.5,
            "my_milestones": [
                {"key": "m1", "label": "Primero", "done": true}
            ],
            "my_tasks": [
                {
                    "id": "t1",
                    "title": "Portar",
                    "status": "blocked",
                    "priority": "high",
                    "is_subtask": false,
                    "is_milestone": false,
                    "milestone_key": "m1",
                    "assignees": ["u2"]
                }
            ],
            "workers_summary": [
                {"pct": 42.5, "done": 17, "name": "Ana", "total": 40, "worker_id": "u2"}
            ]
        }"#,
    );

    assert_eq!(goal.percent, 42.5);
    assert_eq!(goal.weight, 1.5);
    assert_eq!(goal.per_worker.as_ref().map(Vec::len), Some(1));
    assert_eq!(goal.my_milestones.as_ref().map(Vec::len), Some(1));
    assert_eq!(
        goal.my_tasks
            .as_ref()
            .and_then(|tasks| tasks.first())
            .map(|task| &task.status),
        Some(&Status::Blocked)
    );
}

#[test]
fn me_round_trip_keeps_organization_and_capabilities() {
    let me: Me = round_trip(
        r#"{
            "user_id": "u1",
            "name": "Ana",
            "email": "ana@example.com",
            "role": "manager",
            "is_agent": false,
            "avatar_url": "https://example.com/ana.png",
            "organization": {"id": "o1", "slug": "acme", "name": "Acme"},
            "teams": [{"id": "tm1", "name": "Equipo"}],
            "capabilities": {"read_tasks": "all"}
        }"#,
    );

    assert_eq!(me.organization.slug, "acme");
    assert_eq!(me.teams.as_ref().map(Vec::len), Some(1));
    assert_eq!(me.capabilities.read_tasks, "all");
    assert_eq!(
        me.avatar_url.as_deref(),
        Some("https://example.com/ana.png")
    );
    assert_eq!(me.photo_url, None);
}

#[test]
fn message_round_trip_keeps_optional_blocks() {
    let message: Message = round_trip(
        r#"{
            "id": "m1",
            "author_id": "u1",
            "author": {"id": "u1", "name": "Ana", "is_agent": false},
            "body": "hola",
            "is_mine": true,
            "reply_to_id": null,
            "mentions": [
                {"id": "u2", "name": "Beto", "is_agent": true, "avatar_url": "https://example.com/b.png"}
            ],
            "attachments": [
                {
                    "id": "a1",
                    "file_name": "f.pdf",
                    "mime_type": "application/pdf",
                    "size_bytes": 123,
                    "download_url": "https://example.com/f.pdf"
                }
            ],
            "reactions": [
                {"emoji": "+1", "count": 2, "mine": true, "user_id": "u1", "user_name": "Ana"}
            ],
            "created_at": "2026-01-02T03:04:05Z"
        }"#,
    );

    assert_eq!(message.mentions.as_ref().map(Vec::len), Some(1));
    assert_eq!(message.attachments.as_ref().map(Vec::len), Some(1));
    assert_eq!(message.reactions.as_ref().map(Vec::len), Some(1));
    assert_eq!(
        message.author.avatar_url, None,
        "los campos omitidos quedan en None"
    );
}

#[test]
fn room_round_trip_keeps_renamed_counts() {
    let room: Room = round_trip(
        r#"{
            "room_id": "r1",
            "kind": "dm",
            "name": "Ana y Beto",
            "with": {"id": "u2", "name": "Beto", "is_agent": false},
            "last_message_at": "2026-01-02T03:04:05Z",
            "unread_count": 3,
            "mention_count": 1
        }"#,
    );

    assert_eq!(room.room_id, "r1");
    assert_eq!(room.unread, 3);
    assert_eq!(room.mentions, 1);
    assert_eq!(
        room.with.as_ref().map(|person| person.name.as_str()),
        Some("Beto")
    );
    assert!(room.last_message_at.is_some());
}

#[test]
fn task_files_round_trip_keeps_comment_map() {
    let files: TaskFiles = round_trip(
        r#"{
            "files": [
                {
                    "id": "f1",
                    "file_name": "x.txt",
                    "mime_type": "text/plain",
                    "size_bytes": 5,
                    "note": "n",
                    "update_id": "up1",
                    "download_url": "https://example.com/x.txt",
                    "uploaded_by": {"id": "u1", "name": "Ana", "is_agent": false},
                    "attached_at": "2026-01-02T03:04:05Z"
                }
            ],
            "update_comments": {
                "up1": [
                    {
                        "id": "c1",
                        "body": "ok",
                        "author": {"id": "u1", "name": "Ana", "is_agent": false},
                        "created_at": "2026-01-02T03:04:06Z"
                    }
                ]
            }
        }"#,
    );

    assert_eq!(files.files.as_ref().map(Vec::len), Some(1));
    assert_eq!(files.update_comments.get("up1").map(Vec::len), Some(1));
}

#[test]
fn task_input_serializes_every_field_with_its_tag() {
    let input: TaskInput = serde_json::from_str(
        r#"{
            "title": "Nueva",
            "description": "",
            "priority": "low",
            "status": "pending",
            "due_date": "",
            "start_date": "",
            "goal_id": "",
            "subtasks": []
        }"#,
    )
    .unwrap();

    let value: Value = serde_json::to_value(&input).unwrap();
    assert_eq!(
        value,
        json!({
            "title": "Nueva",
            "description": "",
            "priority": "low",
            "status": "pending",
            "due_date": "",
            "start_date": "",
            "goal_id": "",
            "subtasks": []
        })
    );
}

#[test]
fn default_endpoint_is_the_production_server() {
    assert_eq!(
        DEFAULT_ENDPOINT,
        "https://cfamprdygpngrjtaxzes.supabase.co/functions/v1/mcp-server"
    );
}

#[tokio::test]
async fn open_with_empty_token_fails_before_network() {
    let error = Client::open(DEFAULT_ENDPOINT, "", "acme")
        .await
        .unwrap_err();
    assert!(matches!(error, OrgError::IncompleteConfig));
    assert_eq!(error.to_string(), "configuración MCP incompleta");
}

#[tokio::test]
async fn open_with_empty_endpoint_and_token_uses_default_and_fails() {
    let error = Client::open("", "", "acme").await.unwrap_err();
    assert!(matches!(error, OrgError::IncompleteConfig));
}

#[tokio::test]
async fn open_with_invalid_endpoint_fails_before_network() {
    let error = Client::open("no es una url", "token", "acme")
        .await
        .unwrap_err();
    assert!(matches!(error, OrgError::InvalidEndpoint(ref value) if value == "no es una url"));
    assert_eq!(error.to_string(), "endpoint MCP inválido: no es una url");

    let remoto = "http://servidor-remoto.test/mcp";
    let error = Client::open(remoto, "token", "acme").await.unwrap_err();
    assert!(matches!(error, OrgError::InsecureEndpoint(ref value) if value == remoto));
    assert_eq!(
        error.to_string(),
        format!("endpoint MCP inseguro, se exige https salvo localhost: {remoto}")
    );
}
