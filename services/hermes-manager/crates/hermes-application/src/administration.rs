//! Redacted administration queries. Credentials and native files have no types
//! in this API, and presentation cannot reach infrastructure adapters directly.
use async_trait::async_trait;
use hermes_domain::Identity;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{AssignmentView, Error, InstallationScope};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IdentityView {
    pub id: Uuid,
    pub identity: Identity,
    pub directory_verified: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TemplateView {
    pub id: String,
    pub version: i64,
    pub image: String,
    pub native_port: u16,
    pub native_scheme: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperationView {
    pub id: Uuid,
    pub instance_id: Option<Uuid>,
    pub kind: String,
    pub generation: i64,
    pub state: String,
    pub attempt: i32,
    pub next_attempt_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuditView {
    pub entity_type: Option<String>,
    pub entity_id: Option<String>,
    pub id: Uuid,
    pub actor: Identity,
    pub action: String,
    pub assignment_id: Option<Uuid>,
    pub operation_id: Uuid,
    pub request_id: Uuid,
    pub version: i64,
    pub occurred_at: i64,
}

#[async_trait]
pub trait AdministrationReader: Send + Sync {
    async fn identities(
        &self,
        scope: &InstallationScope,
        after: Option<Uuid>,
        limit: u16,
    ) -> Result<Vec<IdentityView>, Error>;
    async fn templates(&self, scope: &InstallationScope) -> Result<Vec<TemplateView>, Error>;
    async fn instance(&self, scope: &InstallationScope, id: Uuid) -> Result<AssignmentView, Error>;
    async fn operation(&self, scope: &InstallationScope, id: Uuid) -> Result<OperationView, Error>;
    async fn audit(
        &self,
        scope: &InstallationScope,
        after: Option<Uuid>,
        limit: u16,
    ) -> Result<Vec<AuditView>, Error>;
}
