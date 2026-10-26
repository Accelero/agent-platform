//! Read-only lookup of people in the identity provider. A result is a
//! candidate for an assignment or grant; it never confers anything.

use async_trait::async_trait;
use serde::Serialize;

use super::error::AppError;
use crate::domain::identity::Identity;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryUser {
    pub identity: Identity,
    pub username: Option<String>,
    pub display_name: Option<String>,
    pub email: Option<String>,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryResults {
    pub items: Vec<DirectoryUser>,
    pub next_cursor: Option<String>,
}

#[async_trait]
pub trait IdentityDirectory: Send + Sync {
    /// Users matching `query`, in the provider's order. An unreachable or
    /// refusing provider is `Unavailable`, never an empty result.
    async fn search(
        &self,
        query: &str,
        after: Option<&str>,
        limit: u16,
    ) -> Result<DirectoryResults, AppError>;
}
