//! Durable periodic observation work, independent of watch/notification delivery.
use async_trait::async_trait;
use hermes_domain::Access;
use serde::{Deserialize, Serialize};

use crate::{Error, InstallationScope};

/// PostgreSQL time drives cadence. Process restarts never reset due work.
pub const RESYNC_INTERVAL_SECONDS: u16 = 30;

/// Snapshot committed with the new operation; retry never rereads mutable input
/// or reopens an administrator's completed command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResyncInput {
    pub access: Access,
    pub template_id: String,
    pub template_version: i64,
    pub assignment_version: i64,
    pub generation: i64,
    pub epoch: i64,
    /// Disabled retained instances also observe uncertain namespace creation
    /// whose result has not yet supplied an authenticated UID binding.
    pub namespace_uid: Option<String>,
}

#[async_trait]
pub trait PeriodicResync: Send + Sync {
    /// Atomically reserve at most 64 due completed instances, enqueue fresh
    /// fenced operations, and advance their durable due times. Never publishes
    /// readiness, retries manual-review failures, or changes desired generation.
    async fn enqueue_due(&self, scope: &InstallationScope, limit: u16) -> Result<u16, Error>;
}
