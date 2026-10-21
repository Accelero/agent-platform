//! Approved terminal configuration. These are references and bounded policy,
//! never Kubernetes manifests, credential values, or client-selected images.
use std::collections::BTreeSet;

use async_trait::async_trait;
use hermes_domain::{Identity, Mount, resources::valid_uid, validate_image, validate_name};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{AdminPolicy, Error, InstallationScope, VerifiedPrincipal};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeRef {
    pub id: String,
    pub version: i64,
}
impl RuntimeRef {
    pub fn validate(&self) -> Result<(), Error> {
        validate_name(&self.id)?;
        if self.version < 1 {
            return Err(Error::Invalid);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkerResources {
    pub cpu_millis: u32,
    pub memory_mib: u32,
    pub ephemeral_storage_mib: u32,
}
impl WorkerResources {
    pub fn validate(&self) -> Result<(), Error> {
        if !(10..=64_000).contains(&self.cpu_millis)
            || !(64..=262_144).contains(&self.memory_mib)
            || !(64..=1_048_576).contains(&self.ephemeral_storage_mib)
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }
}

/// A service identity, not a raw CIDR, unrestricted DNS suffix, or Pod selector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServiceEgress {
    pub namespace: String,
    pub service: String,
    pub port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeDefinition {
    pub schema_version: u16,
    pub runtime: RuntimeRef,
    pub image: String,
    /// managed-ssh-v2 adds a manager-provisioned host key to the v1 entrypoint,
    /// worker user, TCP 2222, authorized-key, SFTP and platform-helper contract.
    pub ssh_contract: String,
    pub uid: u32,
    pub gid: u32,
    pub service_account: String,
    pub resources: WorkerResources,
    /// Explicit public locale, rather than arbitrary env/Secret injection.
    pub locale: String,
    pub egress: Vec<ServiceEgress>,
    /// Names resolve in EACH owner's registered namespace to an exact Secret UID.
    pub pull_secret_names: Vec<String>,
}
impl RuntimeDefinition {
    pub fn validate(&self) -> Result<(), Error> {
        self.runtime.validate()?;
        self.resources.validate()?;
        validate_name(&self.service_account)?;
        validate_image(&self.image)?;
        if self.schema_version != 1
            || !matches!(
                self.ssh_contract.as_str(),
                "managed-ssh-v1" | "managed-ssh-v2"
            )
            || !(1..=65_535).contains(&self.uid)
            || !(1..=65_535).contains(&self.gid)
            || self.service_account != "worker"
            || !["C.UTF-8", "en_US.UTF-8"].contains(&self.locale.as_str())
            || self.egress.len() > 16
            || self.pull_secret_names.len() > 4
        {
            return Err(Error::Invalid);
        }
        let mut secrets = BTreeSet::new();
        for name in &self.pull_secret_names {
            validate_name(name)?;
            if !secrets.insert(name) {
                return Err(Error::Invalid);
            }
        }
        let mut egress = BTreeSet::new();
        for rule in &self.egress {
            validate_name(&rule.namespace)?;
            validate_name(&rule.service)?;
            if rule.port == 0 || !egress.insert((&rule.namespace, &rule.service, rule.port)) {
                return Err(Error::Invalid);
            }
        }
        Ok(())
    }
    pub fn digest(&self) -> Result<String, Error> {
        self.validate()?;
        digest(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackupPolicy {
    PlatformCheckpoint,
    External,
    Excluded,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PermittedMount {
    pub path: String,
    pub read_only: bool,
    pub sub_path: Option<String>,
}
impl PermittedMount {
    pub fn validate(&self) -> Result<(), Error> {
        validate_mounts(std::slice::from_ref(self))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StorageBinding {
    pub id: Uuid,
    pub version: i64,
    pub instance_id: Uuid,
    pub namespace: String,
    pub namespace_uid: String,
    pub pvc_name: String,
    pub pvc_uid: String,
    /// Opaque deployment inventory ID, never a server path or credential.
    pub backing_storage_id: Uuid,
    pub external: bool,
    pub required: bool,
    pub backup: BackupPolicy,
    pub mounts: Vec<PermittedMount>,
}
impl StorageBinding {
    pub fn validate(&self) -> Result<(), Error> {
        validate_name(&self.namespace)?;
        validate_name(&self.pvc_name)?;
        if self.id.is_nil()
            || self.instance_id.is_nil()
            || self.backing_storage_id.is_nil()
            || self.version < 1
            || self.mounts.is_empty()
            || self.mounts.len() > 16
            || !valid_uid(&self.namespace_uid)
            || !valid_uid(&self.pvc_uid)
        {
            return Err(Error::Invalid);
        }
        validate_mounts(&self.mounts)
    }
}
fn validate_mounts(mounts: &[PermittedMount]) -> Result<(), Error> {
    let mounts = mounts
        .iter()
        .map(|mount| Mount {
            storage_binding_id: Uuid::nil(),
            mount_path: mount.path.clone(),
            sub_path: mount.sub_path.clone(),
            read_only: mount.read_only,
        })
        .collect::<Vec<_>>();
    hermes_domain::validate_mounts(&mounts)?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PullSecretBinding {
    pub instance_id: Uuid,
    pub namespace: String,
    pub namespace_uid: String,
    pub name: String,
    pub uid: String,
}
impl PullSecretBinding {
    pub fn validate(&self) -> Result<(), Error> {
        validate_name(&self.namespace)?;
        validate_name(&self.name)?;
        if self.instance_id.is_nil() || !valid_uid(&self.namespace_uid) || !valid_uid(&self.uid) {
            return Err(Error::Invalid);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MountSelection {
    pub binding_id: Uuid,
    pub binding_version: i64,
    pub mount: PermittedMount,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AllowedRuntime {
    pub runtime: RuntimeRef,
    pub resources: WorkerResources,
    pub mounts: Vec<MountSelection>,
    #[serde(default, skip_serializing_if = "default_limits")]
    pub limits: crate::terminal::leases::ExecutionLimits,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssignmentExecution {
    pub default_runtime_id: String,
    pub allowed: Vec<AllowedRuntime>,
}
impl AssignmentExecution {
    pub fn validate(&self) -> Result<(), Error> {
        validate_name(&self.default_runtime_id)?;
        let mut ids = BTreeSet::new();
        if self.allowed.is_empty() || self.allowed.len() > 16 {
            return Err(Error::Invalid);
        }
        for allowed in &self.allowed {
            allowed.runtime.validate()?;
            allowed.resources.validate()?;
            allowed.limits.validate()?;
            if !ids.insert(&allowed.runtime.id) || allowed.mounts.len() > 16 {
                return Err(Error::Invalid);
            }
            let mounts = allowed
                .mounts
                .iter()
                .map(|m| m.mount.clone())
                .collect::<Vec<_>>();
            validate_mounts(&mounts)?;
            for mount in &allowed.mounts {
                if mount.binding_id.is_nil() || mount.binding_version < 1 {
                    return Err(Error::Invalid);
                }
            }
        }
        if !ids.contains(&self.default_runtime_id) {
            return Err(Error::Invalid);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedMount {
    pub binding: StorageBinding,
    pub mount: PermittedMount,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedRuntime {
    pub definition: RuntimeDefinition,
    pub definition_digest: String,
    pub resources: WorkerResources,
    pub mounts: Vec<ResolvedMount>,
    pub pull_secrets: Vec<PullSecretBinding>,
    #[serde(default, skip_serializing_if = "default_limits")]
    pub limits: crate::terminal::leases::ExecutionLimits,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedPolicy {
    pub installation_id: Uuid,
    pub instance_id: Uuid,
    pub namespace: String,
    pub namespace_uid: String,
    pub generation: i64,
    pub default_runtime_id: String,
    pub runtimes: Vec<ResolvedRuntime>,
}
impl ResolvedPolicy {
    pub fn digest(&self) -> Result<String, Error> {
        digest(self)
    }
}
// Preserve the immutable digest of earlier default-only policy snapshots and
// idempotent mutation bodies. Explicit nondefault limits remain authenticated.
fn default_limits(value: &crate::terminal::leases::ExecutionLimits) -> bool {
    value == &Default::default()
}
fn digest(value: &impl Serialize) -> Result<String, Error> {
    Ok(
        Sha256::digest(serde_json::to_vec(value).map_err(|_| Error::Invalid)?)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    )
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExecutionCommand {
    RegisterRuntime {
        definition: RuntimeDefinition,
    },
    RegisterStorage {
        binding: StorageBinding,
    },
    UpdateStorage {
        expected_version: i64,
        binding: StorageBinding,
    },
    RegisterPullSecret {
        binding: PullSecretBinding,
    },
    SetPolicy {
        assignment_id: Uuid,
        expected_version: i64,
        policy: AssignmentExecution,
    },
}
impl ExecutionCommand {
    pub fn validate(&self) -> Result<(), Error> {
        match self {
            Self::RegisterRuntime { definition } => definition.validate(),
            Self::RegisterStorage { binding } => binding.validate(),
            Self::UpdateStorage {
                expected_version,
                binding,
            } => {
                if *expected_version < 1 || expected_version.checked_add(1) != Some(binding.version)
                {
                    return Err(Error::Invalid);
                }
                binding.validate()
            }
            Self::RegisterPullSecret { binding } => binding.validate(),
            Self::SetPolicy {
                assignment_id,
                expected_version,
                policy,
            } => {
                if assignment_id.is_nil() || *expected_version < 1 {
                    return Err(Error::Invalid);
                }
                policy.validate()
            }
        }
    }
    pub fn action(&self) -> &'static str {
        match self {
            Self::RegisterRuntime { .. } => "execution.runtime.register",
            Self::RegisterStorage { .. } => "execution.storage.register",
            Self::UpdateStorage { .. } => "execution.storage.update",
            Self::RegisterPullSecret { .. } => "execution.pull-secret.register",
            Self::SetPolicy { .. } => "execution.policy.set",
        }
    }
}
#[derive(Debug, Clone)]
pub struct ExecutionMutation {
    pub scope: InstallationScope,
    pub actor: Identity,
    pub request_id: Uuid,
    pub idempotency_key: String,
    pub command: ExecutionCommand,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecutionAccepted {
    pub event_id: Uuid,
    pub assignment_version: Option<i64>,
    pub policy_generation: Option<i64>,
    pub policy_digest: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecutionPolicyView {
    pub assignment_id: Uuid,
    pub assignment_version: i64,
    pub desired_generation: i64,
    pub applied_generation: i64,
    pub state: String,
    pub desired: ResolvedPolicy,
    pub digest: String,
}
#[derive(Debug, Clone)]
pub struct PolicyClaim {
    pub instance_id: Uuid,
    pub assignment_version: i64,
    pub generation: i64,
    pub epoch: i64,
    pub worker: Uuid,
    pub fence: i64,
    pub digest: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyCondition {
    AdmissionPending,
    StoragePending,
    DrainPending,
    DependencyUnavailable,
}

#[async_trait]
pub trait ExecutionRepository: Send + Sync {
    async fn runtime_catalog(
        &self,
        scope: &InstallationScope,
        after: Option<&str>,
        limit: u16,
    ) -> Result<Vec<RuntimeDefinition>, Error>;
    async fn storage_binding(
        &self,
        scope: &InstallationScope,
        id: Uuid,
    ) -> Result<StorageBinding, Error>;
    async fn runtime_versions(
        &self,
        scope: &InstallationScope,
        runtime_id: &str,
        after_version: i64,
        limit: u16,
    ) -> Result<Vec<RuntimeDefinition>, Error>;
    async fn owner_storage_bindings(
        &self,
        scope: &InstallationScope,
        instance_id: Uuid,
        after: Option<Uuid>,
        limit: u16,
    ) -> Result<Vec<StorageBinding>, Error>;
    async fn mutate_execution(
        &self,
        mutation: ExecutionMutation,
    ) -> Result<ExecutionAccepted, Error>;
    async fn execution_policy(
        &self,
        scope: &InstallationScope,
        assignment_id: Uuid,
    ) -> Result<ExecutionPolicyView, Error>;
    async fn claim_policy(
        &self,
        scope: &InstallationScope,
        worker: Uuid,
        lease_seconds: u16,
    ) -> Result<Option<PolicyClaim>, Error>;
    async fn policy_plan(
        &self,
        scope: &InstallationScope,
        claim: &PolicyClaim,
    ) -> Result<ResolvedPolicy, Error>;
    async fn defer_policy(
        &self,
        scope: &InstallationScope,
        claim: &PolicyClaim,
        condition: PolicyCondition,
        seconds: u16,
    ) -> Result<(), Error>;
    // Activation is a separate trusted observation port in terminal::activation;
    // administrative desired-state mutations cannot mark a policy applied.
}

pub struct ExecutionAdministration<S> {
    store: S,
    scope: InstallationScope,
    policy: AdminPolicy,
}
impl<S: ExecutionRepository> ExecutionAdministration<S> {
    pub fn new(store: S, scope: InstallationScope, policy: AdminPolicy) -> Result<Self, Error> {
        hermes_domain::validate_issuer(&scope.issuer)?;
        if scope.id.is_nil() || scope.epoch < 1 || scope.issuer != policy.issuer {
            return Err(Error::Invalid);
        }
        Ok(Self {
            store,
            scope,
            policy,
        })
    }
    pub async fn execute(
        &self,
        actor: &VerifiedPrincipal,
        request_id: Uuid,
        idempotency_key: &str,
        command: ExecutionCommand,
    ) -> Result<ExecutionAccepted, Error> {
        self.policy.authorize(actor)?;
        command.validate()?;
        if request_id.is_nil()
            || idempotency_key.is_empty()
            || idempotency_key.len() > 128
            || !idempotency_key.bytes().all(|b| b.is_ascii_graphic())
        {
            return Err(Error::Invalid);
        }
        self.store
            .mutate_execution(ExecutionMutation {
                scope: self.scope.clone(),
                actor: actor.identity.clone(),
                request_id,
                idempotency_key: idempotency_key.into(),
                command,
            })
            .await
    }
}
