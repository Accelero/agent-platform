//! Effective policy is an observed controller result, never an administrator or
//! execution-client flag. Every external CAS has an immutable committed input.
use crate::{
    Error, InstallationScope,
    execution::{PolicyClaim, ResolvedPolicy},
    resources::OwnedResource,
};
use async_trait::async_trait;
use hermes_domain::{instance_namespace, resources::valid_uid, validate_sha256};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub struct ActivationPlan {
    pub policy: ResolvedPolicy,
    pub assignment_generation: i64,
    pub namespace: OwnedResource,
    pub parameters: OwnedResource,
    pub worker_account: OwnedResource,
    pub authorized_keys: OwnedResource,
    /// First-winner public renderer output; a new binary reuses these bytes.
    pub worker: Value,
    pub rendering_digest: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PolicyAdmissionInput {
    pub installation_id: Uuid,
    pub instance_id: Uuid,
    pub generation: i64,
    pub epoch: i64,
    pub assignment_generation: i64,
    pub namespace: String,
    pub namespace_uid: String,
    pub resource_id: Uuid,
    pub policy_uid: String,
    pub resource_version: String,
    pub policy_digest: String,
    pub rendering_digest: String,
}
impl PolicyAdmissionInput {
    pub fn digest(&self) -> Result<String, Error> {
        if self.installation_id.is_nil()
            || self.instance_id.is_nil()
            || self.resource_id.is_nil()
            || self.generation < 1
            || self.epoch < 1
            || self.assignment_generation < 1
            || self.namespace != instance_namespace(self.installation_id, self.instance_id)
            || [
                &self.namespace_uid,
                &self.policy_uid,
                &self.resource_version,
            ]
            .iter()
            .any(|v| !valid_uid(v))
        {
            return Err(Error::Invalid);
        }
        validate_sha256(&self.policy_digest)?;
        validate_sha256(&self.rendering_digest)?;
        public_digest(self)
    }
}
pub fn public_digest(value: &impl Serialize) -> Result<String, Error> {
    Ok(
        Sha256::digest(serde_json::to_vec(value).map_err(|_| Error::Invalid)?)
            .iter()
            .map(|v| format!("{v:02x}"))
            .collect(),
    )
}
pub struct PreparedPolicyAdmission {
    pub plan: ActivationPlan,
    pub input: PolicyAdmissionInput,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PolicyAdmissionObservation {
    pub input_digest: String,
    pub policy_uid: String,
    pub resource_version: String,
    pub namespace_uid: String,
    pub rendering_digest: String,
}
#[async_trait]
pub trait PolicyActivationRepository: Send + Sync {
    async fn renew_policy_activation(
        &self,
        scope: &InstallationScope,
        claim: &PolicyClaim,
    ) -> Result<(), Error>;
    async fn activation_plan(
        &self,
        scope: &InstallationScope,
        claim: &PolicyClaim,
    ) -> Result<ActivationPlan, Error>;
    async fn prepare_policy_admission(
        &self,
        scope: &InstallationScope,
        claim: &PolicyClaim,
        input: &PolicyAdmissionInput,
    ) -> Result<PreparedPolicyAdmission, Error>;
    async fn pending_policy_admissions(
        &self,
        scope: &InstallationScope,
        claim: &PolicyClaim,
    ) -> Result<Vec<PreparedPolicyAdmission>, Error>;
    async fn record_policy_admission(
        &self,
        scope: &InstallationScope,
        claim: &PolicyClaim,
        observation: &PolicyAdmissionObservation,
    ) -> Result<(), Error>;
    async fn activate_execution_policy(
        &self,
        scope: &InstallationScope,
        claim: &PolicyClaim,
        input_digest: &str,
    ) -> Result<(), Error>;
}
