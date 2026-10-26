//! Route resolution cases: two owners on one hostname, denial without side
//! effects, and decisions that follow access and readiness immediately.

use std::sync::Arc;

use serde_json::json;

use super::super::fakes::FakeCluster;
use super::super::{Inspect, identity, installation};
use super::assignments::{bob_spec, create};
use super::controller::{access, change, controller, setup, until};
use crate::app::controller::InstanceStore;
use crate::app::faults::NoFaults;
use crate::app::ports::Store;
use crate::app::routing::{RouteStore, Routing};
use crate::domain::assignments::{Access, AssignmentSpec};
use crate::domain::identity::Identity;
use crate::domain::instances::Phase;
use crate::domain::routing::{Decision, PendingReason, Route};

fn carol_spec() -> AssignmentSpec {
    serde_json::from_value(json!({
        "access": "enabled",
        "template": {"id": "personal-default", "version": 1},
        "volumes": {
            "workspace": {"source": "nvme", "sizeGib": 20},
            "home": {"source": "nas", "path": "carol"},
        },
        "terminal": {"default": "python-tools", "allowed": {"python-tools": 1}},
    }))
    .expect("fixture assignment")
}

async fn route<S: RouteStore + Clone>(store: &S, identity: &Identity) -> Decision {
    Routing::new(store.clone(), installation())
        .resolve(identity)
        .await
        .expect("a well-formed identity")
}

fn ready(decision: Decision) -> Route {
    match decision {
        Decision::Ready(route) => route,
        other => panic!("not a route: {other:?}"),
    }
}

pub async fn each_identity_reaches_only_its_own_instance<
    S: Store + InstanceStore + RouteStore + Inspect + Clone,
>(
    store: S,
) {
    let bob = setup(&store, bob_spec()).await;
    let carol = create(&store, "carol", carol_spec()).await.expect("carol");
    assert_eq!(
        route(&store, &identity("bob")).await,
        Decision::Pending(PendingReason::InstanceNotReady)
    );
    // One fake cluster world models one instance.
    let epoch = store.advance_epoch().await.unwrap();
    for instance in [bob.instance_id, carol.instance_id] {
        let cluster = FakeCluster::default();
        let controller = controller(&store, &cluster, Arc::new(NoFaults));
        until(&controller, epoch, instance, Phase::Ready).await;
    }

    let bob_route = ready(route(&store, &identity("bob")).await);
    let carol_route = ready(route(&store, &identity("carol")).await);
    assert_eq!(bob_route.instance_id, bob.instance_id);
    assert_eq!(carol_route.instance_id, carol.instance_id);
    assert_eq!(
        bob_route.destination.namespace,
        store.instance_namespace(bob.instance_id).await
    );
    assert_eq!(
        carol_route.destination.namespace,
        store.instance_namespace(carol.instance_id).await
    );
    assert_ne!(bob_route.destination, carol_route.destination);
    assert_eq!(
        (bob_route.route_generation, bob_route.authorization_version),
        (
            store.instance_generations(bob.instance_id).await.0,
            bob.version
        )
    );
    // Subjects are exact: case and whitespace variants are other identities.
    for subject in ["Bob", "bob ", "carol\u{0}"] {
        let decision = Routing::new(store.clone(), installation())
            .resolve(&identity(subject))
            .await;
        assert!(
            matches!(decision, Ok(Decision::Denied) | Err(_)),
            "{subject:?}"
        );
    }
}

pub async fn unassigned_and_foreign_identities_are_denied_without_side_effects<
    S: Store + InstanceStore + RouteStore + Inspect + Clone,
>(
    store: S,
) {
    setup(&store, bob_spec()).await;
    let receipts = store.receipt_count().await;
    let audit = store.audit_actions().await;
    let instances = store.instance_ids().await.unwrap();
    // Alice administers but has no assignment; Mallory is unknown.
    for subject in ["alice", "mallory"] {
        assert_eq!(route(&store, &identity(subject)).await, Decision::Denied);
    }
    let foreign = Identity {
        issuer: "https://other.example.org/realms/agents".into(),
        subject: "bob".into(),
    };
    assert_eq!(route(&store, &foreign).await, Decision::Denied);
    for malformed in [
        identity(""),
        identity(&"x".repeat(256)),
        Identity {
            issuer: "http://sso.example.org/realms/agents".into(),
            subject: "bob".into(),
        },
    ] {
        let refused = Routing::new(store.clone(), installation())
            .resolve(&malformed)
            .await;
        assert_eq!(refused.unwrap_err().code, "invalid_request");
    }
    assert_eq!(store.receipt_count().await, receipts);
    assert_eq!(store.audit_actions().await, audit);
    assert_eq!(store.instance_ids().await.unwrap(), instances);
    assert_eq!(store.route_row(&identity("mallory")).await.unwrap(), None);
}

pub async fn decisions_follow_access_and_readiness_without_waiting_for_the_controller<
    S: Store + InstanceStore + RouteStore + Inspect + Clone,
>(
    store: S,
) {
    let bob = setup(&store, bob_spec()).await;
    let cluster = FakeCluster::default();
    let controller = controller(&store, &cluster, Arc::new(NoFaults));
    let epoch = store.advance_epoch().await.unwrap();
    until(&controller, epoch, bob.instance_id, Phase::Ready).await;
    let first = ready(route(&store, &identity("bob")).await);

    // Suspension denies at once, before the controller has stopped anything.
    let suspended = change(&store, &bob, access(Access::Suspended)).await;
    assert_eq!(route(&store, &identity("bob")).await, Decision::Denied);
    until(&controller, epoch, bob.instance_id, Phase::Retained).await;
    assert_eq!(route(&store, &identity("bob")).await, Decision::Denied);

    // Re-enabling waits for the new generation to be ready.
    let enabled = change(&store, &suspended, access(Access::Enabled)).await;
    assert_eq!(
        route(&store, &identity("bob")).await,
        Decision::Pending(PendingReason::InstanceNotReady)
    );
    until(&controller, epoch, bob.instance_id, Phase::Ready).await;
    let second = ready(route(&store, &identity("bob")).await);
    assert_eq!(second.instance_id, first.instance_id);
    assert_eq!(second.destination, first.destination);
    assert!(second.route_generation > first.route_generation);
    assert_eq!(second.authorization_version, enabled.version);

    change(&store, &enabled, access(Access::Revoked)).await;
    assert_eq!(route(&store, &identity("bob")).await, Decision::Denied);
}
