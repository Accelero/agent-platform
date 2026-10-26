//! Administration API (`openapi.yaml`) request and response bodies that are
//! not query views.

use hermes_core::app::assignments::RolloutMember;
use hermes_core::domain::assignments::AssignmentSpec;
use hermes_core::domain::catalog::VersionRef;
use hermes_core::domain::identity::Identity;
use hermes_core::domain::secrets::SecretKind;
use hermes_core::domain::storage::{DataIdentity, StorageSource};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{ErrorBody, ProblemBody};

/// `GET /api/v1/session`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub actor: Identity,
    pub permitted_actions: Vec<&'static str>,
    /// Present for browser sessions; `null` for bearer tokens.
    pub csrf_token: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateAssignment {
    pub identity: Identity,
    pub spec: AssignmentSpec,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Rollout {
    pub template: VersionRef,
    pub items: Vec<RolloutMember>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RolloutResult<R> {
    pub results: Vec<RolloutItem<R>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RolloutItem<R> {
    pub assignment_id: Uuid,
    pub outcome: &'static str,
    pub receipt: Option<R>,
    pub error: Option<ErrorBody>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    pub valid: bool,
    pub problems: Vec<ProblemBody>,
    pub restarts_hermes: bool,
    pub recreates_terminals: bool,
}

/// A catalog draft body: `{"definition": …}`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Draft<D> {
    pub definition: D,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Validation {
    pub valid: bool,
    pub problems: Vec<ProblemBody>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageSourceList {
    pub items: Vec<StorageSource>,
    pub data_identity: DataIdentity,
}

/// `PUT /api/v1/secrets/{name}`. The value is write-only and never echoed.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretWrite {
    pub kind: SecretKind,
    pub value: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GrantRequest {
    pub identity: Identity,
    pub can_manage_administrators: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GrantChange {
    pub active: bool,
    pub can_manage_administrators: bool,
}

/// `GET /api/v1/directory-users`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryPage<U> {
    pub enabled: bool,
    pub items: Vec<U>,
    pub next_cursor: Option<String>,
}
