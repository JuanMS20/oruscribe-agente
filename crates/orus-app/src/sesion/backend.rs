use async_trait::async_trait;
use chrono::{DateTime, Utc};
use orus_org::doorbell::Config as DoorbellConfig;
use orus_org::{
    ActivityEvent, Alert, Decision, Digest, DigestSource, Goal, GoalDetail, GoalInput, JsonObject,
    Me, Mention, MessagesPage, OrgError, Pagina, Period, PeriodInput, Progress, Room,
    Status as TaskStatus, Task, TaskFiles, TaskInput, TeamOverview, ToolSpec,
};

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("no has iniciado sesión")]
    LoggedOut,
    #[error("no se pudo leer la sesión guardada: {0}")]
    StoreLeer(String),
    #[error("no se pudo guardar la sesión renovada: {0}")]
    Guardar(String),
    /// El error crudo del `Store` al importar un token.
    #[error("{0}")]
    GuardarToken(String),
    #[error("cerrar sesión: {0}")]
    Cerrar(String),
    #[error("la sesión se cerró, pero no se pudieron borrar las conversaciones: {0}")]
    Conversaciones(String),
    /// Un 401/403 del servidor. No se reintenta.
    #[error("la sesión no está autorizada")]
    Auth,
    #[error("{0}")]
    Connect(String),
    /// El error crudo del cliente MCP con su variante de transporte. El
    /// adaptador del arnés lo devuelve tal cual para que `mcptools` decida si
    /// la llamada llegó al servidor (#148).
    #[error(transparent)]
    Org(#[from] OrgError),
    #[error("el servidor rechazó renovar la sesión")]
    RefreshRejected,
    #[error("{0}")]
    Tool(String),
    #[error("este cliente no ofrece timbre")]
    NoDoorbell,
    /// Un dato de la ruta que el contrato responde como 400.
    #[error("{0}")]
    Invalido(String),
    /// Una operación que choca con una ejecución del agente; el contrato responde 409.
    #[error("{0}")]
    Ocupado(String),
    /// La organización se quedó sin almacenamiento; el contrato responde 507.
    #[error("la organización se quedó sin espacio para archivos: {0}")]
    SinEspacio(String),
    /// El límite local de llamadas MCP cortó un posible sondeo; el borde responde 429.
    #[error("{0}")]
    RateLimited(String),
}

/// El recorte del cliente MCP que usa la app.
#[async_trait]
pub trait Backend: Send + Sync {
    async fn me(&self) -> Result<Me, SessionError>;
    async fn tasks(&self) -> Result<Vec<Task>, SessionError>;
    async fn tasks_page(&self, _offset: i64) -> Result<Pagina<Task>, SessionError> {
        Ok(Pagina {
            items: self.tasks().await?,
            ..Pagina::default()
        })
    }
    async fn rooms(&self) -> Result<Vec<Room>, SessionError>;
    async fn rooms_page(&self, _offset: i64) -> Result<Pagina<Room>, SessionError> {
        Ok(Pagina {
            items: self.rooms().await?,
            ..Pagina::default()
        })
    }
    async fn messages(
        &self,
        room_id: &str,
        limit: i64,
        offset: i64,
    ) -> Result<MessagesPage, SessionError>;
    async fn send(&self, room_id: &str, body: &str, reply_to: &str) -> Result<(), SessionError>;
    async fn tool_specs(&self) -> Result<Vec<ToolSpec>, SessionError>;
    async fn call_tool(&self, name: &str, args: JsonObject) -> Result<String, SessionError>;
    async fn doorbell(&self) -> Result<DoorbellConfig, SessionError>;

    /// Busca tareas. `status` llega como texto del query y el `Client` lo
    /// parsea: un valor desconocido no filtra.
    ///
    /// El trait no usa `orus_org::Status` para que `orus-api` no dependa de
    /// `orus-org` (D161); el parseo vive en la implementación real.
    async fn search_tasks(
        &self,
        _q: &str,
        _status: &str,
        _limit: i64,
    ) -> Result<Vec<Task>, SessionError> {
        Err(no_soportado("search_tasks"))
    }

    async fn search_tasks_page(
        &self,
        q: &str,
        status: &str,
        limit: i64,
    ) -> Result<Pagina<Task>, SessionError> {
        Ok(Pagina {
            items: self.search_tasks(q, status, limit).await?,
            ..Pagina::default()
        })
    }

    async fn task_files(&self, _task: &str) -> Result<TaskFiles, SessionError> {
        Err(no_soportado("task_files"))
    }

    async fn alerts(&self) -> Result<Vec<Alert>, SessionError> {
        Err(no_soportado("alerts"))
    }

    async fn alerts_page(&self) -> Result<Pagina<Alert>, SessionError> {
        Ok(Pagina {
            items: self.alerts().await?,
            ..Pagina::default()
        })
    }

    async fn mentions(&self) -> Result<Vec<Mention>, SessionError> {
        Err(no_soportado("mentions"))
    }

    async fn mentions_page(&self) -> Result<Pagina<Mention>, SessionError> {
        Ok(Pagina {
            items: self.mentions().await?,
            ..Pagina::default()
        })
    }

    async fn goals(&self) -> Result<Vec<Goal>, SessionError> {
        Err(no_soportado("goals"))
    }

    async fn goal_detail(&self, _id: &str) -> Result<GoalDetail, SessionError> {
        Err(no_soportado("goal_detail"))
    }

    async fn periods(&self) -> Result<Vec<Period>, SessionError> {
        Err(no_soportado("periods"))
    }

    async fn activity(
        &self,
        _since: Option<DateTime<Utc>>,
        _limit: i64,
    ) -> Result<Vec<ActivityEvent>, SessionError> {
        Err(no_soportado("activity"))
    }

    async fn activity_page(
        &self,
        since: Option<DateTime<Utc>>,
        limit: i64,
    ) -> Result<Pagina<ActivityEvent>, SessionError> {
        Ok(Pagina {
            items: self.activity(since, limit).await?,
            ..Pagina::default()
        })
    }

    async fn decisions(&self, _q: &str, _limit: i64) -> Result<Vec<Decision>, SessionError> {
        Err(no_soportado("decisions"))
    }

    async fn team_status(&self) -> Result<Vec<TeamOverview>, SessionError> {
        Err(no_soportado("team_status"))
    }

    async fn create_task(&self, _input: &TaskInput) -> Result<Task, SessionError> {
        Err(no_soportado("create_task"))
    }

    async fn set_task_status(
        &self,
        _task_id: &str,
        _status: TaskStatus,
        _note: &str,
    ) -> Result<Progress, SessionError> {
        Err(no_soportado("set_task_status"))
    }

    async fn comment_update(
        &self,
        _task_id: &str,
        _update_id: &str,
        _body: &str,
    ) -> Result<(), SessionError> {
        Err(no_soportado("comment_update"))
    }

    async fn send_direct(
        &self,
        _to: &str,
        _body: &str,
        _reply_to: &str,
    ) -> Result<(), SessionError> {
        Err(no_soportado("send_direct"))
    }

    async fn mark_room_read(&self, _room_id: &str) -> Result<(), SessionError> {
        Err(no_soportado("mark_room_read"))
    }

    async fn mark_alerts_read(&self, _ids: &[String]) -> Result<(), SessionError> {
        Err(no_soportado("mark_alerts_read"))
    }

    async fn mark_mentions_read(&self, _ids: &[String]) -> Result<(), SessionError> {
        Err(no_soportado("mark_mentions_read"))
    }

    async fn create_goal(&self, _input: GoalInput) -> Result<Goal, SessionError> {
        Err(no_soportado("create_goal"))
    }

    async fn create_period(&self, _input: &PeriodInput) -> Result<Period, SessionError> {
        Err(no_soportado("create_period"))
    }

    async fn propose_digest(
        &self,
        _since: &str,
        _sources: &[DigestSource],
    ) -> Result<Digest, SessionError> {
        Err(no_soportado("propose_digest"))
    }

    async fn apply_digest(
        &self,
        _digest_id: &str,
        _accept: &[String],
    ) -> Result<String, SessionError> {
        Err(no_soportado("apply_digest"))
    }

    async fn send_alert(
        &self,
        _kind: &str,
        _message: &str,
        _team_id: &str,
        _user_id: &str,
    ) -> Result<(), SessionError> {
        Err(no_soportado("send_alert"))
    }

    async fn react(&self, _message_id: &str, _emoji: &str) -> Result<(), SessionError> {
        Err(no_soportado("react"))
    }

    async fn update_context(
        &self,
        _focus: &str,
        _mood: &str,
        _notes: &str,
    ) -> Result<(), SessionError> {
        Err(no_soportado("update_context"))
    }

    /// Sube un archivo a una tarea y devuelve el `update_id`.
    async fn upload_advance(
        &self,
        _task_id: &str,
        _name: &str,
        _mime: &str,
        _data: &[u8],
        _note: &str,
    ) -> Result<String, SessionError> {
        Err(no_soportado("upload_advance"))
    }

    /// Sube un archivo a una sala y publica el mensaje con el adjunto.
    async fn upload_chat_file(
        &self,
        _room_id: &str,
        _name: &str,
        _mime: &str,
        _data: &[u8],
        _body: &str,
    ) -> Result<(), SessionError> {
        Err(no_soportado("upload_chat_file"))
    }

    /// Lectura local del consumo MCP de este cliente (sin llamadas de red).
    fn mcp_quota(&self) -> Option<orus_org::McpQuota> {
        None
    }
}

/// Lo que devuelve un backend que no implementa una capacidad (D146).
fn no_soportado(nombre: &str) -> SessionError {
    SessionError::Tool(format!("{nombre} no soportado por este backend"))
}

/// `OrgError` no trae un código HTTP propio salvo en `UploadStatus`: el
/// 401/403 real del transporte MCP viaja en la cadena de fuentes de
/// `Connect`/`Call`. Solo esas variantes pueden tumbar la sesión; el mensaje
/// de una herramienta que cite «401» ya no la tumba (#95).
pub(super) fn org_error(error: OrgError) -> SessionError {
    if es_auth(&error) {
        return SessionError::Auth;
    }
    let text = error.to_string();
    match error {
        OrgError::Connect(_) => SessionError::Connect(text),
        // Un blip de transporte no llegó al servidor: la variante viaja hasta
        // el adaptador y `mcptools` reembolsa el cupo de la lectura (#148).
        // `NotConnected` es el mismo caso, sin conexión viva.
        OrgError::Call { .. } | OrgError::NotConnected => SessionError::Org(error),
        OrgError::SinEspacio { detalle } => SessionError::SinEspacio(detalle),
        OrgError::RateLimited { .. } => SessionError::RateLimited(text),
        _ => SessionError::Tool(text),
    }
}

#[cfg(test)]
impl SessionError {
    /// El mapeo real de [`org_error`] para los tests de otros módulos: el
    /// módulo `backend` es privado y el adaptador del arnés vive en `agent`.
    pub(crate) fn desde_org(error: OrgError) -> Self {
        org_error(error)
    }
}

/// Auth por código de estado estructurado, nunca por subcadena del mensaje
/// de una herramienta (corregido en #95).
fn es_auth(error: &OrgError) -> bool {
    match error {
        OrgError::UploadStatus { status, .. } => matches!(status, 401 | 403),
        OrgError::ToolContext { source, .. } => es_auth(source),
        OrgError::Connect(_) | OrgError::Call { .. } => cadena_de_auth(error),
        _ => false,
    }
}

/// Recorre la cadena de fuentes de un error de transporte buscando las
/// señales de auth que deja rmcp ante un 401/403 (`AuthRequired`,
/// `InsufficientScope`). Solo se usa sobre `Connect`/`Call`, nunca sobre el
/// mensaje de una herramienta (#95).
fn cadena_de_auth(error: &dyn std::error::Error) -> bool {
    let mut fuente = Some(error);
    while let Some(actual) = fuente {
        let texto = actual.to_string().to_lowercase();
        if texto.contains("auth required")
            || texto.contains("authorization required")
            || texto.contains("insufficient scope")
        {
            return true;
        }
        fuente = actual.source();
    }
    false
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    /// #95: un error de herramienta que cita «401» (un estado de un tercero,
    /// una ruta, un título) no puede tumbar la sesión.
    #[test]
    fn un_error_de_herramienta_con_401_no_tumba_la_sesion() {
        let error = OrgError::Tool {
            tool: "buscar_tareas".to_owned(),
            message: "el servidor de terceros respondió 401 Unauthorized".to_owned(),
        };
        assert!(
            matches!(org_error(error), SessionError::Tool(_)),
            "un 401 citado por una herramienta no es un 401 de la sesión"
        );
    }

    /// El estado estructurado de la subida sí manda: 401/403 son Auth.
    #[test]
    fn upload_status_401_y_403_son_auth() {
        for status in [401, 403] {
            let error = OrgError::UploadStatus {
                status,
                body: "cuerpo".to_owned(),
            };
            assert!(
                matches!(org_error(error), SessionError::Auth),
                "status {status} tiene que ser Auth"
            );
        }
    }

    /// Un estado que no es de auth no se vuelve Auth aunque el cuerpo cite
    /// «forbidden».
    #[test]
    fn upload_status_500_no_es_auth() {
        let error = OrgError::UploadStatus {
            status: 500,
            body: "forbidden".to_owned(),
        };
        assert!(matches!(org_error(error), SessionError::Tool(_)));
    }

    /// El contexto de una herramienta hereda el estado del error envuelto.
    #[test]
    fn tool_context_hereda_el_status() {
        let error = OrgError::ToolContext {
            tool: "upload_task_file".to_owned(),
            source: Box::new(OrgError::UploadStatus {
                status: 403,
                body: String::new(),
            }),
        };
        assert!(matches!(org_error(error), SessionError::Auth));
    }

    /// #148: el transporte caído no se aplana a `Tool`; el adaptador del arnés
    /// lo necesita entero para que `mcptools` reembolse el cupo de la lectura.
    #[test]
    fn sin_conexion_conserva_la_variante() {
        assert!(matches!(
            org_error(OrgError::NotConnected),
            SessionError::Org(OrgError::NotConnected)
        ));
    }

    /// La cadena de fuentes del transporte deja las señales de rmcp; un
    /// «401» suelto en el texto no es una de ellas.
    #[test]
    fn la_cadena_de_auth_solo_mira_las_senales_del_transporte() {
        #[derive(Debug)]
        struct SenalTransporte;
        impl std::fmt::Display for SenalTransporte {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("Auth required")
            }
        }
        impl std::error::Error for SenalTransporte {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&FuenteAuth)
            }
        }
        #[derive(Debug)]
        struct FuenteAuth;
        impl std::fmt::Display for FuenteAuth {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("authorization required: Bearer realm=\"mcp\"")
            }
        }
        impl std::error::Error for FuenteAuth {}

        #[derive(Debug)]
        struct TextoSuelto;
        impl std::fmt::Display for TextoSuelto {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("la tarea 401 no se pudo mover")
            }
        }
        impl std::error::Error for TextoSuelto {}

        assert!(cadena_de_auth(&SenalTransporte));
        assert!(!cadena_de_auth(&TextoSuelto));
    }
}
