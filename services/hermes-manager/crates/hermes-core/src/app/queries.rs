//! The administration read side. Views serialize exactly as the
//! administration contract publishes them; contract tests check every view
//! against its schema. Reads take no locks and change nothing.

use async_trait::async_trait;
use serde::Serialize;
use uuid::Uuid;

use super::catalog::Definition;
use super::error::{AppError, StoreError};
use crate::domain::assignments::{Access, AssignmentSpec};
use crate::domain::catalog::{CatalogKind, VersionRef, VersionState};
use crate::domain::identity::Identity;
use crate::domain::secrets::SecretKind;

pub const DEFAULT_LIMIT: u16 = 50;
pub const MAX_LIMIT: u16 = 100;

/// Keyset pagination: an opaque cursor from the previous page and a bound.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageRequest {
    pub after: Option<String>,
    pub limit: u16,
}

impl PageRequest {
    pub fn new(after: Option<String>, limit: Option<u16>) -> Result<Self, AppError> {
        let limit = limit.unwrap_or(DEFAULT_LIMIT);
        let after_ok = after
            .as_ref()
            .is_none_or(|a| !a.is_empty() && a.len() <= 256);
        if !(1..=MAX_LIMIT).contains(&limit) || !after_ok {
            return Err(AppError::invalid());
        }
        Ok(Self { after, limit })
    }
}

impl Default for PageRequest {
    fn default() -> Self {
        Self {
            after: None,
            limit: DEFAULT_LIMIT,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
}

/// A bounded search string; empty means no filter.
pub fn search(q: Option<String>) -> Result<Option<String>, AppError> {
    match q {
        Some(q) if q.len() > 128 || q.chars().any(char::is_control) => Err(AppError::invalid()),
        Some(q) if q.trim().is_empty() => Ok(None),
        other => Ok(other),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IdentitySummary {
    pub id: Uuid,
    pub identity: Identity,
    pub display_name: Option<String>,
    pub email: Option<String>,
    pub verified: bool,
    pub assignment_id: Option<Uuid>,
    pub administrator: bool,
}

pub use crate::domain::instances::Phase;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Condition {
    #[serde(rename = "type")]
    pub kind: String,
    pub status: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceView {
    pub id: Uuid,
    pub assignment_id: Uuid,
    pub namespace: String,
    pub desired_generation: i64,
    pub observed_generation: i64,
    pub execution_generation: i64,
    pub phase: Phase,
    pub blocked_reason: Option<String>,
    pub conditions: Vec<Condition>,
    pub observed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssignmentSummary {
    pub id: Uuid,
    pub identity: Identity,
    pub access: Access,
    pub template: VersionRef,
    pub version: i64,
    pub instance_id: Uuid,
    pub phase: Phase,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssignmentView {
    pub id: Uuid,
    pub identity_id: Uuid,
    pub identity: Identity,
    pub spec: AssignmentSpec,
    pub version: i64,
    pub instance: InstanceView,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AssignmentFilter {
    pub q: Option<String>,
    pub access: Option<Access>,
    pub template_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogSummary {
    pub id: String,
    pub versions: i64,
    pub latest_published: Option<i64>,
    pub draft: Option<i64>,
    pub assignments: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionSummary {
    pub id: String,
    pub version: i64,
    pub state: VersionState,
    pub revision: i64,
    pub updated_at: String,
    pub assignments: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionView<D> {
    pub id: String,
    pub version: i64,
    pub state: VersionState,
    pub revision: i64,
    pub updated_at: String,
    pub assignments: i64,
    pub definition: D,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VolumeUse {
    pub assignment_id: Uuid,
    pub identity: Identity,
    pub name: String,
    pub source: String,
    pub path: Option<String>,
    pub size_gib: Option<u32>,
    pub read_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretSummary {
    pub name: String,
    pub kind: SecretKind,
    pub version: i64,
    pub updated_at: String,
    pub references: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdministratorView {
    pub identity_id: Uuid,
    pub identity: Identity,
    pub active: bool,
    pub can_manage_administrators: bool,
    pub version: i64,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditActor {
    pub kind: String,
    pub identity: Option<Identity>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AuditTargetView {
    pub kind: String,
    pub id: String,
}

/// `change` is the redacted JSON a command recorded. It is opaque here, like
/// `adminConfig`, and never holds secret values.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditView {
    pub id: Uuid,
    pub occurred_at: String,
    pub actor: AuditActor,
    pub action: String,
    pub target: AuditTargetView,
    pub request_id: Uuid,
    pub change: serde_json::Value,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuditFilter {
    pub target_kind: Option<String>,
    pub target_id: Option<String>,
}

#[async_trait]
pub trait AdminQueries: Send + Sync {
    async fn identities(
        &self,
        q: Option<&str>,
        page: &PageRequest,
    ) -> Result<Page<IdentitySummary>, StoreError>;
    async fn identity(&self, id: Uuid) -> Result<Option<IdentitySummary>, StoreError>;
    /// Signed-in identities without a grant.
    async fn administrator_candidates(
        &self,
        q: Option<&str>,
        page: &PageRequest,
    ) -> Result<Page<IdentitySummary>, StoreError>;
    async fn assignments(
        &self,
        filter: &AssignmentFilter,
        page: &PageRequest,
    ) -> Result<Page<AssignmentSummary>, StoreError>;
    async fn assignment(&self, id: Uuid) -> Result<Option<AssignmentView>, StoreError>;
    async fn instance(&self, id: Uuid) -> Result<Option<InstanceView>, StoreError>;
    async fn catalog(
        &self,
        kind: CatalogKind,
        q: Option<&str>,
        page: &PageRequest,
    ) -> Result<Page<CatalogSummary>, StoreError>;
    async fn catalog_versions(
        &self,
        kind: CatalogKind,
        id: &str,
        page: &PageRequest,
    ) -> Result<Page<VersionSummary>, StoreError>;
    async fn catalog_version<D: Definition>(
        &self,
        id: &str,
        version: i64,
    ) -> Result<Option<VersionView<D>>, StoreError>;
    async fn volumes(
        &self,
        source: Option<&str>,
        page: &PageRequest,
    ) -> Result<Page<VolumeUse>, StoreError>;
    async fn secrets(&self, page: &PageRequest) -> Result<Page<SecretSummary>, StoreError>;
    async fn administrators(
        &self,
        page: &PageRequest,
    ) -> Result<Page<AdministratorView>, StoreError>;
    async fn administrator(&self, identity: Uuid) -> Result<Option<AdministratorView>, StoreError>;
    async fn audit(
        &self,
        filter: &AuditFilter,
        page: &PageRequest,
    ) -> Result<Page<AuditView>, StoreError>;
    async fn audit_event(&self, id: Uuid) -> Result<Option<AuditView>, StoreError>;
    /// The instance's environments, newest last; `None` for an unknown instance.
    async fn environments(
        &self,
        instance: Uuid,
    ) -> Result<Option<Vec<EnvironmentAdminView>>, StoreError>;
    /// The controller's latest orphan report.
    async fn orphans(&self) -> Result<OrphanReport, StoreError>;
}

/// One terminal environment as administrators see it: no keys, addresses or
/// invocation paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentAdminView {
    pub id: Uuid,
    pub backend: crate::domain::catalog::VersionRef,
    pub mode: crate::domain::assignments::LifetimeMode,
    /// The session of a `per_session` environment.
    pub session: Option<String>,
    pub generation: i64,
    pub state: crate::domain::execution::EnvironmentState,
    /// Why its worker failed, when it did.
    pub failure: Option<&'static str>,
    pub leases: i64,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EnvironmentList {
    pub items: Vec<EnvironmentAdminView>,
}

/// An owned cluster object without a registry record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrphanView {
    pub kind: &'static str,
    pub name: String,
    pub uid: String,
    pub first_seen_at: String,
    pub last_seen_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrphanReport {
    /// When the controller last completed a scan.
    pub scanned_at: Option<String>,
    pub items: Vec<OrphanView>,
}
