//! In-memory store with optimistic transactions.
//!
//! A transaction works on a copy of the state and commits only if no other
//! transaction committed since it began; otherwise it fails with
//! [`StoreError::Conflict`], which the envelope retries. That gives the same
//! observable results as PostgreSQL row locks for every contract case.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use uuid::Uuid;

use super::Inspect;
use crate::app::actor::Actor;
use crate::app::catalog::Definition;
use crate::app::controller::WorkerEnvironment;
use crate::app::controller::{
    AllowedBackend, DesiredInstance, InstanceStore, NewMaterial, SealedSecret, StoredMaterial,
};
use crate::app::error::StoreError;
use crate::app::execution::{
    BackendLimits, Client, EnvironmentRow, ExecutionStore, ExecutionTx, LeaseRow, NewEnvironment,
    StoredRequest,
};
use crate::app::ports::{
    AccessReader, AccessTx, AssignmentRecord, AssignmentTx, AuditEvent, CatalogEntry, CatalogTx,
    IdentityRecord, KernelTx, Reservation, SecretRecord, SecretTx, Store,
};
use crate::app::routing::RouteStore;
use crate::domain::access::Grant;
use crate::domain::assignments::{Access, LifetimeMode};
use crate::domain::backends::BackendDefinition;
use crate::domain::catalog::{CatalogKind, VersionStatus};
use crate::domain::execution::{
    DRAIN_DEADLINE_SECONDS, EnvironmentState, EnvironmentView, Invocation, LeaseState,
    WorkerFailure,
};
use crate::domain::identity::Identity;
use crate::domain::installation::Installation;
use crate::domain::instances::{InstanceStatus, Phase};
use crate::domain::materials::MaterialPurpose;
use crate::domain::orphans::Orphan;
use crate::domain::routing::RouteRow;
use crate::domain::secrets::SecretKind;
use crate::domain::storage::VolumeSpec;
use crate::domain::templates::TemplateDefinition;

type ReceiptKey = (String, &'static str, String);

/// A secret's kind and its `(version, sealed value)` history.
type SecretVersions = (SecretKind, Vec<(i64, Vec<u8>)>);

#[derive(Clone, Default)]
struct State {
    installation: Option<Installation>,
    receipts: BTreeMap<ReceiptKey, ([u8; 32], Option<Vec<u8>>)>,
    audit: Vec<AuditEvent>,
    identities: BTreeMap<Identity, IdentityRecord>,
    grants: BTreeMap<Uuid, Grant>,
    notifications: Vec<Uuid>,
    catalog: BTreeMap<(&'static str, String, i64), (VersionStatus, serde_json::Value)>,
    assignments: BTreeMap<Uuid, AssignmentRecord>,
    instances: BTreeMap<Uuid, Instance>,
    secrets: BTreeMap<String, SecretVersions>,
    materials: BTreeMap<(Uuid, MaterialPurpose), StoredMaterial>,
    epoch: i64,
    environments: BTreeMap<Uuid, MemEnvironment>,
    leases: BTreeMap<Uuid, MemLease>,
    requests: BTreeMap<(Uuid, String), StoredRequest>,
    orphans: Vec<Orphan>,
    /// Seconds of fake database time.
    clock: i64,
}

#[derive(Clone)]
struct MemEnvironment {
    instance: Uuid,
    state: EnvironmentState,
    mode: LifetimeMode,
    backend: (String, i64),
    execution_generation: i64,
    key: String,
    invocation: Option<Invocation>,
    pod_uid: Option<String>,
    ssh_host: Option<String>,
    created: (i64, u64),
    last_used: i64,
    drain_deadline: Option<i64>,
    failure: Option<WorkerFailure>,
}

#[derive(Clone)]
struct MemLease {
    instance: Uuid,
    environment: Uuid,
    state: LeaseState,
    created: i64,
    expires: i64,
}

#[derive(Clone)]
struct Instance {
    namespace: String,
    namespace_uid: Option<String>,
    desired_generation: i64,
    execution_generation: i64,
    status: Option<(i64, InstanceStatus)>,
    observed_generation: i64,
}

impl State {
    fn template(&self, pin: &crate::domain::catalog::VersionRef) -> Option<TemplateDefinition> {
        let (_, value) =
            self.catalog
                .get(&(CatalogKind::Template.as_str(), pin.id.clone(), pin.version))?;
        serde_json::from_value(value.clone()).ok()
    }

    fn backend(&self, id: &str, version: i64) -> Option<BackendDefinition> {
        let (_, value) = self.catalog.get(&(
            CatalogKind::TerminalBackend.as_str(),
            id.to_owned(),
            version,
        ))?;
        serde_json::from_value(value.clone()).ok()
    }

    /// Whether an assignment (through its own references, pinned template or
    /// allowed backends) uses the secret.
    fn assignment_uses(&self, record: &AssignmentRecord, name: &str) -> bool {
        let spec = &record.spec;
        spec.secrets.iter().any(|r| r.name == name)
            || self
                .template(&spec.template)
                .is_some_and(|t| t.secret_uses().iter().any(|u| u.name == name))
            || spec.terminal.allowed.iter().any(|(id, version)| {
                self.backend(id, *version)
                    .is_some_and(|b| b.secret_uses().iter().any(|u| u.name == name))
            })
    }
}

struct Shared {
    version: u64,
    state: State,
}

#[derive(Clone)]
pub struct MemStore {
    shared: Arc<Mutex<Shared>>,
}

impl MemStore {
    /// An initialized, empty registry for `installation`.
    pub fn new(installation: Installation) -> Self {
        let state = State {
            installation: Some(installation),
            ..State::default()
        };
        Self {
            shared: Arc::new(Mutex::new(Shared { version: 0, state })),
        }
    }

    fn read<R>(&self, read: impl FnOnce(&State) -> R) -> R {
        read(&self.shared.lock().expect("store lock").state)
    }
}

pub struct MemTx {
    shared: Arc<Mutex<Shared>>,
    base: u64,
    state: State,
}

#[async_trait]
impl Store for MemStore {
    type Tx = MemTx;

    async fn begin(&self) -> Result<MemTx, StoreError> {
        let shared = self.shared.lock().expect("store lock");
        Ok(MemTx {
            shared: self.shared.clone(),
            base: shared.version,
            state: shared.state.clone(),
        })
    }
}

fn actor_key(actor: &Actor) -> String {
    match actor {
        Actor::Operator => "operator".to_owned(),
        Actor::Administrator(identity) => {
            format!("administrator\0{}\0{}", identity.issuer, identity.subject)
        }
    }
}

#[async_trait]
impl KernelTx for MemTx {
    async fn installation(&mut self) -> Result<Option<Installation>, StoreError> {
        Ok(self.state.installation.clone())
    }

    async fn reserve_receipt(
        &mut self,
        actor: &Actor,
        action: &'static str,
        key: &str,
        digest: [u8; 32],
    ) -> Result<Reservation, StoreError> {
        let key = (actor_key(actor), action, key.to_owned());
        match self.state.receipts.get(&key) {
            Some((stored, _)) if *stored != digest => Ok(Reservation::Conflict),
            Some((_, Some(receipt))) => Ok(Reservation::Replay(receipt.clone())),
            Some((_, None)) => Ok(Reservation::Fresh),
            None => {
                self.state.receipts.insert(key, (digest, None));
                Ok(Reservation::Fresh)
            }
        }
    }

    async fn store_receipt(
        &mut self,
        actor: &Actor,
        action: &'static str,
        key: &str,
        receipt: Vec<u8>,
    ) -> Result<(), StoreError> {
        let key = (actor_key(actor), action, key.to_owned());
        let entry = self
            .state
            .receipts
            .get_mut(&key)
            .ok_or(StoreError::Unavailable)?;
        if entry.1.is_some() {
            return Err(StoreError::Unavailable);
        }
        entry.1 = Some(receipt);
        Ok(())
    }

    async fn append_audit(&mut self, event: AuditEvent) -> Result<(), StoreError> {
        self.state.audit.push(event);
        Ok(())
    }

    async fn notify_instance(&mut self, instance: Uuid) -> Result<(), StoreError> {
        self.state.notifications.push(instance);
        Ok(())
    }

    async fn commit(self) -> Result<(), StoreError> {
        let mut shared = self.shared.lock().expect("store lock");
        if shared.version != self.base {
            return Err(StoreError::Conflict);
        }
        shared.version += 1;
        shared.state = self.state;
        Ok(())
    }
}

#[async_trait]
impl AccessTx for MemTx {
    async fn identity(
        &mut self,
        identity: &Identity,
    ) -> Result<Option<IdentityRecord>, StoreError> {
        Ok(self.state.identities.get(identity).copied())
    }

    async fn ensure_identity(&mut self, identity: &Identity) -> Result<IdentityRecord, StoreError> {
        Ok(*self
            .state
            .identities
            .entry(identity.clone())
            .or_insert(IdentityRecord {
                id: Uuid::new_v4(),
                verified: false,
            }))
    }

    async fn grant(&mut self, identity: Uuid) -> Result<Option<Grant>, StoreError> {
        Ok(self.state.grants.get(&identity).copied())
    }

    async fn grant_count(&mut self) -> Result<usize, StoreError> {
        Ok(self.state.grants.len())
    }

    async fn lock_active_managers(&mut self) -> Result<usize, StoreError> {
        Ok(self.state.grants.values().filter(|g| g.manages()).count())
    }

    async fn put_grant(&mut self, identity: Uuid, grant: Grant) -> Result<(), StoreError> {
        self.state.grants.insert(identity, grant);
        Ok(())
    }
}

#[async_trait]
impl CatalogTx for MemTx {
    async fn catalog_version<D: Definition>(
        &mut self,
        id: &str,
        version: i64,
    ) -> Result<Option<CatalogEntry<D>>, StoreError> {
        let Some((status, value)) =
            self.state
                .catalog
                .get(&(D::KIND.as_str(), id.to_owned(), version))
        else {
            return Ok(None);
        };
        let definition =
            serde_json::from_value(value.clone()).map_err(|_| StoreError::Unavailable)?;
        Ok(Some(CatalogEntry {
            status: *status,
            definition,
        }))
    }

    async fn last_catalog_version(
        &mut self,
        kind: CatalogKind,
        id: &str,
    ) -> Result<i64, StoreError> {
        Ok(self
            .state
            .catalog
            .keys()
            .filter(|(k, i, _)| *k == kind.as_str() && i == id)
            .map(|(_, _, version)| *version)
            .max()
            .unwrap_or(0))
    }

    async fn put_catalog_version<D: Definition>(
        &mut self,
        id: &str,
        version: i64,
        entry: &CatalogEntry<D>,
    ) -> Result<(), StoreError> {
        let value = serde_json::to_value(&entry.definition).map_err(|_| StoreError::Unavailable)?;
        self.state.catalog.insert(
            (D::KIND.as_str(), id.to_owned(), version),
            (entry.status, value),
        );
        Ok(())
    }
}

#[async_trait]
impl AssignmentTx for MemTx {
    async fn assignment(&mut self, id: Uuid) -> Result<Option<AssignmentRecord>, StoreError> {
        Ok(self.state.assignments.get(&id).cloned())
    }

    async fn assignment_of(&mut self, identity: Uuid) -> Result<Option<Uuid>, StoreError> {
        Ok(self
            .state
            .assignments
            .values()
            .find(|record| record.identity_id == identity)
            .map(|record| record.id))
    }

    async fn insert_assignment(
        &mut self,
        record: &AssignmentRecord,
        namespace: &str,
    ) -> Result<(), StoreError> {
        self.state.assignments.insert(record.id, record.clone());
        self.state.instances.insert(
            record.instance_id,
            Instance {
                namespace: namespace.to_owned(),
                namespace_uid: None,
                desired_generation: 1,
                execution_generation: 1,
                status: None,
                observed_generation: 0,
            },
        );
        Ok(())
    }

    async fn update_assignment(&mut self, record: &AssignmentRecord) -> Result<(), StoreError> {
        self.state.assignments.insert(record.id, record.clone());
        Ok(())
    }

    async fn advance_instance(
        &mut self,
        instance: Uuid,
        execution: bool,
    ) -> Result<(), StoreError> {
        let instance = self
            .state
            .instances
            .get_mut(&instance)
            .ok_or(StoreError::Unavailable)?;
        instance.desired_generation += 1;
        if execution {
            instance.execution_generation += 1;
        }
        Ok(())
    }

    async fn lock_source_volumes(
        &mut self,
        sources: &[String],
        except: Option<Uuid>,
    ) -> Result<Vec<VolumeSpec>, StoreError> {
        Ok(self
            .state
            .assignments
            .values()
            .filter(|record| Some(record.id) != except)
            .flat_map(|record| record.spec.volumes.values())
            .filter(|volume| volume.path.is_some() && sources.contains(&volume.source))
            .cloned()
            .collect())
    }
}

#[async_trait]
impl SecretTx for MemTx {
    async fn secret(&mut self, name: &str) -> Result<Option<SecretRecord>, StoreError> {
        Ok(self
            .state
            .secrets
            .get(name)
            .map(|(kind, versions)| SecretRecord {
                kind: *kind,
                version: versions.last().map(|(v, _)| *v).unwrap_or(0),
            }))
    }

    async fn put_secret_version(
        &mut self,
        name: &str,
        record: SecretRecord,
        sealed: &[u8],
    ) -> Result<(), StoreError> {
        let entry = self
            .state
            .secrets
            .entry(name.to_owned())
            .or_insert_with(|| (record.kind, Vec::new()));
        entry.1.push((record.version, sealed.to_vec()));
        Ok(())
    }

    async fn delete_secret(&mut self, name: &str) -> Result<(), StoreError> {
        self.state.secrets.remove(name);
        Ok(())
    }

    async fn secret_referenced(&mut self, name: &str) -> Result<bool, StoreError> {
        let in_catalog = self.state.catalog.iter().any(|((kind, _, _), (_, value))| {
            let uses = if *kind == CatalogKind::Template.as_str() {
                serde_json::from_value::<TemplateDefinition>(value.clone()).map(|d| d.secret_uses())
            } else {
                serde_json::from_value::<BackendDefinition>(value.clone()).map(|d| d.secret_uses())
            };
            uses.is_ok_and(|uses| uses.iter().any(|u| u.name == name))
        });
        let in_assignments = self
            .state
            .assignments
            .values()
            .any(|record| record.spec.secrets.iter().any(|r| r.name == name));
        Ok(in_catalog || in_assignments)
    }

    async fn instances_using_secret(&mut self, name: &str) -> Result<Vec<Uuid>, StoreError> {
        Ok(self
            .state
            .assignments
            .values()
            .filter(|record| self.state.assignment_uses(record, name))
            .map(|record| record.instance_id)
            .collect())
    }
}

#[async_trait]
impl AccessReader for MemStore {
    async fn grant_of(&self, identity: &Identity) -> Result<Option<Grant>, StoreError> {
        Ok(self.read(|s| {
            let record = s.identities.get(identity)?;
            s.grants.get(&record.id).copied()
        }))
    }

    async fn record_login(&self, identity: &Identity) -> Result<(), StoreError> {
        self.mark_verified(identity).await;
        Ok(())
    }
}

#[async_trait]
impl RouteStore for MemStore {
    async fn route_row(&self, identity: &Identity) -> Result<Option<RouteRow>, StoreError> {
        Ok(self.read(|s| {
            let record = s.identities.get(identity)?;
            let assignment = s
                .assignments
                .values()
                .find(|a| a.identity_id == record.id)?;
            let instance = s.instances.get(&assignment.instance_id)?;
            Some(RouteRow {
                instance_id: assignment.instance_id,
                namespace: instance.namespace.clone(),
                namespace_bound: instance.namespace_uid.is_some(),
                access: assignment.spec.access,
                phase: instance
                    .status
                    .as_ref()
                    .map_or(Phase::Provisioning, |(_, status)| status.phase),
                desired_generation: instance.desired_generation,
                observed_generation: instance.observed_generation,
                assignment_version: assignment.version,
            })
        }))
    }
}

#[async_trait]
impl Inspect for MemStore {
    async fn audit_actions(&self) -> Vec<String> {
        self.read(|s| s.audit.iter().map(|e| e.action.to_owned()).collect())
    }

    async fn receipt_count(&self) -> usize {
        self.read(|s| s.receipts.len())
    }

    async fn grant_of(&self, identity: &Identity) -> Option<Grant> {
        self.read(|s| {
            let record = s.identities.get(identity)?;
            s.grants.get(&record.id).copied()
        })
    }

    async fn active_managers(&self) -> usize {
        self.read(|s| s.grants.values().filter(|g| g.manages()).count())
    }

    async fn instance_generations(&self, instance: Uuid) -> (i64, i64) {
        self.read(|s| {
            let instance = &s.instances[&instance];
            (instance.desired_generation, instance.execution_generation)
        })
    }

    async fn instance_namespace(&self, instance: Uuid) -> String {
        self.read(|s| s.instances[&instance].namespace.clone())
    }

    async fn secret_versions(&self, name: &str) -> Vec<(i64, Vec<u8>)> {
        self.read(|s| {
            s.secrets
                .get(name)
                .map(|(_, v)| v.clone())
                .unwrap_or_default()
        })
    }

    async fn instance_status(&self, instance: Uuid) -> (String, Option<String>, i64) {
        self.read(|s| {
            let row = &s.instances[&instance];
            match &row.status {
                Some((_, status)) => (
                    status.phase.as_str().to_owned(),
                    status.blocked.map(|b| b.as_str().to_owned()),
                    row.observed_generation,
                ),
                None => ("provisioning".to_owned(), None, row.observed_generation),
            }
        })
    }

    async fn advance_time(&self, seconds: i64) {
        self.advance(seconds);
    }

    async fn orphans(&self) -> Vec<Orphan> {
        self.read(|s| s.orphans.clone())
    }

    async fn mark_verified(&self, identity: &Identity) {
        let mut shared = self.shared.lock().expect("store lock");
        shared.version += 1;
        shared
            .state
            .identities
            .entry(identity.clone())
            .or_insert(IdentityRecord {
                id: Uuid::new_v4(),
                verified: false,
            })
            .verified = true;
    }
}

impl MemStore {
    fn write<R>(&self, write: impl FnOnce(&mut State) -> R) -> R {
        let mut shared = self.shared.lock().expect("store lock");
        shared.version += 1;
        write(&mut shared.state)
    }
}

#[async_trait]
impl InstanceStore for MemStore {
    async fn instance_ids(&self) -> Result<Vec<Uuid>, StoreError> {
        Ok(self.read(|s| s.instances.keys().copied().collect()))
    }

    async fn desired_instance(
        &self,
        instance: Uuid,
    ) -> Result<Option<DesiredInstance>, StoreError> {
        Ok(self.read(|s| {
            let row = s.instances.get(&instance)?;
            let assignment = s.assignments.values().find(|a| a.instance_id == instance)?;
            let identity = s
                .identities
                .iter()
                .find(|(_, record)| record.id == assignment.identity_id)
                .map(|(identity, _)| identity.clone())?;
            let definition = s.template(&assignment.spec.template)?;
            let mut admin_config = definition.admin_config.clone();
            crate::domain::settings::merge(&mut admin_config, &assignment.spec.admin_config);
            let secrets = definition
                .secrets
                .iter()
                .chain(&assignment.spec.secrets)
                .cloned()
                .collect();
            let backends = assignment
                .spec
                .terminal
                .allowed
                .iter()
                .filter_map(|(id, version)| {
                    let definition = s.backend(id, *version)?;
                    Some((
                        id.clone(),
                        AllowedBackend {
                            version: *version,
                            definition,
                        },
                    ))
                })
                .collect();
            Some(DesiredInstance {
                instance_id: instance,
                assignment_id: assignment.id,
                identity,
                namespace: row.namespace.clone(),
                namespace_uid: row.namespace_uid.clone(),
                access: assignment.spec.access,
                desired_generation: row.desired_generation,
                execution_generation: row.execution_generation,
                template: assignment.spec.template.clone(),
                definition,
                volumes: assignment.spec.volumes.clone(),
                terminal: assignment.spec.terminal.clone(),
                admin_config,
                secrets,
                backends,
            })
        }))
    }

    async fn materials(&self, instance: Uuid) -> Result<Vec<StoredMaterial>, StoreError> {
        Ok(self.read(|s| {
            s.materials
                .iter()
                .filter(|((owner, _), _)| *owner == instance)
                .map(|(_, row)| row.clone())
                .collect()
        }))
    }

    async fn insert_material(
        &self,
        instance: Uuid,
        purpose: MaterialPurpose,
        material: &NewMaterial,
    ) -> Result<(), StoreError> {
        self.write(|s| {
            s.materials
                .entry((instance, purpose))
                .or_insert_with(|| StoredMaterial {
                    id: Uuid::new_v4(),
                    purpose,
                    sealed: material.sealed.clone(),
                    public: material.public.clone(),
                });
        });
        Ok(())
    }

    async fn bind_namespace(&self, instance: Uuid, uid: &str) -> Result<(), StoreError> {
        self.write(|s| {
            if let Some(row) = s.instances.get_mut(&instance)
                && row.namespace_uid.is_none()
            {
                row.namespace_uid = Some(uid.to_owned());
            }
        });
        Ok(())
    }

    async fn record_status(
        &self,
        epoch: i64,
        instance: Uuid,
        generation: i64,
        status: &InstanceStatus,
    ) -> Result<bool, StoreError> {
        Ok(self.write(|s| {
            if epoch != s.epoch {
                return false;
            }
            let Some(row) = s.instances.get_mut(&instance) else {
                return false;
            };
            if status.converged {
                row.observed_generation = row.observed_generation.max(generation);
            }
            row.status = Some((epoch, status.clone()));
            true
        }))
    }

    async fn latest_secrets(&self, names: &[String]) -> Result<Vec<SealedSecret>, StoreError> {
        Ok(self.read(|s| {
            names
                .iter()
                .filter_map(|name| {
                    let (kind, versions) = s.secrets.get(name)?;
                    let (version, sealed) = versions.last()?;
                    Some(SealedSecret {
                        name: name.clone(),
                        kind: *kind,
                        version: *version,
                        sealed: sealed.clone(),
                    })
                })
                .collect()
        }))
    }

    async fn advance_epoch(&self) -> Result<i64, StoreError> {
        Ok(self.write(|s| {
            s.epoch += 1;
            s.epoch
        }))
    }

    async fn worker_environments(
        &self,
        instance: Uuid,
    ) -> Result<Vec<WorkerEnvironment>, StoreError> {
        Ok(self.read(|s| {
            s.environments
                .iter()
                .filter(|(_, e)| e.instance == instance && e.state != EnvironmentState::Retired)
                .map(|(id, e)| self.worker_view(*id, e, s))
                .collect()
        }))
    }

    async fn transition_environment(
        &self,
        environment: Uuid,
        from: EnvironmentState,
        to: EnvironmentState,
        detail: Option<&str>,
    ) -> Result<bool, StoreError> {
        Ok(self.write(|s| {
            let clock = s.clock;
            let Some(e) = s.environments.get_mut(&environment) else {
                return false;
            };
            if e.state != from || !from.allows(to) {
                return false;
            }
            e.state = to;
            match to {
                EnvironmentState::Starting => e.pod_uid = detail.map(str::to_owned),
                EnvironmentState::Ready => e.ssh_host = detail.map(str::to_owned),
                EnvironmentState::Draining => {
                    e.drain_deadline
                        .get_or_insert(clock + i64::from(DRAIN_DEADLINE_SECONDS));
                }
                EnvironmentState::Pending | EnvironmentState::Retired => {}
            }
            true
        }))
    }

    async fn fail_environment(
        &self,
        environment: Uuid,
        from: EnvironmentState,
        failure: WorkerFailure,
    ) -> Result<bool, StoreError> {
        Ok(self.write(|s| {
            let clock = s.clock;
            let Some(e) = s.environments.get_mut(&environment) else {
                return false;
            };
            if e.state != from
                || !matches!(from, EnvironmentState::Pending | EnvironmentState::Starting)
            {
                return false;
            }
            e.state = EnvironmentState::Draining;
            e.failure = Some(failure);
            e.drain_deadline
                .get_or_insert(clock + i64::from(DRAIN_DEADLINE_SECONDS));
            true
        }))
    }

    async fn instance_namespaces(&self) -> Result<BTreeMap<Uuid, String>, StoreError> {
        Ok(self.read(|s| {
            s.instances
                .iter()
                .map(|(id, row)| (*id, row.namespace.clone()))
                .collect()
        }))
    }

    async fn record_orphans(&self, epoch: i64, orphans: &[Orphan]) -> Result<bool, StoreError> {
        Ok(self.write(|s| {
            if epoch != s.epoch {
                return false;
            }
            s.orphans = orphans.to_vec();
            true
        }))
    }
}

impl State {
    fn live_leases(&self, environment: Uuid) -> i64 {
        self.leases
            .values()
            .filter(|l| {
                l.environment == environment
                    && l.state == LeaseState::Active
                    && l.expires > self.clock
            })
            .count() as i64
    }

    fn environment_row(&self, id: Uuid) -> Option<EnvironmentRow> {
        let e = self.environments.get(&id)?;
        Some(EnvironmentRow {
            id,
            state: e.state,
            mode: e.mode,
            backend: e.backend.clone(),
            execution_generation: e.execution_generation,
            key: e.key.clone(),
            pod_uid: e.pod_uid.clone(),
            ssh_host: e.ssh_host.clone(),
            drain_expired: e.drain_deadline.is_some_and(|d| d <= self.clock),
            live_leases: self.live_leases(id),
            failure: e.failure,
        })
    }

    fn expire(&mut self, instance: Option<Uuid>) -> u64 {
        let clock = self.clock;
        let mut expired = 0;
        for lease in self.leases.values_mut() {
            if instance.is_none_or(|i| i == lease.instance)
                && lease.state == LeaseState::Active
                && lease.expires <= clock
            {
                lease.state = LeaseState::Expired;
                expired += 1;
            }
        }
        expired
    }
}

impl MemStore {
    /// Advances fake database time.
    pub fn advance(&self, seconds: i64) {
        self.write(|s| s.clock += seconds);
    }
}

#[async_trait]
impl ExecutionTx for MemTx {
    async fn authenticate(
        &mut self,
        credential_digest: &str,
    ) -> Result<Option<Client>, StoreError> {
        let s = &self.state;
        let Some((instance, _)) = s.materials.keys().find(|(instance, purpose)| {
            *purpose == MaterialPurpose::ExecutionCredential
                && s.materials[&(*instance, *purpose)].public.as_deref() == Some(credential_digest)
        }) else {
            return Ok(None);
        };
        let instance = *instance;
        let Some(assignment) = s.assignments.values().find(|a| a.instance_id == instance) else {
            return Ok(None);
        };
        if assignment.spec.access != Access::Enabled {
            return Ok(None);
        }
        let limits = assignment
            .spec
            .terminal
            .allowed
            .iter()
            .filter_map(|(id, version)| {
                let backend = s.backend(id, *version)?;
                Some((
                    id.clone(),
                    BackendLimits {
                        max_lease_seconds: backend.limits.max_lease_seconds,
                        max_environments: backend.limits.max_environments,
                    },
                ))
            })
            .collect();
        Ok(Some(Client {
            instance_id: instance,
            execution_generation: s.instances[&instance].execution_generation,
            grant: assignment.spec.terminal.clone(),
            limits,
        }))
    }

    async fn expire_due(&mut self, instance: Uuid) -> Result<(), StoreError> {
        self.state.expire(Some(instance));
        Ok(())
    }

    async fn request(
        &mut self,
        instance: Uuid,
        request: &str,
    ) -> Result<Option<StoredRequest>, StoreError> {
        Ok(self
            .state
            .requests
            .get(&(instance, request.to_owned()))
            .cloned())
    }

    async fn put_request(
        &mut self,
        instance: Uuid,
        request: &str,
        digest: &[u8],
        environment: Uuid,
    ) -> Result<(), StoreError> {
        let key = (instance, request.to_owned());
        if self.state.requests.contains_key(&key) {
            return Err(StoreError::Conflict);
        }
        self.state.requests.insert(
            key,
            StoredRequest {
                digest: digest.to_vec(),
                environment,
                lease: None,
            },
        );
        Ok(())
    }

    async fn grant_request(
        &mut self,
        instance: Uuid,
        request: &str,
        lease: Uuid,
    ) -> Result<(), StoreError> {
        let stored = self
            .state
            .requests
            .get_mut(&(instance, request.to_owned()))
            .ok_or(StoreError::Unavailable)?;
        stored.lease = Some(lease);
        Ok(())
    }

    async fn environment(
        &mut self,
        instance: Uuid,
        id: Uuid,
    ) -> Result<Option<EnvironmentRow>, StoreError> {
        Ok(self
            .state
            .environments
            .get(&id)
            .filter(|e| e.instance == instance)
            .and_then(|_| self.state.environment_row(id)))
    }

    async fn live_environment(
        &mut self,
        instance: Uuid,
        backend: &(String, i64),
        execution_generation: i64,
        mode: LifetimeMode,
        key: &str,
    ) -> Result<Option<EnvironmentRow>, StoreError> {
        let found = self.state.environments.iter().find(|(_, e)| {
            e.instance == instance
                && e.state != EnvironmentState::Retired
                && &e.backend == backend
                && e.execution_generation == execution_generation
                && e.mode == mode
                && e.key == key
        });
        Ok(found.and_then(|(id, _)| self.state.environment_row(*id)))
    }

    async fn count_environments(
        &mut self,
        instance: Uuid,
        backend: &str,
    ) -> Result<(u32, u32), StoreError> {
        let live: Vec<_> = self
            .state
            .environments
            .values()
            .filter(|e| e.instance == instance && e.state != EnvironmentState::Retired)
            .collect();
        let same = live.iter().filter(|e| e.backend.0 == backend).count();
        Ok((same as u32, live.len() as u32))
    }

    async fn insert_environment(&mut self, environment: &NewEnvironment) -> Result<(), StoreError> {
        let order = self.state.environments.len() as u64;
        self.state.environments.insert(
            environment.id,
            MemEnvironment {
                instance: environment.instance_id,
                state: EnvironmentState::Pending,
                mode: environment.mode,
                backend: environment.backend.clone(),
                execution_generation: environment.execution_generation,
                key: environment.key.clone(),
                invocation: environment.invocation.clone(),
                pod_uid: None,
                ssh_host: None,
                created: (self.state.clock, order),
                last_used: self.state.clock,
                drain_deadline: None,
                failure: None,
            },
        );
        Ok(())
    }

    async fn touch_environment(&mut self, id: Uuid) -> Result<(), StoreError> {
        let clock = self.state.clock;
        if let Some(environment) = self.state.environments.get_mut(&id) {
            environment.last_used = clock;
        }
        Ok(())
    }

    async fn drain_session(
        &mut self,
        instance: Uuid,
        backend: &str,
        key: &str,
    ) -> Result<(), StoreError> {
        let deadline = self.state.clock + i64::from(DRAIN_DEADLINE_SECONDS);
        for environment in self.state.environments.values_mut() {
            if environment.instance == instance
                && environment.mode == LifetimeMode::PerSession
                && environment.backend.0 == backend
                && environment.key == key
                && environment.state.allows(EnvironmentState::Draining)
            {
                environment.state = EnvironmentState::Draining;
                environment.drain_deadline.get_or_insert(deadline);
            }
        }
        Ok(())
    }

    async fn lease(&mut self, instance: Uuid, id: Uuid) -> Result<Option<LeaseRow>, StoreError> {
        Ok(self
            .state
            .leases
            .get(&id)
            .filter(|l| l.instance == instance)
            .map(|l| LeaseRow {
                id,
                environment: l.environment,
                active: l.state == LeaseState::Active,
                remaining: l.expires - self.state.clock,
            }))
    }

    async fn insert_lease(
        &mut self,
        id: Uuid,
        instance: Uuid,
        environment: Uuid,
        _parent: Option<Uuid>,
        ttl: u32,
    ) -> Result<(), StoreError> {
        let clock = self.state.clock;
        self.state.leases.insert(
            id,
            MemLease {
                instance,
                environment,
                state: LeaseState::Active,
                created: clock,
                expires: clock + i64::from(ttl),
            },
        );
        Ok(())
    }

    async fn extend_lease(
        &mut self,
        id: Uuid,
        ttl: u32,
        max_seconds: u32,
    ) -> Result<Option<i64>, StoreError> {
        let clock = self.state.clock;
        let Some(lease) = self.state.leases.get_mut(&id) else {
            return Ok(None);
        };
        if lease.state != LeaseState::Active || lease.expires <= clock {
            return Ok(None);
        }
        let target = (clock + i64::from(ttl)).min(lease.created + i64::from(max_seconds));
        lease.expires = lease.expires.max(target);
        Ok(Some(lease.expires - clock))
    }

    async fn release_lease(&mut self, id: Uuid) -> Result<(), StoreError> {
        if let Some(lease) = self.state.leases.get_mut(&id)
            && lease.state == LeaseState::Active
        {
            lease.state = LeaseState::Released;
        }
        Ok(())
    }

    async fn environments(&mut self, instance: Uuid) -> Result<Vec<EnvironmentRow>, StoreError> {
        let mut ids: Vec<_> = self
            .state
            .environments
            .iter()
            .filter(|(_, e)| e.instance == instance)
            .map(|(id, e)| (e.created, *id))
            .collect();
        ids.sort();
        Ok(ids
            .into_iter()
            .filter_map(|(_, id)| self.state.environment_row(id))
            .collect())
    }

    async fn worker_host_key(&mut self, instance: Uuid) -> Result<Option<String>, StoreError> {
        Ok(self
            .state
            .materials
            .get(&(instance, MaterialPurpose::WorkerHost))
            .and_then(|m| m.public.clone()))
    }

    async fn notify(&mut self, instance: Uuid) -> Result<(), StoreError> {
        self.state.notifications.push(instance);
        Ok(())
    }

    async fn commit(self) -> Result<(), StoreError> {
        KernelTx::commit(self).await
    }
}

#[async_trait]
impl ExecutionStore for MemStore {
    type Tx = MemTx;

    async fn begin_execution(&self) -> Result<MemTx, StoreError> {
        Store::begin(self).await
    }

    async fn expire_leases(&self) -> Result<u64, StoreError> {
        Ok(self.write(|s| s.expire(None)))
    }
}

impl MemStore {
    fn worker_view(&self, id: Uuid, e: &MemEnvironment, s: &State) -> WorkerEnvironment {
        WorkerEnvironment {
            id,
            view: EnvironmentView {
                state: e.state,
                mode: e.mode,
                backend: e.backend.clone(),
                execution_generation: e.execution_generation,
                pod_uid: e.pod_uid.clone(),
                live_leases: s.live_leases(id) as u32,
                idle_seconds: (s.clock - e.last_used).max(0) as u64,
                drain_expired: e.drain_deadline.is_some_and(|d| d <= s.clock),
            },
            invocation: e.invocation.clone(),
        }
    }
}
