//! Pinned commissioning inputs and persisted controller scheduling decisions.
use crate::{AssignmentView, Error, InstallationScope, OperationClaim};
use async_trait::async_trait;
use hermes_domain::{validate_image, validate_name};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TemplateDefinition {
    #[schemars(range(min = 1, max = 1))]
    pub schema_version: u32,
    #[schemars(regex(pattern = r"^.+@sha256:[0-9a-f]{64}$"))]
    pub image: String,
    #[schemars(length(min = 1, max = 63), regex(pattern = r"^[a-z][a-z0-9-]{0,62}$"))]
    pub storage_class: String,
    #[schemars(range(min = 1, max = 1048576))]
    pub home_gib: u32,
    #[schemars(range(min = 1, max = 1048576))]
    pub workspace_gib: u32,
    #[schemars(range(min = 10, max = 64000))]
    pub cpu_millis: u32,
    #[schemars(range(min = 128, max = 262144))]
    pub memory_mib: u32,
    /// Read-only native managed scope. User config stays on the home PVC.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub admin_config: serde_json::Map<String, serde_json::Value>,
}
impl TemplateDefinition {
    pub fn validate(&self) -> Result<(), Error> {
        validate_image(&self.image)?;
        validate_name(&self.storage_class)?;
        if let Some(terminal) = self.admin_config.get("terminal")
            && (!terminal.is_object()
                || terminal
                    .get("backend")
                    .is_some_and(|v| v != "managed_execution"))
        {
            return Err(Error::Invalid);
        }
        if let Some(disabled) = self
            .admin_config
            .get("plugins")
            .and_then(|p| p.get("disabled"))
            && (!disabled.is_array()
                || disabled
                    .as_array()
                    .is_some_and(|names| names.iter().any(|name| name == "managed_execution")))
        {
            return Err(Error::Invalid);
        }
        if serde_json::to_vec(&self.admin_config)
            .map_err(|_| Error::Invalid)?
            .len()
            > 8192
        {
            return Err(Error::Invalid);
        }
        if self.schema_version != 1
            || !(1..=1048576).contains(&self.home_gib)
            || !(1..=1048576).contains(&self.workspace_gib)
            || !(10..=64000).contains(&self.cpu_millis)
            || !(128..=262144).contains(&self.memory_mib)
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }
    pub fn digest(&self) -> Result<String, Error> {
        self.validate()?;
        // Serialize the typed definition, not PostgreSQL's JSONB text ordering.
        let hash = Sha256::digest(serde_json::to_vec(self).map_err(|_| Error::Invalid)?);
        Ok(hash.iter().map(|v| format!("{v:02x}")).collect())
    }
}

pub struct CommissionPlan {
    pub assignment: AssignmentView,
    pub namespace_uid: Option<String>,
    pub template: TemplateDefinition,
    pub template_digest: String,
}

#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConditionReason {
    Provisioning,
    Draining,
    StoragePending,
    NativePending,
    NativeAuthInvalid,
    ExecutionPolicyPending,
    NativeReady,
    NativeRemoved,
    AdmissionUnavailable,
    TemplateUnavailable,
    OwnershipConflict,
    DependencyUnavailable,
    DeadlineExceeded,
}

/// Waiting for an observable dependency is not a failed API action. Both kinds
/// have a durable due time; waiting is still bounded by an operation deadline.
pub enum Schedule {
    Waiting {
        reason: ConditionReason,
        seconds: u16,
    },
    Retry {
        reason: ConditionReason,
        seconds: u16,
    },
    Blocked {
        reason: ConditionReason,
    },
}

#[async_trait]
pub trait CommissioningRepository: Send + Sync {
    async fn commission_plan(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
    ) -> Result<CommissionPlan, Error>;
    async fn schedule(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
        schedule: Schedule,
    ) -> Result<(), Error>;
}

#[cfg(test)]
mod schema_tests {
    use super::*;
    #[test]
    fn reviewed_envelope_schema_matches_the_rust_type() {
        let schema = schemars::generate::SchemaSettings::draft07()
            .into_generator()
            .into_root_schema_for::<TemplateDefinition>();
        let mut generated = serde_json::to_value(schema).unwrap();
        generated["$id"] = serde_json::json!("urn:hermes-manager:template:1");
        generated["x-revision"] = serde_json::json!(1);
        let committed: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../../contracts/hermes-manager/schemas/template-v1.json"
        ))
        .unwrap();
        assert_eq!(generated, committed);
    }
}
