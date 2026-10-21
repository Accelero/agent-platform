//! Native terminal wire contract and durable execution ports. Invocation paths
//! are bounded metadata; they never select Kubernetes mounts or Pod fields.
use super::{clients::ExecutionClient, identifier};
use crate::{Error, InstallationScope, execution::ResolvedPolicy};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvocationContext {
    pub hermes_home: String,
    pub workdir: String,
}
impl InvocationContext {
    pub fn validate(&self) -> Result<(), Error> {
        for path in [&self.hermes_home, &self.workdir] {
            if path.is_empty()
                || path.len() > 4096
                || !path.starts_with('/')
                || path.chars().any(char::is_control)
            {
                return Err(Error::Invalid);
            }
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifetimeMode {
    PerSession,
    PerProfileOnDemand,
    PerProfileAlwaysRunning,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcquireRequest {
    pub profile: String,
    pub session: String,
    pub mode: LifetimeMode,
    pub request: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_lease: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<InvocationContext>,
}
impl AcquireRequest {
    pub fn validate(&self) -> Result<(), Error> {
        identifier(&self.profile)?;
        identifier(&self.session)?;
        identifier(&self.request)?;
        if let Some(id) = &self.runtime_id {
            hermes_domain::validate_name(id)?;
        }
        if let Some(c) = &self.runtime {
            c.validate()?;
        }
        if self.parent_lease.is_some_and(|p| p.is_nil()) {
            return Err(Error::Invalid);
        }
        Ok(())
    }
    pub fn digest(&self) -> Result<Vec<u8>, Error> {
        self.validate()?;
        Ok(Sha256::digest(serde_json::to_vec(self).map_err(|_| Error::Invalid)?).to_vec())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeaseRequest {
    pub lease: Uuid,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloseSessionRequest {
    pub profile: String,
    pub session: String,
    #[serde(default)]
    pub runtime_id: Option<String>,
    #[serde(default)]
    pub runtime: Option<InvocationContext>,
}
impl CloseSessionRequest {
    pub fn validate(&self) -> Result<(), Error> {
        identifier(&self.profile)?;
        identifier(&self.session)?;
        if let Some(id) = &self.runtime_id {
            hermes_domain::validate_name(id)?;
        }
        if let Some(c) = &self.runtime {
            c.validate()?;
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SshEndpoint {
    pub host: String,
    pub port: u16,
    pub user: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_key: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AcquireReply {
    pub environment: Uuid,
    pub generation: String,
    pub lease: Uuid,
    pub ttl: i64,
    pub ssh: SshEndpoint,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcquireOutcome {
    Pending,
    Granted(AcquireReply),
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HeartbeatReply {
    pub ttl: i64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReleaseReply {
    pub released: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CloseReply {
    pub closed: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EnvironmentSummary {
    pub id: Uuid,
    pub profile: String,
    pub session: String,
    pub mode: LifetimeMode,
    pub generation: String,
    pub stale: bool,
    pub provisioned: bool,
    pub leases: i64,
    pub runtime_id: String,
}
#[async_trait]
pub trait ExecutionLeaseRepository: Send + Sync {
    async fn acquire_execution_lease(
        &self,
        scope: &InstallationScope,
        client: &ExecutionClient,
        request: &AcquireRequest,
    ) -> Result<AcquireOutcome, Error>;
    async fn heartbeat_execution_lease(
        &self,
        scope: &InstallationScope,
        client: &ExecutionClient,
        request: &LeaseRequest,
    ) -> Result<HeartbeatReply, Error>;
    async fn release_execution_lease(
        &self,
        scope: &InstallationScope,
        client: &ExecutionClient,
        request: &LeaseRequest,
    ) -> Result<ReleaseReply, Error>;
    async fn close_execution_session(
        &self,
        scope: &InstallationScope,
        client: &ExecutionClient,
        request: &CloseSessionRequest,
    ) -> Result<CloseReply, Error>;
    async fn list_execution_environments(
        &self,
        scope: &InstallationScope,
        client: &ExecutionClient,
    ) -> Result<Vec<EnvironmentSummary>, Error>;
    async fn expire_execution_leases(
        &self,
        scope: &InstallationScope,
        limit: u32,
    ) -> Result<u64, Error>;
}
/// Fenced work includes only reviewed immutable policy and server-selected IDs.
#[derive(Debug, Clone)]
pub struct EnvironmentClaim {
    pub installation_id: Uuid,
    pub instance_id: Uuid,
    pub environment_id: Uuid,
    pub generation: i64,
    pub epoch: i64,
    pub worker: Uuid,
    pub fence: i64,
    pub assignment_generation: i64,
    pub policy: ResolvedPolicy,
    pub runtime_id: String,
    pub pod_name: String,
}
#[derive(Debug, Clone)]
pub struct WorkerIntent {
    pub claim: EnvironmentClaim,
    pub spec_digest: String,
}
/// Produced by the trusted Kubernetes+SSH observer, never decoded from a client.
#[derive(Debug, Clone)]
pub struct WorkerReady {
    pub observed_spec_digest: String,
    pub pod_uid: String,
    pub resource_version: String,
    pub spec_digest: String,
    pub host: String,
    pub host_key: Option<String>,
}
#[derive(Debug, Clone)]
pub struct WorkerStartIntent {
    pub intent: WorkerIntent,
    pub pod_uid: String,
    pub resource_version: String,
    pub observed_spec_digest: String,
}
#[derive(Debug, Clone)]
pub struct WorkerBinding {
    pub gated: bool,
    pub pod_uid: String,
    pub resource_version: String,
    pub observed_spec_digest: String,
}
#[derive(Debug, Clone)]
pub struct WorkerRetirement {
    pub claim: EnvironmentClaim,
    pub intent: Option<WorkerIntent>,
    pub pod_uid: Option<String>,
}
#[async_trait]
pub trait ExecutionEnvironmentRepository: Send + Sync {
    async fn claim_execution_environment(
        &self,
        scope: &InstallationScope,
        worker: Uuid,
        lease_seconds: u32,
    ) -> Result<Option<EnvironmentClaim>, Error>;
    async fn prepare_execution_worker(
        &self,
        scope: &InstallationScope,
        claim: &EnvironmentClaim,
        spec_digest: &str,
    ) -> Result<WorkerIntent, Error>;
    async fn bind_execution_worker(
        &self,
        scope: &InstallationScope,
        intent: &WorkerIntent,
        binding: &WorkerBinding,
    ) -> Result<WorkerStartIntent, Error>;
    async fn record_execution_worker_ready(
        &self,
        scope: &InstallationScope,
        intent: &WorkerIntent,
        ready: &WorkerReady,
    ) -> Result<(), Error>;
}
pub fn worker_pod_name(environment: Uuid, generation: i64) -> Result<String, Error> {
    if environment.is_nil() || generation < 1 {
        return Err(Error::Invalid);
    }
    Ok(format!("hw-{}-g{generation}", environment.simple()))
}
/// Administrator-approved execution behavior, immutable with resolved policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecutionLimits {
    pub ttl_seconds: u32,
    pub exclusive: bool,
    pub serialize_profile: bool,
    pub max_environments: u32,
    pub max_leases: u32,
    pub idle_seconds: u32,
}
impl Default for ExecutionLimits {
    fn default() -> Self {
        Self {
            ttl_seconds: 120,
            exclusive: false,
            serialize_profile: false,
            max_environments: 8,
            max_leases: 32,
            idle_seconds: 900,
        }
    }
}
impl ExecutionLimits {
    pub fn validate(&self) -> Result<(), Error> {
        if !(30..=600).contains(&self.ttl_seconds)
            || !(1..=64).contains(&self.max_environments)
            || !(1..=128).contains(&self.max_leases)
            || !(30..=86400).contains(&self.idle_seconds)
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strict_native_contract_and_bounded_metadata() {
        let base =
            r#"{"profile":"default","session":"session","mode":"per_session","request":"request"}"#;
        let request: AcquireRequest = serde_json::from_str(base).unwrap();
        request.validate().unwrap();
        for extra in [
            r#", "image":"attacker""#,
            r#", "profile":"foreign""#,
            r#", "runtime":{"hermes_home":"/x","workdir":"/y","mount":"/host"}"#,
        ] {
            let body = format!("{}{}{}", &base[..base.len() - 1], extra, "}");
            assert!(serde_json::from_str::<AcquireRequest>(&body).is_err());
        }
        let mut long = request.clone();
        long.session = "a".repeat(257);
        assert_eq!(long.validate(), Err(Error::Invalid));
        long.session = "a".repeat(256);
        long.validate().unwrap();
        assert_ne!(long.digest().unwrap(), request.digest().unwrap());
        assert_eq!(worker_pod_name(Uuid::new_v4(), i64::MAX).unwrap().len(), 56);
    }
}
