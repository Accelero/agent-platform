//! Read-only external identity lookup. Directory labels never grant authority.
use crate::Error;
use async_trait::async_trait;
use hermes_domain::Identity;
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DirectoryUser {
    pub identity: Identity,
    pub username: Option<String>,
    pub display_name: Option<String>,
    pub email: Option<String>,
    pub enabled: bool,
    pub directory_verified: bool,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryPage {
    pub enabled: bool,
    pub items: Vec<DirectoryUser>,
    pub next_cursor: Option<String>,
}
#[async_trait]
pub trait IdentityDirectory: Send + Sync {
    async fn search(
        &self,
        query: &str,
        after: Option<&str>,
        limit: u16,
    ) -> Result<DirectoryPage, Error>;
}
