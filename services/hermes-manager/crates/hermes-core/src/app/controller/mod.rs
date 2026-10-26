//! The controller: one level-triggered reconcile per instance, from desired
//! state in PostgreSQL and observed state in the cluster.
//!
//! [`Controller::reconcile`] is one pass. It never remembers anything between
//! passes: a restarted controller resumes by observation. Scheduling (the
//! [`queue`], notifications, resync and leadership) is the caller's.

pub mod queue;

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Map, Value};
use thiserror::Error;
use uuid::Uuid;
use zeroize::Zeroizing;

use super::error::{AppError, StoreError};
use super::faults::{FaultHook, FaultPoint};
use super::settings::Settings;
use crate::domain::assignments::{Access, TerminalGrant};
use crate::domain::backends::BackendDefinition;
use crate::domain::catalog::VersionRef;
use crate::domain::execution::{
    EnvironmentContext, EnvironmentState, EnvironmentView, Invocation, ObservedWorker, SshCheck,
    WorkerFailure, WorkerStep, plan_environment,
};
use crate::domain::identity::Identity;
use crate::domain::instances::{
    self, Apply, BlockedReason, InstanceStatus, NativeCheck, Observed, Plan, Precondition, Wanted,
};
use crate::domain::materials::MaterialPurpose;
use crate::domain::orphans::{self, Orphan, OwnedObject};
use crate::domain::secrets::{SecretKind, SecretReference};
use crate::domain::storage::VolumeSpec;
use crate::domain::templates::TemplateDefinition;

fn earliest(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// Name of the claim that holds the private Hermes home.
pub const HOME_CLAIM: &str = "hermes-home";

/// Plaintext key material or a secret value. Wiped on drop, never printed.
pub struct Plain(Zeroizing<Vec<u8>>);

impl Plain {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(Zeroizing::new(bytes))
    }

    pub fn expose(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for Plain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Plain(redacted)")
    }
}

/// Everything the controller needs to render one instance, read in one
/// transaction.
#[derive(Debug, Clone, PartialEq)]
pub struct DesiredInstance {
    pub instance_id: Uuid,
    pub assignment_id: Uuid,
    pub identity: Identity,
    pub namespace: String,
    pub namespace_uid: Option<String>,
    pub access: Access,
    pub desired_generation: i64,
    pub execution_generation: i64,
    pub template: VersionRef,
    pub definition: TemplateDefinition,
    /// The assignment's volumes, template defaults included.
    pub volumes: BTreeMap<String, VolumeSpec>,
    pub terminal: TerminalGrant,
    /// Template settings with the assignment's overrides layered over them.
    pub admin_config: Map<String, Value>,
    /// Template references followed by the assignment's.
    pub secrets: Vec<SecretReference>,
    /// The definitions of the allowed backend versions, by backend ID.
    pub backends: BTreeMap<String, AllowedBackend>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AllowedBackend {
    pub version: i64,
    pub definition: BackendDefinition,
}

impl DesiredInstance {
    /// Claims the instance needs: the home and one per volume.
    pub fn claims(&self) -> Vec<String> {
        std::iter::once(HOME_CLAIM.to_owned())
            .chain(self.volumes.keys().cloned())
            .collect()
    }

    pub fn wanted(&self) -> Wanted<'_> {
        Wanted {
            access: self.access,
            generation: self.desired_generation,
            namespace_uid: self.namespace_uid.as_deref(),
            claims: self.claims(),
        }
    }
}

/// A stored material row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredMaterial {
    pub id: Uuid,
    pub purpose: MaterialPurpose,
    pub sealed: Vec<u8>,
    /// The public half, if any: an SSH public key or a credential digest.
    pub public: Option<String>,
}

/// Freshly generated material, sealed for storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewMaterial {
    pub sealed: Vec<u8>,
    pub public: Option<String>,
}

/// The latest version of an admin secret, as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealedSecret {
    pub name: String,
    pub kind: SecretKind,
    pub version: i64,
    pub sealed: Vec<u8>,
}

/// An opened admin secret for one instance's namespace.
#[derive(Debug)]
pub struct DeliveredSecret {
    pub name: String,
    pub kind: SecretKind,
    pub version: i64,
    pub value: Plain,
}

/// Opened instance materials.
#[derive(Debug)]
pub struct InstanceMaterials {
    /// The `Initialization` row's ID; bound into the home receipt.
    pub initialization_id: Uuid,
    pub plain: BTreeMap<MaterialPurpose, Plain>,
    pub public: BTreeMap<MaterialPurpose, String>,
}

/// What the home-initialization receipt must be bound to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitBinding {
    pub installation_id: Uuid,
    pub instance_id: Uuid,
    pub material_id: Uuid,
    pub owner: Identity,
}

/// One environment the controller manages: not retired.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerEnvironment {
    pub id: Uuid,
    pub view: EnvironmentView,
    pub invocation: Option<Invocation>,
}

/// What a worker Pod is rendered from.
pub struct WorkerRequest<'a> {
    pub desired: &'a DesiredInstance,
    pub environment: &'a WorkerEnvironment,
    pub backend: &'a AllowedBackend,
}

/// The cluster's view of one instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterView {
    pub observed: Observed,
    /// The current Hermes Pod's home-initialization receipt, if reported.
    pub receipt: Option<String>,
}

/// One apply: the object set, or only the Deployment's scale.
pub struct ApplyRequest<'a> {
    pub desired: &'a DesiredInstance,
    pub apply: Apply,
    pub materials: Option<&'a InstanceMaterials>,
    pub secrets: &'a [DeliveredSecret],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ClusterError {
    /// Refused as invalid or forbidden, for example by admission. Retrying
    /// the same objects does not help.
    #[error("rejected")]
    Rejected,
    /// A precondition or optimistic write lost a race.
    #[error("conflict")]
    Conflict,
    #[error("unavailable")]
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeOutcome {
    Verified,
    Unavailable,
    Invalid,
}

/// PostgreSQL as the controller sees it.
#[async_trait]
pub trait InstanceStore: Send + Sync {
    async fn instance_ids(&self) -> Result<Vec<Uuid>, StoreError>;
    async fn desired_instance(&self, instance: Uuid)
    -> Result<Option<DesiredInstance>, StoreError>;
    async fn materials(&self, instance: Uuid) -> Result<Vec<StoredMaterial>, StoreError>;
    /// Inserts unless a row for `(instance, purpose)` exists; never replaces.
    async fn insert_material(
        &self,
        instance: Uuid,
        purpose: MaterialPurpose,
        material: &NewMaterial,
    ) -> Result<(), StoreError>;
    /// Binds the namespace UID if none is bound.
    async fn bind_namespace(&self, instance: Uuid, uid: &str) -> Result<(), StoreError>;
    /// Writes status only while `epoch` is the current controller epoch.
    /// Returns whether it was written.
    async fn record_status(
        &self,
        epoch: i64,
        instance: Uuid,
        generation: i64,
        status: &InstanceStatus,
    ) -> Result<bool, StoreError>;
    async fn latest_secrets(&self, names: &[String]) -> Result<Vec<SealedSecret>, StoreError>;
    /// Starts a new leadership term.
    async fn advance_epoch(&self) -> Result<i64, StoreError>;
    /// The instance's environments that are not retired.
    async fn worker_environments(
        &self,
        instance: Uuid,
    ) -> Result<Vec<WorkerEnvironment>, StoreError>;
    /// Moves an environment from `from` to `to`, recording the Pod UID on
    /// `Starting` and the SSH address on `Ready`. False when it was not in
    /// `from` any more.
    async fn transition_environment(
        &self,
        environment: Uuid,
        from: EnvironmentState,
        to: EnvironmentState,
        detail: Option<&str>,
    ) -> Result<bool, StoreError>;
    /// Drains a `from` environment because its worker failed, recording why.
    /// False when it was not in `from` any more.
    async fn fail_environment(
        &self,
        environment: Uuid,
        from: EnvironmentState,
        failure: WorkerFailure,
    ) -> Result<bool, StoreError>;
    /// Every instance with its namespace.
    async fn instance_namespaces(&self) -> Result<BTreeMap<Uuid, String>, StoreError>;
    /// Replaces the orphan report while `epoch` is the current controller
    /// epoch. Returns whether it was written.
    async fn record_orphans(&self, epoch: i64, orphans: &[Orphan]) -> Result<bool, StoreError>;
}

/// The cluster: observation and writes for one instance.
#[async_trait]
pub trait Cluster: Send + Sync {
    async fn observe(&self, desired: &DesiredInstance) -> Result<ClusterView, ClusterError>;
    async fn apply(&self, request: &ApplyRequest<'_>) -> Result<(), ClusterError>;
    async fn delete_deployment(
        &self,
        namespace: &str,
        precondition: &Precondition,
    ) -> Result<(), ClusterError>;
    /// Worker Pods of the instance by environment.
    async fn observe_workers(
        &self,
        desired: &DesiredInstance,
    ) -> Result<BTreeMap<Uuid, ObservedWorker>, ClusterError>;
    async fn apply_worker(&self, request: &WorkerRequest<'_>) -> Result<(), ClusterError>;
    async fn delete_worker(
        &self,
        namespace: &str,
        environment: Uuid,
        precondition: &Precondition,
    ) -> Result<(), ClusterError>;
    /// Namespaces and PersistentVolumes carrying the installation label.
    async fn owned_objects(&self) -> Result<Vec<OwnedObject>, ClusterError>;
}

/// Generation, sealing and opening of material and admin secrets. Only the
/// controller holds an implementation.
pub trait Vault: Send + Sync {
    fn generate(&self, instance: Uuid, purpose: MaterialPurpose) -> Result<NewMaterial, AppError>;
    fn open_material(
        &self,
        instance: Uuid,
        purpose: MaterialPurpose,
        sealed: &[u8],
    ) -> Result<Plain, AppError>;
    fn open_secret(&self, name: &str, kind: SecretKind, sealed: &[u8]) -> Result<Plain, AppError>;
}

/// Checks against the running native Hermes.
#[async_trait]
pub trait Native: Send + Sync {
    fn receipt_valid(&self, key: &Plain, binding: &InitBinding, receipt: &str) -> bool;
    async fn probe(&self, namespace: &str) -> ProbeOutcome;
    /// Logs in to a worker with the instance's client key, pinning its host
    /// key, and checks that the shell runs as `uid`.
    async fn ssh_ready(
        &self,
        address: &str,
        client_key: &Plain,
        host_key: &str,
        uid: u32,
    ) -> SshCheck;
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ControllerError {
    #[error("store: {0}")]
    Store(#[from] StoreError),
    #[error("cluster: {0}")]
    Cluster(ClusterError),
    #[error("vault: {0}")]
    Vault(AppError),
    /// Another controller holds a newer epoch; this one must stop writing.
    #[error("deposed")]
    Deposed,
    #[error("fault injected")]
    Fault,
}

/// One reconcile pass's result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reconciled {
    pub plan: Plan,
    /// Reconcile again after this many seconds.
    pub requeue_seconds: Option<u64>,
}

pub struct Controller<S, C, V, N> {
    pub store: S,
    pub cluster: C,
    pub vault: V,
    pub native: N,
    pub settings: Arc<Settings>,
    pub faults: Arc<dyn FaultHook>,
}

impl<S: InstanceStore, C: Cluster, V: Vault, N: Native> Controller<S, C, V, N> {
    fn fault(&self, point: FaultPoint) -> Result<(), ControllerError> {
        self.faults.hit(point).map_err(|_| ControllerError::Fault)
    }

    /// Reconciles one instance once. `None` when the instance does not exist.
    pub async fn reconcile(
        &self,
        epoch: i64,
        instance: Uuid,
    ) -> Result<Option<Reconciled>, ControllerError> {
        let Some(desired) = self.store.desired_instance(instance).await? else {
            return Ok(None);
        };
        let ClusterView {
            mut observed,
            receipt,
        } = self
            .cluster
            .observe(&desired)
            .await
            .map_err(ControllerError::Cluster)?;
        let wanted = desired.wanted();
        if Plan::wants_native_checks(&wanted, &observed) {
            observed.native = self.native_check(&desired, receipt.as_deref()).await?;
        }
        let mut plan = instances::plan(&wanted, &observed);
        if let Some(uid) = &plan.bind_namespace {
            self.store.bind_namespace(instance, uid).await?;
        }
        if let Err(error) = self.apply(&desired, &plan).await {
            match error {
                ControllerError::Cluster(ClusterError::Rejected) => {
                    plan = instances::rejected(&plan)
                }
                other => return Err(other),
            }
        }
        if let Some(precondition) = &plan.delete_deployment {
            match self
                .cluster
                .delete_deployment(&desired.namespace, precondition)
                .await
            {
                // A newer incarnation or an already finished delete: the next
                // observation decides.
                Ok(()) | Err(ClusterError::Conflict) => {}
                Err(error) => return Err(ControllerError::Cluster(error)),
            }
            self.fault(FaultPoint::ControllerAfterDelete)?;
        }
        // Workers live inside the namespace: never touched unless it is ours.
        let owned = !matches!(
            plan.status.blocked,
            Some(BlockedReason::NamespaceConflict | BlockedReason::NamespaceMissing)
        );
        let workers = if owned && observed.namespace.is_some() {
            self.reconcile_environments(&desired).await?
        } else {
            None
        };
        self.fault(FaultPoint::ControllerBeforeStatus)?;
        if !self
            .store
            .record_status(epoch, instance, desired.desired_generation, &plan.status)
            .await?
        {
            return Err(ControllerError::Deposed);
        }
        let requeue_seconds = earliest(plan.requeue_seconds, workers);
        Ok(Some(Reconciled {
            plan,
            requeue_seconds,
        }))
    }

    /// Records the owned objects that belong to no instance; returns how
    /// many there are. Nothing is adopted or deleted.
    pub async fn report_orphans(&self, epoch: i64) -> Result<usize, ControllerError> {
        // The cluster first: a namespace or volume is created only after its
        // instance is recorded, so a later registry read knows every object
        // that was already listed.
        let owned = self
            .cluster
            .owned_objects()
            .await
            .map_err(ControllerError::Cluster)?;
        let recorded = self.store.instance_namespaces().await?;
        let found = orphans::orphans(&owned, &recorded);
        if !self.store.record_orphans(epoch, &found).await? {
            return Err(ControllerError::Deposed);
        }
        Ok(found.len())
    }

    /// One pass over the instance's environments; returns their earliest
    /// requeue.
    async fn reconcile_environments(
        &self,
        desired: &DesiredInstance,
    ) -> Result<Option<u64>, ControllerError> {
        let environments = self.store.worker_environments(desired.instance_id).await?;
        if environments.is_empty() {
            return Ok(None);
        }
        let workers = self
            .cluster
            .observe_workers(desired)
            .await
            .map_err(ControllerError::Cluster)?;
        let mut materials: Option<InstanceMaterials> = None;
        let mut requeue = None;
        for environment in &environments {
            let backend = desired
                .backends
                .get(&environment.view.backend.0)
                .filter(|b| b.version == environment.view.backend.1);
            let context = EnvironmentContext {
                access: desired.access,
                execution_generation: desired.execution_generation,
                grant: &desired.terminal,
                idle_seconds: backend.map_or(900, |b| u64::from(b.definition.limits.idle_seconds)),
            };
            let observed = workers.get(&environment.id);
            let plan = plan_environment(&environment.view, &context, observed);
            let mut transition = plan.transition;
            let mut failure = plan.failure;
            match (&plan.step, backend) {
                (WorkerStep::Nothing, _) => {}
                (WorkerStep::Apply, Some(backend)) => {
                    let request = WorkerRequest {
                        desired,
                        environment,
                        backend,
                    };
                    match self.cluster.apply_worker(&request).await {
                        Ok(()) => {}
                        // An unrenderable or refused worker never starts.
                        Err(ClusterError::Rejected) => {
                            transition = Some(EnvironmentState::Draining);
                            failure = Some(WorkerFailure::ContractFailed);
                        }
                        Err(error) => return Err(ControllerError::Cluster(error)),
                    }
                }
                (WorkerStep::Verify(address), Some(backend)) => {
                    if materials.is_none() {
                        let stored = self.store.materials(desired.instance_id).await?;
                        materials = Some(self.open_materials(desired.instance_id, stored)?);
                    }
                    let opened = materials
                        .as_ref()
                        .ok_or(ControllerError::Vault(AppError::unavailable()))?;
                    let (Some(client), Some(host)) = (
                        opened.plain.get(&MaterialPurpose::SshClient),
                        opened.public.get(&MaterialPurpose::WorkerHost),
                    ) else {
                        return Err(ControllerError::Vault(AppError::unavailable()));
                    };
                    match self
                        .native
                        .ssh_ready(address, client, host, backend.definition.uid)
                        .await
                    {
                        SshCheck::Ready => {
                            self.store
                                .transition_environment(
                                    environment.id,
                                    EnvironmentState::Starting,
                                    EnvironmentState::Ready,
                                    Some(address),
                                )
                                .await?;
                        }
                        SshCheck::NotYet => {}
                        SshCheck::ContractFailed => {
                            transition = Some(EnvironmentState::Draining);
                            failure = Some(WorkerFailure::ContractFailed);
                        }
                    }
                }
                (WorkerStep::Delete(precondition), _) => {
                    match self
                        .cluster
                        .delete_worker(&desired.namespace, environment.id, precondition)
                        .await
                    {
                        Ok(()) | Err(ClusterError::Conflict) => {}
                        Err(error) => return Err(ControllerError::Cluster(error)),
                    }
                }
                // The backend left the assignment: the drain trigger retires it.
                (WorkerStep::Apply | WorkerStep::Verify(_), None) => {
                    transition = Some(EnvironmentState::Draining)
                }
            }
            match (transition, failure) {
                (Some(EnvironmentState::Draining), Some(failure)) => {
                    self.store
                        .fail_environment(environment.id, environment.view.state, failure)
                        .await?;
                }
                (Some(next), _) => {
                    let detail = match next {
                        EnvironmentState::Starting => observed.map(|w| w.uid.as_str()),
                        _ => None,
                    };
                    self.store
                        .transition_environment(
                            environment.id,
                            environment.view.state,
                            next,
                            detail,
                        )
                        .await?;
                }
                (None, _) => {}
            }
            requeue = earliest(requeue, plan.requeue_seconds);
        }
        Ok(requeue)
    }

    async fn apply(&self, desired: &DesiredInstance, plan: &Plan) -> Result<(), ControllerError> {
        let (materials, secrets) = match plan.apply {
            Apply::Nothing => return Ok(()),
            Apply::Scale(_) => (None, Vec::new()),
            Apply::Everything(_) => (
                Some(self.ensure_materials(desired).await?),
                self.secrets(desired).await?,
            ),
        };
        self.fault(FaultPoint::ControllerBeforeApply)?;
        self.cluster
            .apply(&ApplyRequest {
                desired,
                apply: plan.apply,
                materials: materials.as_ref(),
                secrets: &secrets,
            })
            .await
            .map_err(ControllerError::Cluster)?;
        self.fault(FaultPoint::ControllerAfterApply)
    }

    /// Creates every missing material, then opens the stored rows. A lost
    /// race keeps the other writer's row.
    async fn ensure_materials(
        &self,
        desired: &DesiredInstance,
    ) -> Result<InstanceMaterials, ControllerError> {
        let instance = desired.instance_id;
        let stored = self.store.materials(instance).await?;
        for purpose in MaterialPurpose::ALL {
            if stored.iter().any(|row| row.purpose == purpose) {
                continue;
            }
            let material = self
                .vault
                .generate(instance, purpose)
                .map_err(ControllerError::Vault)?;
            self.store
                .insert_material(instance, purpose, &material)
                .await?;
            self.fault(FaultPoint::ControllerAfterMaterialInsert)?;
        }
        self.open_materials(instance, self.store.materials(instance).await?)
    }

    fn open_materials(
        &self,
        instance: Uuid,
        stored: Vec<StoredMaterial>,
    ) -> Result<InstanceMaterials, ControllerError> {
        let mut plain = BTreeMap::new();
        let mut public = BTreeMap::new();
        let mut initialization_id = None;
        for row in stored {
            if row.purpose == MaterialPurpose::Initialization {
                initialization_id = Some(row.id);
            }
            let value = self
                .vault
                .open_material(instance, row.purpose, &row.sealed)
                .map_err(ControllerError::Vault)?;
            plain.insert(row.purpose, value);
            if let Some(key) = row.public {
                public.insert(row.purpose, key);
            }
        }
        let initialization_id =
            initialization_id.ok_or(ControllerError::Vault(AppError::unavailable()))?;
        if plain.len() != MaterialPurpose::ALL.len() {
            return Err(ControllerError::Vault(AppError::unavailable()));
        }
        Ok(InstanceMaterials {
            initialization_id,
            plain,
            public,
        })
    }

    async fn secrets(
        &self,
        desired: &DesiredInstance,
    ) -> Result<Vec<DeliveredSecret>, ControllerError> {
        let mut names: Vec<String> = desired.secrets.iter().map(|r| r.name.clone()).collect();
        // Registry credentials of private backend images.
        names.extend(
            desired
                .backends
                .values()
                .filter_map(|b| b.definition.registry_secret.clone()),
        );
        names.sort();
        names.dedup();
        if names.is_empty() {
            return Ok(Vec::new());
        }
        let sealed = self.store.latest_secrets(&names).await?;
        if sealed.len() != names.len() {
            // Referenced secrets cannot be deleted; a gap means the registry
            // is inconsistent. Retry rather than render without it.
            return Err(ControllerError::Store(StoreError::Unavailable));
        }
        sealed
            .into_iter()
            .map(|secret| {
                let value = self
                    .vault
                    .open_secret(&secret.name, secret.kind, &secret.sealed)
                    .map_err(ControllerError::Vault)?;
                Ok(DeliveredSecret {
                    name: secret.name,
                    kind: secret.kind,
                    version: secret.version,
                    value,
                })
            })
            .collect()
    }

    async fn native_check(
        &self,
        desired: &DesiredInstance,
        receipt: Option<&str>,
    ) -> Result<NativeCheck, ControllerError> {
        let Some(receipt) = receipt else {
            return Ok(NativeCheck::ReceiptMissing);
        };
        let stored = self.store.materials(desired.instance_id).await?;
        let Some(row) = stored
            .iter()
            .find(|row| row.purpose == MaterialPurpose::Initialization)
        else {
            // A running Pod without a stored key was not commissioned from
            // this registry.
            return Ok(NativeCheck::ReceiptInvalid);
        };
        let key = self
            .vault
            .open_material(
                desired.instance_id,
                MaterialPurpose::Initialization,
                &row.sealed,
            )
            .map_err(ControllerError::Vault)?;
        let binding = InitBinding {
            installation_id: self.settings.installation.id,
            instance_id: desired.instance_id,
            material_id: row.id,
            owner: desired.identity.clone(),
        };
        if !self.native.receipt_valid(&key, &binding, receipt) {
            return Ok(NativeCheck::ReceiptInvalid);
        }
        Ok(match self.native.probe(&desired.namespace).await {
            ProbeOutcome::Verified => NativeCheck::Verified,
            ProbeOutcome::Unavailable => NativeCheck::ProbeUnavailable,
            ProbeOutcome::Invalid => NativeCheck::AuthInvalid,
        })
    }
}
