//! Retained native scale-down: a CAS-only action, never deletion or drain proof.
use async_trait::async_trait;
use hermes_domain::{
    Access, instance_namespace,
    resources::{ResourceKind, valid_uid},
    validate_name, validate_sha256,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    Action, Error, InstallationScope, JournalStep, OperationClaim, StepIntent, StepResult,
    namespace_fence::{Admission, NamespaceFenceState},
    resources::OwnedResource,
};

/// Read-only driver evidence about the bound Deployment. The digest covers the
/// exact live Pod template, selector and rollout strategy, excluding replicas.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeStopInspection {
    pub uid: String,
    pub resource_version: String,
    pub preserved_spec_digest: String,
}
impl NativeStopInspection {
    pub fn validate(&self) -> Result<(), Error> {
        validate_sha256(&self.preserved_spec_digest)?;
        if !valid_uid(&self.uid) || !valid_uid(&self.resource_version) {
            return Err(Error::Invalid);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeStopInput {
    pub installation_id: Uuid,
    pub instance_id: Uuid,
    pub operation_id: Uuid,
    pub namespace: String,
    pub namespace_uid: String,
    pub deployment_uid: String,
    pub resource_version: String,
    pub preserved_spec_digest: String,
    pub closed_fence_key: String,
    pub closed_fence_digest: String,
    pub fence_state: NamespaceFenceState,
    pub drain_generation: i64,
    pub drain_epoch: i64,
}
impl NativeStopInput {
    pub fn validate(&self) -> Result<(), Error> {
        self.fence_state.validate()?;
        validate_name(&self.closed_fence_key)?;
        validate_sha256(&self.closed_fence_digest)?;
        NativeStopInspection {
            uid: self.deployment_uid.clone(),
            resource_version: self.resource_version.clone(),
            preserved_spec_digest: self.preserved_spec_digest.clone(),
        }
        .validate()?;
        if self.installation_id.is_nil()
            || self.instance_id.is_nil()
            || self.operation_id.is_nil()
            || self.namespace != instance_namespace(self.installation_id, self.instance_id)
            || !valid_uid(&self.namespace_uid)
            || self.fence_state.admission != Admission::Closed
            || !(1..=self.fence_state.generation).contains(&self.drain_generation)
            || !(1..=self.fence_state.epoch).contains(&self.drain_epoch)
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
            action: Action::NativeStop,
            creation_id,
            kind: "Deployment".into(),
            namespace: Some(self.namespace.clone()),
            namespace_uid: Some(self.namespace_uid.clone()),
            name: "hermes".into(),
            expected_uid: Some(self.deployment_uid.clone()),
            expected_resource_version: Some(self.resource_version.clone()),
            manifest_digest: self.digest()?,
            encrypted_material_ref: None,
        })
    }
}

pub struct PreparedNativeStop {
    pub key: String,
    pub namespace: OwnedResource,
    pub deployment: OwnedResource,
    pub input: NativeStopInput,
    pub step: JournalStep,
}
impl PreparedNativeStop {
    pub fn validate(&self, claim: &OperationClaim) -> Result<(), Error> {
        self.input.validate()?;
        validate_name(&self.key)?;
        self.step.intent.validate()?;
        let input = &self.input;
        for owned in [&self.namespace, &self.deployment] {
            owned.creation.spec.validate()?;
            if owned.state != "Bound"
                || owned.creation.installation_id != input.installation_id
                || owned.creation.instance_id != input.instance_id
                || !(1..=claim.epoch).contains(&owned.creation.epoch)
                || !(1..=claim.generation).contains(&owned.creation.generation)
            {
                return Err(Error::OwnershipConflict);
            }
        }
        let spec = &self.deployment.creation.spec;
        if self.namespace.creation.spec.kind != ResourceKind::Namespace
            || self.namespace.creation.spec.name != input.namespace
            || self.namespace.uid.as_ref() != Some(&input.namespace_uid)
            || spec.kind != ResourceKind::Deployment
            || spec.name != "hermes"
            || spec.namespace.as_ref() != Some(&input.namespace)
            || spec.namespace_uid.as_ref() != Some(&input.namespace_uid)
            || self.deployment.uid.as_ref() != Some(&input.deployment_uid)
            || claim.id != input.operation_id
            || claim.instance_id != input.instance_id
            || claim.epoch != input.fence_state.epoch
            || claim.generation != input.fence_state.generation
            || claim.worker.is_nil()
            || claim.fence < 1
            || self.step.intent != input.intent(self.deployment.creation.id)?
        {
            return Err(Error::OwnershipConflict);
        }
        match (input.fence_state.access, claim.kind.as_str()) {
            (Access::Enabled, "commission" | "resync")
            | (Access::Suspended, "suspend" | "resync")
            | (Access::Revoked, "revoke" | "resync") => {}
            _ => return Err(Error::Forbidden),
        }
        match &self.step.result {
            StepResult::Unknown => Ok(()),
            StepResult::Verified { uid, .. } if uid == &input.deployment_uid => Ok(()),
            _ => Err(Error::OwnershipConflict),
        }
    }
}

pub struct NativeStopObservation {
    pub uid: String,
    pub resource_version: String,
    pub input_digest: String,
    pub preserved_spec_digest: String,
}

#[async_trait]
pub trait NativeStopRepository: Send + Sync {
    /// Reclaim at most 64 incomplete inputs for THIS operation. Original CAS
    /// versions and namespace-fence references remain immutable.
    async fn load_pending_native_stops(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
    ) -> Result<Vec<PreparedNativeStop>, Error>;
    async fn prepare_native_stop(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
        key: &str,
        closed_fence_key: &str,
        inspection: NativeStopInspection,
    ) -> Result<PreparedNativeStop, Error>;
    async fn complete_native_stop(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
        key: &str,
        observation: NativeStopObservation,
    ) -> Result<(), Error>;
}
