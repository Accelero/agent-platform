#[path = "support/grants.rs"]
mod grant_fixture;
use hermes_adapters::postgres::{FreshInventory, PgStore};
use hermes_application::{
    AdminPolicy, AssignmentCommand, Assignments, Error, InstallationScope, OperationClaim,
    OperationQueue, VerifiedPrincipal,
    namespace_fence::{
        Admission, NamespaceFenceObservation, NamespaceFenceRepository, PreparedNamespaceFence,
    },
    resources::{OwnedResource, ResourceObservation, ResourceRepository, ResourceVersion},
};
use hermes_domain::{
    Access, Identity, TemplateVersion,
    resources::{ResourceKind, ResourceSpec, Retention},
};
use sqlx::PgPool;
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

struct Fixture {
    store: PgStore,
    scope: InstallationScope,
    claim: OperationClaim,
    assignments: Assignments<PgStore>,
    actor: VerifiedPrincipal,
    namespace: OwnedResource,
}
async fn fixture(pool: &PgPool) -> Fixture {
    let scope = InstallationScope {
        id: Uuid::new_v4(),
        epoch: 1,
        issuer: "https://sso.example.org/realms/agents".into(),
    };
    let store = PgStore::connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    store
        .initialize_fresh(
            &scope,
            FreshInventory {
                cluster_identity: "fence-test".into(),
                digest: "a".repeat(64),
                registered_instances: 0,
                unexplained_resources: 0,
            },
        )
        .await
        .unwrap();
    grant_fixture::grant(pool, &scope, &["administrator"]).await;
    sqlx::query("INSERT INTO manager.instance_templates(id,version,schema_version,image,content_digest,approved) VALUES('personal-default',1,1,$1,$2,true)").bind(format!("ghcr.io/example/native@sha256:{}","a".repeat(64))).bind("a".repeat(64)).execute(pool).await.unwrap();
    let actor = VerifiedPrincipal {
        identity: Identity {
            issuer: scope.issuer.clone(),
            subject: "administrator".into(),
        },
        audiences: BTreeSet::from(["manager-api".into()]),
        client_roles: BTreeMap::from([(
            "manager-admin".into(),
            BTreeSet::from(["administrator".into()]),
        )]),
    };
    let assignments = Assignments::new(
        store.clone(),
        scope.clone(),
        AdminPolicy {
            issuer: scope.issuer.clone(),
            audience: "manager-api".into(),
            management_client: "manager-admin".into(),
            administrator_role: "administrator".into(),
        },
    )
    .unwrap();
    assignments
        .execute(
            &actor,
            Uuid::new_v4(),
            "create",
            AssignmentCommand::Create {
                identity: Identity {
                    issuer: scope.issuer.clone(),
                    subject: "owner-a".into(),
                },
                access: Access::Enabled,
                template: TemplateVersion {
                    id: "personal-default".into(),
                    version: 1,
                },
            },
        )
        .await
        .unwrap();
    let claim = store
        .claim(&scope, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    let spec = ResourceSpec {
        kind: ResourceKind::Namespace,
        namespace: None,
        namespace_uid: None,
        name: hermes_domain::instance_namespace(scope.id, claim.instance_id),
        manifest_digest: "b".repeat(64),
        purpose: "namespace".into(),
        retention: Retention::Retain,
        encrypted_material_ref: None,
    };
    store
        .prepare_resource(&scope, &claim, "namespace", spec.clone())
        .await
        .unwrap();
    store
        .bind_resource(
            &scope,
            &claim,
            "namespace",
            ResourceObservation {
                uid: "namespace-uid".into(),
                resource_version: "1".into(),
                manifest_digest: spec.manifest_digest.clone(),
            },
        )
        .await
        .unwrap();
    let namespace = store
        .find_resource(&scope, &claim, &spec)
        .await
        .unwrap()
        .unwrap();
    Fixture {
        store,
        scope,
        claim,
        assignments,
        actor,
        namespace,
    }
}
fn version(rv: &str) -> ResourceVersion {
    ResourceVersion {
        uid: "namespace-uid".into(),
        resource_version: rv.into(),
    }
}
fn observation(prepared: &PreparedNamespaceFence, rv: &str) -> NamespaceFenceObservation {
    NamespaceFenceObservation {
        uid: prepared.input.namespace_uid.clone(),
        resource_version: rv.into(),
        input_digest: prepared.input.digest().unwrap(),
        state: prepared.input.state.clone(),
    }
}

use hermes_application::native_children::{
    NativeChildAction, NativeChildObservation, NativeChildRepository, NativeChildResult,
};
async fn deployment(f: &Fixture) {
    let spec = ResourceSpec {
        kind: ResourceKind::Deployment,
        namespace: Some(f.namespace.creation.spec.name.clone()),
        namespace_uid: Some("namespace-uid".into()),
        name: "hermes".into(),
        manifest_digest: "c".repeat(64),
        purpose: "hermes-workload".into(),
        retention: Retention::Delete,
        encrypted_material_ref: None,
    };
    f.store
        .prepare_resource(&f.scope, &f.claim, "deployment", spec.clone())
        .await
        .unwrap();
    f.store
        .bind_resource(
            &f.scope,
            &f.claim,
            "deployment",
            ResourceObservation {
                uid: "deployment-uid".into(),
                resource_version: "10".into(),
                manifest_digest: spec.manifest_digest,
            },
        )
        .await
        .unwrap();
}
async fn fence(f: &Fixture, admission: Admission, key: &str) {
    let p = f
        .store
        .prepare_namespace_fence(&f.scope, &f.claim, key, admission, version("10"))
        .await
        .unwrap();
    f.store
        .complete_namespace_fence(&f.scope, &f.claim, key, observation(&p, "11"))
        .await
        .unwrap();
}
fn pod(uid: &str, gated: bool) -> NativeChildObservation {
    NativeChildObservation {
        namespace_uid: "namespace-uid".into(),
        deployment_uid: "deployment-uid".into(),
        replica_set_name: "hermes-replica".into(),
        replica_set_uid: "rs-uid".into(),
        pod_name: "hermes-pod".into(),
        pod_uid: uid.into(),
        resource_version: "20".into(),
        spec_digest: "d".repeat(64),
        gated,
    }
}
#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn pod_identity_and_immutable_start_intent_commit_before_gate_removal(pool: PgPool) {
    let f = fixture(&pool).await;
    deployment(&f).await;
    assert!(matches!(
        f.store
            .prepare_native_child(
                &f.scope,
                &f.claim,
                NativeChildAction::Start,
                "open",
                pod("pod-uid", true)
            )
            .await,
        Err(Error::OwnershipConflict)
    ));
    fence(&f, Admission::Closed, "close").await;
    fence(&f, Admission::Open, "open").await;
    assert!(matches!(
        f.store
            .prepare_native_child(
                &f.scope,
                &f.claim,
                NativeChildAction::Start,
                "open",
                pod("unregistered-uid", false)
            )
            .await,
        Err(Error::OwnershipConflict)
    ));
    let p = f
        .store
        .prepare_native_child(
            &f.scope,
            &f.claim,
            NativeChildAction::Start,
            "open",
            pod("pod-uid", true),
        )
        .await
        .unwrap();
    assert!(!p.completed);
    assert_eq!(
        f.store
            .native_children(&f.scope, &f.claim)
            .await
            .unwrap()
            .len(),
        1
    );
    let counts:(i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM manager.native_children),(SELECT count(*) FROM manager.native_child_actions WHERE completed_at IS NULL)").fetch_one(&pool).await.unwrap();
    assert_eq!(counts, (1, 1));
    assert!(
        sqlx::query("UPDATE manager.native_child_actions SET input='{}'")
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM manager.native_children")
            .execute(&pool)
            .await
            .is_err()
    );
    let mut updated = pod("pod-uid", false);
    updated.resource_version = "21".into();
    let retry = f
        .store
        .prepare_native_child(
            &f.scope,
            &f.claim,
            NativeChildAction::Start,
            "open",
            updated,
        )
        .await
        .unwrap();
    assert_eq!(retry.input.resource_version, "21");
    let reopened = PgStore::connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    reopened
        .complete_native_child(
            &f.scope,
            &f.claim,
            &p.input,
            NativeChildResult {
                pod_uid: "pod-uid".into(),
                input_digest: p.input.digest().unwrap(),
                absent: false,
            },
        )
        .await
        .unwrap();
    assert!(
        reopened
            .prepare_native_child(
                &f.scope,
                &f.claim,
                NativeChildAction::Start,
                "open",
                pod("pod-uid", false)
            )
            .await
            .unwrap()
            .completed
    );
    assert!(sqlx::query("UPDATE manager.native_child_actions SET completed_at=NULL WHERE completed_at IS NOT NULL").execute(&pool).await.is_err());
    let mut foreign = pod("pod-uid", true);
    foreign.replica_set_uid = "foreign-rs".into();
    assert!(matches!(
        f.store
            .prepare_native_child(
                &f.scope,
                &f.claim,
                NativeChildAction::Start,
                "open",
                foreign
            )
            .await,
        Err(Error::OwnershipConflict)
    ));
}
#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn disable_fences_old_start_and_requires_closed_fence_and_observed_uid_absence(pool: PgPool) {
    let mut f = fixture(&pool).await;
    deployment(&f).await;
    fence(&f, Admission::Closed, "close").await;
    fence(&f, Admission::Open, "open").await;
    let p = f
        .store
        .prepare_native_child(
            &f.scope,
            &f.claim,
            NativeChildAction::Start,
            "open",
            pod("pod-uid", true),
        )
        .await
        .unwrap();
    let old = f.claim.clone();
    let assignment_id: Uuid = sqlx::query_scalar("SELECT id FROM manager.assignments")
        .fetch_one(&pool)
        .await
        .unwrap();
    f.assignments
        .execute(
            &f.actor,
            Uuid::new_v4(),
            "disable",
            AssignmentCommand::SetAccess {
                assignment_id,
                expected_version: 1,
                access: Access::Suspended,
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        f.store
            .complete_native_child(
                &f.scope,
                &old,
                &p.input,
                NativeChildResult {
                    pod_uid: "pod-uid".into(),
                    input_digest: p.input.digest().unwrap(),
                    absent: false
                }
            )
            .await,
        Err(Error::StaleFence)
    ));
    f.claim = f
        .store
        .claim(&f.scope, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        f.store
            .prepare_native_child(
                &f.scope,
                &f.claim,
                NativeChildAction::Stop,
                "close",
                pod("pod-uid", false)
            )
            .await,
        Err(Error::OwnershipConflict)
    ));
    fence(&f, Admission::Closed, "close-disabled").await;
    assert!(matches!(
        f.store
            .prepare_native_child(
                &f.scope,
                &f.claim,
                NativeChildAction::Start,
                "close-disabled",
                pod("pod-uid", false)
            )
            .await,
        Err(Error::Forbidden)
    ));
    let stop = f
        .store
        .prepare_native_child(
            &f.scope,
            &f.claim,
            NativeChildAction::Stop,
            "close-disabled",
            pod("pod-uid", false),
        )
        .await
        .unwrap();
    assert!(matches!(
        f.store
            .complete_native_child(
                &f.scope,
                &f.claim,
                &stop.input,
                NativeChildResult {
                    pod_uid: "pod-uid".into(),
                    input_digest: stop.input.digest().unwrap(),
                    absent: false
                }
            )
            .await,
        Err(Error::OwnershipConflict)
    ));
    assert_eq!(
        f.store
            .native_children(&f.scope, &f.claim)
            .await
            .unwrap()
            .len(),
        1
    );
    f.store
        .complete_native_child(
            &f.scope,
            &f.claim,
            &stop.input,
            NativeChildResult {
                pod_uid: "pod-uid".into(),
                input_digest: stop.input.digest().unwrap(),
                absent: true,
            },
        )
        .await
        .unwrap();
    assert!(
        f.store
            .native_children(&f.scope, &f.claim)
            .await
            .unwrap()
            .is_empty()
    );
    let retained:(i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM manager.owned_resources),(SELECT count(*) FROM manager.native_children)").fetch_one(&pool).await.unwrap();
    assert_eq!(retained, (2, 1));
    let pending: bool =
        sqlx::query_scalar("SELECT drain_completed_at IS NULL FROM manager.instances")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(
        pending,
        "native child stop must not complete the execution drain"
    );
    // A later enable may never recycle a UID whose delete was dispatched.
    let latest = f
        .assignments
        .execute(
            &f.actor,
            Uuid::new_v4(),
            "enable-again",
            AssignmentCommand::SetAccess {
                assignment_id,
                expected_version: 2,
                access: Access::Enabled,
            },
        )
        .await
        .unwrap();
    f.claim = f
        .store
        .claim(&f.scope, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    // Test-only lifecycle proof fixture: even after the prior barrier completes,
    // this retired UID remains terminal for future start authorization.
    sqlx::query("UPDATE manager.instances SET drain_completed_at=clock_timestamp() WHERE id=$1")
        .bind(latest.instance_id)
        .execute(&pool)
        .await
        .unwrap();
    fence(&f, Admission::Closed, "close-enabled").await;
    fence(&f, Admission::Open, "open-enabled").await;
    assert!(matches!(
        f.store
            .prepare_native_child(
                &f.scope,
                &f.claim,
                NativeChildAction::Start,
                "open-enabled",
                pod("pod-uid", false)
            )
            .await,
        Err(Error::OwnershipConflict)
    ));
    let completion_fixture: bool =
        sqlx::query_scalar("SELECT drain_completed_at IS NULL FROM manager.instances")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!completion_fixture);
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn pending_start_keeps_original_input_rejects_historical_open_and_records_terminal_absence(
    pool: PgPool,
) {
    let f = fixture(&pool).await;
    deployment(&f).await;
    fence(&f, Admission::Closed, "close").await;
    assert!(matches!(
        f.store
            .prepare_native_child(
                &f.scope,
                &f.claim,
                NativeChildAction::Stop,
                "close",
                pod("no-drain-stop", true)
            )
            .await,
        Err(Error::Forbidden)
    ));
    fence(&f, Admission::Open, "open").await;
    let original = f
        .store
        .prepare_native_child(
            &f.scope,
            &f.claim,
            NativeChildAction::Start,
            "open",
            pod("old-uid", true),
        )
        .await
        .unwrap();
    assert_eq!(
        f.store.finish(&f.scope, &f.claim).await,
        Err(Error::OwnershipConflict)
    );
    fence(&f, Admission::Closed, "closed-later").await;
    assert!(matches!(
        f.store
            .load_pending_native_children(&f.scope, &f.claim, NativeChildAction::Start)
            .await,
        Err(Error::OwnershipConflict)
    ));
    assert_eq!(
        f.store
            .complete_native_child(
                &f.scope,
                &f.claim,
                &original.input,
                NativeChildResult {
                    pod_uid: "old-uid".into(),
                    input_digest: original.input.digest().unwrap(),
                    absent: false
                }
            )
            .await,
        Err(Error::OwnershipConflict)
    );
    fence(&f, Admission::Open, "opened-later").await;
    let reopened = PgStore::connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    let pending = reopened
        .load_pending_native_children(&f.scope, &f.claim, NativeChildAction::Start)
        .await
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert!(pending[0].input == original.input);
    for _ in 0..2 {
        reopened
            .complete_native_child(
                &f.scope,
                &f.claim,
                &original.input,
                NativeChildResult {
                    pod_uid: "old-uid".into(),
                    input_digest: original.input.digest().unwrap(),
                    absent: true,
                },
            )
            .await
            .unwrap();
    }
    assert!(
        reopened
            .load_pending_native_children(&f.scope, &f.claim, NativeChildAction::Start)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        reopened
            .native_children(&f.scope, &f.claim)
            .await
            .unwrap()
            .is_empty()
    );
    let outcome: String = sqlx::query_scalar(
        "SELECT outcome FROM manager.native_child_actions WHERE pod_uid='old-uid'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(outcome, "absent");
    assert!(matches!(
        reopened
            .prepare_native_child(
                &f.scope,
                &f.claim,
                NativeChildAction::Start,
                "opened-later",
                pod("old-uid", true)
            )
            .await,
        Err(Error::OwnershipConflict)
    ));
    assert_eq!(
        reopened
            .complete_native_child(
                &f.scope,
                &f.claim,
                &original.input,
                NativeChildResult {
                    pod_uid: "old-uid".into(),
                    input_digest: original.input.digest().unwrap(),
                    absent: false
                }
            )
            .await,
        Err(Error::OwnershipConflict)
    );
    assert!(
        sqlx::query(
            "UPDATE manager.native_child_actions SET outcome='started' WHERE pod_uid='old-uid'"
        )
        .execute(&pool)
        .await
        .is_err()
    );
    reopened
        .prepare_native_child(
            &f.scope,
            &f.claim,
            NativeChildAction::Start,
            "opened-later",
            pod("replacement-uid", true),
        )
        .await
        .unwrap();
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn fresh_cas_start_and_original_pending_input_can_both_commit_the_same_observed_start(
    pool: PgPool,
) {
    let f = fixture(&pool).await;
    deployment(&f).await;
    fence(&f, Admission::Closed, "close").await;
    fence(&f, Admission::Open, "open").await;
    let original = f
        .store
        .prepare_native_child(
            &f.scope,
            &f.claim,
            NativeChildAction::Start,
            "open",
            pod("pod-uid", true),
        )
        .await
        .unwrap();
    let mut current = pod("pod-uid", true);
    current.resource_version = "21".into();
    let fresh = f
        .store
        .prepare_native_child(
            &f.scope,
            &f.claim,
            NativeChildAction::Start,
            "open",
            current,
        )
        .await
        .unwrap();
    let pending = f
        .store
        .load_pending_native_children(&f.scope, &f.claim, NativeChildAction::Start)
        .await
        .unwrap();
    assert_eq!(pending.len(), 2);
    assert_eq!(original.input.resource_version, "20");
    assert_eq!(fresh.input.resource_version, "21");
    for input in [&fresh.input, &original.input] {
        f.store
            .complete_native_child(
                &f.scope,
                &f.claim,
                input,
                NativeChildResult {
                    pod_uid: input.pod_uid.clone(),
                    input_digest: input.digest().unwrap(),
                    absent: false,
                },
            )
            .await
            .unwrap();
    }
    assert!(
        f.store
            .load_pending_native_children(&f.scope, &f.claim, NativeChildAction::Start)
            .await
            .unwrap()
            .is_empty()
    );
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM manager.native_child_actions WHERE outcome='started'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 2);
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn retained_history_over_256_children_drains_in_bounded_batches_without_lifetime_cap(
    pool: PgPool,
) {
    let mut f = fixture(&pool).await;
    deployment(&f).await;
    fence(&f, Admission::Closed, "close").await;
    fence(&f, Admission::Open, "open").await;
    for n in 0..257 {
        if n % 32 == 0 {
            f.store.renew(&f.scope, &f.claim, 30).await.unwrap();
        }
        let prepared = f
            .store
            .prepare_native_child(
                &f.scope,
                &f.claim,
                NativeChildAction::Start,
                "open",
                pod(&format!("history-{n}"), true),
            )
            .await
            .unwrap();
        f.store
            .complete_native_child(
                &f.scope,
                &f.claim,
                &prepared.input,
                NativeChildResult {
                    pod_uid: prepared.input.pod_uid.clone(),
                    input_digest: prepared.input.digest().unwrap(),
                    absent: false,
                },
            )
            .await
            .unwrap();
    }
    assert_eq!(
        f.store
            .native_children(&f.scope, &f.claim)
            .await
            .unwrap()
            .len(),
        32
    );
    let assignment_id: Uuid = sqlx::query_scalar("SELECT id FROM manager.assignments")
        .fetch_one(&pool)
        .await
        .unwrap();
    f.assignments
        .execute(
            &f.actor,
            Uuid::new_v4(),
            "disable-many",
            AssignmentCommand::SetAccess {
                assignment_id,
                expected_version: 1,
                access: Access::Suspended,
            },
        )
        .await
        .unwrap();
    f.claim = f
        .store
        .claim(&f.scope, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    fence(&f, Admission::Closed, "disabled-close").await;
    let mut drained = 0;
    loop {
        f.store.renew(&f.scope, &f.claim, 30).await.unwrap();
        let batch = f.store.native_children(&f.scope, &f.claim).await.unwrap();
        assert!(batch.len() <= 32);
        if batch.is_empty() {
            break;
        }
        for child in batch {
            let prepared = f
                .store
                .prepare_native_child(
                    &f.scope,
                    &f.claim,
                    NativeChildAction::Stop,
                    "disabled-close",
                    child,
                )
                .await
                .unwrap();
            f.store
                .complete_native_child(
                    &f.scope,
                    &f.claim,
                    &prepared.input,
                    NativeChildResult {
                        pod_uid: prepared.input.pod_uid.clone(),
                        input_digest: prepared.input.digest().unwrap(),
                        absent: true,
                    },
                )
                .await
                .unwrap();
            drained += 1;
        }
    }
    assert_eq!(drained, 257);
    let retained: i64 = sqlx::query_scalar("SELECT count(*) FROM manager.native_children")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(retained, 257);
    let complete: bool =
        sqlx::query_scalar("SELECT drain_completed_at IS NOT NULL FROM manager.instances")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!complete);
}
