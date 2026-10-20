//! Retained instance state used by stop/drain work, never by an HTTP caller.
use async_trait::async_trait;
use uuid::Uuid;

use crate::{AssignmentView, Error, InstallationScope, OperationClaim, resources::OwnedResource};

/// PostgreSQL produces timestamps and expiry decisions. A retry, restart or
/// rapid re-enable cannot grant an active drain a new deadline.
pub struct DrainBarrier {
    pub generation: i64,
    pub epoch: i64,
    pub started_at_millis: i64,
    pub deadline_at_millis: i64,
    pub completed_at_millis: Option<i64>,
    pub deadline_expired: bool,
}

pub struct RenderingReference {
    pub id: Uuid,
    pub purpose: String,
    pub generation: i64,
}

pub struct RetainedResource {
    pub owned: OwnedResource,
    /// Exact original rendering for read-only recovery of an unbound CREATE.
    /// Missing snapshots never authorize an apply or replacement creation.
    pub original_rendering: Option<RenderingReference>,
}

pub struct LifecyclePlan {
    pub assignment: AssignmentView,
    pub namespace_uid: Option<String>,
    pub namespace: Option<RetainedResource>,
    pub deployment: Option<RetainedResource>,
    pub drain: Option<DrainBarrier>,
}

#[async_trait]
pub trait LifecycleRepository: Send + Sync {
    async fn lifecycle_plan(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
    ) -> Result<LifecyclePlan, Error>;
    // Completion requires verified admission, child-stop and execution-drain
    // observations. There is deliberately no supplied-boolean shortcut.
}
