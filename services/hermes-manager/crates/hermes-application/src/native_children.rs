//! Durable native Pod start/stop intents. ReplicaSet labels alone confer no ownership.
use crate::{Error, InstallationScope, OperationClaim, resources::OwnedResource};
use async_trait::async_trait;
use hermes_domain::{resources::valid_uid, validate_name, validate_sha256};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub const START_GATE: &str = "agent-platform.io/native-start";

/// Produced by the trusted Kubernetes driver after checking the complete
/// Pod -> ReplicaSet -> bound Deployment UID chain, never deserialized from HTTP.
#[derive(Clone)]
pub struct NativeChildObservation {
    pub namespace_uid: String,
    pub deployment_uid: String,
    pub replica_set_name: String,
    pub replica_set_uid: String,
    pub pod_name: String,
    pub pod_uid: String,
    pub resource_version: String,
    pub spec_digest: String,
    pub gated: bool,
}
impl NativeChildObservation {
    pub fn validate(&self) -> Result<(), Error> {
        validate_name(&self.replica_set_name)?;
        validate_name(&self.pod_name)?;
        validate_sha256(&self.spec_digest)?;
        if [
            &self.namespace_uid,
            &self.deployment_uid,
            &self.replica_set_uid,
            &self.pod_uid,
            &self.resource_version,
        ]
        .iter()
        .any(|s| !valid_uid(s))
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeChildAction {
    Start,
    Stop,
}
impl NativeChildAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
        }
    }
}

/// Complete public write input, including original UID/RV preconditions.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeChildInput {
    pub installation_id: Uuid,
    pub instance_id: Uuid,
    pub operation_id: Uuid,
    pub deployment_id: Uuid,
    pub fence_key: String,
    pub namespace: String,
    pub namespace_uid: String,
    pub deployment_uid: String,
    pub replica_set_name: String,
    pub replica_set_uid: String,
    pub pod_name: String,
    pub pod_uid: String,
    pub resource_version: String,
    pub spec_digest: String,
    pub generation: i64,
    pub epoch: i64,
    pub action: NativeChildAction,
}
impl NativeChildInput {
    pub fn validate(&self) -> Result<(), Error> {
        if [
            self.installation_id,
            self.instance_id,
            self.operation_id,
            self.deployment_id,
        ]
        .iter()
        .any(Uuid::is_nil)
            || self.generation < 1
            || self.epoch < 1
            || self.namespace
                != hermes_domain::instance_namespace(self.installation_id, self.instance_id)
        {
            return Err(Error::Invalid);
        }
        validate_name(&self.fence_key)?;
        self.observation(true).validate()
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
    pub fn observation(&self, gated: bool) -> NativeChildObservation {
        NativeChildObservation {
            namespace_uid: self.namespace_uid.clone(),
            deployment_uid: self.deployment_uid.clone(),
            replica_set_name: self.replica_set_name.clone(),
            replica_set_uid: self.replica_set_uid.clone(),
            pod_name: self.pod_name.clone(),
            pod_uid: self.pod_uid.clone(),
            resource_version: self.resource_version.clone(),
            spec_digest: self.spec_digest.clone(),
            gated,
        }
    }
}
pub struct PreparedNativeChild {
    pub deployment: OwnedResource,
    pub input: NativeChildInput,
    pub completed: bool,
}
impl PreparedNativeChild {
    pub fn validate(&self, claim: &OperationClaim) -> Result<(), Error> {
        self.input.validate()?;
        let c = &self.deployment.creation;
        if claim.worker.is_nil()
            || claim.fence < 1
            || self.input.operation_id != claim.id
            || self.input.instance_id != claim.instance_id
            || self.input.generation != claim.generation
            || self.input.epoch != claim.epoch
            || self.input.deployment_id != c.id
            || self.input.installation_id != c.installation_id
            || self.deployment.state != "Bound"
            || self.deployment.uid.as_ref() != Some(&self.input.deployment_uid)
            || c.spec.kind != hermes_domain::resources::ResourceKind::Deployment
            || c.spec.name != "hermes"
            || c.spec.namespace.as_ref() != Some(&self.input.namespace)
            || c.spec.namespace_uid.as_ref() != Some(&self.input.namespace_uid)
            || !(1..=claim.epoch).contains(&c.epoch)
            || !(1..=claim.generation).contains(&c.generation)
        {
            return Err(Error::OwnershipConflict);
        }
        Ok(())
    }
}
/// Driver proves gate removal or exact UID absence, including a pending Start
/// whose Pod vanished. Absence never supplies readiness. A delete acceptance
/// by itself must never produce this result.
pub struct NativeChildResult {
    pub pod_uid: String,
    pub input_digest: String,
    pub absent: bool,
}
#[async_trait]
pub trait NativeChildRepository: Send + Sync {
    /// A bounded batch of original incomplete inputs for this operation/action.
    /// Reclaiming never changes original RV, owner identity or fence reference.
    async fn load_pending_native_children(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
        action: NativeChildAction,
    ) -> Result<Vec<PreparedNativeChild>, Error>;
    async fn prepare_native_child(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
        action: NativeChildAction,
        fence_key: &str,
        observation: NativeChildObservation,
    ) -> Result<PreparedNativeChild, Error>;
    async fn complete_native_child(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
        prepared: &NativeChildInput,
        result: NativeChildResult,
    ) -> Result<(), Error>;
    /// First batch of at most 32 retained known children, including older
    /// generations. Verified absence removes a UID from future batches, while
    /// its immutable provenance remains. This is not an exhaustive inventory.
    async fn native_children(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
    ) -> Result<Vec<NativeChildObservation>, Error>;
}
