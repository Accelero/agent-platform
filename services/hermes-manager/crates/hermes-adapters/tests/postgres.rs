//! Real PostgreSQL tests: no in-memory persistence replacement and no silent skip.
use std::collections::{BTreeMap, BTreeSet};

use hermes_adapters::postgres::{FreshInventory, PgStore};
use hermes_application::routing::{
    DeniedReason, PendingReason, RouteDecision, RouteRegistry, UnavailableReason,
};
use hermes_application::{
    Accepted, Action, AdminPolicy, AssignmentCommand, AssignmentReader, Assignments, Error,
    FailureReason, InstallationScope, OperationQueue, StepIntent, StepResult, VerifiedPrincipal,
};
use hermes_domain::{Access, Identity, TemplateVersion};
use sqlx::{PgPool, Row};
use uuid::Uuid;

const ISSUER: &str = "https://sso.example.org/realms/agents";

fn policy() -> AdminPolicy {
    AdminPolicy {
        issuer: ISSUER.into(),
        audience: "manager-api".into(),
        management_client: "manager-admin".into(),
        administrator_role: "platform-administrator".into(),
    }
}

fn actor(subject: &str) -> VerifiedPrincipal {
    VerifiedPrincipal {
        identity: Identity {
            issuer: ISSUER.into(),
            subject: subject.into(),
        },
        audiences: BTreeSet::from(["manager-api".into()]),
        client_roles: BTreeMap::from([(
            "manager-admin".into(),
            BTreeSet::from(["platform-administrator".into()]),
        )]),
    }
}

fn create(subject: &str, access: Access) -> AssignmentCommand {
    AssignmentCommand::Create {
        identity: Identity {
            issuer: ISSUER.into(),
            subject: subject.into(),
        },
        access,
        template: TemplateVersion {
            id: "personal-default".into(),
            version: 1,
        },
    }
}

fn evidence() -> FreshInventory {
    FreshInventory {
        cluster_identity: "disposable-test-cluster".into(),
        digest: "a".repeat(64),
        registered_instances: 0,
        unexplained_resources: 0,
    }
}

async fn fixture(pool: &PgPool) -> (PgStore, InstallationScope, Assignments<PgStore>) {
    let store = PgStore::connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    let scope = InstallationScope {
        id: Uuid::new_v4(),
        epoch: 1,
        issuer: ISSUER.into(),
    };
    store.initialize_fresh(&scope, evidence()).await.unwrap();
    store.check_existing(&scope).await.unwrap();
    sqlx::query("INSERT INTO manager.instance_templates (id,version,schema_version,image,content_digest,approved) VALUES ('personal-default',1,1,$1,$2,true)")
        .bind(format!("ghcr.io/example/hermes@sha256:{}", "a".repeat(64))).bind("a".repeat(64)).execute(pool).await.unwrap();
    let service = Assignments::new(store.clone(), scope.clone(), policy()).unwrap();
    (store, scope, service)
}

async fn enable(service: &Assignments<PgStore>, subject: &str) -> Accepted {
    service
        .execute(
            &actor("administrator"),
            Uuid::new_v4(),
            &format!("enable-{subject}"),
            create(subject, Access::Enabled),
        )
        .await
        .unwrap()
}

async fn count(pool: &PgPool, table: &str) -> i64 {
    let query = match table {
        "installations" => "SELECT count(*) FROM manager.installations",
        "identities" => "SELECT count(*) FROM manager.identities",
        "assignments" => "SELECT count(*) FROM manager.assignments",
        "instances" => "SELECT count(*) FROM manager.instances",
        "operations" => "SELECT count(*) FROM manager.operations",
        "audit_events" => "SELECT count(*) FROM manager.audit_events",
        "mutation_requests" => "SELECT count(*) FROM manager.mutation_requests",
        _ => panic!("unknown fixture table"),
    };
    sqlx::query_scalar(query).fetch_one(pool).await.unwrap()
}

fn intent() -> StepIntent {
    StepIntent {
        action: Action::Apply,
        creation_id: Uuid::new_v4(),
        kind: "Namespace".into(),
        namespace: None,
        name: "hi-fixture".into(),
        namespace_uid: None,
        expected_uid: None,
        expected_resource_version: None,
        manifest_digest: "b".repeat(64),
        encrypted_material_ref: Some(Uuid::new_v4()),
    }
}

async fn expire(pool: &PgPool, operation: Uuid) {
    sqlx::query("UPDATE manager.operations SET lease_until=clock_timestamp()-interval '1 second' WHERE id=$1")
        .bind(operation).execute(pool).await.unwrap();
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn acknowledgement_is_durable_and_lost_response_reuses_exact_receipt(pool: PgPool) {
    let (store, scope, service) = fixture(&pool).await;
    let first = enable(&service, "owner-a").await;
    assert_eq!(count(&pool, "assignments").await, 1);
    assert_eq!(count(&pool, "instances").await, 1);
    assert_eq!(count(&pool, "operations").await, 1);
    assert_eq!(count(&pool, "audit_events").await, 1);
    assert_eq!(count(&pool, "mutation_requests").await, 1);
    let view = store
        .get_assignment(&scope, first.assignment_id)
        .await
        .unwrap();
    assert_eq!(view.desired_generation, 1);
    assert_eq!(view.observed_generation, 0);
    assert_eq!(view.state, "Pending");
    drop(service);
    drop(store);
    let reopened = PgStore::connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    reopened.check_existing(&scope).await.unwrap();
    let service = Assignments::new(reopened.clone(), scope.clone(), policy()).unwrap();
    assert_eq!(enable(&service, "owner-a").await, first);
    assert_eq!(count(&pool, "audit_events").await, 1);
    let claim = reopened
        .claim(&scope, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claim.id, first.operation_id);
    assert_eq!(claim.kind, "commission");
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn concurrent_same_key_commands_converge_on_one_instance_and_operation(pool: PgPool) {
    let (_, _, service) = fixture(&pool).await;
    // join_all isn't needed: scoped JoinSet tasks own an Arc service.
    let service = std::sync::Arc::new(service);
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..16 {
        let service = service.clone();
        tasks.spawn(async move { enable(&service, "owner-a").await });
    }
    let first = tasks.join_next().await.unwrap().unwrap();
    while let Some(result) = tasks.join_next().await {
        assert_eq!(result.unwrap(), first);
    }
    assert_eq!(count(&pool, "assignments").await, 1);
    assert_eq!(count(&pool, "instances").await, 1);
    assert_eq!(count(&pool, "operations").await, 1);
    assert_eq!(count(&pool, "audit_events").await, 1);
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn concurrent_different_keys_cannot_duplicate_an_identity_assignment(pool: PgPool) {
    let (_, _, service) = fixture(&pool).await;
    let service = std::sync::Arc::new(service);
    let mut tasks = tokio::task::JoinSet::new();
    for index in 0..12 {
        let service = service.clone();
        tasks.spawn(async move {
            service
                .execute(
                    &actor("administrator"),
                    Uuid::new_v4(),
                    &format!("request-{index}"),
                    create("owner-a", Access::Enabled),
                )
                .await
        });
    }
    let mut successes = 0;
    while let Some(result) = tasks.join_next().await {
        match result.unwrap() {
            Ok(_) => successes += 1,
            Err(error) => assert_eq!(error, Error::AlreadyExists),
        }
    }
    assert_eq!(successes, 1);
    assert_eq!(count(&pool, "identities").await, 1);
    assert_eq!(count(&pool, "instances").await, 1);
    assert_eq!(count(&pool, "mutation_requests").await, 1);
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn idempotency_detects_payload_changes_and_is_scoped_to_exact_actor(pool: PgPool) {
    let (_, _, service) = fixture(&pool).await;
    let original = service
        .execute(
            &actor("admin-a"),
            Uuid::new_v4(),
            "same-key",
            create("owner-a", Access::Enabled),
        )
        .await
        .unwrap();
    assert_eq!(
        service
            .execute(
                &actor("admin-a"),
                Uuid::new_v4(),
                "same-key",
                create("owner-b", Access::Enabled)
            )
            .await,
        Err(Error::IdempotencyConflict)
    );
    let different = service
        .execute(
            &actor("admin-b"),
            Uuid::new_v4(),
            "same-key",
            create("owner-b", Access::Enabled),
        )
        .await
        .unwrap();
    assert_ne!(original.instance_id, different.instance_id);
    assert_eq!(count(&pool, "audit_events").await, 2);
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn optimistic_disable_race_has_one_winner_and_preserves_owner_data(pool: PgPool) {
    let (store, scope, service) = fixture(&pool).await;
    let first = enable(&service, "owner-a").await;
    let old_claim = store
        .claim(&scope, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    let command = |access| AssignmentCommand::SetAccess {
        assignment_id: first.assignment_id,
        expected_version: 1,
        access,
    };
    let admin = actor("administrator");
    let (left, right) = tokio::join!(
        service.execute(
            &admin,
            Uuid::new_v4(),
            "suspend",
            command(Access::Suspended)
        ),
        service.execute(&admin, Uuid::new_v4(), "revoke", command(Access::Revoked)),
    );
    assert!(matches!(
        (&left, &right),
        (Ok(_), Err(Error::PreconditionFailed)) | (Err(Error::PreconditionFailed), Ok(_))
    ));
    let view = store
        .get_assignment(&scope, first.assignment_id)
        .await
        .unwrap();
    assert_eq!(view.instance_id, first.instance_id);
    assert_eq!(view.version, 2);
    assert_eq!(view.desired_generation, 2);
    assert_ne!(view.access, Access::Enabled);
    assert_eq!(
        store.renew(&scope, &old_claim, 30).await,
        Err(Error::StaleFence)
    );
    assert_eq!(
        store
            .prepare_step(&scope, &old_claim, "create-namespace", intent())
            .await,
        Err(Error::StaleFence)
    );
    assert_eq!(count(&pool, "instances").await, 1);
    assert_eq!(count(&pool, "audit_events").await, 2);
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn revoked_assignment_retry_cannot_enable_or_change_identity(pool: PgPool) {
    let (store, scope, service) = fixture(&pool).await;
    let first = service
        .execute(
            &actor("administrator"),
            Uuid::new_v4(),
            "disabled",
            create("owner-a", Access::Revoked),
        )
        .await
        .unwrap();
    let retry = service
        .execute(
            &actor("administrator"),
            Uuid::new_v4(),
            "retry",
            AssignmentCommand::Retry {
                instance_id: first.instance_id,
                expected_version: 1,
            },
        )
        .await
        .unwrap();
    assert_eq!(retry.instance_id, first.instance_id);
    let view = store
        .get_assignment(&scope, first.assignment_id)
        .await
        .unwrap();
    assert_eq!(view.access, Access::Revoked);
    assert_eq!(view.identity.subject, "owner-a");
    let claim = store
        .claim(&scope, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claim.kind, "revoke");
    assert_eq!(claim.generation, 2);
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn failure_before_audit_commit_rolls_back_every_record(pool: PgPool) {
    let (_, _, service) = fixture(&pool).await;
    sqlx::raw_sql("CREATE TRIGGER test_audit_failure BEFORE INSERT ON manager.audit_events FOR EACH ROW EXECUTE FUNCTION manager.reject_change();")
        .execute(&pool).await.unwrap();
    let result = service
        .execute(
            &actor("administrator"),
            Uuid::new_v4(),
            "request-a",
            create("owner-a", Access::Enabled),
        )
        .await;
    assert_eq!(result, Err(Error::Unavailable));
    for table in [
        "identities",
        "assignments",
        "instances",
        "operations",
        "audit_events",
        "mutation_requests",
    ] {
        assert_eq!(count(&pool, table).await, 0, "{table}");
    }
    sqlx::query("DROP TRIGGER test_audit_failure ON manager.audit_events")
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        service
            .execute(
                &actor("administrator"),
                Uuid::new_v4(),
                "request-a",
                create("owner-a", Access::Enabled)
            )
            .await
            .is_ok()
    );
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn unknown_templates_and_foreign_issuers_do_not_mutate_registry(pool: PgPool) {
    let (_, _, service) = fixture(&pool).await;
    let command = AssignmentCommand::Create {
        identity: Identity {
            issuer: ISSUER.into(),
            subject: "owner-a".into(),
        },
        access: Access::Enabled,
        template: TemplateVersion {
            id: "unknown".into(),
            version: 1,
        },
    };
    assert_eq!(
        service
            .execute(&actor("administrator"), Uuid::new_v4(), "unknown", command)
            .await,
        Err(Error::TemplateUnavailable)
    );
    let command = AssignmentCommand::Create {
        identity: Identity {
            issuer: "https://foreign.example.org".into(),
            subject: "owner-a".into(),
        },
        access: Access::Enabled,
        template: TemplateVersion {
            id: "personal-default".into(),
            version: 1,
        },
    };
    assert_eq!(
        service
            .execute(&actor("administrator"), Uuid::new_v4(), "foreign", command)
            .await,
        Err(Error::Invalid)
    );
    assert_eq!(count(&pool, "identities").await, 0);
    assert_eq!(count(&pool, "mutation_requests").await, 0);
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn database_guards_prevent_reassigning_identity_or_rewriting_audit_and_templates(
    pool: PgPool,
) {
    let (_, _, service) = fixture(&pool).await;
    let first = enable(&service, "owner-a").await;
    enable(&service, "owner-b").await;
    assert!(sqlx::query("UPDATE manager.assignments SET identity_id=(SELECT id FROM manager.identities WHERE subject='owner-b') WHERE id=$1")
        .bind(first.assignment_id).execute(&pool).await.is_err());
    for query in [
        "UPDATE manager.identities SET subject='changed'",
        "DELETE FROM manager.audit_events",
        "UPDATE manager.instance_templates SET image='other'",
    ] {
        assert!(sqlx::query(query).execute(&pool).await.is_err());
    }
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn installation_modes_ids_epochs_and_schema_fail_closed(pool: PgPool) {
    let (store, scope, service) = fixture(&pool).await;
    let mut wrong = scope.clone();
    wrong.id = Uuid::new_v4();
    assert_eq!(
        store.check_existing(&wrong).await,
        Err(Error::RecoveryRequired)
    );
    wrong = scope.clone();
    wrong.epoch += 1;
    assert_eq!(
        store.check_existing(&wrong).await,
        Err(Error::RecoveryRequired)
    );
    assert_eq!(
        store.initialize_fresh(&scope, evidence()).await,
        Err(Error::RecoveryRequired)
    );
    sqlx::query("UPDATE manager.installations SET mode='maintenance'")
        .execute(&pool)
        .await
        .unwrap();
    store.check_registry(&scope).await.unwrap();
    assert_eq!(
        service
            .execute(
                &actor("administrator"),
                Uuid::new_v4(),
                "request-a",
                create("owner-a", Access::Enabled)
            )
            .await,
        Err(Error::Maintenance)
    );
    assert_eq!(
        store.claim(&scope, Uuid::new_v4(), 30).await.unwrap_err(),
        Error::Maintenance
    );
    sqlx::query("UPDATE manager.installations SET mode='restore'")
        .execute(&pool)
        .await
        .unwrap();
    store.check_registry(&scope).await.unwrap();
    assert_eq!(
        store.check_existing(&scope).await,
        Err(Error::RecoveryRequired)
    );
    sqlx::query("UPDATE manager.installations SET mode='active'")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE _sqlx_migrations SET checksum='modified'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        store.check_existing(&scope).await,
        Err(Error::RecoveryRequired)
    );
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn fresh_requires_empty_external_inventory_and_existing_never_self_bootstraps(pool: PgPool) {
    let store = PgStore::connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    let scope = InstallationScope {
        id: Uuid::new_v4(),
        epoch: 1,
        issuer: ISSUER.into(),
    };
    assert_eq!(
        store.check_existing(&scope).await,
        Err(Error::RecoveryRequired)
    );
    let mut nonempty = evidence();
    nonempty.unexplained_resources = 1;
    assert_eq!(
        store.initialize_fresh(&scope, nonempty).await,
        Err(Error::RecoveryRequired)
    );
    assert_eq!(count(&pool, "installations").await, 0);
    store.initialize_fresh(&scope, evidence()).await.unwrap();
    store.check_existing(&scope).await.unwrap();
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn unlogged_manager_tables_block_startup(pool: PgPool) {
    let (store, scope, _) = fixture(&pool).await;
    sqlx::query("ALTER TABLE manager.resource_steps SET UNLOGGED")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        store.check_existing(&scope).await,
        Err(Error::RecoveryRequired)
    );
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn concurrent_controllers_cannot_claim_the_same_operation(pool: PgPool) {
    let (store, scope, service) = fixture(&pool).await;
    enable(&service, "owner-a").await;
    let (left, right) = tokio::join!(
        store.claim(&scope, Uuid::new_v4(), 30),
        store.claim(&scope, Uuid::new_v4(), 30)
    );
    assert_eq!(
        usize::from(left.unwrap().is_some()) + usize::from(right.unwrap().is_some()),
        1
    );
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn expired_claim_recovers_original_intent_and_rejects_stale_worker_results(pool: PgPool) {
    let (store, scope, service) = fixture(&pool).await;
    enable(&service, "owner-a").await;
    let old = store
        .claim(&scope, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    let original = intent();
    let prepared = store
        .prepare_step(&scope, &old, "create-namespace", original.clone())
        .await
        .unwrap();
    assert_eq!(prepared.result, StepResult::Unknown);
    // Model process death after committed intent / external success, before UID
    // persistence. A fresh store has no in-memory work or credential state.
    expire(&pool, old.id).await;
    assert_eq!(store.renew(&scope, &old, 30).await, Err(Error::StaleFence));
    let reopened = PgStore::connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    let recovered = reopened
        .claim(&scope, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(recovered.id, old.id);
    assert!(recovered.fence > old.fence);
    assert_eq!(store.renew(&scope, &old, 30).await, Err(Error::StaleFence));
    let verified = StepResult::Verified {
        uid: "namespace-uid-a".into(),
        resource_version: "7".into(),
    };
    assert_eq!(
        store
            .record_result(&scope, &old, "create-namespace", verified.clone())
            .await,
        Err(Error::StaleFence)
    );
    let replay = reopened
        .prepare_step(&scope, &recovered, "create-namespace", original.clone())
        .await
        .unwrap();
    assert_eq!(replay.intent, original);
    assert_eq!(replay.result, StepResult::Unknown);
    let mut changed = original;
    changed.creation_id = Uuid::new_v4();
    assert_eq!(
        reopened
            .prepare_step(&scope, &recovered, "create-namespace", changed)
            .await,
        Err(Error::OwnershipConflict)
    );
    assert_eq!(
        reopened.finish(&scope, &recovered).await,
        Err(Error::OwnershipConflict)
    );
    reopened
        .record_result(&scope, &recovered, "create-namespace", verified.clone())
        .await
        .unwrap();
    reopened
        .record_result(&scope, &recovered, "create-namespace", verified)
        .await
        .unwrap();
    assert_eq!(
        reopened
            .record_result(
                &scope,
                &recovered,
                "create-namespace",
                StepResult::Verified {
                    uid: "foreign-uid".into(),
                    resource_version: "8".into()
                }
            )
            .await,
        Err(Error::OwnershipConflict)
    );
    reopened.finish(&scope, &recovered).await.unwrap();
    assert!(
        reopened
            .claim(&scope, Uuid::new_v4(), 30)
            .await
            .unwrap()
            .is_none()
    );
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn deletion_requires_exact_uid_and_verified_absence_before_completion(pool: PgPool) {
    let (store, scope, service) = fixture(&pool).await;
    enable(&service, "owner-a").await;
    let claim = store
        .claim(&scope, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    let mut deletion = intent();
    deletion.action = Action::Delete;
    assert_eq!(
        store
            .prepare_step(&scope, &claim, "delete-pod", deletion.clone())
            .await,
        Err(Error::Invalid)
    );
    deletion.expected_uid = Some("owned-uid".into());
    deletion.expected_resource_version = Some("3".into());
    store
        .prepare_step(&scope, &claim, "delete-pod", deletion)
        .await
        .unwrap();
    assert_eq!(
        store
            .record_result(
                &scope,
                &claim,
                "delete-pod",
                StepResult::Absent {
                    deleted_uid: "foreign-uid".into()
                }
            )
            .await,
        Err(Error::OwnershipConflict)
    );
    store
        .record_result(&scope, &claim, "delete-pod", StepResult::Unknown)
        .await
        .unwrap();
    assert_eq!(
        store.finish(&scope, &claim).await,
        Err(Error::OwnershipConflict)
    );
    store
        .record_result(
            &scope,
            &claim,
            "delete-pod",
            StepResult::Absent {
                deleted_uid: "owned-uid".into(),
            },
        )
        .await
        .unwrap();
    store.finish(&scope, &claim).await.unwrap();
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn uncertain_initializer_blocks_instead_of_automatic_replay(pool: PgPool) {
    let (store, scope, service) = fixture(&pool).await;
    enable(&service, "owner-a").await;
    let claim = store
        .claim(&scope, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    let mut initialization = intent();
    initialization.action = Action::Initialize;
    store
        .prepare_step(&scope, &claim, "initialize-home", initialization)
        .await
        .unwrap();
    store
        .record_result(
            &scope,
            &claim,
            "initialize-home",
            StepResult::Blocked {
                reason: FailureReason::InitializationUncertain,
            },
        )
        .await
        .unwrap();
    assert_eq!(store.finish(&scope, &claim).await, Err(Error::StaleFence));
    assert!(
        store
            .claim(&scope, Uuid::new_v4(), 30)
            .await
            .unwrap()
            .is_none()
    );
    let row = sqlx::query("SELECT state,lease_owner FROM manager.operations WHERE id=$1")
        .bind(claim.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(row.get::<String, _>("state"), "Blocked");
    assert!(row.get::<Option<Uuid>, _>("lease_owner").is_none());
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn exhausted_work_is_bounded_and_visible_as_blocked(pool: PgPool) {
    let (store, scope, service) = fixture(&pool).await;
    let first = enable(&service, "owner-a").await;
    sqlx::query("UPDATE manager.operations SET max_attempts=1 WHERE id=$1")
        .bind(first.operation_id)
        .execute(&pool)
        .await
        .unwrap();
    let claim = store
        .claim(&scope, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    expire(&pool, claim.id).await;
    assert!(
        store
            .claim(&scope, Uuid::new_v4(), 30)
            .await
            .unwrap()
            .is_none()
    );
    let state: String = sqlx::query_scalar("SELECT state FROM manager.operations WHERE id=$1")
        .bind(claim.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(state, "Blocked");
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn epoch_change_fences_live_claim_and_pagination_is_bounded(pool: PgPool) {
    let (store, scope, service) = fixture(&pool).await;
    for subject in ["owner-a", "owner-b", "owner-c"] {
        enable(&service, subject).await;
    }
    let page = store.list_assignments(&scope, None, 2).await.unwrap();
    assert_eq!(page.len(), 2);
    let next = store
        .list_assignments(&scope, Some(page[1].id), 2)
        .await
        .unwrap();
    assert_eq!(next.len(), 1);
    assert!(next[0].id > page[1].id);
    assert_eq!(
        store.list_assignments(&scope, None, 101).await,
        Err(Error::Invalid)
    );
    let claim = store
        .claim(&scope, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    sqlx::query("UPDATE manager.installations SET epoch=epoch+1")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        store
            .prepare_step(&scope, &claim, "create-namespace", intent())
            .await,
        Err(Error::RecoveryRequired)
    );
    let current = InstallationScope { epoch: 2, ..scope };
    assert_eq!(
        store.renew(&current, &claim, 30).await,
        Err(Error::StaleFence)
    );
}

async fn fixture_route(pool: &PgPool, instance: Uuid) {
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("UPDATE manager.instances SET namespace_uid=$2,state='Ready',observed_generation=desired_generation,route_generation=1 WHERE id=$1")
        .bind(instance).bind(format!("namespace-uid-{instance}")).execute(&mut *tx).await.unwrap();
    sqlx::query("INSERT INTO manager.instance_routes (instance_id,namespace_uid,service,port,scheme,generation,verified_owner,verified_storage,verified_policy,verified_backend) VALUES ($1,$2,'hermes',8080,'http',1,true,true,true,true)")
        .bind(instance).bind(format!("namespace-uid-{instance}")).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn route_resolution_is_exact_owner_read_only_and_immediately_denies_revoke(pool: PgPool) {
    let (store, scope, service) = fixture(&pool).await;
    let owner = |subject: &str| Identity {
        issuer: ISSUER.into(),
        subject: subject.into(),
    };
    assert_eq!(
        store.resolve(&scope, &owner("unassigned")).await.unwrap(),
        RouteDecision::Denied {
            reason: DeniedReason::AccessDenied
        }
    );
    assert_eq!(count(&pool, "instances").await, 0);
    let a = enable(&service, "owner-a").await;
    let b = enable(&service, "owner-b").await;
    assert_eq!(
        store.resolve(&scope, &owner("owner-a")).await.unwrap(),
        RouteDecision::Pending {
            reason: PendingReason::InstanceNotReady
        }
    );
    fixture_route(&pool, a.instance_id).await;
    fixture_route(&pool, b.instance_id).await;
    let a_route = store.resolve(&scope, &owner("owner-a")).await.unwrap();
    let b_route = store.resolve(&scope, &owner("owner-b")).await.unwrap();
    assert!(
        matches!(a_route,RouteDecision::Ready { instance_id, .. } if instance_id == a.instance_id)
    );
    assert!(
        matches!(b_route,RouteDecision::Ready { instance_id, .. } if instance_id == b.instance_id)
    );
    assert_ne!(a_route, b_route);
    assert_eq!(
        store.resolve(&scope, &owner("Owner-A")).await.unwrap(),
        RouteDecision::Denied {
            reason: DeniedReason::AccessDenied
        }
    );
    service
        .execute(
            &actor("administrator"),
            Uuid::new_v4(),
            "revoke",
            AssignmentCommand::SetAccess {
                assignment_id: a.assignment_id,
                expected_version: 1,
                access: Access::Revoked,
            },
        )
        .await
        .unwrap();
    for _ in 0..3 {
        assert_eq!(
            store.resolve(&scope, &owner("owner-a")).await.unwrap(),
            RouteDecision::Denied {
                reason: DeniedReason::AccessDenied
            }
        );
    }
    assert!(
        matches!(store.resolve(&scope, &owner("owner-b")).await.unwrap(),RouteDecision::Ready { instance_id,.. } if instance_id == b.instance_id)
    );
    assert_eq!(count(&pool, "instances").await, 2);
    assert_eq!(count(&pool, "operations").await, 3);
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn stale_missing_or_foreign_route_bindings_have_no_destination(pool: PgPool) {
    let (store, scope, service) = fixture(&pool).await;
    let a = enable(&service, "owner-a").await;
    let identity = Identity {
        issuer: ISSUER.into(),
        subject: "owner-a".into(),
    };
    fixture_route(&pool, a.instance_id).await;
    for statement in [
        "UPDATE manager.instance_routes SET namespace_uid='foreign-uid'",
        "UPDATE manager.instance_routes SET generation=2",
        "UPDATE manager.instance_routes SET port=443",
        "UPDATE manager.instance_routes SET scheme='https'",
    ] {
        let mut tx = pool.begin().await.unwrap();
        sqlx::query(statement).execute(&mut *tx).await.unwrap();
        tx.commit().await.unwrap();
        assert_eq!(
            store.resolve(&scope, &identity).await.unwrap(),
            RouteDecision::Unavailable {
                reason: UnavailableReason::RegistryUnavailable
            }
        );
        sqlx::query("UPDATE manager.instance_routes SET namespace_uid=$1,generation=1,port=8080,scheme='http'")
            .bind(format!("namespace-uid-{}",a.instance_id)).execute(&pool).await.unwrap();
    }
    sqlx::query("DELETE FROM manager.instance_routes")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        store.resolve(&scope, &identity).await.unwrap(),
        RouteDecision::Unavailable {
            reason: UnavailableReason::RegistryUnavailable
        }
    );
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn default_template_is_pinned_once_and_catalog_changes_do_not_break_replay(pool: PgPool) {
    let (store, scope, service) = fixture(&pool).await;
    let command = AssignmentCommand::CreateDefaultTemplate {
        identity: Identity {
            issuer: ISSUER.into(),
            subject: "owner-a".into(),
        },
        access: Access::Enabled,
        template_id: "personal-default".into(),
    };
    let accepted = service
        .execute(
            &actor("administrator"),
            Uuid::new_v4(),
            "default-template",
            command.clone(),
        )
        .await
        .unwrap();
    sqlx::query("INSERT INTO manager.instance_templates (id,version,schema_version,image,content_digest,approved) VALUES ('personal-default',2,1,$1,$2,true)")
        .bind(format!("ghcr.io/example/hermes@sha256:{}","b".repeat(64))).bind("b".repeat(64)).execute(&pool).await.unwrap();
    assert_eq!(
        service
            .execute(
                &actor("administrator"),
                Uuid::new_v4(),
                "default-template",
                command
            )
            .await
            .unwrap(),
        accepted
    );
    assert_eq!(
        store
            .get_assignment(&scope, accepted.assignment_id)
            .await
            .unwrap()
            .template
            .version,
        1
    );
    let newer = service
        .execute(
            &actor("administrator"),
            Uuid::new_v4(),
            "new-default",
            AssignmentCommand::CreateDefaultTemplate {
                identity: Identity {
                    issuer: ISSUER.into(),
                    subject: "owner-b".into(),
                },
                access: Access::Enabled,
                template_id: "personal-default".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .get_assignment(&scope, newer.assignment_id)
            .await
            .unwrap()
            .template
            .version,
        2
    );
    assert_eq!(count(&pool, "audit_events").await, 2);
}

#[sqlx::test(migrations = false)]
async fn existing_registry_upgrade_requires_matching_identity_and_maintenance(pool: PgPool) {
    // Install the real first migration, then upgrade using the current compiled
    // migrator. This exercises a genuine old schema, not edited success flags.
    let directory = tempfile::tempdir().unwrap();
    let initial = hermes_adapters::postgres::MIGRATOR.iter().next().unwrap();
    tokio::fs::write(
        directory.path().join("0001_registry.sql"),
        initial.sql.as_str().as_bytes(),
    )
    .await
    .unwrap();
    sqlx::migrate::Migrator::new(directory.path())
        .await
        .unwrap()
        .run(&pool)
        .await
        .unwrap();
    let store = PgStore::connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    let scope = InstallationScope {
        id: Uuid::new_v4(),
        epoch: 1,
        issuer: ISSUER.into(),
    };
    store.initialize_fresh(&scope, evidence()).await.unwrap();
    let wrong = InstallationScope {
        id: Uuid::new_v4(),
        ..scope.clone()
    };
    assert_eq!(store.migrate(&wrong).await, Err(Error::RecoveryRequired));
    assert_eq!(store.migrate(&scope).await, Err(Error::Maintenance));
    sqlx::query("UPDATE manager.installations SET mode='maintenance'")
        .execute(&pool)
        .await
        .unwrap();
    store.migrate(&scope).await.unwrap();
    store.check_registry(&scope).await.unwrap();
    assert_eq!(store.check_existing(&scope).await, Err(Error::Maintenance));
    sqlx::query("UPDATE manager.installations SET mode='active'")
        .execute(&pool)
        .await
        .unwrap();
    store.check_existing(&scope).await.unwrap();
}
