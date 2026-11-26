//! Lectura del espacio de trabajo: tareas, salas, mensajes, avisos, metas,
//! periodos, actividad, decisiones y equipos.
//!
//! Los
//! adaptadores solo validan la query, piden el cliente y arman la clave que la
//! ventana espera; los contadores y el mapeo de `TaskFiles` viven aquí porque
//! El contrato los compone en el handler. Las fotos que el backend no trae se
//! rellenan con la caché de avatares (`orus-app/src/avatares.rs`).

use std::sync::Arc;

use axum::extract::Query;
use axum::extract::State as AxumState;
use axum::http::StatusCode;
use axum::response::Response;
use chrono::{DateTime, Duration, Utc};
use orus_app::avatares::con_avatar;
use orus_app::{App, Pagina};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{ErrorRuta, write_json};

#[derive(Deserialize, Default)]
#[serde(default)]
pub(crate) struct ParamsTareas {
    offset: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub(crate) struct ParamsSalas {
    offset: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub(crate) struct ParamsBusqueda {
    q: String,
    status: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub(crate) struct ParamsArchivos {
    task: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub(crate) struct ParamsMensajes {
    room: String,
    offset: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub(crate) struct ParamsDetalle {
    id: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub(crate) struct ParamsActividad {
    days: String,
    since: String,
}

fn offset_de(raw: &str) -> i64 {
    raw.parse::<i64>()
        .ok()
        .filter(|offset| *offset >= 0)
        .unwrap_or(0)
}

fn pagina_json<T: Serialize>(clave: &str, pagina: Pagina<T>) -> Value {
    let mut cuerpo = serde_json::Map::new();
    cuerpo.insert(clave.to_owned(), json!(pagina.items));
    cuerpo.insert("count".to_owned(), json!(pagina.count));
    cuerpo.insert("total".to_owned(), json!(pagina.total));
    cuerpo.insert("has_more".to_owned(), json!(pagina.has_more));
    cuerpo.insert("truncated".to_owned(), json!(pagina.truncated));
    cuerpo.insert("next_offset".to_owned(), json!(pagina.next_offset));
    Value::Object(cuerpo)
}

/// `GET /api/tasks`.
pub(crate) async fn tareas(
    AxumState(app): AxumState<Arc<App>>,
    Query(params): Query<ParamsTareas>,
) -> Result<Response, ErrorRuta> {
    let cliente = app.client().await?;
    let pagina = cliente.tasks_page(offset_de(&params.offset)).await?;
    Ok(write_json(StatusCode::OK, &pagina_json("tasks", pagina)))
}

/// `GET /api/tasks/search`: el estado desconocido no filtra
/// porque `status, _ := ParseStatus` ignora el error.
pub(crate) async fn buscar_tareas(
    AxumState(app): AxumState<Arc<App>>,
    Query(params): Query<ParamsBusqueda>,
) -> Result<Response, ErrorRuta> {
    let cliente = app.client().await?;
    let pagina = cliente
        .search_tasks_page(&params.q, &params.status, 50)
        .await?;
    Ok(write_json(StatusCode::OK, &pagina_json("tasks", pagina)))
}

/// `GET /api/tasks/files`: el contrato mapea `update_comments` a
/// `comments`, completa `files` con `[]` y `comments` con `{}`.
pub(crate) async fn archivos_de_tarea(
    AxumState(app): AxumState<Arc<App>>,
    Query(params): Query<ParamsArchivos>,
) -> Result<Response, ErrorRuta> {
    if params.task.is_empty() {
        return Err(ErrorRuta::Invalido("falta la tarea".to_owned()));
    }
    let cliente = app.client().await?;
    let archivos = cliente.task_files(&params.task).await?;
    Ok(write_json(
        StatusCode::OK,
        &json!({
            "files": archivos.files.unwrap_or_default(),
            "comments": archivos.update_comments,
            "count": archivos.count,
            "total": archivos.total,
            "has_more": archivos.has_more,
            "truncated": archivos.truncated,
            "next_offset": archivos.next_offset,
        }),
    ))
}

/// `GET /api/rooms`. La foto de la otra persona de una
/// sala directa se rellena con la caché.
pub(crate) async fn salas(
    AxumState(app): AxumState<Arc<App>>,
    Query(params): Query<ParamsSalas>,
) -> Result<Response, ErrorRuta> {
    let cliente = app.client().await?;
    let mut pagina = cliente.rooms_page(offset_de(&params.offset)).await?;
    let urls = app.avatar_map();
    for sala in &mut pagina.items {
        if let Some(with) = sala.with.as_mut() {
            con_avatar(with, &urls);
        }
    }
    Ok(write_json(StatusCode::OK, &pagina_json("rooms", pagina)))
}

/// `GET /api/messages`: un offset inválido o negativo vale
/// 0, y la página lleva `messages`, `has_more` y `next_offset`.
pub(crate) async fn mensajes(
    AxumState(app): AxumState<Arc<App>>,
    Query(params): Query<ParamsMensajes>,
) -> Result<Response, ErrorRuta> {
    if params.room.is_empty() {
        return Err(ErrorRuta::Invalido("falta la sala".to_owned()));
    }
    let offset = offset_de(&params.offset);
    let cliente = app.client().await?;
    let mut pagina = cliente.messages(&params.room, 50, offset).await?;
    // La foto del autor y de las menciones tampoco viene del backend: se
    // rellena igual que en salas.
    let urls = app.avatar_map();
    for mensaje in &mut pagina.messages {
        con_avatar(&mut mensaje.author, &urls);
        if let Some(menciones) = mensaje.mentions.as_mut() {
            for mencion in menciones {
                con_avatar(mencion, &urls);
            }
        }
    }
    // `MessagesPage` no deriva `Serialize` y no es de este crate: se mapea.
    Ok(write_json(
        StatusCode::OK,
        &json!({
            "messages": pagina.messages,
            "has_more": pagina.has_more,
            "next_offset": pagina.next_offset,
        }),
    ))
}

/// `GET /api/alerts`: `unread` cuenta las no leídas.
pub(crate) async fn alertas(AxumState(app): AxumState<Arc<App>>) -> Result<Response, ErrorRuta> {
    let cliente = app.client().await?;
    let pagina = cliente.alerts_page().await?;
    let unread = pagina.items.iter().filter(|alerta| !alerta.read).count();
    let mut cuerpo = pagina_json("alerts", pagina);
    cuerpo["unread"] = json!(unread);
    Ok(write_json(StatusCode::OK, &cuerpo))
}

/// `GET /api/mentions`: `unread` cuenta las no leídas.
pub(crate) async fn menciones(AxumState(app): AxumState<Arc<App>>) -> Result<Response, ErrorRuta> {
    let cliente = app.client().await?;
    let pagina = cliente.mentions_page().await?;
    let unread = pagina.items.iter().filter(|mencion| !mencion.read).count();
    let mut cuerpo = pagina_json("mentions", pagina);
    cuerpo["unread"] = json!(unread);
    Ok(write_json(StatusCode::OK, &cuerpo))
}

/// `GET /api/goals`.
pub(crate) async fn metas(AxumState(app): AxumState<Arc<App>>) -> Result<Response, ErrorRuta> {
    let cliente = app.client().await?;
    let metas = cliente.goals().await?;
    Ok(write_json(StatusCode::OK, &json!({"goals": metas})))
}

/// `GET /api/goals/detail`: responde el `GoalDetail`
/// directo y cambia `linked_tasks: null` por `[]`.
pub(crate) async fn detalle_de_meta(
    AxumState(app): AxumState<Arc<App>>,
    Query(params): Query<ParamsDetalle>,
) -> Result<Response, ErrorRuta> {
    if params.id.is_empty() {
        return Err(ErrorRuta::Invalido("falta la meta".to_owned()));
    }
    let cliente = app.client().await?;
    let detalle = cliente.goal_detail(&params.id).await?;
    let mut value: Value = serde_json::to_value(detalle)
        .map_err(|error| orus_app::SessionError::Tool(error.to_string()))?;
    if value.get("linked_tasks").is_none_or(Value::is_null) {
        value["linked_tasks"] = json!([]);
    }
    Ok(write_json(StatusCode::OK, &value))
}

/// `GET /api/periods`.
pub(crate) async fn periodos(AxumState(app): AxumState<Arc<App>>) -> Result<Response, ErrorRuta> {
    let cliente = app.client().await?;
    let periodos = cliente.periods().await?;
    Ok(write_json(StatusCode::OK, &json!({"periods": periodos})))
}

/// `days` laxo: espacios
/// iniciales, signo opcional y dígitos hasta el primer carácter que no lo sea.
fn dias_laxos(texto: &str) -> Option<i64> {
    let texto = texto.trim_start();
    let (signo, resto) = match texto.as_bytes().first() {
        Some(b'-') => (-1, &texto[1..]),
        Some(b'+') => (1, &texto[1..]),
        _ => (1, texto),
    };
    let digitos: String = resto.chars().take_while(char::is_ascii_digit).collect();
    if digitos.is_empty() {
        return None;
    }
    let valor = digitos.parse::<i64>().ok()?;
    Some(signo * valor)
}

fn instante(texto: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(texto)
        .ok()
        .map(|fecha| fecha.with_timezone(&Utc))
}

/// `GET /api/activity`: `days` válido (1..90) manda; un
/// `since` RFC3339 válido lo pisa; un `since` inválido no descarta `days`.
pub(crate) async fn actividad(
    AxumState(app): AxumState<Arc<App>>,
    Query(params): Query<ParamsActividad>,
) -> Result<Response, ErrorRuta> {
    let mut since = Utc::now() - Duration::days(7);
    if !params.days.is_empty()
        && let Some(days) = dias_laxos(&params.days)
        && (1..=90).contains(&days)
    {
        since = Utc::now() - Duration::days(days);
    }
    if !params.since.is_empty()
        && let Some(valor) = instante(&params.since)
    {
        since = valor;
    }
    let cliente = app.client().await?;
    let pagina = cliente.activity_page(Some(since), 60).await?;
    Ok(write_json(StatusCode::OK, &pagina_json("events", pagina)))
}

/// `GET /api/decisions`.
pub(crate) async fn decisiones(
    AxumState(app): AxumState<Arc<App>>,
    Query(params): Query<ParamsBusqueda>,
) -> Result<Response, ErrorRuta> {
    let cliente = app.client().await?;
    let decisiones = cliente.decisions(&params.q, 30).await?;
    Ok(write_json(
        StatusCode::OK,
        &json!({"decisions": decisiones}),
    ))
}

/// `GET /api/team`. La foto de cada trabajador se
/// rellena con la caché.
pub(crate) async fn equipos(AxumState(app): AxumState<Arc<App>>) -> Result<Response, ErrorRuta> {
    let cliente = app.client().await?;
    let mut equipos = cliente.team_status().await?;
    let urls = app.avatar_map();
    for equipo in &mut equipos {
        if let Some(trabajadores) = equipo.workers.as_mut() {
            for trabajador in trabajadores {
                con_avatar(&mut trabajador.user, &urls);
            }
        }
    }
    Ok(write_json(StatusCode::OK, &json!({"teams": equipos})))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dias_laxos_imita_a_sscanf() {
        assert_eq!(dias_laxos("14"), Some(14));
        assert_eq!(dias_laxos("14x"), Some(14));
        assert_eq!(dias_laxos("  14"), Some(14));
        assert_eq!(dias_laxos("+14"), Some(14));
        assert_eq!(dias_laxos("-14"), Some(-14));
        assert_eq!(dias_laxos("basura"), None);
        assert_eq!(dias_laxos(""), None);
        assert_eq!(dias_laxos("x14"), None);
    }

    #[test]
    fn pagina_json_conserva_los_campos_de_paginacion() {
        let pagina = Pagina {
            items: vec![1, 2],
            count: 2,
            total: 9,
            has_more: true,
            truncated: false,
            next_offset: 2,
        };

        let cuerpo = pagina_json("items", pagina);

        assert_eq!(cuerpo["items"], json!([1, 2]));
        assert_eq!(cuerpo["count"], json!(2));
        assert_eq!(cuerpo["total"], json!(9));
        assert_eq!(cuerpo["has_more"], json!(true));
        assert_eq!(cuerpo["truncated"], json!(false));
        assert_eq!(cuerpo["next_offset"], json!(2));
    }
}
