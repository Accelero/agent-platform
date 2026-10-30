//! Fakes of the controller's cluster, vault and native ports.
//!
//! [`FakeCluster`] behaves like a tiny Kubernetes: applies create or update
//! objects, and each observation first advances the "kubelet" by one step, so
//! a rollout takes one pass to finish. It renders nothing; it records what the
//! controller asked for.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use uuid::Uuid;

use crate::app::controller::{
    ApplyRequest, Cluster, ClusterError, ClusterView, DesiredInstance, InitBinding, Native,
    NewMaterial, Plain, ProbeOutcome, Vault, WorkerRequest,
};
use crate::app::error::AppError;
use crate::domain::execution::{ObservedWorker, SshCheck, WorkerFailure};
use crate::domain::instances::{
    Apply, ClaimPhase, NativeCheck, Observed, ObservedDeployment, ObservedNamespace, Precondition,
    Workload,
};
use crate::domain::materials::MaterialPurpose;
use crate::domain::orphans::{OrphanKind, OwnedObject};
use crate::domain::secrets::SecretKind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FakeDeployment {
    pub uid: String,
    pub resource_version: u64,
    pub replicas: i32,
    pub applied_generation: i64,
    pub rolled_out: bool,
}

/// One recorded apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    pub apply: Apply,
    pub generation: i64,
    /// `(name, version, value)` of every delivered secret.
    pub secrets: Vec<(String, i64, Vec<u8>)>,
    /// Plain material values by purpose, for Everything applies.
    pub materials: BTreeMap<MaterialPurpose, Vec<u8>>,
}

#[derive(Debug, Default)]
pub struct World {
    pub namespace: Option<ObservedNamespace>,
    pub deployment: Option<FakeDeployment>,
    pub hermes_pods: u32,
    pub worker_pods: u32,
    pub claims: BTreeMap<String, ClaimPhase>,
    pub receipt: Option<String>,
    /// Refuse every apply, as admission would.
    pub reject: bool,
    /// Report a receipt signed for someone else.
    pub forge_receipt: bool,
    /// Every worker Pod reports this start failure.
    pub worker_failure: Option<WorkerFailure>,
    /// A volume cannot be mounted, as with a missing static directory: the
    /// Pod exists but never starts.
    pub mount_failed: bool,
    /// The home initializer refuses the home, so the Pod never starts Hermes.
    pub home_refused: bool,
    /// Namespaces and volumes with the installation label: the instance's
    /// namespace once applied, and whatever a test adds.
    pub owned: Vec<OwnedObject>,
    pub applies: Vec<Applied>,
    pub deletes: Vec<Precondition>,
    /// Worker Pods by environment: `(uid, resource version, ready)`.
    pub workers: BTreeMap<Uuid, (String, u64, bool)>,
    pub worker_applies: Vec<Uuid>,
    pub worker_deletes: Vec<Uuid>,
    uids: u32,
    init: Option<(Vec<u8>, Uuid, Uuid, String)>,
}

#[derive(Clone, Default)]
pub struct FakeCluster {
    pub world: Arc<Mutex<World>>,
}

impl FakeCluster {
    pub fn with<R>(&self, f: impl FnOnce(&mut World) -> R) -> R {
        f(&mut self.world.lock().expect("world lock"))
    }
}

/// The receipt [`FakeNative`] accepts.
pub fn receipt_for(key: &[u8], material: Uuid, instance: Uuid, owner: &str) -> String {
    let key: String = key.iter().map(|b| format!("{b:02x}")).collect();
    format!("receipt:{key}:{material}:{instance}:{owner}")
}

impl World {
    fn uid(&mut self, kind: &str) -> String {
        self.uids += 1;
        format!("{kind}-uid-{}", self.uids)
    }

    /// One kubelet step.
    fn tick(&mut self) {
        for worker in self.workers.values_mut() {
            worker.2 = true;
        }
        match &mut self.deployment {
            Some(deployment)
                if deployment.replicas == 1 && (self.mount_failed || self.home_refused) =>
            {
                deployment.rolled_out = false;
                self.hermes_pods = 1;
                self.receipt = None;
            }
            Some(deployment) if deployment.replicas == 1 => {
                if !deployment.rolled_out {
                    deployment.rolled_out = true;
                    self.hermes_pods = 1;
                    for phase in self.claims.values_mut() {
                        if *phase == ClaimPhase::Pending {
                            *phase = ClaimPhase::Bound;
                        }
                    }
                    self.receipt = self.init.as_ref().map(|(key, material, instance, owner)| {
                        if self.forge_receipt {
                            receipt_for(key, *material, *instance, "someone-else")
                        } else {
                            receipt_for(key, *material, *instance, owner)
                        }
                    });
                }
            }
            Some(_) | None => {
                self.hermes_pods = 0;
                self.receipt = None;
            }
        }
    }
}

#[async_trait]
impl Cluster for FakeCluster {
    async fn observe(&self, _desired: &DesiredInstance) -> Result<ClusterView, ClusterError> {
        self.with(|world| {
            world.tick();
            let deployment = world.deployment.as_ref().map(|d| ObservedDeployment {
                uid: d.uid.clone(),
                resource_version: d.resource_version.to_string(),
                replicas: d.replicas,
                applied_generation: Some(d.applied_generation),
                rolled_out: d.rolled_out,
            });
            let current_pod_ready = world
                .deployment
                .as_ref()
                .is_some_and(|d| d.replicas == 1 && d.rolled_out)
                && world.hermes_pods == 1;
            Ok(ClusterView {
                observed: Observed {
                    namespace: world.namespace.clone(),
                    deployment,
                    hermes_pods: world.hermes_pods,
                    worker_pods: world.worker_pods + world.workers.len() as u32,
                    current_pod_ready,
                    claims: world.claims.clone(),
                    native: NativeCheck::NotChecked,
                    mount_failed: world.mount_failed && !current_pod_ready,
                    home_refused: world.home_refused && !current_pod_ready,
                },
                receipt: world.receipt.clone(),
            })
        })
    }

    async fn apply(&self, request: &ApplyRequest<'_>) -> Result<(), ClusterError> {
        self.with(|world| {
            if world.reject {
                return Err(ClusterError::Rejected);
            }
            let desired = request.desired;
            let workload = match request.apply {
                Apply::Nothing => return Ok(()),
                Apply::Scale(workload) => {
                    let deployment = world.deployment.as_mut().ok_or(ClusterError::Conflict)?;
                    deployment.replicas = i32::from(workload == Workload::Run);
                    deployment.resource_version += 1;
                    workload
                }
                Apply::Everything(workload) => {
                    if world.namespace.is_none() {
                        let uid = world.uid("namespace");
                        world.owned.push(OwnedObject {
                            kind: OrphanKind::Namespace,
                            name: desired.namespace.clone(),
                            uid: uid.clone(),
                            instance: Some(desired.instance_id),
                        });
                        world.namespace = Some(ObservedNamespace {
                            uid,
                            owned: true,
                            terminating: false,
                        });
                    }
                    for claim in desired.claims() {
                        world.claims.entry(claim).or_insert(ClaimPhase::Pending);
                    }
                    let replicas = i32::from(workload == Workload::Run);
                    let uid = world.uid("deployment");
                    match &mut world.deployment {
                        Some(deployment) => {
                            if deployment.applied_generation != desired.desired_generation
                                || deployment.replicas != replicas
                            {
                                deployment.rolled_out = false;
                                deployment.resource_version += 1;
                            }
                            deployment.applied_generation = desired.desired_generation;
                            deployment.replicas = replicas;
                        }
                        None => {
                            world.deployment = Some(FakeDeployment {
                                uid,
                                resource_version: 1,
                                replicas,
                                applied_generation: desired.desired_generation,
                                rolled_out: false,
                            });
                        }
                    }
                    if let Some(materials) = request.materials {
                        let key = materials.plain[&MaterialPurpose::Initialization]
                            .expose()
                            .to_vec();
                        world.init = Some((
                            key,
                            materials.initialization_id,
                            desired.instance_id,
                            desired.identity.subject.clone(),
                        ));
                    }
                    workload
                }
            };
            let _ = workload;
            world.applies.push(Applied {
                apply: request.apply,
                generation: request.desired.desired_generation,
                secrets: request
                    .secrets
                    .iter()
                    .map(|s| (s.name.clone(), s.version, s.value.expose().to_vec()))
                    .collect(),
                materials: request
                    .materials
                    .map(|m| {
                        m.plain
                            .iter()
                            .map(|(p, v)| (*p, v.expose().to_vec()))
                            .collect()
                    })
                    .unwrap_or_default(),
            });
            Ok(())
        })
    }

    async fn observe_workers(
        &self,
        _desired: &DesiredInstance,
    ) -> Result<BTreeMap<Uuid, ObservedWorker>, ClusterError> {
        Ok(self.with(|world| {
            world
                .workers
                .iter()
                .map(|(id, (uid, version, ready))| {
                    (
                        *id,
                        ObservedWorker {
                            uid: uid.clone(),
                            resource_version: version.to_string(),
                            ready: (*ready && world.worker_failure.is_none())
                                .then(|| "10.42.0.9".to_owned()),
                            gone: world.worker_failure == Some(WorkerFailure::StartFailed),
                            failure: world.worker_failure,
                        },
                    )
                })
                .collect()
        }))
    }

    async fn apply_worker(&self, request: &WorkerRequest<'_>) -> Result<(), ClusterError> {
        self.with(|world| {
            if world.reject {
                return Err(ClusterError::Rejected);
            }
            let uid = world.uid("worker");
            world
                .workers
                .entry(request.environment.id)
                .or_insert((uid, 1, false));
            world.worker_applies.push(request.environment.id);
            Ok(())
        })
    }

    async fn delete_worker(
        &self,
        _namespace: &str,
        environment: Uuid,
        precondition: &Precondition,
    ) -> Result<(), ClusterError> {
        self.with(|world| {
            let (uid, version, _) = world
                .workers
                .get(&environment)
                .ok_or(ClusterError::Conflict)?;
            if *uid != precondition.uid || version.to_string() != precondition.resource_version {
                return Err(ClusterError::Conflict);
            }
            world.workers.remove(&environment);
            world.worker_deletes.push(environment);
            Ok(())
        })
    }

    async fn owned_objects(&self) -> Result<Vec<OwnedObject>, ClusterError> {
        Ok(self.with(|world| world.owned.clone()))
    }

    async fn delete_deployment(
        &self,
        _namespace: &str,
        precondition: &Precondition,
    ) -> Result<(), ClusterError> {
        self.with(|world| {
            let current = world.deployment.as_ref().ok_or(ClusterError::Conflict)?;
            if current.uid != precondition.uid
                || current.resource_version.to_string() != precondition.resource_version
            {
                return Err(ClusterError::Conflict);
            }
            world.deployment = None;
            world.deletes.push(precondition.clone());
            Ok(())
        })
    }
}

/// Seals by prefixing; opens by checking the prefix. Generates distinct
/// values on every call, so a regeneration would be visible.
#[derive(Clone, Default)]
pub struct FakeVault;

impl Vault for FakeVault {
    fn generate(&self, instance: Uuid, purpose: MaterialPurpose) -> Result<NewMaterial, AppError> {
        // The execution credential has its real shape, so the execution API
        // accepts it; everything else is a distinct marker.
        let value = match purpose {
            MaterialPurpose::ExecutionCredential => {
                format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
            }
            _ => Uuid::new_v4().to_string(),
        };
        let public = match purpose {
            MaterialPurpose::ExecutionCredential => {
                crate::app::execution::credential_digest(&value)
            }
            _ => Some(format!("public-{}", purpose.as_str())),
        };
        Ok(NewMaterial {
            sealed: format!("sealed:{instance}:{}:{value}", purpose.as_str()).into_bytes(),
            public,
        })
    }

    fn open_material(
        &self,
        instance: Uuid,
        purpose: MaterialPurpose,
        sealed: &[u8],
    ) -> Result<Plain, AppError> {
        let prefix = format!("sealed:{instance}:{}:", purpose.as_str());
        sealed
            .strip_prefix(prefix.as_bytes())
            .map(|value| Plain::new(value.to_vec()))
            .ok_or_else(AppError::unavailable)
    }

    fn open_secret(
        &self,
        _name: &str,
        _kind: SecretKind,
        sealed: &[u8],
    ) -> Result<Plain, AppError> {
        Ok(Plain::new(sealed.to_vec()))
    }
}

/// A vault whose key is not the one the material was sealed with: it seals
/// as [`FakeVault`] does but opens nothing.
#[derive(Clone, Default)]
pub struct WrongKeyVault;

impl Vault for WrongKeyVault {
    fn generate(&self, instance: Uuid, purpose: MaterialPurpose) -> Result<NewMaterial, AppError> {
        FakeVault.generate(instance, purpose)
    }

    fn open_material(
        &self,
        _instance: Uuid,
        _purpose: MaterialPurpose,
        _sealed: &[u8],
    ) -> Result<Plain, AppError> {
        Err(AppError::unavailable())
    }

    fn open_secret(
        &self,
        _name: &str,
        _kind: SecretKind,
        _sealed: &[u8],
    ) -> Result<Plain, AppError> {
        Err(AppError::unavailable())
    }
}

/// Whether logins succeed, and every `(address, host key, uid)` tried.
pub type SshAttempts = (SshCheck, Vec<(String, String, u32)>);

/// Accepts [`receipt_for`] receipts; the probe answers as configured.
#[derive(Clone)]
pub struct FakeNative {
    pub probe: Arc<Mutex<ProbeOutcome>>,
    /// Whether SSH logins succeed; every attempt records `(address, host key, uid)`.
    pub ssh: Arc<Mutex<SshAttempts>>,
}

impl Default for FakeNative {
    fn default() -> Self {
        Self {
            probe: Arc::new(Mutex::new(ProbeOutcome::Verified)),
            ssh: Arc::new(Mutex::new((SshCheck::Ready, Vec::new()))),
        }
    }
}

#[async_trait]
impl Native for FakeNative {
    fn receipt_valid(&self, key: &Plain, binding: &InitBinding, receipt: &str) -> bool {
        receipt
            == receipt_for(
                key.expose(),
                binding.material_id,
                binding.instance_id,
                &binding.owner.subject,
            )
    }

    async fn probe(&self, _namespace: &str) -> ProbeOutcome {
        *self.probe.lock().expect("probe lock")
    }

    async fn ssh_ready(
        &self,
        address: &str,
        _client_key: &Plain,
        host_key: &str,
        uid: u32,
    ) -> SshCheck {
        let mut ssh = self.ssh.lock().expect("ssh lock");
        ssh.1.push((address.to_owned(), host_key.to_owned(), uid));
        ssh.0
    }
}
