//! Controller cases: convergence, create-once material, crash recovery,
//! leadership epochs, stopping and secret delivery. The store is real (or
//! the in-memory fake); the cluster, vault and native checks are fakes.

use std::sync::Arc;

use serde_json::json;
use uuid::Uuid;

use super::super::fakes::{FakeCluster, FakeNative, FakeVault};
use super::super::{Inspect, ScriptedFaults, identity, settings};
use super::access::{admin, bootstrap, envelope, key};
use super::assignments::bob_spec;
use super::catalog::published_catalog;
use crate::app::assignments::{
    AssignmentChanges, AssignmentReceipt, CreateAssignment, UpdateAssignment,
};
use crate::app::controller::{Controller, ControllerError, InstanceStore, Reconciled};
use crate::app::faults::{FaultHook, FaultPoint, NoFaults};
use crate::app::ports::Store;
use crate::app::secrets::WriteSecret;
use crate::domain::assignments::{Access, AssignmentSpec};
use crate::domain::execution::SshCheck;
use crate::domain::instances::{Apply, BlockedReason, Phase, Workload};
use crate::domain::materials::MaterialPurpose;
use crate::domain::secrets::SecretKind;

pub(super) type Fixture<S> = Controller<S, FakeCluster, FakeVault, FakeNative>;

pub(super) fn controller<S: Clone>(
    store: &S,
    cluster: &FakeCluster,
    faults: Arc<dyn FaultHook>,
) -> Fixture<S> {
    Controller {
        store: store.clone(),
        cluster: cluster.clone(),
        vault: FakeVault,
        native: FakeNative::default(),
        settings: settings(),
        faults,
    }
}

async fn assign<S: Store + Clone>(store: &S, spec: AssignmentSpec) -> AssignmentReceipt {
    envelope(store)
        .execute(
            &admin("alice"),
            Uuid::new_v4(),
            &key(&Uuid::new_v4().to_string()),
            &CreateAssignment {
                identity: identity("bob"),
                spec,
            },
        )
        .await
        .expect("assignment")
}

pub(super) async fn change<S: Store + Clone>(
    store: &S,
    receipt: &AssignmentReceipt,
    changes: AssignmentChanges,
) -> AssignmentReceipt {
    envelope(store)
        .execute(
            &admin("alice"),
            Uuid::new_v4(),
            &key(&Uuid::new_v4().to_string()),
            &UpdateAssignment {
                assignment_id: receipt.assignment_id,
                expected_version: receipt.version,
                changes,
            },
        )
        .await
        .expect("change")
}

pub(super) fn access(access: Access) -> AssignmentChanges {
    serde_json::from_value(json!({"access": access.as_str()})).expect("changes")
}

/// Reconciles until the phase is reached; returns the last pass.
pub(super) async fn until<S: Store + InstanceStore + Clone>(
    controller: &Fixture<S>,
    epoch: i64,
    instance: Uuid,
    phase: Phase,
) -> Reconciled {
    let mut last = None;
    for _ in 0..12 {
        let pass = controller
            .reconcile(epoch, instance)
            .await
            .expect("reconcile")
            .expect("instance exists");
        if pass.plan.status.phase == phase {
            return pass;
        }
        last = Some(pass);
    }
    panic!("never reached {phase:?}: {last:?}");
}

pub(super) async fn setup<S: Store + Clone>(store: &S, spec: AssignmentSpec) -> AssignmentReceipt {
    bootstrap(store, "alice").await;
    published_catalog(store).await;
    assign(store, spec).await
}

async fn sealed<S: InstanceStore>(store: &S, instance: Uuid) -> Vec<(MaterialPurpose, Vec<u8>)> {
    let mut rows: Vec<_> = store
        .materials(instance)
        .await
        .expect("materials")
        .into_iter()
        .map(|row| (row.purpose, row.sealed))
        .collect();
    rows.sort();
    rows
}

pub async fn a_new_instance_converges_to_ready_with_material_created_once<
    S: Store + InstanceStore + Inspect + Clone,
>(
    store: S,
) {
    let receipt = setup(&store, bob_spec()).await;
    let cluster = FakeCluster::default();
    let controller = controller(&store, &cluster, Arc::new(NoFaults));
    let epoch = store.advance_epoch().await.unwrap();
    let first = controller
        .reconcile(epoch, receipt.instance_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.plan.apply, Apply::Everything(Workload::Run));
    assert_eq!(first.plan.status.phase, Phase::Provisioning);
    let material = sealed(&store, receipt.instance_id).await;
    assert_eq!(material.len(), MaterialPurpose::ALL.len());

    let ready = until(&controller, epoch, receipt.instance_id, Phase::Ready).await;
    assert!(ready.plan.status.converged);
    let (phase, blocked, observed) = store.instance_status(receipt.instance_id).await;
    assert_eq!((phase.as_str(), blocked, observed), ("ready", None, 1));
    assert_eq!(
        sealed(&store, receipt.instance_id).await,
        material,
        "material is never regenerated"
    );
    let desired = store
        .desired_instance(receipt.instance_id)
        .await
        .unwrap()
        .unwrap();
    assert!(
        desired.namespace_uid.is_some(),
        "the namespace UID is bound on first sight"
    );
    cluster.with(|world| {
        assert!(
            world
                .applies
                .iter()
                .all(|a| a.materials.len() == MaterialPurpose::ALL.len())
        );
        let first = &world.applies[0].materials;
        assert!(
            world.applies.iter().all(|a| &a.materials == first),
            "every apply renders the same material"
        );
    });
    // A healthy instance is left alone; another pass changes nothing.
    let again = controller
        .reconcile(epoch, receipt.instance_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(again.plan.status.phase, Phase::Ready);
    assert_eq!(
        cluster.with(|w| w.deployment.as_ref().unwrap().resource_version),
        1
    );
}

pub async fn a_crash_at_any_controller_point_resumes_without_regenerating_material<
    S: Store + InstanceStore + Inspect + Clone,
>(
    store: S,
) {
    let points = [
        FaultPoint::ControllerAfterMaterialInsert,
        FaultPoint::ControllerBeforeApply,
        FaultPoint::ControllerAfterApply,
        FaultPoint::ControllerBeforeStatus,
    ];
    let receipt = setup(&store, bob_spec()).await;
    let cluster = FakeCluster::default();
    let epoch = store.advance_epoch().await.unwrap();
    let mut material = None;
    for point in points {
        let crashing = controller(&store, &cluster, Arc::new(ScriptedFaults::once(point)));
        let crash = crashing.reconcile(epoch, receipt.instance_id).await;
        assert_eq!(crash.unwrap_err(), ControllerError::Fault, "{point:?}");
        let rows = sealed(&store, receipt.instance_id).await;
        assert!(!rows.is_empty(), "{point:?}");
        if rows.len() == MaterialPurpose::ALL.len() {
            assert_eq!(
                material.get_or_insert_with(|| rows.clone()),
                &rows,
                "{point:?}"
            );
        }
    }
    let healthy = controller(&store, &cluster, Arc::new(NoFaults));
    until(&healthy, epoch, receipt.instance_id, Phase::Ready).await;
    assert_eq!(Some(sealed(&store, receipt.instance_id).await), material);
}

pub async fn a_deposed_controller_cannot_record_status<
    S: Store + InstanceStore + Inspect + Clone,
>(
    store: S,
) {
    let receipt = setup(&store, bob_spec()).await;
    let cluster = FakeCluster::default();
    let old = store.advance_epoch().await.unwrap();
    let new = store.advance_epoch().await.unwrap();
    assert!(new > old);
    let controller = controller(&store, &cluster, Arc::new(NoFaults));
    let deposed = controller.reconcile(old, receipt.instance_id).await;
    assert_eq!(deposed.unwrap_err(), ControllerError::Deposed);
    let (phase, _, observed) = store.instance_status(receipt.instance_id).await;
    assert_eq!(
        (phase.as_str(), observed),
        ("provisioning", 0),
        "nothing recorded by the old leader"
    );
    assert!(
        controller
            .reconcile(new, receipt.instance_id)
            .await
            .unwrap()
            .is_some()
    );
}

pub async fn suspension_stops_then_retains_and_reenabling_reuses_everything<
    S: Store + InstanceStore + Inspect + Clone,
>(
    store: S,
) {
    let receipt = setup(&store, bob_spec()).await;
    let cluster = FakeCluster::default();
    let controller = controller(&store, &cluster, Arc::new(NoFaults));
    let epoch = store.advance_epoch().await.unwrap();
    until(&controller, epoch, receipt.instance_id, Phase::Ready).await;
    let material = sealed(&store, receipt.instance_id).await;
    let namespace = cluster.with(|w| w.namespace.clone());

    let suspended = change(&store, &receipt, access(Access::Suspended)).await;
    let stopping = controller
        .reconcile(epoch, receipt.instance_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stopping.plan.status.phase, Phase::Stopping);
    assert_eq!(stopping.plan.apply, Apply::Scale(Workload::Stop));
    until(&controller, epoch, receipt.instance_id, Phase::Retained).await;
    cluster.with(|world| {
        assert!(world.deployment.is_none());
        assert_eq!(
            world.deletes.len(),
            1,
            "deleted once, with the observed UID and version"
        );
        assert!(!world.claims.is_empty(), "claims are kept");
    });
    let (_, _, observed) = store.instance_status(receipt.instance_id).await;
    assert_eq!(
        observed, 2,
        "retained is the converged state of the suspended generation"
    );

    change(&store, &suspended, access(Access::Enabled)).await;
    until(&controller, epoch, receipt.instance_id, Phase::Ready).await;
    assert_eq!(
        cluster.with(|w| w.namespace.clone()),
        namespace,
        "same namespace"
    );
    assert_eq!(
        sealed(&store, receipt.instance_id).await,
        material,
        "same credentials"
    );
}

pub async fn a_rejected_apply_blocks_with_a_reason<S: Store + InstanceStore + Inspect + Clone>(
    store: S,
) {
    let receipt = setup(&store, bob_spec()).await;
    let cluster = FakeCluster::default();
    cluster.with(|world| world.reject = true);
    let controller = controller(&store, &cluster, Arc::new(NoFaults));
    let epoch = store.advance_epoch().await.unwrap();
    let pass = controller
        .reconcile(epoch, receipt.instance_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pass.plan.status.blocked, Some(BlockedReason::ApplyRejected));
    let (phase, blocked, _) = store.instance_status(receipt.instance_id).await;
    assert_eq!(
        (phase.as_str(), blocked.as_deref()),
        ("blocked", Some("apply_rejected"))
    );
    cluster.with(|world| world.reject = false);
    until(&controller, epoch, receipt.instance_id, Phase::Ready).await;
}

pub async fn a_missing_static_directory_blocks_until_it_can_be_mounted<
    S: Store + InstanceStore + Inspect + Clone,
>(
    store: S,
) {
    let receipt = setup(&store, bob_spec()).await;
    let cluster = FakeCluster::default();
    cluster.with(|world| world.mount_failed = true);
    let controller = controller(&store, &cluster, Arc::new(NoFaults));
    let epoch = store.advance_epoch().await.unwrap();
    let blocked = until(&controller, epoch, receipt.instance_id, Phase::Blocked).await;
    assert_eq!(
        blocked.plan.status.blocked,
        Some(BlockedReason::StorageUnavailable)
    );
    // The objects stay applied; nothing substitutes an empty directory.
    assert!(matches!(
        blocked.plan.apply,
        Apply::Everything(Workload::Run)
    ));
    assert_eq!(
        store
            .instance_status(receipt.instance_id)
            .await
            .1
            .as_deref(),
        Some("storage_unavailable")
    );
    cluster.with(|world| world.mount_failed = false);
    until(&controller, epoch, receipt.instance_id, Phase::Ready).await;
}

pub async fn an_invalid_receipt_blocks_and_never_reaches_ready<
    S: Store + InstanceStore + Inspect + Clone,
>(
    store: S,
) {
    let receipt = setup(&store, bob_spec()).await;
    let cluster = FakeCluster::default();
    cluster.with(|world| world.forge_receipt = true);
    let controller = controller(&store, &cluster, Arc::new(NoFaults));
    let epoch = store.advance_epoch().await.unwrap();
    until(&controller, epoch, receipt.instance_id, Phase::Blocked).await;
    let (_, blocked, observed) = store.instance_status(receipt.instance_id).await;
    assert_eq!(
        (blocked.as_deref(), observed),
        (Some("invalid_initialization_receipt"), 0)
    );
}

pub async fn secrets_are_delivered_opened_at_their_latest_version<
    S: Store + InstanceStore + Inspect + Clone,
>(
    store: S,
) {
    bootstrap(&store, "alice").await;
    for value in [b"first".to_vec(), b"second".to_vec()] {
        envelope(&store)
            .execute(
                &admin("alice"),
                Uuid::new_v4(),
                &key(&Uuid::new_v4().to_string()),
                &WriteSecret {
                    name: "llm-key".into(),
                    kind: SecretKind::Env,
                    sealed: value,
                },
            )
            .await
            .expect("secret");
    }
    let mut spec = bob_spec();
    spec.secrets =
        serde_json::from_value(json!([{"name": "llm-key", "env": "OPENAI_API_KEY"}])).unwrap();
    published_catalog(&store).await;
    let receipt = assign(&store, spec).await;
    let cluster = FakeCluster::default();
    let controller = controller(&store, &cluster, Arc::new(NoFaults));
    let epoch = store.advance_epoch().await.unwrap();
    controller
        .reconcile(epoch, receipt.instance_id)
        .await
        .unwrap();
    cluster.with(|world| {
        assert_eq!(
            world.applies[0].secrets,
            vec![("llm-key".to_owned(), 2, b"second".to_vec())]
        );
    });
}

/// Bob's example assignment on a fresh registry, for adapter tests that need a
/// committed instance.
pub async fn bob_assignment<S: Store + Clone>(store: &S) -> AssignmentReceipt {
    setup(store, bob_spec()).await
}

/// Changes an assignment's access, as an administrator command.
pub async fn set_access<S: Store + Clone>(
    store: &S,
    receipt: &AssignmentReceipt,
    value: Access,
) -> AssignmentReceipt {
    change(store, receipt, access(value)).await
}

pub async fn workers_start_only_after_ssh_verification_and_drain_when_suspended<
    S: Store + InstanceStore + crate::app::execution::ExecutionStore + Inspect + Clone,
>(
    store: S,
) {
    use crate::app::controller::Vault;
    use crate::app::execution::{AcquireRequest, Execution};
    use crate::domain::assignments::LifetimeMode;
    use crate::domain::execution::{DRAIN_DEADLINE_SECONDS, EnvironmentState};

    let receipt = setup(&store, bob_spec()).await;
    let cluster = FakeCluster::default();
    let controller = controller(&store, &cluster, Arc::new(NoFaults));
    let epoch = store.advance_epoch().await.unwrap();
    until(&controller, epoch, receipt.instance_id, Phase::Ready).await;
    let credential = store
        .materials(receipt.instance_id)
        .await
        .unwrap()
        .into_iter()
        .find(|m| m.purpose == MaterialPurpose::ExecutionCredential)
        .unwrap();
    let token = FakeVault
        .open_material(
            receipt.instance_id,
            MaterialPurpose::ExecutionCredential,
            &credential.sealed,
        )
        .unwrap();
    let token = String::from_utf8(token.expose().to_vec()).unwrap();
    let execution = Execution::new(store.clone());
    let request = |id: &str| AcquireRequest {
        session: "s1".into(),
        mode: LifetimeMode::PerSession,
        request: id.into(),
        runtime_id: None,
        parent_lease: None,
        runtime: None,
    };
    assert_eq!(
        execution
            .acquire(&token, &request("r1"))
            .await
            .unwrap_err()
            .code,
        "environment_pending"
    );

    let state = || async {
        store
            .worker_environments(receipt.instance_id)
            .await
            .unwrap()[0]
            .view
            .state
    };
    controller
        .reconcile(epoch, receipt.instance_id)
        .await
        .unwrap();
    assert_eq!(
        cluster.with(|w| w.worker_applies.len()),
        1,
        "the worker Pod is applied"
    );
    controller
        .reconcile(epoch, receipt.instance_id)
        .await
        .unwrap();
    assert_eq!(
        state().await,
        EnvironmentState::Starting,
        "the applied Pod is recorded"
    );
    assert_eq!(
        execution
            .acquire(&token, &request("r1"))
            .await
            .unwrap_err()
            .code,
        "environment_pending"
    );
    let native = controller.native.clone();
    native.ssh.lock().unwrap().0 = SshCheck::NotYet;
    controller
        .reconcile(epoch, receipt.instance_id)
        .await
        .unwrap();
    assert_eq!(
        state().await,
        EnvironmentState::Starting,
        "a failed SSH login keeps it pending"
    );
    native.ssh.lock().unwrap().0 = SshCheck::Ready;
    controller
        .reconcile(epoch, receipt.instance_id)
        .await
        .unwrap();
    assert_eq!(state().await, EnvironmentState::Ready);
    let (address, host_key, uid) = native.ssh.lock().unwrap().1.last().cloned().unwrap();
    assert_eq!(
        (address.as_str(), host_key.as_str(), uid),
        ("10.42.0.9", "public-worker-host", 1000)
    );
    let granted = execution.acquire(&token, &request("r1")).await.unwrap();
    assert_eq!(granted.ssh.host, "10.42.0.9");

    set_access(&store, &receipt, Access::Suspended).await;
    controller
        .reconcile(epoch, receipt.instance_id)
        .await
        .unwrap();
    assert_eq!(state().await, EnvironmentState::Draining);
    controller
        .reconcile(epoch, receipt.instance_id)
        .await
        .unwrap();
    assert!(
        cluster.with(|w| w.worker_deletes.is_empty()),
        "a live lease holds the drain until its deadline"
    );
    store
        .advance_time(i64::from(DRAIN_DEADLINE_SECONDS) + 1)
        .await;
    let mut phase = Phase::Stopping;
    for _ in 0..8 {
        phase = controller
            .reconcile(epoch, receipt.instance_id)
            .await
            .unwrap()
            .unwrap()
            .plan
            .status
            .phase;
        if phase == Phase::Retained {
            break;
        }
    }
    assert_eq!(
        phase,
        Phase::Retained,
        "Hermes stops after its workers are retired"
    );
    assert_eq!(
        cluster.with(|w| w.worker_deletes.clone()),
        vec![granted.environment]
    );
    assert!(
        store
            .worker_environments(receipt.instance_id)
            .await
            .unwrap()
            .is_empty(),
        "retired"
    );
}

pub async fn a_failed_worker_is_reported_and_never_replaced<
    S: Store + InstanceStore + crate::app::execution::ExecutionStore + Inspect + Clone,
>(
    store: S,
) {
    use crate::app::controller::Vault;
    use crate::app::execution::{AcquireRequest, Execution};
    use crate::domain::assignments::LifetimeMode;
    use crate::domain::execution::{EnvironmentState, WorkerFailure};

    let receipt = setup(&store, bob_spec()).await;
    let cluster = FakeCluster::default();
    let controller = controller(&store, &cluster, Arc::new(NoFaults));
    let epoch = store.advance_epoch().await.unwrap();
    until(&controller, epoch, receipt.instance_id, Phase::Ready).await;
    let credential = store
        .materials(receipt.instance_id)
        .await
        .unwrap()
        .into_iter()
        .find(|m| m.purpose == MaterialPurpose::ExecutionCredential)
        .unwrap();
    let token = FakeVault
        .open_material(
            receipt.instance_id,
            MaterialPurpose::ExecutionCredential,
            &credential.sealed,
        )
        .unwrap();
    let token = String::from_utf8(token.expose().to_vec()).unwrap();
    let execution = Execution::new(store.clone());
    let request = |session: &str, id: &str| AcquireRequest {
        session: session.into(),
        mode: LifetimeMode::PerSession,
        request: id.into(),
        runtime_id: None,
        parent_lease: None,
        runtime: None,
    };
    let reconcile = || async {
        for _ in 0..3 {
            controller
                .reconcile(epoch, receipt.instance_id)
                .await
                .unwrap();
        }
    };
    let failures = || async {
        store
            .worker_environments(receipt.instance_id)
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.view.state)
            .collect::<Vec<_>>()
    };

    // The image cannot be pulled: the environment drains with its reason and
    // the same request is refused instead of waiting or trying another image.
    cluster.with(|world| world.worker_failure = Some(WorkerFailure::ImageUnavailable));
    assert_eq!(
        execution
            .acquire(&token, &request("s1", "r1"))
            .await
            .unwrap_err()
            .code,
        "environment_pending"
    );
    reconcile().await;
    assert_eq!(failures().await, vec![EnvironmentState::Draining]);
    for attempt in [request("s1", "r1"), request("s1", "r2")] {
        assert_eq!(
            execution.acquire(&token, &attempt).await.unwrap_err().code,
            "backend_failed"
        );
    }
    assert_eq!(
        cluster.with(|w| w.worker_applies.len()),
        1,
        "no other worker is started in its place"
    );

    // A worker that answers but breaks the contract is reported too.
    cluster.with(|world| {
        world.worker_failure = None;
        world.workers.clear();
    });
    reconcile().await;
    controller.native.ssh.lock().unwrap().0 = SshCheck::ContractFailed;
    assert_eq!(
        execution
            .acquire(&token, &request("s2", "r3"))
            .await
            .unwrap_err()
            .code,
        "environment_pending"
    );
    reconcile().await;
    reconcile().await;
    assert_eq!(
        execution
            .acquire(&token, &request("s2", "r3"))
            .await
            .unwrap_err()
            .code,
        "backend_failed"
    );
}

pub async fn orphans_are_reported_by_the_current_leader_and_never_adopted<
    S: Store + InstanceStore + Inspect + Clone,
>(
    store: S,
) {
    use crate::domain::orphans::{Orphan, OrphanKind, OwnedObject};

    let receipt = setup(&store, bob_spec()).await;
    let cluster = FakeCluster::default();
    let controller = controller(&store, &cluster, Arc::new(NoFaults));
    let epoch = store.advance_epoch().await.unwrap();
    until(&controller, epoch, receipt.instance_id, Phase::Ready).await;
    assert_eq!(controller.report_orphans(epoch).await.unwrap(), 0);
    assert_eq!(store.orphans().await, vec![]);

    // Objects of an instance the registry does not know, as after restoring
    // an older database: reported, and the instance's own objects are not.
    let stray = Uuid::from_u128(0x5);
    let namespace = format!("hermes-{}", stray.simple());
    cluster.with(|world| {
        world.owned.push(OwnedObject {
            kind: OrphanKind::Namespace,
            name: namespace.clone(),
            uid: "stray-namespace".into(),
            instance: Some(stray),
        });
        world.owned.push(OwnedObject {
            kind: OrphanKind::PersistentVolume,
            name: format!("{namespace}-workspace"),
            uid: "stray-volume".into(),
            instance: Some(stray),
        });
    });
    let applies = cluster.with(|world| world.applies.len());
    assert_eq!(controller.report_orphans(epoch).await.unwrap(), 2);
    let expected = vec![
        Orphan {
            kind: OrphanKind::Namespace,
            name: namespace.clone(),
            uid: "stray-namespace".into(),
        },
        Orphan {
            kind: OrphanKind::PersistentVolume,
            name: format!("{namespace}-workspace"),
            uid: "stray-volume".into(),
        },
    ];
    assert_eq!(store.orphans().await, expected);
    // Nothing is adopted: no instance appears and nothing is written.
    assert_eq!(
        store.instance_ids().await.unwrap(),
        vec![receipt.instance_id]
    );
    assert_eq!(cluster.with(|world| world.applies.len()), applies);

    // A deposed leader cannot replace the report.
    let newer = store.advance_epoch().await.unwrap();
    cluster.with(|world| world.owned.retain(|o| o.instance != Some(stray)));
    assert_eq!(
        controller.report_orphans(epoch).await.unwrap_err(),
        ControllerError::Deposed
    );
    assert_eq!(store.orphans().await, expected);
    // Removed objects leave the report on the next scan.
    assert_eq!(controller.report_orphans(newer).await.unwrap(), 0);
    assert_eq!(store.orphans().await, vec![]);
}
