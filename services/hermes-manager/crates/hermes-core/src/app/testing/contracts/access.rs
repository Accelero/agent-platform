//! Envelope and access cases.

use std::sync::Arc;

use uuid::Uuid;

use super::super::{Inspect, ScriptedFaults, identity, installation, join, settings};
use crate::app::access::{
    self, BootstrapAdministrator, GrantAdministrator, GrantReceipt, UpdateAdministrator,
};
use crate::app::actor::Actor;
use crate::app::envelope::{CommandEnvelope, IdempotencyKey};
use crate::app::error::AppError;
use crate::app::faults::{FaultPoint, NoFaults};
use crate::app::ports::{AccessReader, Store};
use crate::app::settings::Settings;
use crate::domain::ErrorKind;
use crate::domain::identity::Identity;
use crate::domain::installation::Installation;

pub(super) fn envelope<S: Store + Clone>(store: &S) -> CommandEnvelope<S> {
    CommandEnvelope::new(store.clone(), settings(), Arc::new(NoFaults))
}

fn crashing<S: Store + Clone>(store: &S, point: FaultPoint) -> CommandEnvelope<S> {
    CommandEnvelope::new(
        store.clone(),
        settings(),
        Arc::new(ScriptedFaults::once(point)),
    )
}

pub(super) fn key(value: &str) -> IdempotencyKey {
    IdempotencyKey::parse(value).expect("valid key")
}

pub(super) fn admin(subject: &str) -> Actor {
    Actor::Administrator(identity(subject))
}

pub(super) async fn bootstrap<S: Store + Clone>(store: &S, subject: &str) -> GrantReceipt {
    let command = BootstrapAdministrator {
        identity: identity(subject),
        recover: false,
    };
    envelope(store)
        .execute(
            &Actor::Operator,
            Uuid::new_v4(),
            &key("bootstrap"),
            &command,
        )
        .await
        .expect("bootstrap")
}

pub(super) fn grant(subject: &str, manage: bool) -> GrantAdministrator {
    GrantAdministrator {
        identity: identity(subject),
        can_manage_administrators: manage,
    }
}

/// `by` grants `subject` administration after `subject` has signed in once.
pub(super) async fn grant_verified<S: Store + Inspect + Clone>(
    store: &S,
    by: &str,
    subject: &str,
    manage: bool,
) -> GrantReceipt {
    store.mark_verified(&identity(subject)).await;
    envelope(store)
        .execute(
            &admin(by),
            Uuid::new_v4(),
            &key(&format!("grant-{subject}")),
            &grant(subject, manage),
        )
        .await
        .expect("grant")
}

pub(super) fn revoke(receipt: &GrantReceipt) -> UpdateAdministrator {
    UpdateAdministrator {
        identity_id: receipt.identity_id,
        expected_version: receipt.version,
        active: false,
        can_manage_administrators: false,
    }
}

pub(super) fn code<T>(result: Result<T, AppError>) -> &'static str {
    result.err().expect("refused").code
}

pub async fn bootstrap_creates_one_manager_and_replays_its_receipt<S: Store + Inspect + Clone>(
    store: S,
) {
    let alice = identity("alice");
    let command = BootstrapAdministrator {
        identity: alice.clone(),
        recover: false,
    };
    let envelope = envelope(&store);
    let first = envelope
        .execute(&Actor::Operator, Uuid::new_v4(), &key("first"), &command)
        .await
        .unwrap();
    let replay = envelope
        .execute(&Actor::Operator, Uuid::new_v4(), &key("first"), &command)
        .await
        .unwrap();
    assert_eq!(first, replay);
    assert_eq!(first.version, 1);
    assert_eq!(store.audit_actions().await, ["administrator.bootstrap"]);
    assert!(store.grant_of(&alice).await.unwrap().manages());

    let second = BootstrapAdministrator {
        identity: identity("bob"),
        recover: false,
    };
    let refused = envelope
        .execute(&Actor::Operator, Uuid::new_v4(), &key("second"), &second)
        .await;
    assert_eq!(code(refused), "administrators_exist");
    assert_eq!(store.grant_of(&identity("bob")).await, None);

    let recover = BootstrapAdministrator {
        identity: alice,
        recover: true,
    };
    let recovered = envelope
        .execute(&Actor::Operator, Uuid::new_v4(), &key("recover"), &recover)
        .await
        .unwrap();
    assert_eq!(recovered.version, 2);
}

pub async fn changed_body_with_the_same_key_is_an_idempotency_conflict<
    S: Store + Inspect + Clone,
>(
    store: S,
) {
    bootstrap(&store, "alice").await;
    let other = BootstrapAdministrator {
        identity: identity("bob"),
        recover: true,
    };
    let result = envelope(&store)
        .execute(&Actor::Operator, Uuid::new_v4(), &key("bootstrap"), &other)
        .await;
    assert_eq!(result, Err(AppError::idempotency_conflict()));
    assert_eq!(store.grant_of(&identity("bob")).await, None);
}

pub async fn idempotency_keys_are_scoped_to_the_actor<S: Store + Inspect + Clone>(store: S) {
    bootstrap(&store, "alice").await;
    store.mark_verified(&identity("bob")).await;
    store.mark_verified(&identity("carol")).await;
    let envelope = envelope(&store);
    envelope
        .execute(
            &admin("alice"),
            Uuid::new_v4(),
            &key("same"),
            &grant("bob", true),
        )
        .await
        .unwrap();
    envelope
        .execute(
            &admin("bob"),
            Uuid::new_v4(),
            &key("same"),
            &grant("carol", false),
        )
        .await
        .unwrap();
    assert!(store.grant_of(&identity("carol")).await.is_some());
}

pub async fn rejected_command_leaves_no_receipt_audit_or_state<S: Store + Inspect + Clone>(
    store: S,
) {
    bootstrap(&store, "alice").await;
    let envelope = envelope(&store);
    let refused = envelope
        .execute(
            &admin("alice"),
            Uuid::new_v4(),
            &key("k"),
            &grant("bob", false),
        )
        .await;
    assert_eq!(code(refused), "identity_not_verified");
    assert_eq!(store.audit_actions().await, ["administrator.bootstrap"]);
    assert_eq!(store.receipt_count().await, 1);

    // The refused reservation rolled back, so the key is free for another body.
    store.mark_verified(&identity("bob")).await;
    envelope
        .execute(
            &admin("alice"),
            Uuid::new_v4(),
            &key("k"),
            &grant("bob", true),
        )
        .await
        .unwrap();
    assert!(store.grant_of(&identity("bob")).await.unwrap().manages());
}

pub async fn crash_before_commit_persists_nothing<S: Store + Inspect + Clone>(store: S) {
    bootstrap(&store, "alice").await;
    store.mark_verified(&identity("bob")).await;
    let command = grant("bob", false);
    let crashed = crashing(&store, FaultPoint::EnvelopeBeforeCommit)
        .execute(&admin("alice"), Uuid::new_v4(), &key("k"), &command)
        .await;
    assert_eq!(crashed, Err(AppError::fault_injected()));
    assert_eq!(store.grant_of(&identity("bob")).await, None);
    assert_eq!(store.audit_actions().await, ["administrator.bootstrap"]);

    envelope(&store)
        .execute(&admin("alice"), Uuid::new_v4(), &key("k"), &command)
        .await
        .unwrap();
    assert_eq!(store.audit_actions().await.len(), 2);
}

pub async fn crash_after_commit_replays_the_recorded_receipt<S: Store + Inspect + Clone>(store: S) {
    bootstrap(&store, "alice").await;
    store.mark_verified(&identity("bob")).await;
    let command = grant("bob", false);
    let lost = crashing(&store, FaultPoint::EnvelopeAfterCommit)
        .execute(&admin("alice"), Uuid::new_v4(), &key("k"), &command)
        .await;
    assert_eq!(lost, Err(AppError::fault_injected()));
    assert!(store.grant_of(&identity("bob")).await.is_some());

    let retried = envelope(&store)
        .execute(&admin("alice"), Uuid::new_v4(), &key("k"), &command)
        .await
        .unwrap();
    assert_eq!(retried.version, 1);
    assert_eq!(
        store.audit_actions().await,
        ["administrator.bootstrap", "administrator.grant"]
    );
}

pub async fn concurrent_same_key_requests_apply_once<S: Store + Inspect + Clone>(store: S) {
    bootstrap(&store, "alice").await;
    store.mark_verified(&identity("bob")).await;
    let command = grant("bob", false);
    let (first, second) = (envelope(&store), envelope(&store));
    let actor = admin("alice");
    let (a, b) = join(
        first.execute(&actor, Uuid::new_v4(), &key("k"), &command),
        second.execute(&actor, Uuid::new_v4(), &key("k"), &command),
    )
    .await;
    assert_eq!(a.unwrap(), b.unwrap());
    assert_eq!(store.audit_actions().await.len(), 2);
}

pub async fn actors_need_a_current_matching_grant<S: Store + Inspect + Clone>(store: S) {
    bootstrap(&store, "alice").await;
    store.mark_verified(&identity("bob")).await;
    store.mark_verified(&identity("carol")).await;
    let envelope = envelope(&store);
    let attempt = |actor: Actor, command: GrantAdministrator| {
        let envelope = &envelope;
        async move {
            envelope
                .execute(
                    &actor,
                    Uuid::new_v4(),
                    &key(&Uuid::new_v4().to_string()),
                    &command,
                )
                .await
        }
    };
    let without_grant = attempt(admin("bob"), grant("carol", false)).await;
    assert_eq!(without_grant, Err(AppError::forbidden()));

    let bob = grant_verified(&store, "alice", "bob", false).await;
    let without_manage = attempt(admin("bob"), grant("carol", false)).await;
    assert_eq!(without_manage, Err(AppError::forbidden()));

    let operator = attempt(Actor::Operator, grant("carol", false)).await;
    assert_eq!(operator, Err(AppError::forbidden()));
    let foreign = Actor::Administrator(Identity {
        issuer: "https://other.example.org/realms/agents".into(),
        subject: "alice".into(),
    });
    assert_eq!(
        attempt(foreign, grant("carol", false)).await,
        Err(AppError::forbidden())
    );

    let bootstrap_by_admin = envelope
        .execute(
            &admin("alice"),
            Uuid::new_v4(),
            &key("b"),
            &BootstrapAdministrator {
                identity: identity("carol"),
                recover: true,
            },
        )
        .await;
    assert_eq!(bootstrap_by_admin, Err(AppError::forbidden()));

    envelope
        .execute(
            &admin("alice"),
            Uuid::new_v4(),
            &key("revoke"),
            &revoke(&bob),
        )
        .await
        .unwrap();
    assert!(!store.grant_of(&identity("bob")).await.unwrap().active);
    assert_eq!(store.grant_of(&identity("carol")).await, None);
}

pub async fn last_manager_survives_concurrent_revocations<S: Store + Inspect + Clone>(store: S) {
    let alice = bootstrap(&store, "alice").await;
    let bob = grant_verified(&store, "alice", "bob", true).await;
    let (first, second) = (envelope(&store), envelope(&store));
    let (by_alice, by_bob) = (admin("alice"), admin("bob"));
    let (revoke_bob, revoke_alice) = (revoke(&bob), revoke(&alice));
    let (a, b) = join(
        first.execute(&by_alice, Uuid::new_v4(), &key("revoke"), &revoke_bob),
        second.execute(&by_bob, Uuid::new_v4(), &key("revoke"), &revoke_alice),
    )
    .await;
    assert_eq!(
        [&a, &b].iter().filter(|r| r.is_ok()).count(),
        1,
        "{a:?} {b:?}"
    );
    let refused = a.err().or(b.err()).unwrap();
    assert!(
        matches!(refused.code, "access_denied" | "last_administrator"),
        "{refused:?}"
    );
    assert_eq!(store.active_managers().await, 1);
}

pub async fn foreign_installation_is_refused_without_writes<S: Store + Inspect + Clone>(store: S) {
    let foreign = Installation {
        id: Uuid::new_v4(),
        ..installation()
    };
    let command = BootstrapAdministrator {
        identity: identity("alice"),
        recover: false,
    };
    let result = CommandEnvelope::new(
        store.clone(),
        Arc::new(Settings::minimal(foreign)),
        Arc::new(NoFaults),
    )
    .execute(&Actor::Operator, Uuid::new_v4(), &key("k"), &command)
    .await;
    assert_eq!(result, Err(AppError::registry_mismatch()));
    assert!(store.audit_actions().await.is_empty());
    assert_eq!(store.receipt_count().await, 0);
}

pub async fn stale_version_is_a_precondition_failure<S: Store + Inspect + Clone>(store: S) {
    bootstrap(&store, "alice").await;
    let bob = grant_verified(&store, "alice", "bob", false).await;
    let stale = UpdateAdministrator {
        expected_version: bob.version + 5,
        ..revoke(&bob)
    };
    let result = envelope(&store)
        .execute(&admin("alice"), Uuid::new_v4(), &key("k"), &stale)
        .await
        .unwrap_err();
    assert_eq!(
        (result.kind, result.code),
        (ErrorKind::Precondition, "stale_version")
    );
    assert!(store.grant_of(&identity("bob")).await.unwrap().active);
}

pub async fn access_reads_follow_committed_grants<S: Store + AccessReader + Inspect + Clone>(
    store: S,
) {
    let installation = installation();
    let bob = identity("bob");
    assert_eq!(
        access::admit(&store, &installation, &bob).await,
        Err(AppError::forbidden())
    );
    // The refused sign-in still counts as login evidence for a later grant.
    bootstrap(&store, "alice").await;
    let receipt = envelope(&store)
        .execute(
            &admin("alice"),
            Uuid::new_v4(),
            &key("grant-bob"),
            &grant("bob", false),
        )
        .await
        .unwrap();
    let granted = access::admit(&store, &installation, &bob).await.unwrap();
    assert!(!granted.manages());
    assert_eq!(
        access::permitted_actions(&granted),
        access::ADMINISTER_ACTIONS
    );

    envelope(&store)
        .execute(
            &admin("alice"),
            Uuid::new_v4(),
            &key("revoke-bob"),
            &revoke(&receipt),
        )
        .await
        .unwrap();
    assert_eq!(
        access::current_grant(&store, &bob).await,
        Err(AppError::forbidden())
    );
    let foreign = Identity {
        issuer: "https://other.example.org/realms/agents".into(),
        subject: "bob".into(),
    };
    assert_eq!(
        access::admit(&store, &installation, &foreign).await,
        Err(AppError::forbidden())
    );
}
