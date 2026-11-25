use async_trait::async_trait;
use chrono::{DateTime, Utc};
use orus_org::doorbell::Config as DoorbellConfig;
use orus_org::{
    ActivityEvent, Alert, Client, Decision, Digest, DigestSource, Goal, GoalDetail, GoalInput,
    JsonObject, Me, Mention, MessagesPage, Pagina, Period, PeriodInput, Progress, Room,
    Status as TaskStatus, Task, TaskFiles, TaskInput, TeamOverview, ToolSpec,
};

use super::backend::{Backend, SessionError, org_error};

#[async_trait]
impl Backend for Client {
    async fn me(&self) -> Result<Me, SessionError> {
        Client::me(self).await.map_err(org_error)
    }

    fn mcp_quota(&self) -> Option<orus_org::McpQuota> {
        Some(Client::mcp_quota(self))
    }

    async fn tasks(&self) -> Result<Vec<Task>, SessionError> {
        Ok(Backend::tasks_page(self, 0).await?.items)
    }

    async fn tasks_page(&self, offset: i64) -> Result<Pagina<Task>, SessionError> {
        Client::tasks_page(self, offset).await.map_err(org_error)
    }

    async fn rooms(&self) -> Result<Vec<Room>, SessionError> {
        Ok(Backend::rooms_page(self, 0).await?.items)
    }

    async fn rooms_page(&self, offset: i64) -> Result<Pagina<Room>, SessionError> {
        Client::rooms_page(self, offset).await.map_err(org_error)
    }

    async fn messages(
        &self,
        room_id: &str,
        limit: i64,
        offset: i64,
    ) -> Result<MessagesPage, SessionError> {
        Client::messages(self, room_id, limit, offset)
            .await
            .map_err(org_error)
    }

    async fn send(&self, room_id: &str, body: &str, reply_to: &str) -> Result<(), SessionError> {
        Client::send(self, room_id, body, reply_to)
            .await
            .map_err(org_error)
    }

    async fn search_tasks(
        &self,
        q: &str,
        status: &str,
        limit: i64,
    ) -> Result<Vec<Task>, SessionError> {
        Ok(self.search_tasks_page(q, status, limit).await?.items)
    }

    async fn search_tasks_page(
        &self,
        q: &str,
        status: &str,
        limit: i64,
    ) -> Result<Pagina<Task>, SessionError> {
        let status = orus_org::parse_status(status).unwrap_or_default();
        Client::search_tasks(self, q, status, limit)
            .await
            .map_err(org_error)
    }

    async fn task_files(&self, task: &str) -> Result<TaskFiles, SessionError> {
        Client::task_files(self, task).await.map_err(org_error)
    }

    async fn alerts(&self) -> Result<Vec<Alert>, SessionError> {
        Ok(self.alerts_page().await?.items)
    }

    async fn alerts_page(&self) -> Result<Pagina<Alert>, SessionError> {
        Client::alerts(self).await.map_err(org_error)
    }

    async fn mentions(&self) -> Result<Vec<Mention>, SessionError> {
        Ok(self.mentions_page().await?.items)
    }

    async fn mentions_page(&self) -> Result<Pagina<Mention>, SessionError> {
        Client::mentions(self).await.map_err(org_error)
    }

    async fn goals(&self) -> Result<Vec<Goal>, SessionError> {
        Client::goals(self).await.map_err(org_error)
    }

    async fn goal_detail(&self, id: &str) -> Result<GoalDetail, SessionError> {
        Client::goal_detail(self, id).await.map_err(org_error)
    }

    async fn periods(&self) -> Result<Vec<Period>, SessionError> {
        Client::periods(self).await.map_err(org_error)
    }

    async fn activity(
        &self,
        since: Option<DateTime<Utc>>,
        limit: i64,
    ) -> Result<Vec<ActivityEvent>, SessionError> {
        Ok(self.activity_page(since, limit).await?.items)
    }

    async fn activity_page(
        &self,
        since: Option<DateTime<Utc>>,
        limit: i64,
    ) -> Result<Pagina<ActivityEvent>, SessionError> {
        Client::activity(self, since, limit)
            .await
            .map_err(org_error)
    }

    async fn decisions(&self, q: &str, limit: i64) -> Result<Vec<Decision>, SessionError> {
        Client::decisions(self, q, limit).await.map_err(org_error)
    }

    async fn team_status(&self) -> Result<Vec<TeamOverview>, SessionError> {
        Client::team_status(self).await.map_err(org_error)
    }

    async fn create_task(&self, input: &TaskInput) -> Result<Task, SessionError> {
        Client::create_task(self, input).await.map_err(org_error)
    }

    async fn set_task_status(
        &self,
        task_id: &str,
        status: TaskStatus,
        note: &str,
    ) -> Result<Progress, SessionError> {
        Client::set_task_status(self, task_id, status, note)
            .await
            .map_err(org_error)
    }

    async fn comment_update(
        &self,
        task_id: &str,
        update_id: &str,
        body: &str,
    ) -> Result<(), SessionError> {
        Client::comment_update(self, task_id, update_id, body)
            .await
            .map_err(org_error)
    }

    async fn send_direct(&self, to: &str, body: &str, reply_to: &str) -> Result<(), SessionError> {
        Client::send_direct(self, to, body, reply_to)
            .await
            .map_err(org_error)
    }

    async fn mark_room_read(&self, room_id: &str) -> Result<(), SessionError> {
        Client::mark_room_read(self, room_id)
            .await
            .map_err(org_error)
    }

    async fn mark_alerts_read(&self, ids: &[String]) -> Result<(), SessionError> {
        Client::mark_alerts_read(self, ids).await.map_err(org_error)
    }

    async fn mark_mentions_read(&self, ids: &[String]) -> Result<(), SessionError> {
        Client::mark_mentions_read(self, ids)
            .await
            .map_err(org_error)
    }

    async fn create_goal(&self, input: GoalInput) -> Result<Goal, SessionError> {
        Client::create_goal(self, input).await.map_err(org_error)
    }

    async fn create_period(&self, input: &PeriodInput) -> Result<Period, SessionError> {
        Client::create_period(self, input).await.map_err(org_error)
    }

    async fn propose_digest(
        &self,
        since: &str,
        sources: &[DigestSource],
    ) -> Result<Digest, SessionError> {
        Client::propose_digest(self, since, sources)
            .await
            .map_err(org_error)
    }

    async fn apply_digest(
        &self,
        digest_id: &str,
        accept: &[String],
    ) -> Result<String, SessionError> {
        Client::apply_digest(self, digest_id, accept)
            .await
            .map_err(org_error)
    }

    async fn send_alert(
        &self,
        kind: &str,
        message: &str,
        team_id: &str,
        user_id: &str,
    ) -> Result<(), SessionError> {
        Client::send_alert(self, kind, message, team_id, user_id)
            .await
            .map_err(org_error)
    }

    async fn react(&self, message_id: &str, emoji: &str) -> Result<(), SessionError> {
        Client::react(self, message_id, emoji)
            .await
            .map_err(org_error)
    }

    async fn update_context(
        &self,
        focus: &str,
        mood: &str,
        notes: &str,
    ) -> Result<(), SessionError> {
        Client::update_context(self, focus, mood, notes)
            .await
            .map_err(org_error)
    }

    async fn upload_advance(
        &self,
        task_id: &str,
        name: &str,
        mime: &str,
        data: &[u8],
        note: &str,
    ) -> Result<String, SessionError> {
        Client::upload_advance(self, task_id, name, mime, data, note)
            .await
            .map_err(org_error)
    }

    async fn upload_chat_file(
        &self,
        room_id: &str,
        name: &str,
        mime: &str,
        data: &[u8],
        body: &str,
    ) -> Result<(), SessionError> {
        Client::upload_chat_file(self, room_id, name, mime, data, body)
            .await
            .map_err(org_error)
    }

    async fn tool_specs(&self) -> Result<Vec<ToolSpec>, SessionError> {
        Client::tool_specs(self).await.map_err(org_error)
    }

    async fn call_tool(&self, name: &str, args: JsonObject) -> Result<String, SessionError> {
        Client::call(self, name, args).await.map_err(org_error)
    }

    async fn doorbell(&self) -> Result<DoorbellConfig, SessionError> {
        Client::call_json(self, "get_my_doorbell", JsonObject::new())
            .await
            .map_err(org_error)
    }
}
