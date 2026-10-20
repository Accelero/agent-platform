//! Real PostgreSQL plan pinning and durable controller wait/retry scheduling.
use hermes_adapters::postgres::{FreshInventory, PgStore};
use hermes_application::{
    AdminPolicy, AssignmentCommand, Assignments, Error, InstallationScope, OperationClaim,
    OperationQueue, VerifiedPrincipal,
    commissioning::{CommissioningRepository, ConditionReason, Schedule, TemplateDefinition},
};
use hermes_domain::{Access, Identity, TemplateVersion};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

fn definition() -> TemplateDefinition {
    TemplateDefinition {
        schema_version: 1,
        image: format!("ghcr.io/example/hermes@sha256:{}", "a".repeat(64)),
        storage_class: "local-path".into(),
        home_gib: 5,
        workspace_gib: 10,
        cpu_millis: 500,
        memory_mib: 1024,
    }
}
fn actor(scope: &InstallationScope) -> VerifiedPrincipal {
    VerifiedPrincipal {
        identity: Identity {
            issuer: scope.issuer.clone(),
            subject: "admin".into(),
        },
        audiences: BTreeSet::from(["manager-api".into()]),
        client_roles: BTreeMap::from([(
            "manager-admin".into(),
            BTreeSet::from(["administrator".into()]),
        )]),
    }
}
async fn fixture(
    pool: &PgPool,
    body: Option<Value>,
    digest: Option<String>,
) -> (
    PgStore,
    InstallationScope,
    OperationClaim,
    Assignments<PgStore>,
) {
    let store = PgStore::connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    let scope = InstallationScope {
        id: Uuid::new_v4(),
        epoch: 1,
        issuer: "https://sso.example.org/realms/agents".into(),
    };
    store
        .initialize_fresh(
            &scope,
            FreshInventory {
                cluster_identity: "commissioning-fixture".into(),
                digest: "a".repeat(64),
                registered_instances: 0,
                unexplained_resources: 0,
            },
        )
        .await
        .unwrap();
    let definition = definition();
    sqlx::query("INSERT INTO manager.instance_templates(id,version,schema_version,image,content_digest,approved) VALUES('personal-default',1,1,$1,$2,true)").bind(&definition.image).bind(digest.unwrap_or_else(||definition.digest().unwrap())).execute(pool).await.unwrap();
    if let Some(body) = body {
        sqlx::query("INSERT INTO manager.template_definitions(template_id,template_version,definition) VALUES('personal-default',1,$1)").bind(body).execute(pool).await.unwrap();
    }
    let service = Assignments::new(
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
    service
        .execute(
            &actor(&scope),
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
    (store, scope, claim, service)
}
async fn make_due(pool: &PgPool) {
    sqlx::query("UPDATE manager.operations SET next_attempt_at=clock_timestamp()-interval '1 second' WHERE state IN ('Waiting','RetryAt')").execute(pool).await.unwrap();
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn approved_exact_definition_is_pinned_to_operation_and_cannot_be_rewritten(pool: PgPool) {
    let definition = definition();
    let (store, scope, claim, _) = fixture(
        &pool,
        Some(serde_json::to_value(&definition).unwrap()),
        None,
    )
    .await;
    let plan = store.commission_plan(&scope, &claim).await.unwrap();
    assert!(plan.template == definition);
    assert_eq!(plan.template_digest, definition.digest().unwrap());
    assert!(plan.namespace_uid.is_none());
    assert_eq!(plan.assignment.identity.subject, "owner-a");
    assert_eq!(plan.assignment.desired_generation, claim.generation);
    assert!(
        sqlx::query("UPDATE manager.template_definitions SET definition='{}'")
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM manager.template_definitions")
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(sqlx::query(
        "UPDATE manager.operations SET input=jsonb_set(input,'{templateVersion}','2') WHERE id=$1",
    )
    .bind(claim.id)
    .execute(&pool)
    .await
    .is_err());
    // An assignment changed outside the audited generation-advance command
    // cannot silently change a previously committed operation's template.
    sqlx::query("INSERT INTO manager.instance_templates(id,version,schema_version,image,content_digest,approved) SELECT id,2,schema_version,image,content_digest,approved FROM manager.instance_templates WHERE version=1").execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO manager.template_definitions(template_id,template_version,definition) SELECT template_id,2,definition FROM manager.template_definitions WHERE template_version=1").execute(&pool).await.unwrap();
    sqlx::query("UPDATE manager.assignments SET template_version=2")
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        store.commission_plan(&scope, &claim).await,
        Err(Error::OwnershipConflict)
    ));
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn reference_only_catalog_row_never_invents_a_definition(pool: PgPool) {
    let (store, scope, claim, _) = fixture(&pool, None, None).await;
    assert!(matches!(
        store.commission_plan(&scope, &claim).await,
        Err(Error::TemplateUnavailable)
    ));
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn mismatched_digest_or_unreviewed_fields_block_before_external_actions(pool: PgPool) {
    let (store, scope, claim, _) = fixture(
        &pool,
        Some(serde_json::to_value(definition()).unwrap()),
        Some("b".repeat(64)),
    )
    .await;
    assert!(matches!(
        store.commission_plan(&scope, &claim).await,
        Err(Error::TemplateUnavailable)
    ));
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM manager.owned_resources")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    let mut definition = serde_json::to_value(definition()).unwrap();
    definition["arbitraryPod"] = json!({"hostNetwork":true});
    assert!(serde_json::from_value::<TemplateDefinition>(definition).is_err());
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn waiting_reopens_from_database_due_time_without_spending_failure_budget_or_publishing_route(
    pool: PgPool,
) {
    let (store, scope, claim, _) = fixture(
        &pool,
        Some(serde_json::to_value(definition()).unwrap()),
        None,
    )
    .await;
    store
        .schedule(
            &scope,
            &claim,
            Schedule::Waiting {
                reason: ConditionReason::StoragePending,
                seconds: 20,
            },
        )
        .await
        .unwrap();
    let row=sqlx::query("SELECT state,attempt,next_attempt_at>clock_timestamp() AS future,last_condition FROM manager.operations WHERE id=$1").bind(claim.id).fetch_one(&pool).await.unwrap();
    assert_eq!(row.get::<String, _>("state"), "Waiting");
    assert_eq!(row.get::<i32, _>("attempt"), 0);
    assert!(row.get::<bool, _>("future"));
    assert_eq!(
        row.get::<Value, _>("last_condition")["reason"],
        "storage_pending"
    );
    assert!(
        store
            .claim(&scope, Uuid::new_v4(), 30)
            .await
            .unwrap()
            .is_none()
    );
    let reopened = PgStore::connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    make_due(&pool).await;
    let next = reopened
        .claim(&scope, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(next.id, claim.id);
    assert_eq!(next.generation, claim.generation);
    assert!(next.fence > claim.fence);
    assert!(matches!(
        store
            .schedule(
                &scope,
                &claim,
                Schedule::Blocked {
                    reason: ConditionReason::OwnershipConflict
                }
            )
            .await,
        Err(Error::StaleFence)
    ));
    reopened.commission_plan(&scope, &next).await.unwrap();
    let routes: i64 = sqlx::query_scalar("SELECT count(*) FROM manager.instance_routes")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(routes, 0);
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn repeated_dependency_failures_are_bounded_and_waiting_has_an_absolute_deadline(
    pool: PgPool,
) {
    let (store, scope, claim, _) = fixture(
        &pool,
        Some(serde_json::to_value(definition()).unwrap()),
        None,
    )
    .await;
    sqlx::query("UPDATE manager.operations SET max_attempts=2")
        .execute(&pool)
        .await
        .unwrap();
    store
        .schedule(
            &scope,
            &claim,
            Schedule::Retry {
                reason: ConditionReason::DependencyUnavailable,
                seconds: 1,
            },
        )
        .await
        .unwrap();
    make_due(&pool).await;
    let next = store
        .claim(&scope, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    store
        .schedule(
            &scope,
            &next,
            Schedule::Retry {
                reason: ConditionReason::DependencyUnavailable,
                seconds: 1,
            },
        )
        .await
        .unwrap();
    assert!(
        store
            .claim(&scope, Uuid::new_v4(), 30)
            .await
            .unwrap()
            .is_none()
    );
    let state: String = sqlx::query_scalar("SELECT state FROM manager.instances")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(state, "Blocked");
    // A separate pending generation expires by database time even if every
    // attempt is an ordinary readiness wait rather than an API failure.
    // Advance the operation's age only inside this isolated fixture. Production
    // writers cannot rewrite the original deadline or creation provenance.
    let mut time_jump = pool.begin().await.unwrap();
    sqlx::raw_sql("ALTER TABLE manager.operations DISABLE TRIGGER immutable_operation_identity; UPDATE manager.operations SET state='Pending',created_at=clock_timestamp()-interval '16 minutes',attempt=0,next_attempt_at=clock_timestamp(); ALTER TABLE manager.operations ENABLE TRIGGER immutable_operation_identity;").execute(&mut *time_jump).await.unwrap();
    time_jump.commit().await.unwrap();
    let expired = store
        .claim(&scope, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    store
        .schedule(
            &scope,
            &expired,
            Schedule::Waiting {
                reason: ConditionReason::NativePending,
                seconds: 5,
            },
        )
        .await
        .unwrap();
    let condition: Value = sqlx::query_scalar("SELECT last_condition FROM manager.operations")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(condition["reason"], "deadline_exceeded");
    let state: String = sqlx::query_scalar("SELECT state FROM manager.operations")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(state, "Blocked");
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn revoke_fences_old_plan_and_scheduling_cannot_reenable_access(pool: PgPool) {
    let (store, scope, claim, service) = fixture(
        &pool,
        Some(serde_json::to_value(definition()).unwrap()),
        None,
    )
    .await;
    let plan = store.commission_plan(&scope, &claim).await.unwrap();
    service
        .execute(
            &actor(&scope),
            Uuid::new_v4(),
            "revoke",
            AssignmentCommand::SetAccess {
                assignment_id: plan.assignment.id,
                expected_version: 1,
                access: Access::Revoked,
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        store.commission_plan(&scope, &claim).await,
        Err(Error::StaleFence)
    ));
    let disabled = store
        .claim(&scope, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        store.commission_plan(&scope, &disabled).await,
        Err(Error::Forbidden)
    ));
    store
        .schedule(
            &scope,
            &disabled,
            Schedule::Waiting {
                reason: ConditionReason::NativePending,
                seconds: 5,
            },
        )
        .await
        .unwrap();
    let row=sqlx::query("SELECT a.access,i.state FROM manager.assignments a JOIN manager.instances i ON i.assignment_id=a.id").fetch_one(&pool).await.unwrap();
    assert_eq!(row.get::<String, _>("access"), "revoked");
    assert_eq!(row.get::<String, _>("state"), "Suspending");
}
