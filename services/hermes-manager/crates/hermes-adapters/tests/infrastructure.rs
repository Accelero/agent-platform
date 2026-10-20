use hermes_adapters::postgres::{FreshInventory, PgStore};
use hermes_application::{Error, InstallationScope, OperationQueue};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use uuid::Uuid;

async fn setup(pool: &PgPool) -> (PgStore, InstallationScope) {
    let store = PgStore::connect_with(pool.connect_options().as_ref().clone())
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
                cluster_identity: "fixture-cluster".into(),
                digest: "a".repeat(64),
                registered_instances: 0,
                unexplained_resources: 0,
            },
        )
        .await
        .unwrap();
    let definition = json!({"schemaVersion":1,"image":format!("ghcr.io/example/hermes@sha256:{}","b".repeat(64)),"storageClass":"local-path","homeGib":1,"workspaceGib":1,"cpuMillis":1000,"memoryMib":1024,"adminConfig":{"agent":{"max_turns":12}}});
    sqlx::query("INSERT INTO manager.infrastructure_templates(installation_id,id,definition) VALUES($1,'personal',$2)").bind(scope.id).bind(definition).execute(pool).await.unwrap();
    (store, scope)
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn desired_mappings_converge_transactionally_and_fence_old_workers(pool: PgPool) {
    let (store, scope) = setup(&pool).await;
    sqlx::query("INSERT INTO manager.desired_users VALUES($1,'owner-a','personal'),($1,'owner-b','personal')").bind(scope.id).execute(&pool).await.unwrap();
    let (a, b) = tokio::join!(
        store.reconcile_infrastructure(&scope),
        store.reconcile_infrastructure(&scope)
    );
    assert_eq!(a.unwrap() + b.unwrap(), 2);
    assert_eq!(store.reconcile_infrastructure(&scope).await.unwrap(), 0);
    let identities: Vec<(Uuid,String)> = sqlx::query_as("SELECT i.id,k.subject FROM manager.instances i JOIN manager.assignments a ON a.id=i.assignment_id JOIN manager.identities k ON k.id=a.identity_id ORDER BY k.subject").fetch_all(&pool).await.unwrap();
    assert_ne!(identities[0].0, identities[1].0);
    let claim = store
        .claim(&scope, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    sqlx::query("UPDATE manager.infrastructure_templates SET definition=jsonb_set(definition,'{image}',to_jsonb($2::text)) WHERE installation_id=$1").bind(scope.id).bind(format!("ghcr.io/example/hermes@sha256:{}","c".repeat(64))).execute(&pool).await.unwrap();
    assert_eq!(store.reconcile_infrastructure(&scope).await.unwrap(), 2);
    assert!(matches!(
        store.renew(&scope, &claim, 30).await,
        Err(Error::StaleFence)
    ));
    let versions: Vec<i64> =
        sqlx::query_scalar("SELECT template_version FROM manager.assignments ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(versions, vec![2, 2]);
    let reopened = PgStore::connect_with(pool.connect_options().as_ref().clone())
        .await
        .unwrap();
    assert_eq!(reopened.reconcile_infrastructure(&scope).await.unwrap(), 0);
    sqlx::query("DELETE FROM manager.desired_users WHERE subject='owner-a'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(reopened.reconcile_infrastructure(&scope).await.unwrap(), 1);
    let row = sqlx::query("SELECT a.access,i.id,i.desired_generation,i.drain_deadline_at-i.drain_started_at=interval '5 minutes' AS bounded FROM manager.assignments a JOIN manager.instances i ON i.assignment_id=a.id JOIN manager.identities k ON k.id=a.identity_id WHERE k.subject='owner-a'").fetch_one(&pool).await.unwrap();
    assert_eq!(row.get::<String, _>("access"), "revoked");
    assert_eq!(row.get::<Uuid, _>("id"), identities[0].0);
    assert_eq!(row.get::<i64, _>("desired_generation"), 3);
    assert!(row.get::<bool, _>("bounded"));
    assert_eq!(reopened.reconcile_infrastructure(&scope).await.unwrap(), 0);
    let operations: i64 =
        sqlx::query_scalar("SELECT count(*) FROM manager.operations WHERE state='Pending'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(operations, 2);
    // Historical definitions and attribution survive removal of the input.
    let old: Value = sqlx::query_scalar(
        "SELECT definition FROM manager.template_definitions WHERE template_version=1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(old["image"].as_str().unwrap().ends_with(&"b".repeat(64)));
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn invalid_template_cannot_dispatch_resources_or_partially_commit(pool: PgPool) {
    let (store, scope) = setup(&pool).await;
    sqlx::query("INSERT INTO manager.desired_users VALUES($1,'owner','personal')")
        .bind(scope.id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE manager.infrastructure_templates SET definition=jsonb_set(definition,'{image}','\"unreviewed:latest\"')").execute(&pool).await.unwrap();
    assert_eq!(store.reconcile_infrastructure(&scope).await.unwrap(), 0);
    let condition: String =
        sqlx::query_scalar("SELECT condition FROM manager.infrastructure_templates")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(condition, "invalid_definition");
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM manager.instances")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn changed_source_waits_for_original_uncertain_creation_and_transient_failures_resync(
    pool: PgPool,
) {
    use hermes_application::{
        commissioning::{CommissioningRepository, ConditionReason, Schedule},
        resources::{ResourceObservation, ResourceRepository},
        resync::PeriodicResync,
    };
    use hermes_domain::resources::{ResourceKind, ResourceSpec, Retention};
    let (store, scope) = setup(&pool).await;
    sqlx::query("INSERT INTO manager.desired_users VALUES($1,'owner','personal')")
        .bind(scope.id)
        .execute(&pool)
        .await
        .unwrap();
    store.reconcile_infrastructure(&scope).await.unwrap();
    let claim = store
        .claim(&scope, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    let namespace: String = sqlx::query_scalar("SELECT namespace FROM manager.instances")
        .fetch_one(&pool)
        .await
        .unwrap();
    let spec = ResourceSpec {
        kind: ResourceKind::Namespace,
        namespace: None,
        name: namespace,
        namespace_uid: None,
        manifest_digest: "d".repeat(64),
        purpose: "namespace".into(),
        retention: Retention::Retain,
        encrypted_material_ref: None,
    };
    store
        .prepare_resource(&scope, &claim, "namespace", spec)
        .await
        .unwrap();
    sqlx::query("UPDATE manager.infrastructure_templates SET definition=jsonb_set(definition,'{adminConfig,agent,max_turns}','24')").execute(&pool).await.unwrap();
    assert_eq!(store.reconcile_infrastructure(&scope).await.unwrap(), 0);
    store.renew(&scope, &claim, 30).await.unwrap();
    store
        .bind_resource(
            &scope,
            &claim,
            "namespace",
            ResourceObservation {
                uid: "namespace-uid".into(),
                resource_version: "1".into(),
                manifest_digest: "d".repeat(64),
            },
        )
        .await
        .unwrap();
    assert_eq!(store.reconcile_infrastructure(&scope).await.unwrap(), 1);
    assert!(matches!(
        store.renew(&scope, &claim, 30).await,
        Err(Error::StaleFence)
    ));
    let claim = store
        .claim(&scope, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    store
        .schedule(
            &scope,
            &claim,
            Schedule::Blocked {
                reason: ConditionReason::DependencyUnavailable,
            },
        )
        .await
        .unwrap();
    assert_eq!(store.enqueue_due(&scope, 64).await.unwrap(), 0);
    sqlx::query(
        "UPDATE manager.instances SET reconcile_after=clock_timestamp()-interval '1 second'",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(store.enqueue_due(&scope, 64).await.unwrap(), 1);
    let retry = store
        .claim(&scope, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(retry.kind, "resync");
    assert_eq!(retry.generation, claim.generation);
    // Ownership/admission failures still require intervention, never blind retry.
    store
        .schedule(
            &scope,
            &retry,
            Schedule::Blocked {
                reason: ConditionReason::OwnershipConflict,
            },
        )
        .await
        .unwrap();
    sqlx::query(
        "UPDATE manager.instances SET reconcile_after=clock_timestamp()-interval '1 second'",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(store.enqueue_due(&scope, 64).await.unwrap(), 0);
}
