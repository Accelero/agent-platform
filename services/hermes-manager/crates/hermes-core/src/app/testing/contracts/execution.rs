//! Execution API cases: pending acquisition, idempotent replay, backend and
//! mode authorization, lease renewal, release and database-time expiry,
//! parent joins, session close and environment bounds.

use uuid::Uuid;

use super::super::{Inspect, join};
use super::assignments::bob_spec;
use super::catalog::backend_definition;
use super::controller::{bob_assignment, set_access, setup_with_backend};
use crate::app::assignments::AssignmentReceipt;
use crate::app::controller::{InstanceStore, NewMaterial};
use crate::app::error::AppError;
use crate::app::execution::{
    AcquireRequest, CloseSessionRequest, Execution, ExecutionStore, LeaseRequest, credential_digest,
};
use crate::app::ports::Store;
use crate::domain::ErrorKind;
use crate::domain::assignments::{Access, LifetimeMode};
use crate::domain::execution::{EnvironmentState, Invocation, LEASE_TTL_SECONDS};
use crate::domain::materials::MaterialPurpose;

pub const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const HOST_KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIFixture";

/// Bob's instance with a known execution credential and worker host key.
async fn client<S: Store + InstanceStore + Clone>(store: &S) -> AssignmentReceipt {
    credentialed(store, bob_assignment(store).await).await
}

async fn credentialed<S: Store + InstanceStore + Clone>(
    store: &S,
    receipt: AssignmentReceipt,
) -> AssignmentReceipt {
    for (purpose, public) in [
        (
            MaterialPurpose::ExecutionCredential,
            credential_digest(TOKEN).unwrap(),
        ),
        (MaterialPurpose::WorkerHost, HOST_KEY.to_owned()),
    ] {
        store
            .insert_material(
                receipt.instance_id,
                purpose,
                &NewMaterial {
                    sealed: b"sealed".to_vec(),
                    public: Some(public),
                },
            )
            .await
            .unwrap();
    }
    receipt
}

fn acquire(request: &str, session: &str) -> AcquireRequest {
    AcquireRequest {
        session: session.into(),
        mode: LifetimeMode::PerSession,
        request: request.into(),
        runtime_id: None,
        parent_lease: None,
        runtime: Some(Invocation {
            hermes_home: "/opt/data".into(),
            workdir: "/workspace".into(),
        }),
    }
}

fn code<T: std::fmt::Debug>(result: Result<T, AppError>) -> &'static str {
    result.unwrap_err().code
}

/// What the controller does: Pod applied, then SSH verified.
async fn make_ready<S: InstanceStore>(store: &S, instance: Uuid) -> Uuid {
    let environments = store.worker_environments(instance).await.unwrap();
    let pending = environments
        .iter()
        .find(|e| e.view.state == EnvironmentState::Pending)
        .expect("a pending environment");
    assert!(
        store
            .transition_environment(
                pending.id,
                EnvironmentState::Pending,
                EnvironmentState::Starting,
                Some("pod-uid")
            )
            .await
            .unwrap()
    );
    assert!(
        store
            .transition_environment(
                pending.id,
                EnvironmentState::Starting,
                EnvironmentState::Ready,
                Some("10.42.0.9")
            )
            .await
            .unwrap()
    );
    pending.id
}

pub async fn acquisition_waits_for_a_ready_environment_then_grants_and_replays<
    S: Store + ExecutionStore + InstanceStore + Inspect + Clone,
>(
    store: S,
) {
    let receipt = client(&store).await;
    let execution = Execution::new(store.clone());
    let first = execution
        .acquire(TOKEN, &acquire("r1", "s1"))
        .await
        .unwrap_err();
    assert_eq!(
        (first.kind, first.code),
        (ErrorKind::Pending, "environment_pending")
    );
    assert_eq!(
        code(execution.acquire(TOKEN, &acquire("r1", "s1")).await),
        "environment_pending"
    );
    assert_eq!(
        store
            .worker_environments(receipt.instance_id)
            .await
            .unwrap()
            .len(),
        1,
        "one environment per key"
    );
    let environment = make_ready(&store, receipt.instance_id).await;

    let granted = execution
        .acquire(TOKEN, &acquire("r1", "s1"))
        .await
        .unwrap();
    assert_eq!(granted.environment, environment);
    assert_eq!(granted.ttl, i64::from(LEASE_TTL_SECONDS));
    assert_eq!(
        (
            granted.ssh.host.as_str(),
            granted.ssh.port,
            granted.ssh.user
        ),
        ("10.42.0.9", 2222, "worker")
    );
    assert_eq!(granted.ssh.host_key, HOST_KEY);
    let replay = execution
        .acquire(TOKEN, &acquire("r1", "s1"))
        .await
        .unwrap();
    assert_eq!(
        replay.lease, granted.lease,
        "a repeated request returns its lease"
    );
    let mut changed = acquire("r1", "s1");
    changed.runtime_id = Some("python-tools".into());
    assert_eq!(
        code(execution.acquire(TOKEN, &changed).await),
        "idempotency_conflict"
    );
    let second = execution
        .acquire(TOKEN, &acquire("r2", "s1"))
        .await
        .unwrap();
    assert_eq!(
        second.environment, environment,
        "the same session shares its environment"
    );
    assert_ne!(second.lease, granted.lease);
    let other = execution
        .acquire(TOKEN, &acquire("r3", "s2"))
        .await
        .unwrap_err();
    assert_eq!(
        other.code, "environment_pending",
        "another session gets its own environment"
    );
}

pub async fn only_the_assignments_backends_and_mode_are_served<
    S: Store + ExecutionStore + InstanceStore + Inspect + Clone,
>(
    store: S,
) {
    let receipt = client(&store).await;
    let execution = Execution::new(store.clone());
    let mut foreign = acquire("r1", "s1");
    foreign.runtime_id = Some("gpu-tools".into());
    assert_eq!(
        code(execution.acquire(TOKEN, &foreign).await),
        "access_denied"
    );
    let mut mode = acquire("r2", "s1");
    mode.mode = LifetimeMode::PerProfileOnDemand;
    assert_eq!(code(execution.acquire(TOKEN, &mode).await), "access_denied");
    let unknown = "f".repeat(64);
    assert_eq!(
        code(execution.acquire(&unknown, &acquire("r3", "s1")).await),
        "invalid_identity_evidence"
    );
    assert_eq!(
        code(execution.acquire("not-a-token", &acquire("r3", "s1")).await),
        "invalid_identity_evidence"
    );
    let mut invalid = acquire("r4", "s1");
    invalid.session = "-starts-wrong".into();
    assert_eq!(
        code(execution.acquire(TOKEN, &invalid).await),
        "invalid_request"
    );

    let suspended = set_access(&store, &receipt, Access::Suspended).await;
    assert_eq!(
        code(execution.acquire(TOKEN, &acquire("r5", "s1")).await),
        "invalid_identity_evidence",
        "a suspended instance's credential authorizes nothing"
    );
    assert_eq!(
        code(execution.environments(TOKEN).await),
        "invalid_identity_evidence"
    );
    set_access(&store, &suspended, Access::Enabled).await;
    assert_eq!(
        code(execution.acquire(TOKEN, &acquire("r5", "s1")).await),
        "environment_pending",
        "re-enabled, same credential"
    );
}

pub async fn leases_renew_release_and_expire_by_database_time_and_never_revive<
    S: Store + ExecutionStore + InstanceStore + Inspect + Clone,
>(
    store: S,
) {
    let receipt = client(&store).await;
    let execution = Execution::new(store.clone());
    let _ = execution.acquire(TOKEN, &acquire("r0", "s1")).await;
    make_ready(&store, receipt.instance_id).await;
    let lease = execution
        .acquire(TOKEN, &acquire("r1", "s1"))
        .await
        .unwrap()
        .lease;
    let renew = LeaseRequest { lease };
    assert_eq!(
        execution.heartbeat(TOKEN, &renew).await.unwrap().ttl,
        i64::from(LEASE_TTL_SECONDS)
    );
    assert!(execution.release(TOKEN, &renew).await.unwrap().released);
    assert_eq!(
        code(execution.heartbeat(TOKEN, &renew).await),
        "lease_not_live",
        "released is final"
    );
    assert_eq!(
        code(execution.acquire(TOKEN, &acquire("r1", "s1")).await),
        "lease_not_live",
        "a released request is not revived"
    );

    let expiring = execution
        .acquire(TOKEN, &acquire("r2", "s1"))
        .await
        .unwrap()
        .lease;
    store.advance_time(i64::from(LEASE_TTL_SECONDS) + 1).await;
    assert_eq!(
        code(
            execution
                .heartbeat(TOKEN, &LeaseRequest { lease: expiring })
                .await
        ),
        "lease_not_live",
        "expired by database time"
    );
    store
        .advance_time(-(i64::from(LEASE_TTL_SECONDS) + 10))
        .await;
    assert_eq!(
        code(
            execution
                .heartbeat(TOKEN, &LeaseRequest { lease: expiring })
                .await
        ),
        "lease_not_live",
        "expired is final even if the clock seems to allow it"
    );
    store.advance_time(i64::from(LEASE_TTL_SECONDS) + 10).await;

    let swept = execution
        .acquire(TOKEN, &acquire("r3", "s1"))
        .await
        .unwrap()
        .lease;
    store.advance_time(i64::from(LEASE_TTL_SECONDS) + 1).await;
    assert!(store.expire_leases().await.unwrap() >= 1);
    assert_eq!(
        code(
            execution
                .heartbeat(TOKEN, &LeaseRequest { lease: swept })
                .await
        ),
        "lease_not_live"
    );
    assert_eq!(
        code(
            execution
                .heartbeat(
                    TOKEN,
                    &LeaseRequest {
                        lease: Uuid::new_v4()
                    }
                )
                .await
        ),
        "not_found"
    );
    assert_eq!(
        code(
            execution
                .release(
                    TOKEN,
                    &LeaseRequest {
                        lease: Uuid::new_v4()
                    }
                )
                .await
        ),
        "not_found"
    );
}

pub async fn parent_leases_join_only_live_leases_in_the_same_environment<
    S: Store + ExecutionStore + InstanceStore + Inspect + Clone,
>(
    store: S,
) {
    let receipt = client(&store).await;
    let execution = Execution::new(store.clone());
    let _ = execution.acquire(TOKEN, &acquire("r0", "s1")).await;
    make_ready(&store, receipt.instance_id).await;
    let parent = execution
        .acquire(TOKEN, &acquire("r1", "s1"))
        .await
        .unwrap()
        .lease;
    let mut child = acquire("r2", "s1");
    child.parent_lease = Some(parent);
    assert!(execution.acquire(TOKEN, &child).await.is_ok());
    let mut stranger = acquire("r3", "s1");
    stranger.parent_lease = Some(Uuid::new_v4());
    assert_eq!(
        code(execution.acquire(TOKEN, &stranger).await),
        "access_denied"
    );
    execution
        .release(TOKEN, &LeaseRequest { lease: parent })
        .await
        .unwrap();
    let mut late = acquire("r4", "s1");
    late.parent_lease = Some(parent);
    assert_eq!(
        code(execution.acquire(TOKEN, &late).await),
        "access_denied",
        "a released parent admits nobody"
    );
}

pub async fn a_lease_never_outlives_its_backends_lease_limit<
    S: Store + ExecutionStore + InstanceStore + Inspect + Clone,
>(
    store: S,
) {
    const LIMIT: u32 = 60;
    let mut definition = backend_definition();
    definition.limits.max_lease_seconds = LIMIT;
    let receipt = credentialed(&store, setup_with_backend(&store, definition).await).await;
    let execution = Execution::new(store.clone());
    let _ = execution.acquire(TOKEN, &acquire("r0", "s1")).await;
    make_ready(&store, receipt.instance_id).await;

    let granted = execution
        .acquire(TOKEN, &acquire("r1", "s1"))
        .await
        .unwrap();
    assert_eq!(
        granted.ttl,
        i64::from(LIMIT),
        "a lease is granted for no longer than the backend allows, not for the standard TTL"
    );
    let renew = LeaseRequest {
        lease: granted.lease,
    };
    assert_eq!(
        execution.heartbeat(TOKEN, &renew).await.unwrap().ttl,
        i64::from(LIMIT),
        "renewing does not go past the limit"
    );
    store.advance_time(31).await;
    assert_eq!(
        code(execution.heartbeat(TOKEN, &renew).await),
        "lease_not_live",
        "a renewing holder is cut when less than the minimum renewal remains"
    );
    store.advance_time(i64::from(LIMIT)).await;
    assert_eq!(
        code(execution.heartbeat(TOKEN, &renew).await),
        "lease_not_live",
        "and the lease has ended at the limit"
    );
}

pub async fn closing_a_session_drains_its_environment_and_waits_for_retirement<
    S: Store + ExecutionStore + InstanceStore + Inspect + Clone,
>(
    store: S,
) {
    let receipt = client(&store).await;
    let execution = Execution::new(store.clone());
    let _ = execution.acquire(TOKEN, &acquire("r0", "s1")).await;
    let environment = make_ready(&store, receipt.instance_id).await;
    let close = CloseSessionRequest {
        session: "s1".into(),
        runtime_id: None,
        runtime: acquire("r", "s1").runtime,
    };
    assert!(execution.close(TOKEN, &close).await.unwrap().closed);
    let state = |id| {
        let store = store.clone();
        async move {
            store
                .worker_environments(receipt.instance_id)
                .await
                .unwrap()
                .into_iter()
                .find(|e| e.id == id)
                .map(|e| e.view.state)
        }
    };
    assert_eq!(state(environment).await, Some(EnvironmentState::Draining));
    assert_eq!(
        code(execution.acquire(TOKEN, &acquire("r1", "s1")).await),
        "environment_pending",
        "a closed session is not reused while it drains"
    );
    assert!(
        store
            .transition_environment(
                environment,
                EnvironmentState::Draining,
                EnvironmentState::Retired,
                None
            )
            .await
            .unwrap()
    );
    assert_eq!(
        code(execution.acquire(TOKEN, &acquire("r1", "s1")).await),
        "environment_pending"
    );
    let live = store
        .worker_environments(receipt.instance_id)
        .await
        .unwrap();
    assert_eq!(live.len(), 1);
    assert_ne!(
        live[0].id, environment,
        "a new environment replaces the retired one"
    );
    let summaries = execution.environments(TOKEN).await.unwrap();
    assert_eq!(summaries.len(), 2);
    assert!(
        summaries[0].stale && !summaries[1].stale,
        "creation order; retired ones are stale"
    );
    assert_eq!(
        (
            summaries[0].session.as_str(),
            summaries[0].runtime_id.as_str()
        ),
        ("s1", "python-tools")
    );
    let mut foreign = close;
    foreign.runtime_id = Some("gpu-tools".into());
    assert_eq!(
        code(execution.close(TOKEN, &foreign).await),
        "access_denied"
    );
}

pub async fn each_instance_holds_a_bounded_number_of_environments<
    S: Store + ExecutionStore + InstanceStore + Inspect + Clone,
>(
    store: S,
) {
    client(&store).await;
    let execution = Execution::new(store.clone());
    // The fixture backend allows four environments.
    for session in 0..4 {
        assert_eq!(
            code(
                execution
                    .acquire(
                        TOKEN,
                        &acquire(&format!("r{session}"), &format!("s{session}"))
                    )
                    .await
            ),
            "environment_pending"
        );
    }
    let full = execution
        .acquire(TOKEN, &acquire("r9", "s9"))
        .await
        .unwrap_err();
    assert_eq!(
        (full.kind, full.code),
        (ErrorKind::Unavailable, "dependency_unavailable")
    );
    assert_eq!(bob_spec().terminal.mode, LifetimeMode::PerSession);
}

/// A ready environment for session `s1` of Bob's instance.
async fn ready<S: Store + ExecutionStore + InstanceStore + Inspect + Clone>(
    store: &S,
) -> (AssignmentReceipt, Execution<S>) {
    let receipt = client(store).await;
    let execution = Execution::new(store.clone());
    let _ = execution.acquire(TOKEN, &acquire("ready", "s1")).await;
    make_ready(store, receipt.instance_id).await;
    (receipt, execution)
}

pub async fn revocation_racing_acquisitions_grants_nothing_after_it_commits<
    S: Store + ExecutionStore + InstanceStore + Inspect + Clone,
>(
    store: S,
) {
    let (receipt, execution) = ready(&store).await;
    let mut current = receipt;
    for round in 0..6 {
        let (a, b) = (format!("a{round}"), format!("b{round}"));
        let ((first, second), revoked) = join(
            join(
                execution.acquire(TOKEN, &acquire(&a, "s1")),
                execution.acquire(TOKEN, &acquire(&b, "s1")),
            ),
            set_access(&store, &current, Access::Revoked),
        )
        .await;
        // Each racing acquisition committed before the revocation or was
        // refused by it; none fails any other way.
        let mut granted = Vec::new();
        for result in [first, second] {
            match result {
                Ok(lease) => granted.push(lease.lease),
                Err(error) => assert_eq!(
                    (error.kind, error.code),
                    (ErrorKind::Unauthenticated, "invalid_identity_evidence"),
                    "round {round}"
                ),
            }
        }
        // The revocation has returned: nothing is granted and nothing renewed.
        assert_eq!(
            code(
                execution
                    .acquire(TOKEN, &acquire(&format!("late{round}"), "s1"))
                    .await
            ),
            "invalid_identity_evidence",
            "round {round}: no lease after the revocation"
        );
        for lease in &granted {
            assert_eq!(
                code(
                    execution
                        .heartbeat(TOKEN, &LeaseRequest { lease: *lease })
                        .await
                ),
                "invalid_identity_evidence",
                "round {round}: a lease granted in the race cannot be renewed"
            );
        }
        current = set_access(&store, &revoked, Access::Enabled).await;
        for lease in granted {
            assert!(
                execution
                    .release(TOKEN, &LeaseRequest { lease })
                    .await
                    .unwrap()
                    .released
            );
        }
    }
}

pub async fn concurrent_heartbeat_and_release_leave_the_lease_finally_dead<
    S: Store + ExecutionStore + InstanceStore + Inspect + Clone,
>(
    store: S,
) {
    let (_, execution) = ready(&store).await;
    for round in 0..8 {
        let lease = execution
            .acquire(TOKEN, &acquire(&format!("h{round}"), "s1"))
            .await
            .unwrap()
            .lease;
        let request = LeaseRequest { lease };
        let (beat, released) = join(
            execution.heartbeat(TOKEN, &request),
            execution.release(TOKEN, &request),
        )
        .await;
        match beat {
            Ok(renewed) => assert_eq!(renewed.ttl, i64::from(LEASE_TTL_SECONDS)),
            Err(error) => assert_eq!(error.code, "lease_not_live", "round {round}"),
        }
        assert!(released.unwrap().released, "round {round}");
        assert_eq!(
            code(execution.heartbeat(TOKEN, &request).await),
            "lease_not_live",
            "round {round}: released is final, whichever won the race"
        );
    }
}

pub async fn a_heartbeat_racing_the_expiry_sweep_never_revives_an_expired_lease<
    S: Store + ExecutionStore + InstanceStore + Inspect + Clone,
>(
    store: S,
) {
    let (_, execution) = ready(&store).await;
    for round in 0..6 {
        let lease = execution
            .acquire(TOKEN, &acquire(&format!("e{round}"), "s1"))
            .await
            .unwrap()
            .lease;
        store.advance_time(i64::from(LEASE_TTL_SECONDS) + 1).await;
        let request = LeaseRequest { lease };
        let (beat, swept) = join(execution.heartbeat(TOKEN, &request), store.expire_leases()).await;
        assert_eq!(
            code(beat),
            "lease_not_live",
            "round {round}: past its time by the database clock, with or without the sweep"
        );
        assert!(swept.is_ok(), "round {round}");
        assert_eq!(
            code(execution.heartbeat(TOKEN, &request).await),
            "lease_not_live",
            "round {round}: expired is final"
        );
    }
}

pub async fn concurrent_acquisitions_with_one_request_id_grant_one_lease<
    S: Store + ExecutionStore + InstanceStore + Inspect + Clone,
>(
    store: S,
) {
    let (_, execution) = ready(&store).await;
    for round in 0..6 {
        let request = acquire(&format!("same{round}"), "s1");
        let (first, second) = join(
            execution.acquire(TOKEN, &request),
            execution.acquire(TOKEN, &request),
        )
        .await;
        let (first, second) = (first.unwrap(), second.unwrap());
        assert_eq!(first.lease, second.lease, "round {round}: one lease");
        assert_eq!(first.environment, second.environment);
    }
    // The six requests, "ready" (never granted) aside, are six leases.
    let listed = serde_json::to_value(execution.environments(TOKEN).await.unwrap()).unwrap();
    assert_eq!(listed[0]["leases"], 6, "no duplicate grant per request");
}
