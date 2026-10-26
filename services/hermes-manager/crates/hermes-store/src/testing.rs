//! Runs the shared contract cases against PostgreSQL. Each case gets a fresh,
//! migrated database from `sqlx::test`, which needs `DATABASE_URL`.

use async_trait::async_trait;
use hermes_core::app::testing::{Inspect, installation};
use hermes_core::domain::access::Grant;
use hermes_core::domain::identity::Identity;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::PgStore;

pub(crate) async fn fixture(pool: PgPool) -> PgStore {
    let store = PgStore::from_pool(pool);
    store
        .initialize(&installation(), "fixture-cluster")
        .await
        .expect("initialize fixture registry");
    store
}

#[async_trait]
impl Inspect for PgStore {
    async fn audit_actions(&self) -> Vec<String> {
        sqlx::query_scalar("SELECT action FROM manager.audit_events ORDER BY occurred_at, id")
            .fetch_all(&self.pool)
            .await
            .expect("audit")
    }

    async fn receipt_count(&self) -> usize {
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM manager.command_receipts")
            .fetch_one(&self.pool)
            .await
            .expect("receipts");
        count as usize
    }

    async fn grant_of(&self, identity: &Identity) -> Option<Grant> {
        sqlx::query(
            "SELECT g.active, g.can_manage_administrators, g.version \
             FROM manager.administrator_grants g JOIN manager.identities i ON i.id = g.identity_id \
             WHERE i.issuer = $1 AND i.subject = $2",
        )
        .bind(&identity.issuer)
        .bind(&identity.subject)
        .fetch_optional(&self.pool)
        .await
        .expect("grant")
        .map(|row| Grant {
            active: row.get("active"),
            can_manage_administrators: row.get("can_manage_administrators"),
            version: row.get("version"),
        })
    }

    async fn active_managers(&self) -> usize {
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM manager.administrator_grants WHERE active AND can_manage_administrators",
        )
        .fetch_one(&self.pool)
        .await
        .expect("managers");
        count as usize
    }

    async fn instance_generations(&self, instance: Uuid) -> (i64, i64) {
        let row = sqlx::query(
            "SELECT desired_generation, execution_generation FROM manager.instances WHERE id = $1",
        )
        .bind(instance)
        .fetch_one(&self.pool)
        .await
        .expect("instance");
        (row.get(0), row.get(1))
    }

    async fn instance_namespace(&self, instance: Uuid) -> String {
        sqlx::query_scalar("SELECT namespace FROM manager.instances WHERE id = $1")
            .bind(instance)
            .fetch_one(&self.pool)
            .await
            .expect("instance")
    }

    async fn secret_versions(&self, name: &str) -> Vec<(i64, Vec<u8>)> {
        sqlx::query(
            "SELECT version, sealed FROM manager.admin_secrets WHERE name = $1 ORDER BY version",
        )
        .bind(name)
        .fetch_all(&self.pool)
        .await
        .expect("secrets")
        .into_iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect()
    }

    async fn instance_status(&self, instance: Uuid) -> (String, Option<String>, i64) {
        let row = sqlx::query("SELECT phase, blocked_reason, observed_generation FROM manager.instances WHERE id = $1")
            .bind(instance)
            .fetch_one(&self.pool)
            .await
            .expect("instance");
        (row.get(0), row.get(1), row.get(2))
    }

    async fn advance_time(&self, seconds: i64) {
        // Moving the clock forward is moving every timestamp back. Ended
        // leases and retired environments are final and stay untouched.
        let shift = |sql: &'static str| sqlx::query(sql).bind(seconds as f64).execute(&self.pool);
        shift(
            "UPDATE manager.leases SET created_at = created_at - make_interval(secs => $1), \
             expires_at = expires_at - make_interval(secs => $1) WHERE state = 'active'",
        )
        .await
        .expect("shift leases");
        shift(
            "UPDATE manager.environments SET created_at = created_at - make_interval(secs => $1), \
             last_used_at = last_used_at - make_interval(secs => $1), \
             drain_deadline = drain_deadline - make_interval(secs => $1) WHERE state <> 'retired'",
        )
        .await
        .expect("shift environments");
    }

    async fn orphans(&self) -> Vec<hermes_core::domain::orphans::Orphan> {
        use hermes_core::domain::orphans::{Orphan, OrphanKind};
        let rows = sqlx::query("SELECT kind, name, uid FROM manager.orphans")
            .fetch_all(&self.pool)
            .await
            .unwrap();
        let mut orphans: Vec<Orphan> = rows
            .iter()
            .map(|row| Orphan {
                kind: OrphanKind::parse(row.get("kind")).unwrap(),
                name: row.get("name"),
                uid: row.get("uid"),
            })
            .collect();
        orphans.sort();
        orphans
    }

    async fn mark_verified(&self, identity: &Identity) {
        sqlx::query(
            "INSERT INTO manager.identities (id, issuer, subject, verified_at) \
             VALUES ($1, $2, $3, clock_timestamp()) \
             ON CONFLICT (issuer, subject) DO UPDATE SET verified_at = clock_timestamp()",
        )
        .bind(Uuid::new_v4())
        .bind(&identity.issuer)
        .bind(&identity.subject)
        .execute(&self.pool)
        .await
        .expect("verify identity");
    }
}

mod contracts {
    use sqlx::PgPool;

    hermes_core::contract_tests!(#[sqlx::test(migrator = "crate::MIGRATOR")] (pool: PgPool) => super::fixture(pool).await);
}

mod registry {
    use hermes_core::app::error::AppError;
    use hermes_core::app::testing::installation;
    use hermes_core::domain::installation::Installation;
    use sqlx::PgPool;
    use uuid::Uuid;

    use super::fixture;
    use crate::PgStore;

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn startup_check_accepts_only_the_registered_installation(pool: PgPool) {
        let store = fixture(pool).await;
        assert_eq!(store.check(&installation()).await, Ok(()));
        let foreign = Installation {
            id: Uuid::new_v4(),
            ..installation()
        };
        assert_eq!(
            store.check(&foreign).await,
            Err(AppError::registry_mismatch())
        );
        let other_issuer = Installation {
            issuer: "https://other.example.org/realms/agents".into(),
            ..installation()
        };
        assert_eq!(
            store.check(&other_issuer).await,
            Err(AppError::registry_mismatch())
        );
        assert_eq!(store.cluster_uid().await.as_deref(), Ok("fixture-cluster"));
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn startup_check_refuses_an_uninitialized_or_altered_schema(pool: PgPool) {
        let store = PgStore::from_pool(pool.clone());
        assert_eq!(
            store.check(&installation()).await,
            Err(AppError::registry_mismatch())
        );
        store.initialize(&installation(), "c").await.unwrap();

        sqlx::query("CREATE UNLOGGED TABLE manager.scratch (x int)")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            store.check(&installation()).await,
            Err(AppError::registry_mismatch())
        );
        sqlx::query("DROP TABLE manager.scratch")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(store.check(&installation()).await, Ok(()));

        sqlx::query("UPDATE _sqlx_migrations SET checksum = '\\x00' WHERE version = 1")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            store.check(&installation()).await,
            Err(AppError::registry_mismatch())
        );
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn initialize_refuses_a_populated_registry(pool: PgPool) {
        let store = fixture(pool).await;
        assert_eq!(
            store.initialize(&installation(), "fixture-cluster").await,
            Err(AppError::registry_mismatch())
        );
        let other = Installation {
            id: Uuid::new_v4(),
            ..installation()
        };
        assert_eq!(
            store.initialize(&other, "c").await,
            Err(AppError::registry_mismatch())
        );
    }
}

mod guards {
    use hermes_core::app::access::BootstrapAdministrator;
    use hermes_core::app::actor::Actor;
    use hermes_core::app::envelope::{CommandEnvelope, IdempotencyKey};
    use hermes_core::app::faults::NoFaults;
    use hermes_core::app::testing::{identity, settings};
    use sqlx::PgPool;
    use std::sync::Arc;
    use uuid::Uuid;

    use super::fixture;

    async fn bootstrapped(pool: PgPool) {
        let store = fixture(pool).await;
        CommandEnvelope::new(store, settings(), Arc::new(NoFaults))
            .execute(
                &Actor::Operator,
                Uuid::new_v4(),
                &IdempotencyKey::parse("k").unwrap(),
                &BootstrapAdministrator {
                    identity: identity("alice"),
                    recover: false,
                },
            )
            .await
            .unwrap();
    }

    async fn refused(pool: &PgPool, statement: &'static str) {
        assert!(
            sqlx::query(statement).execute(pool).await.is_err(),
            "guard allowed: {statement}"
        );
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn database_guards_refuse_rewriting_history_and_identity(pool: PgPool) {
        bootstrapped(pool.clone()).await;
        refused(&pool, "UPDATE manager.audit_events SET action = 'x'").await;
        refused(&pool, "DELETE FROM manager.audit_events").await;
        refused(&pool, "UPDATE manager.command_receipts SET receipt = '{}'").await;
        refused(&pool, "DELETE FROM manager.command_receipts").await;
        refused(&pool, "UPDATE manager.identities SET subject = 'mallory'").await;
        refused(&pool, "DELETE FROM manager.identities").await;
        refused(
            &pool,
            "UPDATE manager.administrator_grants SET version = version + 2",
        )
        .await;
        refused(
            &pool,
            "UPDATE manager.administrator_grants SET version = version - 1",
        )
        .await;
        refused(&pool, "DELETE FROM manager.administrator_grants").await;
        refused(
            &pool,
            "UPDATE manager.installation SET id = gen_random_uuid()",
        )
        .await;
        refused(
            &pool,
            "UPDATE manager.installation SET issuer = 'https://other.example.org'",
        )
        .await;
        refused(&pool, "DELETE FROM manager.installation").await;
        refused(&pool, "UPDATE manager.controller_epoch SET epoch = epoch").await;
        sqlx::query("UPDATE manager.controller_epoch SET epoch = epoch + 1")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE manager.installation SET cluster_uid = 'rebound'")
            .execute(&pool)
            .await
            .unwrap();
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn instance_guards_keep_material_bindings_and_generations(pool: PgPool) {
        let store = fixture(pool.clone()).await;
        hermes_core::app::testing::contracts::a_new_instance_converges_to_ready_with_material_created_once(store)
            .await;
        refused(&pool, "UPDATE manager.materials SET sealed = 'x'").await;
        refused(&pool, "DELETE FROM manager.materials").await;
        refused(
            &pool,
            "INSERT INTO manager.materials (instance_id, purpose, id, sealed) \
             SELECT id, 'initialization', gen_random_uuid(), 'x' FROM manager.instances",
        )
        .await;
        refused(
            &pool,
            "UPDATE manager.instances SET namespace_uid = 'another'",
        )
        .await;
        refused(
            &pool,
            "UPDATE manager.instances SET desired_generation = desired_generation - 1",
        )
        .await;
        refused(
            &pool,
            "UPDATE manager.instances SET observed_generation = desired_generation + 1",
        )
        .await;
        refused(&pool, "DELETE FROM manager.instances").await;
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn execution_guards_keep_states_forward_and_ended_leases_final(pool: PgPool) {
        let store = fixture(pool.clone()).await;
        hermes_core::app::testing::contracts::leases_renew_release_and_expire_by_database_time_and_never_revive(store)
            .await;
        refused(
            &pool,
            "UPDATE manager.leases SET state = 'active' WHERE state <> 'active'",
        )
        .await;
        refused(&pool, "UPDATE manager.leases SET expires_at = expires_at + interval '1 hour' WHERE state <> 'active'").await;
        refused(&pool, "DELETE FROM manager.leases").await;
        refused(
            &pool,
            "UPDATE manager.environments SET state = 'pending' WHERE state = 'ready'",
        )
        .await;
        refused(
            &pool,
            "UPDATE manager.environments SET pod_uid = 'another' WHERE pod_uid IS NOT NULL",
        )
        .await;
        refused(
            &pool,
            "UPDATE manager.environments SET backend_version = backend_version + 1",
        )
        .await;
        refused(&pool, "DELETE FROM manager.environments").await;
        refused(
            &pool,
            "UPDATE manager.acquire_requests SET body_digest = decode(repeat('00', 32), 'hex')",
        )
        .await;
        refused(
            &pool,
            "UPDATE manager.acquire_requests SET lease_id = NULL WHERE lease_id IS NOT NULL",
        )
        .await;
        refused(&pool, "DELETE FROM manager.acquire_requests").await;
        // A failure is recorded only as a starting environment drains, once.
        refused(
            &pool,
            "UPDATE manager.environments SET failure = 'image_unavailable' WHERE state <> 'pending'",
        )
        .await;
        refused(
            &pool,
            "UPDATE manager.environments SET failure = 'no_such_reason'",
        )
        .await;
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn rebinding_clears_the_old_clusters_bindings_workers_and_orphans(pool: PgPool) {
        use hermes_core::app::controller::InstanceStore;
        use hermes_core::domain::orphans::{Orphan, OrphanKind};

        let store = fixture(pool.clone()).await;
        hermes_core::app::testing::contracts::acquisition_waits_for_a_ready_environment_then_grants_and_replays(store.clone())
            .await;
        let epoch = store.advance_epoch().await.unwrap();
        let stray = Orphan {
            kind: OrphanKind::Namespace,
            name: "hermes-stray".into(),
            uid: "stray".into(),
        };
        assert!(store.record_orphans(epoch, &[stray]).await.unwrap());
        let count = |sql: &'static str| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, i64>(sql)
                    .fetch_one(&pool)
                    .await
                    .unwrap()
            }
        };
        let live = "SELECT count(*) FROM manager.environments WHERE state <> 'retired'";
        let active = "SELECT count(*) FROM manager.leases WHERE state = 'active'";
        let bound = "SELECT count(*) FROM manager.instances WHERE namespace_uid IS NOT NULL";
        assert!(count(live).await > 0 && count(active).await > 0);
        // As the old cluster's controller had bound it.
        let instance: Uuid = sqlx::query_scalar("SELECT id FROM manager.instances")
            .fetch_one(&pool)
            .await
            .unwrap();
        store
            .bind_namespace(instance, "old-namespace")
            .await
            .unwrap();

        let cleared = store.rebind_cluster("new-cluster").await.unwrap();
        assert_eq!(i64::try_from(cleared).unwrap(), 1);
        assert_eq!(store.cluster_uid().await.unwrap(), "new-cluster");
        for sql in [live, active, bound, "SELECT count(*) FROM manager.orphans"] {
            assert_eq!(count(sql).await, 0, "{sql}");
        }
    }
}
