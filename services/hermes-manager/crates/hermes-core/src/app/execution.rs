//! The execution API's use cases (`execution.openapi.yaml`): the
//! `managed_execution` plugin acquires, renews and releases leases on worker
//! environments. Its credential alone selects the instance; every request
//! re-checks that the instance is enabled, in one transaction.

use std::collections::BTreeMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::error::{AppError, StoreError};
use crate::domain::assignments::{LifetimeMode, TerminalGrant};
use crate::domain::execution::{
    EnvironmentState, ExecutionRejected, Invocation, LEASE_TTL_SECONDS, MAX_INSTANCE_ENVIRONMENTS,
    SSH_PORT, SSH_USER, WorkerFailure, environment_key, resolve_backend, summary_session,
    valid_identifier,
};
use crate::domain::names::validate_name;

/// Leases renewed below this remaining lifetime are not live any more.
const MIN_RENEWAL_SECONDS: i64 = 30;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcquireRequest {
    pub session: String,
    pub mode: LifetimeMode,
    pub request: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_lease: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<Invocation>,
}

impl AcquireRequest {
    fn valid(&self) -> bool {
        valid_identifier(&self.session)
            && valid_identifier(&self.request)
            && self
                .runtime_id
                .as_deref()
                .is_none_or(|id| validate_name(id).is_ok())
            && self.runtime.as_ref().is_none_or(Invocation::valid)
            && self.parent_lease.is_none_or(|p| !p.is_nil())
    }

    /// Idempotency digest of the whole body.
    fn digest(&self) -> Vec<u8> {
        Sha256::digest(serde_json::to_vec(self).unwrap_or_default()).to_vec()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeaseRequest {
    pub lease: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloseSessionRequest {
    pub session: String,
    #[serde(default)]
    pub runtime_id: Option<String>,
    #[serde(default)]
    pub runtime: Option<Invocation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SshEndpoint {
    pub host: String,
    pub port: u16,
    pub user: &'static str,
    pub host_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AcquireReply {
    pub environment: Uuid,
    pub generation: String,
    pub lease: Uuid,
    pub ttl: i64,
    pub ssh: SshEndpoint,
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
    pub session: String,
    pub mode: LifetimeMode,
    pub generation: String,
    pub stale: bool,
    pub provisioned: bool,
    pub leases: i64,
    pub runtime_id: String,
}

/// Limits of one allowed backend version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackendLimits {
    pub max_lease_seconds: u32,
    pub max_environments: u32,
}

/// The enabled instance a credential belongs to, locked for the transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Client {
    pub instance_id: Uuid,
    pub execution_generation: i64,
    pub grant: TerminalGrant,
    /// Limits per allowed backend ID.
    pub limits: BTreeMap<String, BackendLimits>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentRow {
    pub id: Uuid,
    pub state: EnvironmentState,
    pub mode: LifetimeMode,
    pub backend: (String, i64),
    pub execution_generation: i64,
    pub key: String,
    pub pod_uid: Option<String>,
    pub ssh_host: Option<String>,
    /// A drain deadline is set and has passed.
    pub drain_expired: bool,
    pub live_leases: i64,
    /// Why its worker failed, when it did.
    pub failure: Option<WorkerFailure>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewEnvironment {
    pub id: Uuid,
    pub instance_id: Uuid,
    pub mode: LifetimeMode,
    pub backend: (String, i64),
    pub execution_generation: i64,
    pub key: String,
    pub invocation: Option<Invocation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredRequest {
    pub digest: Vec<u8>,
    pub environment: Uuid,
    pub lease: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseRow {
    pub id: Uuid,
    pub environment: Uuid,
    pub active: bool,
    /// Remaining seconds by database time; zero or less when due.
    pub remaining: i64,
}

/// One execution transaction. Times are the database's.
#[async_trait]
pub trait ExecutionTx: Send {
    /// The enabled instance with this credential digest, locked; `None` when
    /// unknown or not enabled.
    async fn authenticate(&mut self, credential_digest: &str)
    -> Result<Option<Client>, StoreError>;
    /// Expires this instance's due leases.
    async fn expire_due(&mut self, instance: Uuid) -> Result<(), StoreError>;
    async fn request(
        &mut self,
        instance: Uuid,
        request: &str,
    ) -> Result<Option<StoredRequest>, StoreError>;
    async fn put_request(
        &mut self,
        instance: Uuid,
        request: &str,
        digest: &[u8],
        environment: Uuid,
    ) -> Result<(), StoreError>;
    async fn grant_request(
        &mut self,
        instance: Uuid,
        request: &str,
        lease: Uuid,
    ) -> Result<(), StoreError>;
    async fn environment(
        &mut self,
        instance: Uuid,
        id: Uuid,
    ) -> Result<Option<EnvironmentRow>, StoreError>;
    /// The non-retired environment with these coordinates.
    async fn live_environment(
        &mut self,
        instance: Uuid,
        backend: &(String, i64),
        execution_generation: i64,
        mode: LifetimeMode,
        key: &str,
    ) -> Result<Option<EnvironmentRow>, StoreError>;
    /// `(this backend's, all)` non-retired environments of the instance.
    async fn count_environments(
        &mut self,
        instance: Uuid,
        backend: &str,
    ) -> Result<(u32, u32), StoreError>;
    async fn insert_environment(&mut self, environment: &NewEnvironment) -> Result<(), StoreError>;
    async fn touch_environment(&mut self, id: Uuid) -> Result<(), StoreError>;
    /// Starts draining the matching per-session environments.
    async fn drain_session(
        &mut self,
        instance: Uuid,
        backend: &str,
        key: &str,
    ) -> Result<(), StoreError>;
    async fn lease(&mut self, instance: Uuid, id: Uuid) -> Result<Option<LeaseRow>, StoreError>;
    async fn insert_lease(
        &mut self,
        id: Uuid,
        instance: Uuid,
        environment: Uuid,
        parent: Option<Uuid>,
        ttl: u32,
    ) -> Result<(), StoreError>;
    /// Extends an active, unexpired lease to `ttl` from now, but never past
    /// `max_seconds` after its creation. Returns the new remaining seconds.
    async fn extend_lease(
        &mut self,
        id: Uuid,
        ttl: u32,
        max_seconds: u32,
    ) -> Result<Option<i64>, StoreError>;
    async fn release_lease(&mut self, id: Uuid) -> Result<(), StoreError>;
    async fn environments(&mut self, instance: Uuid) -> Result<Vec<EnvironmentRow>, StoreError>;
    /// The public host key every worker of the instance presents.
    async fn worker_host_key(&mut self, instance: Uuid) -> Result<Option<String>, StoreError>;
    /// Wakes the controller for this instance.
    async fn notify(&mut self, instance: Uuid) -> Result<(), StoreError>;
    async fn commit(self) -> Result<(), StoreError>;
}

#[async_trait]
pub trait ExecutionStore: Send + Sync {
    type Tx: ExecutionTx;
    async fn begin_execution(&self) -> Result<Self::Tx, StoreError>;
    /// Expires every due lease (database time); returns how many.
    async fn expire_leases(&self) -> Result<u64, StoreError>;
}

/// SHA-256 of a 64-hex-character credential; `None` for any other shape.
pub fn credential_digest(credential: &str) -> Option<String> {
    let shaped = credential.len() == 64
        && credential
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    shaped.then(|| {
        Sha256::digest(credential.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    })
}

fn rejected(rejection: ExecutionRejected) -> AppError {
    rejection.into()
}

pub struct Execution<S> {
    store: S,
}

impl<S: ExecutionStore> Execution<S> {
    pub fn new(store: S) -> Self {
        Self { store }
    }

    async fn open(&self, credential: &str) -> Result<(S::Tx, Client), AppError> {
        let digest = credential_digest(credential)
            .ok_or_else(|| rejected(ExecutionRejected::Unauthenticated))?;
        let mut tx = self.store.begin_execution().await?;
        let client = tx
            .authenticate(&digest)
            .await?
            .ok_or_else(|| rejected(ExecutionRejected::Unauthenticated))?;
        tx.expire_due(client.instance_id).await?;
        Ok((tx, client))
    }

    /// Grants a lease on a ready environment, or reports it pending. A
    /// repeated `request` returns its original lease.
    pub async fn acquire(
        &self,
        credential: &str,
        request: &AcquireRequest,
    ) -> Result<AcquireReply, AppError> {
        if !request.valid() {
            return Err(AppError::invalid());
        }
        let (mut tx, client) = self.open(credential).await?;
        let instance = client.instance_id;
        if request.mode != client.grant.mode {
            return Err(rejected(ExecutionRejected::Denied));
        }
        let digest = request.digest();
        let stored = tx.request(instance, &request.request).await?;
        if stored.as_ref().is_some_and(|s| s.digest != digest) {
            return Err(rejected(ExecutionRejected::IdempotencyConflict));
        }
        if let Some(lease) = stored.as_ref().and_then(|s| s.lease) {
            return replay(tx, instance, lease).await;
        }
        let backend =
            resolve_backend(&client.grant, request.runtime_id.as_deref()).map_err(rejected)?;
        let environment = match &stored {
            Some(stored) => tx
                .environment(instance, stored.environment)
                .await?
                .ok_or(StoreError::Unavailable)?,
            None => {
                let key = environment_key(request.mode, &request.session, request.runtime.as_ref());
                match tx
                    .live_environment(
                        instance,
                        &backend,
                        client.execution_generation,
                        request.mode,
                        &key,
                    )
                    .await?
                {
                    // A failed worker is reported, never retried or replaced
                    // while it drains.
                    Some(found) if found.failure.is_some() => {
                        tx.commit().await?;
                        return Err(rejected(ExecutionRejected::BackendFailed));
                    }
                    // A drained environment is not reused; the next one is
                    // created once it has retired.
                    Some(found) if found.state.stale() => {
                        tx.commit().await?;
                        return Err(rejected(ExecutionRejected::Pending));
                    }
                    Some(found) => found,
                    None => {
                        let limits = client
                            .limits
                            .get(&backend.0)
                            .ok_or(StoreError::Unavailable)?;
                        let (backend_count, total) =
                            tx.count_environments(instance, &backend.0).await?;
                        if backend_count >= limits.max_environments
                            || total >= MAX_INSTANCE_ENVIRONMENTS
                        {
                            return Err(rejected(ExecutionRejected::Exhausted));
                        }
                        let created = NewEnvironment {
                            id: Uuid::new_v4(),
                            instance_id: instance,
                            mode: request.mode,
                            backend: backend.clone(),
                            execution_generation: client.execution_generation,
                            key,
                            invocation: request.runtime.clone(),
                        };
                        tx.insert_environment(&created).await?;
                        tx.notify(instance).await?;
                        tx.environment(instance, created.id)
                            .await?
                            .ok_or(StoreError::Unavailable)?
                    }
                }
            }
        };
        if stored.is_none() {
            tx.put_request(instance, &request.request, &digest, environment.id)
                .await?;
        }
        if environment.failure.is_some() {
            tx.commit().await?;
            return Err(rejected(ExecutionRejected::BackendFailed));
        }
        if environment.state.stale() {
            tx.commit().await?;
            return Err(rejected(ExecutionRejected::LeaseNotLive));
        }
        let Some(host) = environment
            .ssh_host
            .clone()
            .filter(|_| environment.state == EnvironmentState::Ready)
        else {
            tx.commit().await?;
            return Err(rejected(ExecutionRejected::Pending));
        };
        if let Some(parent) = request.parent_lease {
            let live = tx
                .lease(instance, parent)
                .await?
                .is_some_and(|p| p.active && p.remaining > 0 && p.environment == environment.id);
            if !live {
                return Err(rejected(ExecutionRejected::Denied));
            }
        }
        let host_key = tx
            .worker_host_key(instance)
            .await?
            .ok_or(StoreError::Unavailable)?;
        let lease = Uuid::new_v4();
        tx.insert_lease(
            lease,
            instance,
            environment.id,
            request.parent_lease,
            LEASE_TTL_SECONDS,
        )
        .await?;
        tx.grant_request(instance, &request.request, lease).await?;
        tx.touch_environment(environment.id).await?;
        tx.commit().await?;
        Ok(AcquireReply {
            environment: environment.id,
            generation: "1".into(),
            lease,
            ttl: i64::from(LEASE_TTL_SECONDS),
            ssh: SshEndpoint {
                host,
                port: SSH_PORT,
                user: SSH_USER,
                host_key,
            },
        })
    }

    pub async fn heartbeat(
        &self,
        credential: &str,
        request: &LeaseRequest,
    ) -> Result<HeartbeatReply, AppError> {
        let (mut tx, client) = self.open(credential).await?;
        let instance = client.instance_id;
        let lease = tx
            .lease(instance, request.lease)
            .await?
            .ok_or_else(|| rejected(ExecutionRejected::NotFound))?;
        let environment = tx
            .environment(instance, lease.environment)
            .await?
            .ok_or(StoreError::Unavailable)?;
        let stale = environment.state == EnvironmentState::Retired
            || (environment.state == EnvironmentState::Draining && environment.drain_expired);
        if !lease.active || lease.remaining <= 0 || stale {
            tx.commit().await?;
            return Err(rejected(ExecutionRejected::LeaseNotLive));
        }
        let max = client
            .limits
            .get(&environment.backend.0)
            .map_or(LEASE_TTL_SECONDS, |l| l.max_lease_seconds);
        let ttl = tx.extend_lease(lease.id, LEASE_TTL_SECONDS, max).await?;
        let Some(ttl) = ttl.filter(|ttl| *ttl >= MIN_RENEWAL_SECONDS) else {
            tx.commit().await?;
            return Err(rejected(ExecutionRejected::LeaseNotLive));
        };
        tx.touch_environment(environment.id).await?;
        tx.commit().await?;
        Ok(HeartbeatReply { ttl })
    }

    pub async fn release(
        &self,
        credential: &str,
        request: &LeaseRequest,
    ) -> Result<ReleaseReply, AppError> {
        let (mut tx, client) = self.open(credential).await?;
        if tx.lease(client.instance_id, request.lease).await?.is_none() {
            return Err(rejected(ExecutionRejected::NotFound));
        }
        tx.release_lease(request.lease).await?;
        tx.commit().await?;
        Ok(ReleaseReply { released: true })
    }

    pub async fn close(
        &self,
        credential: &str,
        request: &CloseSessionRequest,
    ) -> Result<CloseReply, AppError> {
        let valid = valid_identifier(&request.session)
            && request
                .runtime_id
                .as_deref()
                .is_none_or(|id| validate_name(id).is_ok())
            && request.runtime.as_ref().is_none_or(Invocation::valid);
        if !valid {
            return Err(AppError::invalid());
        }
        let (mut tx, client) = self.open(credential).await?;
        let (backend, _) =
            resolve_backend(&client.grant, request.runtime_id.as_deref()).map_err(rejected)?;
        let key = environment_key(
            LifetimeMode::PerSession,
            &request.session,
            request.runtime.as_ref(),
        );
        tx.drain_session(client.instance_id, &backend, &key).await?;
        tx.notify(client.instance_id).await?;
        tx.commit().await?;
        Ok(CloseReply { closed: true })
    }

    pub async fn environments(
        &self,
        credential: &str,
    ) -> Result<Vec<EnvironmentSummary>, AppError> {
        let (mut tx, client) = self.open(credential).await?;
        let rows = tx.environments(client.instance_id).await?;
        tx.commit().await?;
        Ok(rows
            .into_iter()
            .take(64)
            .map(|row| EnvironmentSummary {
                id: row.id,
                session: summary_session(&row.key).to_owned(),
                mode: row.mode,
                generation: "1".into(),
                stale: row.state.stale(),
                provisioned: row.pod_uid.is_some(),
                leases: row.live_leases,
                runtime_id: row.backend.0,
            })
            .collect())
    }
}

async fn replay<T: ExecutionTx>(
    mut tx: T,
    instance: Uuid,
    lease: Uuid,
) -> Result<AcquireReply, AppError> {
    let row = tx
        .lease(instance, lease)
        .await?
        .ok_or(StoreError::Unavailable)?;
    let environment = tx
        .environment(instance, row.environment)
        .await?
        .ok_or(StoreError::Unavailable)?;
    let host_key = tx
        .worker_host_key(instance)
        .await?
        .ok_or(StoreError::Unavailable)?;
    tx.commit().await?;
    let (Some(host), true, true) = (environment.ssh_host, row.active, row.remaining >= 1) else {
        return Err(rejected(ExecutionRejected::LeaseNotLive));
    };
    Ok(AcquireReply {
        environment: environment.id,
        generation: "1".into(),
        lease,
        ttl: row.remaining.min(i64::from(LEASE_TTL_SECONDS)),
        ssh: SshEndpoint {
            host,
            port: SSH_PORT,
            user: SSH_USER,
            host_key,
        },
    })
}
