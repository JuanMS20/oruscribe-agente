use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use orus_auth::Token;
use orus_org::Client;

use super::backend::{Backend, SessionError, org_error};
use crate::sync::lock;

/// El cliente OAuth usa 30 s (`org.go:58`); los 45 s de D36 son del MCP.
const REFRESH_TIMEOUT: Duration = Duration::from_secs(30);

/// Abre el cliente real. Es la costura de `a.open` (`app.go:787`).
#[async_trait]
pub trait Connector: Send + Sync {
    async fn open(
        &self,
        endpoint: &str,
        token: &str,
        org_slug: &str,
    ) -> Result<Arc<dyn Backend>, SessionError>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct OrgConnector;

#[async_trait]
impl Connector for OrgConnector {
    async fn open(
        &self,
        endpoint: &str,
        token: &str,
        org_slug: &str,
    ) -> Result<Arc<dyn Backend>, SessionError> {
        Client::open(endpoint, token, org_slug)
            .await
            .map(|client| Arc::new(client) as Arc<dyn Backend>)
            .map_err(org_error)
    }
}

#[async_trait]
pub trait Refresh: Send + Sync {
    async fn refresh(&self, token: &Token) -> Result<Token, SessionError>;

    fn set_client_id(&self, _client_id: &str) {}
}

/// Cliente OAuth real: descubre el servidor y cambia el refresh token.
pub struct OrgRefresh {
    client_id: Arc<Mutex<String>>,
    http: reqwest::Client,
}

impl OrgRefresh {
    pub fn new(client_id: impl Into<String>) -> Result<Self, SessionError> {
        let http = reqwest::Client::builder()
            .timeout(REFRESH_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| SessionError::Connect(error.to_string()))?;
        Ok(Self {
            client_id: Arc::new(Mutex::new(client_id.into())),
            http,
        })
    }
}

#[async_trait]
impl Refresh for OrgRefresh {
    fn set_client_id(&self, client_id: &str) {
        *lock(&self.client_id) = client_id.to_owned();
    }

    async fn refresh(&self, token: &Token) -> Result<Token, SessionError> {
        let metadata = orus_auth::discover(&self.http, orus_auth::ISSUER)
            .await
            .map_err(refresh_error)?;
        let config = orus_auth::Config {
            metadata,
            client_id: lock(&self.client_id).clone(),
            ..orus_auth::Config::default()
        };
        orus_auth::refresh(&self.http, config, &token.refresh_token)
            .await
            .map_err(refresh_error)
    }
}

fn refresh_error(error: orus_auth::Error) -> SessionError {
    match error {
        orus_auth::Error::RefreshRejected(_) => SessionError::RefreshRejected,
        other => SessionError::Connect(other.to_string()),
    }
}
