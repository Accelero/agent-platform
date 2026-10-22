//! Real PostgreSQL periodic work with completed lifecycle fixtures. Setting a
//! fixture's observed state here is not a production readiness/activation path.
#[path = "support/grants.rs"]
mod grant_fixture;
use std::collections::{BTreeMap, BTreeSet};

use hermes_adapters::postgres::{FreshInventory, PgStore};
use hermes_application::{
    Accepted, AdminPolicy, AssignmentCommand, Assignments, Error, InstallationScope,
    OperationQueue, VerifiedPrincipal,
    commissioning::{CommissioningRepository, ConditionReason, Schedule},
    resync::{PeriodicResync, ResyncInput},
};
use hermes_domain::{Access, Identity, TemplateVersion};
use serde_json::Value;
use sqlx::{PgPool, Row};
use uuid::Uuid;

struct Fixture {
    store: PgStore,
    scope: InstallationScope,
    commands: Assignments<PgStore>,
    actor: VerifiedPrincipal,
}
async fn fixture(pool: &PgPool) -> Fixture {
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
                cluster_identity: "resync-fixture".into(),
                digest: "a".repeat(64),
                registered_instances: 0,
                unexplained_resources: 0,
            },
        )
        .await
        .unwrap();
    grant_fixture::grant(pool, &scope, &["administrator"]).await;
    sqlx::query("INSERT INTO manager.instance_templates(id,version,schema_version,image,content_digest,approved) VALUES('personal-default',1,1,$1,$2,true)")
        .bind(format!("ghcr.io/example/hermes@sha256:{}","a".repeat(64))).bind("b".repeat(64)).execute(pool).await.unwrap();
    let commands = Assignments::new(
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
    Fixture {
        store,
        scope,
        commands,
        actor,
    }
}
async fn completed(pool: &PgPool, f: &Fixture, subject: &str, access: Access) -> Accepted {
    completed_registration(pool, f, subject, access, true).await
}
async fn completed_registration(
    pool: &PgPool,
    f: &Fixture,
    subject: &str,
    access: Access,
    registered: bool,
) -> Accepted {
    let accepted = f
        .commands
        .execute(
            &f.actor,
            Uuid::new_v4(),
            subject,
            AssignmentCommand::Create {
                identity: Identity {
                    issuer: f.scope.issuer.clone(),
                    subject: subject.into(),
                },
                access,
                template: TemplateVersion {
                    id: "personal-default".into(),
                    version: 1,
                },
            },
        )
        .await
        .unwrap();
    let state = match access {
        Access::Enabled => "Ready",
        Access::Suspended => "Suspended",
        Access::Revoked => "Retained",
    };
    sqlx::query("UPDATE manager.operations SET state='Succeeded' WHERE id=$1")
        .bind(accepted.operation_id)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("UPDATE manager.instances SET namespace_uid=$2,state=$3,observed_generation=desired_generation,reconcile_after=clock_timestamp()-interval '1 second' WHERE id=$1")
        .bind(accepted.instance_id).bind(registered.then(||Uuid::new_v4().to_string())).bind(state).execute(pool).await.unwrap();
    accepted
}
async fn due(pool: &PgPool) {
    sqlx::query(
        "UPDATE manager.instances SET reconcile_after=clock_timestamp()-interval '1 second'",
    )
    .execute(pool)
    .await
    .unwrap();
}
async fn history(pool: &PgPool, operation: Uuid) -> Value {
    sqlx::query_scalar("SELECT to_jsonb(o) FROM manager.operations o WHERE id=$1")
        .bind(operation)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn database_due_time_enqueues_fresh_history_without_reopening_or_reauthorizing(pool: PgPool) {
    let f = fixture(&pool).await;
    let owner = completed(&pool, &f, "owner", Access::Enabled).await;
    let command_before = history(&pool, owner.operation_id).await;
    let audit_before: Value =
        sqlx::query_scalar("SELECT jsonb_agg(to_jsonb(a) ORDER BY id) FROM manager.audit_events a")
            .fetch_one(&pool)
            .await
            .unwrap();
    let request_before: Value = sqlx::query_scalar(
        "SELECT jsonb_agg(to_jsonb(r) ORDER BY idempotency_key) FROM manager.mutation_requests r",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    sqlx::query("UPDATE manager.instances SET reconcile_after=clock_timestamp()+interval '1 day'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(f.store.enqueue_due(&f.scope, 64).await.unwrap(), 0);
    due(&pool).await;
    assert_eq!(f.store.enqueue_due(&f.scope, 64).await.unwrap(), 1);
    assert_eq!(f.store.enqueue_due(&f.scope, 64).await.unwrap(), 0);
    let row = sqlx::query(
        "SELECT id,kind,generation,epoch,input,state FROM manager.operations WHERE kind='resync'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let first: Uuid = row.get("id");
    assert_ne!(first, owner.operation_id);
    assert_eq!(row.get::<String, _>("state"), "Pending");
    assert_eq!(row.get::<i64, _>("generation"), 1);
    assert_eq!(row.get::<i64, _>("epoch"), 1);
    let pinned: ResyncInput = serde_json::from_value(row.get("input")).unwrap();
    assert_eq!(pinned.access, Access::Enabled);
    assert_eq!(pinned.template_id, "personal-default");
    assert_eq!(pinned.template_version, 1);
    assert_eq!(pinned.assignment_version, 1);
    assert_eq!(pinned.generation, 1);
    assert_eq!(pinned.epoch, 1);
    let row=sqlx::query("SELECT i.namespace_uid,i.state,i.desired_generation,i.observed_generation,a.version,i.reconcile_after>clock_timestamp()+interval '25 seconds' AND i.reconcile_after<clock_timestamp()+interval '31 seconds' AS bounded_due FROM manager.instances i JOIN manager.assignments a ON a.id=i.assignment_id").fetch_one(&pool).await.unwrap();
    assert!(row.get::<bool, _>("bounded_due"));
    assert_eq!(
        pinned.namespace_uid,
        row.get::<Option<String>, _>("namespace_uid")
    );
    assert_eq!(row.get::<String, _>("state"), "Ready");
    assert_eq!(row.get::<i64, _>("version"), 1);
    assert_eq!(row.get::<i64, _>("desired_generation"), 1);
    assert_eq!(row.get::<i64, _>("observed_generation"), 1);
    assert_eq!(history(&pool, owner.operation_id).await, command_before);
    let audit_after: Value =
        sqlx::query_scalar("SELECT jsonb_agg(to_jsonb(a) ORDER BY id) FROM manager.audit_events a")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(audit_before, audit_after);
    let request_after: Value = sqlx::query_scalar(
        "SELECT jsonb_agg(to_jsonb(r) ORDER BY idempotency_key) FROM manager.mutation_requests r",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(request_before, request_after);
    let routes: i64 = sqlx::query_scalar("SELECT count(*) FROM manager.instance_routes")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(routes, 0);
    // Completed periodic observations remain immutable history. A new due tick
    // gets a NEW operation, at the SAME desired generation.
    sqlx::query("UPDATE manager.operations SET state='Succeeded' WHERE id=$1")
        .bind(first)
        .execute(&pool)
        .await
        .unwrap();
    let first_before = history(&pool, first).await;
    assert_eq!(f.store.enqueue_due(&f.scope, 64).await.unwrap(), 0);
    due(&pool).await;
    let reopened = PgStore::connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    reopened.check_existing(&f.scope).await.unwrap();
    assert_eq!(reopened.enqueue_due(&f.scope, 64).await.unwrap(), 1);
    assert_eq!(history(&pool, first).await, first_before);
    let count:i64=sqlx::query_scalar("SELECT count(*) FROM manager.operations WHERE instance_id=$1 AND generation=1 AND kind='resync'").bind(owner.instance_id).fetch_one(&pool).await.unwrap();
    assert_eq!(count, 2);
    for statement in [
        "UPDATE manager.operations SET input='{}' WHERE kind='resync'",
        "UPDATE manager.operations SET created_at=created_at+interval '1 second' WHERE kind='resync'",
        "UPDATE manager.operations SET epoch=epoch+1 WHERE kind='resync'",
    ] {
        assert!(sqlx::query(statement).execute(&pool).await.is_err());
    }
    assert!(sqlx::query("INSERT INTO manager.operations(id,installation_id,instance_id,kind,generation,epoch,input) SELECT gen_random_uuid(),installation_id,instance_id,kind,generation,epoch,input FROM manager.operations WHERE id=$1").bind(owner.operation_id).execute(&pool).await.is_err());
    assert!(sqlx::query("INSERT INTO manager.operations(id,installation_id,instance_id,kind,generation,epoch,input) SELECT gen_random_uuid(),installation_id,instance_id,kind,generation,epoch,input FROM manager.operations WHERE kind='resync' AND state='Pending'").execute(&pool).await.is_err());
    // The database also rejects active work from a DIFFERENT generation/kind,
    // even when a future adapter forgets the owner lock protocol.
    assert!(sqlx::query("INSERT INTO manager.operations(id,installation_id,instance_id,kind,generation,epoch,input) SELECT gen_random_uuid(),installation_id,instance_id,'commission',generation+1,epoch,input FROM manager.operations WHERE kind='resync' AND state='Pending'").execute(&pool).await.is_err());
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn concurrent_enqueue_skips_locked_owners_and_preserves_active_expired_work(pool: PgPool) {
    let f = fixture(&pool).await;
    let locked = completed(&pool, &f, "locked-owner", Access::Enabled).await;
    completed(&pool, &f, "suspended-owner", Access::Suspended).await;
    completed(&pool, &f, "retained-owner", Access::Revoked).await;
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM manager.assignments WHERE id=$1 FOR UPDATE")
        .bind(locked.assignment_id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    let (a, b, c) = tokio::join!(
        f.store.enqueue_due(&f.scope, 1),
        f.store.enqueue_due(&f.scope, 1),
        f.store.enqueue_due(&f.scope, 1)
    );
    assert_eq!(a.unwrap() + b.unwrap() + c.unwrap(), 2);
    tx.commit().await.unwrap();
    assert_eq!(f.store.enqueue_due(&f.scope, 1).await.unwrap(), 1);
    due(&pool).await;
    assert_eq!(f.store.enqueue_due(&f.scope, 64).await.unwrap(), 0);
    let claim = f
        .store
        .claim(&f.scope, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claim.kind, "resync");
    let reopened = PgStore::connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    assert_eq!(reopened.enqueue_due(&f.scope, 64).await.unwrap(), 0);
    sqlx::query("UPDATE manager.operations SET lease_until=clock_timestamp()-interval '1 second' WHERE id=$1").bind(claim.id).execute(&pool).await.unwrap();
    assert_eq!(reopened.enqueue_due(&f.scope, 64).await.unwrap(), 0);
    // Existing expired work is reclaimed by its own queue, never duplicated by
    // the periodic scheduler. Restrict the other due operations for this proof.
    sqlx::query("UPDATE manager.operations SET next_attempt_at=clock_timestamp()+interval '1 day' WHERE kind='resync' AND id<>$1").bind(claim.id).execute(&pool).await.unwrap();
    let reclaimed = reopened
        .claim(&f.scope, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reclaimed.id, claim.id);
    assert!(reclaimed.fence > claim.fence);
    assert_eq!(
        f.store.renew(&f.scope, &claim, 30).await,
        Err(Error::StaleFence)
    );
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM manager.operations WHERE kind='resync'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 3);
    let assignment_id: Uuid =
        sqlx::query_scalar("SELECT assignment_id FROM manager.instances WHERE id=$1")
            .bind(reclaimed.instance_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    let command = f
        .commands
        .execute(
            &f.actor,
            Uuid::new_v4(),
            "suspend-during-resync",
            AssignmentCommand::SetAccess {
                assignment_id,
                expected_version: 1,
                access: Access::Suspended,
            },
        )
        .await
        .unwrap();
    assert_eq!(command.instance_id, reclaimed.instance_id);
    assert_eq!(
        f.store.finish(&f.scope, &reclaimed).await,
        Err(Error::StaleFence)
    );
    assert_eq!(history(&pool, reclaimed.id).await["state"], "Superseded");
    assert_eq!(
        history(&pool, command.operation_id).await["kind"],
        "suspend"
    );
    assert_eq!(history(&pool, command.operation_id).await["generation"], 2);
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn blocked_failed_partial_and_unregistered_instances_require_their_own_recovery(
    pool: PgPool,
) {
    let f = fixture(&pool).await;
    for (index, state) in [
        "Pending",
        "Provisioning",
        "Blocked",
        "Suspending",
        "Deleting",
        "Deleted",
        "Error",
    ]
    .into_iter()
    .enumerate()
    {
        let owner = completed(&pool, &f, &format!("state-{index}"), Access::Enabled).await;
        sqlx::query("UPDATE manager.instances SET state=$2 WHERE id=$1")
            .bind(owner.instance_id)
            .bind(state)
            .execute(&pool)
            .await
            .unwrap();
    }
    for state in ["Blocked", "Failed", "Pending", "Waiting", "RetryAt"] {
        let owner = completed(&pool, &f, &format!("work-{state}"), Access::Enabled).await;
        sqlx::query("UPDATE manager.operations SET state=$2 WHERE id=$1")
            .bind(owner.operation_id)
            .bind(state)
            .execute(&pool)
            .await
            .unwrap();
    }
    let partial = completed(&pool, &f, "unobserved", Access::Enabled).await;
    sqlx::query("UPDATE manager.instances SET observed_generation=0 WHERE id=$1")
        .bind(partial.instance_id)
        .execute(&pool)
        .await
        .unwrap();
    completed_registration(&pool, &f, "unregistered", Access::Enabled, false).await;
    let inconsistent = completed(&pool, &f, "inconsistent-access", Access::Enabled).await;
    sqlx::query("UPDATE manager.assignments SET access='revoked' WHERE id=$1")
        .bind(inconsistent.assignment_id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(f.store.enqueue_due(&f.scope, 64).await.unwrap(), 0);
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM manager.operations WHERE kind='resync'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 0);
    // A stable disabled instance stays registered even if namespace CREATE
    // never returned a UID. Its periodic work must observe a possible late
    // completion through the original authenticated ownership intent.
    completed_registration(&pool, &f, "unbound-retained", Access::Revoked, false).await;
    completed_registration(&pool, &f, "unbound-suspended", Access::Suspended, false).await;
    assert_eq!(f.store.enqueue_due(&f.scope, 64).await.unwrap(), 2);
    let rows = sqlx::query("SELECT input FROM manager.operations WHERE kind='resync'")
        .fetch_all(&pool)
        .await
        .unwrap();
    for row in rows {
        let input: ResyncInput = serde_json::from_value(row.get("input")).unwrap();
        assert!(input.namespace_uid.is_none());
        assert!([Access::Revoked, Access::Suspended].contains(&input.access));
    }
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn installation_epoch_and_maintenance_gate_every_periodic_commit(pool: PgPool) {
    let f = fixture(&pool).await;
    let owner = completed(&pool, &f, "owner", Access::Enabled).await;
    for limit in [0, 65, u16::MAX] {
        assert_eq!(
            f.store.enqueue_due(&f.scope, limit).await,
            Err(Error::Invalid)
        );
    }
    let mut wrong = f.scope.clone();
    wrong.id = Uuid::new_v4();
    assert_eq!(
        f.store.enqueue_due(&wrong, 1).await,
        Err(Error::RecoveryRequired)
    );
    let mut wrong = f.scope.clone();
    wrong.epoch = 2;
    assert_eq!(
        f.store.enqueue_due(&wrong, 1).await,
        Err(Error::RecoveryRequired)
    );
    let mut wrong = f.scope.clone();
    wrong.issuer = "https://foreign.example.org".into();
    assert_eq!(
        f.store.enqueue_due(&wrong, 1).await,
        Err(Error::RecoveryRequired)
    );
    sqlx::query("UPDATE manager.installations SET mode='maintenance'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        f.store.enqueue_due(&f.scope, 1).await,
        Err(Error::Maintenance)
    );
    sqlx::query("UPDATE manager.installations SET mode='restore'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        f.store.enqueue_due(&f.scope, 1).await,
        Err(Error::RecoveryRequired)
    );
    sqlx::query("UPDATE manager.installations SET mode='active',epoch=2")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        f.store.enqueue_due(&f.scope, 1).await,
        Err(Error::RecoveryRequired)
    );
    let mut current = f.scope.clone();
    current.epoch = 2;
    assert_eq!(f.store.enqueue_due(&current, 1).await.unwrap(), 1);
    let row = sqlx::query("SELECT epoch,input FROM manager.operations WHERE kind='resync'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(row.get::<i64, _>("epoch"), 2);
    assert_eq!(row.get::<Value, _>("input")["epoch"], 2);
    assert_eq!(history(&pool, owner.operation_id).await["epoch"], 1);
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn enabled_resync_waits_as_provisioning_while_disabled_work_remains_suspending(pool: PgPool) {
    let f = fixture(&pool).await;
    completed(&pool, &f, "enabled", Access::Enabled).await;
    completed(&pool, &f, "suspended", Access::Suspended).await;
    completed(&pool, &f, "revoked", Access::Revoked).await;
    assert_eq!(f.store.enqueue_due(&f.scope, 64).await.unwrap(), 3);
    for _ in 0..3 {
        let claim = f
            .store
            .claim(&f.scope, Uuid::new_v4(), 30)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(claim.kind, "resync");
        f.store
            .schedule(
                &f.scope,
                &claim,
                Schedule::Waiting {
                    reason: ConditionReason::Draining,
                    seconds: 5,
                },
            )
            .await
            .unwrap();
        let row=sqlx::query("SELECT a.access,i.state,i.desired_generation,o.state AS operation_state FROM manager.instances i JOIN manager.assignments a ON a.id=i.assignment_id JOIN manager.operations o ON o.id=$2 WHERE i.id=$1")
            .bind(claim.instance_id).bind(claim.id).fetch_one(&pool).await.unwrap();
        assert_eq!(
            row.get::<String, _>("state"),
            if row.get::<String, _>("access") == "enabled" {
                "Provisioning"
            } else {
                "Suspending"
            }
        );
        assert_eq!(row.get::<String, _>("operation_state"), "Waiting");
        assert_eq!(row.get::<i64, _>("desired_generation"), 1);
    }
}
