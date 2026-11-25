//! Tipos del servidor MCP de Oruscribe.
//!
//! Port de `internal/oruscribe/oruscribe.go:28-390` y `capture.go:15-72`. Cada
//! campo conserva el tag JSON de Go; los punteros y las slices nil se modelan
//! con `Option`.

use std::collections::BTreeMap;
use std::fmt;

use chrono::{DateTime, Utc};
use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

/// Un `null` del servidor se lee como el cero de Go, igual que un campo ausente.
/// `#[serde(default)]` no cubre el `null` explícito: los contadores de Go son
/// punteros que pueden llegar así (`get_chat_messages` devuelve `null` en
/// `next_offset` cuando no hay página siguiente).
pub(crate) fn nulo_como_cero<'de, D>(deserializer: D) -> std::result::Result<i64, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<i64>::deserialize(deserializer)?.unwrap_or(0))
}

/// Lo mismo para los decimales: `get_my_goals` manda `pct: null` cuando la meta
/// no tiene avance calculado, y el tipo no puede rechazar la respuesta entera.
pub(crate) fn nulo_como_f64<'de, D>(deserializer: D) -> std::result::Result<f64, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<f64>::deserialize(deserializer)?.unwrap_or(0.0))
}

/// Y para los textos: `search_decisions` manda `rationale: null` en las
/// decisiones que se registraron sin motivo.
pub(crate) fn nulo_como_vacio<'de, D>(deserializer: D) -> std::result::Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
}

/// Estado del ciclo de vida de una tarea.
///
/// Es el vocabulario que acepta el servidor. Un valor fuera de él se conserva
/// como [`Status::Unknown`] al decodificar (igual que el `string` de Go), pero
/// [`parse_status`] lo rechaza y [`Status::valid`] devuelve falso.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Status {
    Pending,
    InProgress,
    Done,
    Blocked,
    Unknown(String),
}

impl Status {
    /// El texto que viaja por el cable.
    pub fn as_str(&self) -> &str {
        match self {
            Status::Pending => "pending",
            Status::InProgress => "in_progress",
            Status::Done => "done",
            Status::Blocked => "blocked",
            Status::Unknown(value) => value,
        }
    }

    /// Indica si el valor es uno de los cuatro que conoce el servidor.
    pub fn valid(&self) -> bool {
        !matches!(self, Status::Unknown(_))
    }

    /// Indica si la tarea todavía necesita trabajo.
    pub fn open(&self) -> bool {
        matches!(self, Status::Pending | Status::InProgress | Status::Blocked)
    }
}

impl Default for Status {
    fn default() -> Self {
        // El cero de Go para Status es la cadena vacía.
        Status::Unknown(String::new())
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for Status {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Status {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct StatusVisitor;

        impl Visitor<'_> for StatusVisitor {
            type Value = Status;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("un estado de tarea")
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Status, E> {
                Ok(match value {
                    "pending" => Status::Pending,
                    "in_progress" => Status::InProgress,
                    "done" => Status::Done,
                    "blocked" => Status::Blocked,
                    other => Status::Unknown(other.to_owned()),
                })
            }
        }

        deserializer.deserialize_str(StatusVisitor)
    }
}

/// Convierte texto del cable en un [`Status`], rechazando lo desconocido.
pub fn parse_status(value: &str) -> crate::Result<Status> {
    match value {
        "pending" => Ok(Status::Pending),
        "in_progress" => Ok(Status::InProgress),
        "done" => Ok(Status::Done),
        "blocked" => Ok(Status::Blocked),
        other => Err(crate::OrgError::UnknownStatus(other.to_owned())),
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Org {
    pub id: String,
    pub slug: String,
    pub name: String,
}

/// Una organización adicional que `get_me` ofrece para cambiar (#117).
/// El servidor manda solo `slug`, `name` y `role`: no es un `Org` completo.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OtherOrg {
    pub slug: String,
    pub name: String,
    pub role: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Team {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Capabilities {
    pub read_tasks: String,
}

/// La persona de la sesión, con lo que puede hacer.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Me {
    pub user_id: String,
    pub name: String,
    pub email: String,
    pub role: String,
    pub is_agent: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub photo_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub picture: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    pub organization: Org,
    /// Otras organizaciones de la persona, tal como las mandó `get_me`.
    /// `None` significa que el servidor no mandó el campo: no hay selector.
    #[serde(default)]
    pub other_organizations: Option<Vec<OtherOrg>>,
    pub teams: Option<Vec<Team>>,
    pub capabilities: Capabilities,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Subtask {
    pub id: String,
    pub title: String,
    pub status: Status,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub description: String,
    pub status: Status,
    pub priority: String,
    pub goal_id: Option<String>,
    pub start_date: Option<String>,
    pub due_date: Option<String>,
    pub updated_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub assigned_to: Option<String>,
    pub parent_task_id: Option<String>,
    pub tags: Option<Vec<String>>,
    pub points: Option<i64>,
    pub subtasks: Option<Vec<Subtask>>,
}

/// Lo que necesita `create_task`. Los tags no llevan `omitempty`: quien llama
/// decide qué campos vacíos omite.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TaskInput {
    pub title: String,
    pub description: String,
    pub priority: String,
    pub status: String,
    pub due_date: String,
    pub start_date: String,
    pub goal_id: String,
    pub subtasks: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Person {
    pub id: String,
    pub name: String,
    pub is_agent: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub photo_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub picture: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Attachment {
    pub id: String,
    pub file_name: String,
    pub mime_type: String,
    #[serde(default, deserialize_with = "nulo_como_cero")]
    pub size_bytes: i64,
    pub download_url: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TaskFile {
    pub id: String,
    pub file_name: String,
    pub mime_type: String,
    #[serde(default, deserialize_with = "nulo_como_cero")]
    pub size_bytes: i64,
    pub note: String,
    pub update_id: String,
    pub download_url: String,
    pub uploaded_by: Person,
    pub attached_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateComment {
    pub id: String,
    pub body: String,
    pub author: Person,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TaskFiles {
    pub files: Option<Vec<TaskFile>>,
    /// El mapa nil de Go se lee como vacío; esta estructura solo se decodifica.
    pub update_comments: BTreeMap<String, Vec<UpdateComment>>,
    /// El servidor pagina los avances (docs/contrato-avances.md §4): con
    /// `has_more` o `truncated` la lista quedó corta y `next_offset` dice
    /// desde dónde pedir la página siguiente. No se descartan: una lista
    /// parcial no puede presentarse como completa (#97).
    #[serde(default)]
    pub has_more: bool,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default, deserialize_with = "nulo_como_cero")]
    pub next_offset: i64,
    #[serde(default, deserialize_with = "nulo_como_cero")]
    pub count: i64,
    #[serde(default, deserialize_with = "nulo_como_cero")]
    pub total: i64,
}

/// Una colección paginada: los datos y lo que el servidor dice sobre lo que
/// queda afuera. La ventana decide si pide `next_offset` o avisa de `truncated`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Pagina<T> {
    pub items: Vec<T>,
    #[serde(default, deserialize_with = "nulo_como_cero")]
    pub count: i64,
    #[serde(default, deserialize_with = "nulo_como_cero")]
    pub total: i64,
    pub has_more: bool,
    pub truncated: bool,
    #[serde(default, deserialize_with = "nulo_como_cero")]
    pub next_offset: i64,
}

/// Los campos de paginación del sobre del servidor. Cada respuesta los aplana
/// junto a su colección y arma la [`Pagina`] con [`Paginacion::con_items`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub(crate) struct Paginacion {
    #[serde(default, deserialize_with = "nulo_como_cero")]
    pub count: i64,
    #[serde(default, deserialize_with = "nulo_como_cero")]
    pub total: i64,
    pub has_more: bool,
    pub truncated: bool,
    #[serde(default, deserialize_with = "nulo_como_cero")]
    pub next_offset: i64,
}

impl Paginacion {
    pub(crate) fn con_items<T>(self, items: Vec<T>) -> Pagina<T> {
        Pagina {
            items,
            count: self.count,
            total: self.total,
            has_more: self.has_more,
            truncated: self.truncated,
            next_offset: self.next_offset,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Message {
    pub id: String,
    pub author_id: String,
    pub author: Person,
    pub body: String,
    pub is_mine: bool,
    pub reply_to_id: Option<String>,
    pub mentions: Option<Vec<Person>>,
    pub attachments: Option<Vec<Attachment>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reactions: Option<Vec<Reaction>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Reaction {
    pub emoji: String,
    #[serde(default, deserialize_with = "nulo_como_cero")]
    pub count: i64,
    pub mine: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_name: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Room {
    #[serde(rename = "room_id")]
    pub room_id: String,
    pub kind: String,
    pub name: String,
    #[serde(rename = "with")]
    pub with: Option<Person>,
    pub last_message_at: Option<DateTime<Utc>>,
    #[serde(rename = "unread_count")]
    pub unread: i64,
    #[serde(rename = "mention_count")]
    pub mentions: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Alert {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub message: String,
    pub read: bool,
    pub task_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// La porción de un objetivo que lleva una persona.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkerProgress {
    #[serde(default, deserialize_with = "nulo_como_f64")]
    pub pct: f64,
    #[serde(default, deserialize_with = "nulo_como_cero")]
    pub done: i64,
    pub name: String,
    #[serde(default, deserialize_with = "nulo_como_cero")]
    pub total: i64,
    pub worker_id: String,
}

/// El objetivo medible de la organización.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Goal {
    pub id: String,
    pub team_id: Option<String>,
    pub title: String,
    pub description: String,
    pub due_date: Option<String>,
    #[serde(default, deserialize_with = "nulo_como_f64")]
    pub weight: f64,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub period_id: Option<String>,
    pub created_by: String,
    pub is_primary: bool,
    #[serde(default, deserialize_with = "nulo_como_f64")]
    pub progress_pct: f64,
    #[serde(default, deserialize_with = "nulo_como_cero")]
    pub cells_done: i64,
    #[serde(default, deserialize_with = "nulo_como_cero")]
    pub cells_total: i64,
    pub mode: String,
    pub per_worker: Option<Vec<WorkerProgress>>,
    pub needs_attention: bool,
    #[serde(rename = "pct", default, deserialize_with = "nulo_como_f64")]
    pub percent: f64,
    pub my_milestones: Option<Vec<Milestone>>,
    pub my_tasks: Option<Vec<GoalTask>>,
    pub workers_summary: Option<Vec<WorkerProgress>>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Milestone {
    pub key: String,
    pub label: String,
    pub done: bool,
}

/// Una tarea como la lista un objetivo: lo justo para enlazarla.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GoalTask {
    pub id: String,
    pub title: String,
    pub status: Status,
    pub priority: String,
    pub is_subtask: bool,
    pub is_milestone: bool,
    pub milestone_key: String,
    pub assignees: Option<Vec<String>>,
}

/// Un objetivo con las tareas enlazadas.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GoalDetail {
    pub goal: Goal,
    pub linked_tasks: Option<Vec<GoalTask>>,
    #[serde(default, deserialize_with = "nulo_como_f64")]
    pub progress_pct: f64,
    #[serde(default, deserialize_with = "nulo_como_cero")]
    pub cells_done: i64,
    #[serde(default, deserialize_with = "nulo_como_cero")]
    pub cells_total: i64,
    pub mode: String,
    pub per_worker: Option<Vec<WorkerProgress>>,
    pub needs_attention: bool,
}

/// Un repositorio o carpeta y el trabajo encontrado en él. Es la forma que
/// espera `propose_work_digest`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DigestSource {
    pub name: String,
    pub items: Option<Vec<DigestItem>>,
    /// El corte a 200 commits o al tope de lectura va en el modelo: stderr no
    /// llega a ninguna ventana y un borrador parcial parecería completo (#130).
    pub truncated: bool,
}

/// Una pieza de trabajo: un commit, un pull request, un issue o una nota. Hoy
/// solo se produce `commit` en local.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DigestItem {
    pub kind: String,
    pub title: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub summary: String,
    #[serde(rename = "ref", skip_serializing_if = "String::is_empty")]
    pub r#ref: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub at: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub state: String,
}

/// La meta que el servidor propone para un candidato.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SuggestedGoal {
    pub id: String,
    pub title: String,
    pub score: f64,
}

/// Un cambio propuesto que espera que una persona lo acepte.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DigestCandidate {
    pub id: String,
    pub action: String,
    pub title: String,
    pub description: String,
    pub status: String,
    pub suggested_goal: Option<SuggestedGoal>,
    pub sources: Option<Vec<String>>,
    pub refs: Option<Vec<String>>,
}

/// El borrador que devuelve Oruscribe Capture. Nada se escribe hasta que la
/// persona acepta algunos candidatos.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Digest {
    #[serde(rename = "digest_id")]
    pub id: String,
    pub expires_at: DateTime<Utc>,
    pub candidates: Option<Vec<DigestCandidate>>,
    pub skipped: Option<Vec<Value>>,
    pub next_step: String,
}

/// Lo que necesita `create_goal`. `milestones` y `workers` solo se mandan
/// cuando vienen los dos, porque el servidor rechaza uno sin el otro.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GoalInput {
    pub title: String,
    pub description: String,
    pub due_date: String,
    pub team_id: String,
    pub period_id: String,
    pub weight: f64,
    pub is_primary: bool,
    pub milestones: Option<Vec<String>>,
    pub workers: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Period {
    pub id: String,
    pub team_id: Option<String>,
    pub name: String,
    pub kind: String,
    pub start_date: String,
    pub end_date: String,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub is_current: bool,
}

/// Lo que necesita `create_period`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PeriodInput {
    pub name: String,
    pub start_date: String,
    pub end_date: String,
    pub kind: String,
    pub team_id: String,
}

/// Una fila del feed mezclado de actividad.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ActivityEvent {
    #[serde(rename = "type", default, deserialize_with = "nulo_como_vacio")]
    pub event_type: String,
    pub timestamp: DateTime<Utc>,
    #[serde(default, deserialize_with = "nulo_como_vacio")]
    pub actor_id: String,
    #[serde(default, deserialize_with = "nulo_como_vacio")]
    pub actor_name: String,
    #[serde(default, deserialize_with = "nulo_como_vacio")]
    pub task_id: String,
    #[serde(default, deserialize_with = "nulo_como_vacio")]
    pub task_title: String,
    #[serde(default, deserialize_with = "nulo_como_vacio")]
    pub from_status: String,
    #[serde(default, deserialize_with = "nulo_como_vacio")]
    pub to_status: String,
    #[serde(default, deserialize_with = "nulo_como_vacio")]
    pub decision_id: String,
    #[serde(default, deserialize_with = "nulo_como_vacio")]
    pub goal_id: String,
    #[serde(default, deserialize_with = "nulo_como_vacio")]
    pub team_id: String,
    #[serde(default, deserialize_with = "nulo_como_vacio")]
    pub title: String,
    #[serde(default, deserialize_with = "nulo_como_vacio")]
    pub current_focus: String,
    #[serde(default, deserialize_with = "nulo_como_vacio")]
    pub mood_signal: String,
    #[serde(default, deserialize_with = "nulo_como_vacio")]
    pub context_type: String,
    #[serde(default, deserialize_with = "nulo_como_vacio")]
    pub notes: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Decision {
    pub id: String,
    pub title: String,
    #[serde(default, deserialize_with = "nulo_como_vacio")]
    pub context: String,
    #[serde(default, deserialize_with = "nulo_como_vacio")]
    pub decision: String,
    #[serde(default, deserialize_with = "nulo_como_vacio")]
    pub rationale: String,
    pub created_at: DateTime<Utc>,
    pub created_by: String,
    pub is_active: bool,
    pub archived_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MemberInfo {
    pub id: String,
    pub name: String,
    pub email: String,
    pub role: String,
    pub is_agent: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub photo_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub picture: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkerContext {
    pub current_focus: String,
    pub mood_signal: String,
    pub notes: String,
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TeamMember {
    pub user: MemberInfo,
    pub context: Option<WorkerContext>,
    pub tasks: Option<Vec<Task>>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TeamOverview {
    pub id: String,
    pub name: String,
    pub goals: Option<Vec<Goal>>,
    pub workers: Option<Vec<TeamMember>>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Mention {
    pub id: String,
    pub room_id: String,
    pub room_name: String,
    pub message_id: String,
    pub body: String,
    pub author: Person,
    pub read: bool,
    pub created_at: DateTime<Utc>,
}

/// Lo que responde el servidor después de un cambio de estado.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Progress {
    #[serde(rename = "progress_reminder")]
    pub reminder: String,
    pub goal_pct: f64,
}

#[cfg(test)]
mod tests {
    use super::{Me, TaskFile, WorkerProgress};

    #[test]
    fn el_archivo_con_tamano_nulo_se_lee() {
        let leido = serde_json::from_str::<TaskFile>(r#"{"size_bytes":null}"#);
        let tamano = leido
            .as_ref()
            .map(|archivo| archivo.size_bytes)
            .map_err(|error| error.to_string());
        assert_eq!(tamano, Ok(0), "un size_bytes nulo se lee como cero");
    }

    #[test]
    fn el_avance_con_pct_nulo_se_lee() {
        let leido = serde_json::from_str::<WorkerProgress>(r#"{"pct":null}"#);
        let pct = leido
            .as_ref()
            .map(|avance| avance.pct)
            .map_err(|error| error.to_string());
        assert_eq!(pct, Ok(0.0), "un pct nulo se lee como cero");
    }

    /// #117: `get_me` manda `other_organizations` con slug, nombre y rol. El
    /// campo llega a `/api/state` para que la interfaz ofrezca el selector.
    #[test]
    fn me_expone_other_organizations_y_tolera_su_ausencia() {
        let con: Me = serde_json::from_value(serde_json::json!({
            "organization": {"id": "o1", "slug": "oruscribe", "name": "Oruscribe"},
            "other_organizations": [{"slug": "talenai", "name": "TalenAI", "role": "worker"}]
        }))
        .unwrap_or_default();
        let expuesto = serde_json::to_value(&con).unwrap_or(serde_json::Value::Null);
        assert_eq!(expuesto["other_organizations"][0]["slug"], "talenai");
        assert_eq!(expuesto["other_organizations"][0]["role"], "worker");

        let sin: Me = serde_json::from_value(serde_json::json!({})).unwrap_or_default();
        let expuesto = serde_json::to_value(&sin).unwrap_or(serde_json::Value::Null);
        assert!(
            expuesto["other_organizations"].is_null(),
            "sin el campo no se inventa una lista: {expuesto}"
        );
    }
}
