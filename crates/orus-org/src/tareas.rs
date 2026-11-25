//! Tareas, metas, periodos, actividad, decisiones y equipos.
//!
//! Port de `internal/oruscribe/oruscribe.go:713-989`. Las funciones públicas
//! reciben un [`ToolCaller`]; [`Client`] lo implementa sobre MCP y los tests lo
//! implementan con respuestas guionadas. El timbre y las herramientas viven en
//! `doorbell` y `client`.

use crate::rfc3339;
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::client::{Client, JsonObject};
use crate::error::{OrgError, Result};
use crate::mensajes::ToolCaller;
use crate::types::{
    ActivityEvent, Decision, Goal, GoalDetail, Mention, Pagina, Paginacion, Period, PeriodInput,
    Status, Task, TaskInput, TeamOverview,
};

#[derive(Debug, Deserialize)]
struct GoalsResponse {
    #[serde(default)]
    goals: Vec<Value>,
}

#[derive(Debug, Deserialize)]
struct PeriodsResponse {
    #[serde(default)]
    periods: Vec<Period>,
}

#[derive(Debug, Deserialize)]
struct PeriodResponse {
    #[serde(default)]
    period: Period,
}

#[derive(Debug, Deserialize)]
struct ActivityResponse {
    #[serde(default)]
    events: Vec<ActivityEvent>,
    #[serde(flatten)]
    pagina: Paginacion,
}

#[derive(Debug, Deserialize)]
struct DecisionsResponse {
    #[serde(default)]
    results: Vec<Decision>,
}

#[derive(Debug, Deserialize)]
struct TeamsResponse {
    #[serde(default)]
    teams: Vec<TeamOverview>,
}

#[derive(Debug, Deserialize)]
struct TaskResponse {
    #[serde(default)]
    task: Task,
}

#[derive(Debug, Deserialize)]
struct TasksResponse {
    #[serde(default)]
    tasks: Vec<Task>,
    #[serde(flatten)]
    pagina: Paginacion,
}

#[derive(Debug, Deserialize)]
struct MentionsResponse {
    #[serde(default)]
    mentions: Vec<Mention>,
    #[serde(flatten)]
    pagina: Paginacion,
}

/// Las metas de la organización con su avance y la gente que las lleva.
///
/// Prefiere `list_goals` porque su respuesta ya tiene la forma que pinta la
/// ventana; `get_my_goals` queda como respaldo para un servidor que todavía no
/// la implementa.
pub async fn goals<C: ToolCaller + ?Sized>(caller: &C) -> Result<Vec<Goal>> {
    let (tool, crudas) = match caller
        .call_json::<GoalsResponse>("list_goals", JsonObject::new())
        .await
    {
        Ok(response) => ("list_goals", response.goals),
        Err(
            error @ (OrgError::RateLimited { .. }
            | OrgError::NotConnected
            | OrgError::Connect(_)
            | OrgError::InvalidJson { .. }),
        ) => return Err(error),
        Err(_) => (
            "get_my_goals",
            caller
                .call_json::<GoalsResponse>("get_my_goals", JsonObject::new())
                .await?
                .goals,
        ),
    };
    crudas
        .into_iter()
        .map(|raw| normalizar_meta(tool, raw))
        .collect()
}

/// El servidor distingue `cells_total` ausente de un cero explícito: solo en
/// el primer caso se deduce de `my_tasks`, y nunca en modo manual (#130).
fn normalizar_meta(tool: &str, raw: Value) -> Result<Goal> {
    let celdas_explicitas = raw.get("cells_total").is_some_and(|valor| !valor.is_null());
    let mut goal: Goal = serde_json::from_value(raw).map_err(|source| OrgError::InvalidJson {
        tool: tool.to_owned(),
        source,
    })?;
    if goal.progress_pct == 0.0 && goal.percent != 0.0 {
        goal.progress_pct = goal.percent;
    }
    if goal.percent == 0.0 && goal.progress_pct != 0.0 {
        goal.percent = goal.progress_pct;
    }
    if !celdas_explicitas
        && goal.mode != "manual"
        && let Some(tasks) = goal.my_tasks.as_ref().filter(|tasks| !tasks.is_empty())
    {
        goal.cells_total = tasks.len() as i64;
        for task in tasks {
            if task.status == Status::Done {
                goal.cells_done += 1;
            }
        }
    }
    Ok(goal)
}

/// Lee una meta con las tareas enlazadas.
pub async fn goal_detail<C: ToolCaller + ?Sized>(caller: &C, goal_id: &str) -> Result<GoalDetail> {
    let mut args = JsonObject::new();
    args.insert("goal_id".to_owned(), Value::from(goal_id));
    let mut out: GoalDetail = caller.call_json("get_goal", args).await?;

    if out.goal.progress_pct == 0.0 {
        out.goal.progress_pct = out.progress_pct;
    }
    if out.goal.cells_done == 0 {
        out.goal.cells_done = out.cells_done;
    }
    if out.goal.cells_total == 0 {
        out.goal.cells_total = out.cells_total;
    }
    if out.goal.mode.is_empty() {
        out.goal.mode = out.mode.clone();
    }
    if out
        .goal
        .per_worker
        .as_ref()
        .is_none_or(|workers| workers.is_empty())
    {
        out.goal.per_worker = out.per_worker.clone();
    }
    out.goal.needs_attention = out.goal.needs_attention || out.needs_attention;
    if out.goal.percent == 0.0 {
        out.goal.percent = out.goal.progress_pct;
    }
    if out.goal.progress_pct == 0.0 {
        out.goal.progress_pct = out.goal.percent;
    }
    Ok(out)
}

/// Los sprints, meses y rangos en los que se agrupan las metas.
pub async fn periods<C: ToolCaller + ?Sized>(caller: &C) -> Result<Vec<Period>> {
    let response: PeriodsResponse = caller.call_json("list_periods", JsonObject::new()).await?;
    Ok(response.periods)
}

/// Crea un sprint, mes, trimestre o rango.
pub async fn create_period<C: ToolCaller + ?Sized>(
    caller: &C,
    input: &PeriodInput,
) -> Result<Period> {
    let name = input.name.trim();
    if name.is_empty() || input.start_date.is_empty() || input.end_date.is_empty() {
        return Err(OrgError::InvalidInput(
            "el periodo necesita nombre, inicio y fin".to_owned(),
        ));
    }
    let mut args = JsonObject::new();
    args.insert("name".to_owned(), Value::from(name));
    args.insert(
        "start_date".to_owned(),
        Value::from(input.start_date.as_str()),
    );
    args.insert("end_date".to_owned(), Value::from(input.end_date.as_str()));
    if !input.kind.is_empty() {
        args.insert("kind".to_owned(), Value::from(input.kind.as_str()));
    }
    if !input.team_id.is_empty() {
        args.insert("team_id".to_owned(), Value::from(input.team_id.as_str()));
    }
    let response: PeriodResponse = caller.call_json("create_period", args).await?;
    if response.period.id.is_empty() {
        return Err(OrgError::MissingId {
            tool: "create_period".to_owned(),
        });
    }
    Ok(response.period)
}

/// Lo que cambió desde un momento, lo más nuevo primero.
///
/// `since = None` representa el cero de Go: se buscan los últimos siete días.
pub async fn activity<C: ToolCaller + ?Sized>(
    caller: &C,
    since: Option<DateTime<Utc>>,
    limit: i64,
) -> Result<Pagina<ActivityEvent>> {
    let since = since.unwrap_or_else(|| Utc::now() - Duration::days(7));
    let limit = if limit <= 0 || limit > 100 { 50 } else { limit };
    let mut args = JsonObject::new();
    args.insert("since".to_owned(), Value::from(rfc3339(since)));
    args.insert("limit".to_owned(), Value::from(limit));
    let response: ActivityResponse = caller.call_json("get_activity", args).await?;
    Ok(response.pagina.con_items(response.events))
}

/// Las decisiones registradas de la organización.
pub async fn decisions<C: ToolCaller + ?Sized>(
    caller: &C,
    query: &str,
    limit: i64,
) -> Result<Vec<Decision>> {
    let limit = if limit <= 0 || limit > 50 { 20 } else { limit };
    let mut args = JsonObject::new();
    args.insert("limit".to_owned(), Value::from(limit));
    let query = query.trim();
    if !query.is_empty() {
        args.insert("query".to_owned(), Value::from(query));
    }
    let response: DecisionsResponse = caller.call_json("search_decisions", args).await?;
    Ok(response.results)
}

/// Los equipos que ve la sesión, con el foco y las tareas de cada persona.
pub async fn team_status<C: ToolCaller + ?Sized>(caller: &C) -> Result<Vec<TeamOverview>> {
    let response: TeamsResponse = caller
        .call_json("get_team_status", JsonObject::new())
        .await?;
    Ok(response.teams)
}

/// Crea una tarea de nivel superior asignada a esta sesión.
pub async fn create_task<C: ToolCaller + ?Sized>(caller: &C, input: &TaskInput) -> Result<Task> {
    let title = input.title.trim();
    let description = input.description.trim();
    if title.is_empty() {
        return Err(OrgError::InvalidInput(
            "la tarea necesita un título".to_owned(),
        ));
    }
    if description.is_empty() {
        return Err(OrgError::InvalidInput(
            "la tarea necesita una descripción".to_owned(),
        ));
    }
    let mut args = JsonObject::new();
    args.insert("title".to_owned(), Value::from(title));
    args.insert("description".to_owned(), Value::from(description));
    if !input.priority.is_empty() {
        args.insert("priority".to_owned(), Value::from(input.priority.as_str()));
    }
    if !input.status.is_empty() {
        args.insert("status".to_owned(), Value::from(input.status.as_str()));
    }
    if !input.due_date.is_empty() {
        args.insert("due_date".to_owned(), Value::from(input.due_date.as_str()));
    }
    if !input.start_date.is_empty() {
        args.insert(
            "start_date".to_owned(),
            Value::from(input.start_date.as_str()),
        );
    }
    if !input.goal_id.is_empty() {
        args.insert("goal_id".to_owned(), Value::from(input.goal_id.as_str()));
    }
    if let Some(subtasks) = input.subtasks.as_ref().filter(|items| !items.is_empty()) {
        args.insert("subtasks".to_owned(), json!(subtasks));
    }
    let response: TaskResponse = caller.call_json("create_task", args).await?;
    if response.task.id.is_empty() {
        return Err(OrgError::InvalidInput(
            "create_task: respuesta sin tarea".to_owned(),
        ));
    }
    Ok(response.task)
}

/// Busca tareas visibles para esta sesión.
pub async fn search_tasks<C: ToolCaller + ?Sized>(
    caller: &C,
    query: &str,
    status: Status,
    limit: i64,
) -> Result<Pagina<Task>> {
    let limit = if limit <= 0 || limit > 50 { 30 } else { limit };
    let mut args = JsonObject::new();
    args.insert("limit".to_owned(), Value::from(limit));
    let query = query.trim();
    if !query.is_empty() {
        args.insert("query".to_owned(), Value::from(query));
    }
    if status.valid() {
        args.insert("status".to_owned(), Value::from(status.as_str()));
    }
    let response: TasksResponse = caller.call_json("search_tasks", args).await?;
    Ok(response.pagina.con_items(response.tasks))
}

/// Manda un aviso a un equipo o a una persona.
pub async fn send_alert<C: ToolCaller + ?Sized>(
    caller: &C,
    kind: &str,
    message: &str,
    team_id: &str,
    user_id: &str,
) -> Result<()> {
    let message = message.trim();
    if message.is_empty() {
        return Err(OrgError::InvalidInput("el aviso está vacío".to_owned()));
    }
    let mut args = JsonObject::new();
    args.insert("type".to_owned(), Value::from(kind));
    args.insert("message".to_owned(), Value::from(message));
    if !team_id.is_empty() {
        args.insert("target_team_id".to_owned(), Value::from(team_id));
    }
    if !user_id.is_empty() {
        args.insert("target_user_id".to_owned(), Value::from(user_id));
    }
    caller.call("send_alert", args).await.map(|_| ())
}

/// Pone un emoji en un mensaje del chat.
pub async fn react<C: ToolCaller + ?Sized>(
    caller: &C,
    message_id: &str,
    emoji: &str,
) -> Result<()> {
    if message_id.is_empty() || emoji.is_empty() {
        return Err(OrgError::InvalidInput("reacción incompleta".to_owned()));
    }
    let mut args = JsonObject::new();
    args.insert("message_id".to_owned(), Value::from(message_id));
    args.insert("emoji".to_owned(), Value::from(emoji));
    caller.call("react_to_chat_message", args).await.map(|_| ())
}

/// Publica el foco y el ánimo que esta persona quiere que vea su equipo.
pub async fn update_context<C: ToolCaller + ?Sized>(
    caller: &C,
    focus: &str,
    mood: &str,
    notes: &str,
) -> Result<()> {
    let mut args = JsonObject::new();
    let focus = focus.trim();
    if !focus.is_empty() {
        args.insert("current_focus".to_owned(), Value::from(focus));
    }
    let notes = notes.trim();
    if !notes.is_empty() {
        args.insert("notes".to_owned(), Value::from(notes));
    }
    let mood = mood.trim();
    if !mood.is_empty() && !matches!(mood, "on_track" | "at_risk" | "blocked") {
        return Err(OrgError::InvalidInput(format!(
            "ánimo desconocido: {mood:?} (use on_track, at_risk o blocked)"
        )));
    }
    if !mood.is_empty() {
        args.insert("mood_signal".to_owned(), Value::from(mood));
    }
    if args.is_empty() {
        return Err(OrgError::InvalidInput(
            "no hay nada que actualizar".to_owned(),
        ));
    }
    caller.call("update_my_context", args).await.map(|_| ())
}

/// Las menciones de la persona en el chat.
pub async fn mentions<C: ToolCaller + ?Sized>(caller: &C) -> Result<Pagina<Mention>> {
    let response: MentionsResponse = caller
        .call_json("get_my_mentions", JsonObject::new())
        .await?;
    Ok(response.pagina.con_items(response.mentions))
}

impl Client {
    /// Las metas de la organización.
    pub async fn goals(&self) -> Result<Vec<Goal>> {
        goals(self).await
    }

    /// Lee una meta con sus tareas enlazadas.
    pub async fn goal_detail(&self, goal_id: &str) -> Result<GoalDetail> {
        goal_detail(self, goal_id).await
    }

    /// Los periodos de la organización.
    pub async fn periods(&self) -> Result<Vec<Period>> {
        periods(self).await
    }

    /// Crea un periodo.
    pub async fn create_period(&self, input: &PeriodInput) -> Result<Period> {
        create_period(self, input).await
    }

    /// La actividad desde un momento.
    pub async fn activity(
        &self,
        since: Option<DateTime<Utc>>,
        limit: i64,
    ) -> Result<Pagina<ActivityEvent>> {
        activity(self, since, limit).await
    }

    /// Las decisiones registradas.
    pub async fn decisions(&self, query: &str, limit: i64) -> Result<Vec<Decision>> {
        decisions(self, query, limit).await
    }

    /// Los equipos visibles para la sesión.
    pub async fn team_status(&self) -> Result<Vec<TeamOverview>> {
        team_status(self).await
    }

    /// Crea una tarea.
    pub async fn create_task(&self, input: &TaskInput) -> Result<Task> {
        create_task(self, input).await
    }

    /// Busca tareas.
    pub async fn search_tasks(
        &self,
        query: &str,
        status: Status,
        limit: i64,
    ) -> Result<Pagina<Task>> {
        search_tasks(self, query, status, limit).await
    }

    /// Manda un aviso.
    pub async fn send_alert(
        &self,
        kind: &str,
        message: &str,
        team_id: &str,
        user_id: &str,
    ) -> Result<()> {
        send_alert(self, kind, message, team_id, user_id).await
    }

    /// Reacciona a un mensaje.
    pub async fn react(&self, message_id: &str, emoji: &str) -> Result<()> {
        react(self, message_id, emoji).await
    }

    /// Publica foco y ánimo.
    pub async fn update_context(&self, focus: &str, mood: &str, notes: &str) -> Result<()> {
        update_context(self, focus, mood, notes).await
    }

    /// Las menciones de la persona.
    pub async fn mentions(&self) -> Result<Pagina<Mention>> {
        mentions(self).await
    }
}
