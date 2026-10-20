//! Typed Namespace admission changes, separate from immutable creation intent.
use crate::{
    Action, Error, InstallationScope, JournalStep, OperationClaim, StepIntent, StepResult,
    resources::{OwnedResource, ResourceVersion},
};
use async_trait::async_trait;
use hermes_domain::{
    Access, instance_namespace,
    resources::{ResourceKind, valid_uid},
    validate_name,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub const ACCESS: &str = "agent-platform.io/access";
pub const ADMISSION: &str = "agent-platform.io/admission";
pub const GENERATION: &str = "agent-platform.io/fence-generation";
pub const EPOCH: &str = "agent-platform.io/fence-epoch";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Admission {
    Closed,
    Open,
}
impl Admission {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Closed => "closed",
            Self::Open => "open",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NamespaceFenceState {
    pub access: Access,
    pub admission: Admission,
    pub generation: i64,
    pub epoch: i64,
}
impl NamespaceFenceState {
    pub fn validate(&self) -> Result<(), Error> {
        if self.generation < 1
            || self.epoch < 1
            || (self.admission == Admission::Open && self.access != Access::Enabled)
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }
    pub fn action(&self) -> Action {
        match self.admission {
            Admission::Closed => Action::FenceClose,
            Admission::Open => Action::FenceOpen,
        }
    }
}

/// Complete immutable public input. Worker leases/fences are stored separately:
/// reclaiming work must not rewrite the original external write preconditions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NamespaceFenceInput {
    pub installation_id: Uuid,
    pub instance_id: Uuid,
    pub operation_id: Uuid,
    pub namespace: String,
    pub namespace_uid: String,
    pub resource_version: String,
    pub state: NamespaceFenceState,
}
impl NamespaceFenceInput {
    pub fn validate(&self) -> Result<(), Error> {
        self.state.validate()?;
        if self.installation_id.is_nil()
            || self.instance_id.is_nil()
            || self.operation_id.is_nil()
            || self.namespace != instance_namespace(self.installation_id, self.instance_id)
            || !valid_uid(&self.namespace_uid)
            || !valid_uid(&self.resource_version)
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }
    pub fn digest(&self) -> Result<String, Error> {
        self.validate()?;
        Ok(
            Sha256::digest(serde_json::to_vec(self).map_err(|_| Error::Invalid)?)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect(),
        )
    }
    pub fn intent(&self, creation_id: Uuid) -> Result<StepIntent, Error> {
        Ok(StepIntent {
            action: self.state.action(),
            creation_id,
            kind: "Namespace".into(),
            namespace: None,
            name: self.namespace.clone(),
            namespace_uid: None,
            expected_uid: Some(self.namespace_uid.clone()),
            expected_resource_version: Some(self.resource_version.clone()),
            manifest_digest: self.digest()?,
            encrypted_material_ref: None,
        })
    }
}

pub struct PreparedNamespaceFence {
    pub key: String,
    pub namespace: OwnedResource,
    pub input: NamespaceFenceInput,
    pub step: JournalStep,
}
impl PreparedNamespaceFence {
    pub fn validate(&self, claim: &OperationClaim) -> Result<(), Error> {
        validate_name(&self.key)?;
        self.input.validate()?;
        self.step.intent.validate()?;
        let creation = &self.namespace.creation;
        creation.spec.validate()?;
        if claim.id != self.input.operation_id
            || claim.instance_id != self.input.instance_id
            || claim.epoch != self.input.state.epoch
            || claim.generation != self.input.state.generation
            || claim.worker.is_nil()
            || claim.fence < 1
            || creation.installation_id != self.input.installation_id
            || creation.instance_id != self.input.instance_id
            || creation.spec.kind != ResourceKind::Namespace
            || creation.spec.name != self.input.namespace
            || !(1..=claim.epoch).contains(&creation.epoch)
            || !(1..=claim.generation).contains(&creation.generation)
            || self.namespace.state != "Bound"
            || self.namespace.uid.as_ref() != Some(&self.input.namespace_uid)
            || self.step.intent != self.input.intent(creation.id)?
        {
            return Err(Error::OwnershipConflict);
        }
        match &self.step.result {
            StepResult::Unknown => {}
            StepResult::Verified { uid, .. } if uid == &self.input.namespace_uid => {}
            _ => return Err(Error::OwnershipConflict),
        }
        match (self.input.state.admission, claim.kind.as_str()) {
            (Admission::Open, "commission" | "resync")
                if self.input.state.access == Access::Enabled => {}
            (Admission::Closed, "commission" | "retry" | "suspend" | "revoke" | "resync") => {}
            _ => return Err(Error::Forbidden),
        }
        Ok(())
    }
}

/// A trusted driver observed these exact fence labels on the bound Namespace.
/// This is never native/backend readiness or permission to publish a route.
pub struct NamespaceFenceObservation {
    pub uid: String,
    pub resource_version: String,
    pub input_digest: String,
    pub state: NamespaceFenceState,
}

#[async_trait]
pub trait NamespaceFenceRepository: Send + Sync {
    /// Reclaim at most 32 incomplete inputs for this operation and admission
    /// state. Only the worker fence changes; original write inputs never do.
    async fn load_pending_namespace_fences(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
        admission: Admission,
    ) -> Result<Vec<PreparedNamespaceFence>, Error>;
    /// A repeated step key retains its ORIGINAL resource-version precondition.
    /// A conflicting write retry must use a new key, never rewrite old intent.
    async fn prepare_namespace_fence(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
        key: &str,
        admission: Admission,
        version: ResourceVersion,
    ) -> Result<PreparedNamespaceFence, Error>;
    async fn complete_namespace_fence(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
        key: &str,
        observation: NamespaceFenceObservation,
    ) -> Result<(), Error>;
}
