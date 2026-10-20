//! Acknowledgement-time drain provenance and read-only retained-state planning.
use hermes_adapters::{
    materials::MaterialVault,
    postgres::{FreshInventory, PgStore},
    session_crypto::Sealer,
    workloads,
};
use hermes_application::{
    Accepted, AdminPolicy, AssignmentCommand, AssignmentReader, Assignments, Error,
    InstallationScope, OperationClaim, OperationQueue, VerifiedPrincipal,
    lifecycle::LifecycleRepository, resources::ResourceRepository,
};
use hermes_domain::{Access, Identity, TemplateVersion};
use sqlx::PgPool;
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;
use zeroize::Zeroizing;

struct Fixture {
    pool: PgPool,
    store: PgStore,
    scope: InstallationScope,
    admin: Assignments<PgStore>,
    actor: VerifiedPrincipal,
    accepted: Accepted,
    claim: OperationClaim,
}
impl Fixture {
    async fn new(pool: &PgPool) -> Self {
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
                    cluster_identity: "lifecycle-plan-fixture".into(),
                    digest: "a".repeat(64),
                    registered_instances: 0,
                    unexplained_resources: 0,
                },
            )
            .await
            .unwrap();
        sqlx::query("INSERT INTO manager.instance_templates(id,version,schema_version,image,content_digest,approved) VALUES('personal-default',1,1,$1,$2,true)")
            .bind(format!("ghcr.io/example/hermes@sha256:{}", "a".repeat(64))).bind("b".repeat(64)).execute(pool).await.unwrap();
        let actor = VerifiedPrincipal {
            identity: Identity {
                issuer: scope.issuer.clone(),
                subject: "fixture-admin".into(),
            },
            audiences: BTreeSet::from(["manager-api".into()]),
            client_roles: BTreeMap::from([(
                "manager-admin".into(),
                BTreeSet::from(["administrator".into()]),
            )]),
        };
        let admin = Assignments::new(
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
        let accepted = admin
            .execute(
                &actor,
                Uuid::new_v4(),
                "create",
                AssignmentCommand::Create {
                    identity: Identity {
                        issuer: scope.issuer.clone(),
                        subject: "fixture-owner".into(),
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
        Self {
            pool: pool.clone(),
            store,
            scope,
            admin,
            actor,
            accepted,
            claim,
        }
    }
    async fn access(&mut self, access: Access, key: &str) {
        self.accepted = self
            .admin
            .execute(
                &self.actor,
                Uuid::new_v4(),
                key,
                AssignmentCommand::SetAccess {
                    assignment_id: self.accepted.assignment_id,
                    expected_version: self.accepted.version,
                    access,
                },
            )
            .await
            .unwrap();
        if access != Access::Enabled {
            let acknowledged: bool = sqlx::query_scalar("SELECT drain_started_at IS NOT NULL AND drain_deadline_at IS NOT NULL FROM manager.instances WHERE id=$1")
                .bind(self.accepted.instance_id).fetch_one(&self.pool).await.unwrap();
            assert!(
                acknowledged,
                "drain deadline must commit before a worker claim"
            );
        }
        self.claim = self
            .store
            .claim(&self.scope, Uuid::new_v4(), 30)
            .await
            .unwrap()
            .unwrap();
    }
    fn vault(&self) -> MaterialVault<PgStore> {
        MaterialVault::new(
            self.store.clone(),
            Sealer::new(
                "fixture".into(),
                BTreeMap::from([("fixture".into(), vec![43; 32])]),
            )
            .unwrap(),
        )
    }
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn disable_acknowledges_original_deadline_and_retry_or_rapid_enable_cannot_reset_it(
    pool: PgPool,
) {
    let mut f = Fixture::new(&pool).await;
    assert!(
        f.store
            .lifecycle_plan(&f.scope, &f.claim)
            .await
            .unwrap()
            .drain
            .is_none()
    );
    let old = f.claim.clone();
    f.access(Access::Suspended, "disable").await;
    let replay = f
        .admin
        .execute(
            &f.actor,
            Uuid::new_v4(),
            "disable",
            AssignmentCommand::SetAccess {
                assignment_id: f.accepted.assignment_id,
                expected_version: 1,
                access: Access::Suspended,
            },
        )
        .await
        .unwrap();
    assert_eq!(replay.operation_id, f.accepted.operation_id);
    assert!(matches!(
        f.store.lifecycle_plan(&f.scope, &old).await,
        Err(Error::StaleFence)
    ));
    let plan = f.store.lifecycle_plan(&f.scope, &f.claim).await.unwrap();
    assert!(plan.namespace.is_none() && plan.deployment.is_none());
    let original = plan.drain.unwrap();
    assert_eq!(original.generation, 2);
    assert_eq!(
        original.deadline_at_millis - original.started_at_millis,
        300_000
    );
    assert!(!original.deadline_expired && original.completed_at_millis.is_none());
    let accepted = f.accepted.clone();
    f.accepted = f
        .admin
        .execute(
            &f.actor,
            Uuid::new_v4(),
            "retry",
            AssignmentCommand::Retry {
                instance_id: accepted.instance_id,
                expected_version: accepted.version,
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
    f.access(Access::Enabled, "rapid-enable").await;
    f.access(Access::Revoked, "rapid-revoke").await;
    let reopened = PgStore::connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    let plan = reopened.lifecycle_plan(&f.scope, &f.claim).await.unwrap();
    let current = plan.drain.unwrap();
    assert_eq!(current.generation, original.generation);
    assert_eq!(current.started_at_millis, original.started_at_millis);
    assert_eq!(current.deadline_at_millis, original.deadline_at_millis);
    assert_eq!(plan.assignment.access, Access::Revoked);
    assert_eq!(plan.assignment.observed_generation, 0);
    let counts: (i64,i64,i64) = sqlx::query_as("SELECT (SELECT count(*) FROM manager.owned_resources),(SELECT count(*) FROM manager.bootstrap_materials),(SELECT count(*) FROM manager.instance_routes)").fetch_one(&pool).await.unwrap();
    assert_eq!(counts, (0, 0, 0));
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn retained_plan_preserves_original_unbound_creation_and_encrypted_snapshot_reference(
    pool: PgPool,
) {
    let mut f = Fixture::new(&pool).await;
    let assignment = f
        .store
        .get_assignment(&f.scope, f.accepted.assignment_id)
        .await
        .unwrap();
    let snapshot = f
        .vault()
        .get_or_create(&f.scope, &f.claim, "render-namespace-g1", || {
            Ok(Zeroizing::new(b"fixture snapshot".to_vec()))
        })
        .await
        .unwrap();
    let namespace = workloads::namespace(&f.scope, &assignment).unwrap();
    let prepared = f
        .store
        .prepare_resource(&f.scope, &f.claim, &namespace.key, namespace.spec)
        .await
        .unwrap();
    f.access(Access::Suspended, "disable").await;
    let plan = f.store.lifecycle_plan(&f.scope, &f.claim).await.unwrap();
    let retained = plan.namespace.unwrap();
    assert!(retained.owned.uid.is_none());
    assert_eq!(retained.owned.creation, prepared.resource.creation);
    let reference = retained.original_rendering.unwrap();
    assert_eq!(reference.id, snapshot.context.id);
    assert_eq!(reference.purpose, snapshot.context.purpose);
    assert_eq!(reference.generation, 1);
    assert!(plan.namespace_uid.is_none());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM manager.bootstrap_materials")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    let mut forged = f.claim.clone();
    forged.kind = "commission".into();
    assert!(matches!(
        f.store.lifecycle_plan(&f.scope, &forged).await,
        Err(Error::OwnershipConflict)
    ));
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn missing_original_snapshot_and_direct_deadline_rewrites_fail_closed(pool: PgPool) {
    let mut f = Fixture::new(&pool).await;
    let assignment = f
        .store
        .get_assignment(&f.scope, f.accepted.assignment_id)
        .await
        .unwrap();
    let namespace = workloads::namespace(&f.scope, &assignment).unwrap();
    f.store
        .prepare_resource(&f.scope, &f.claim, &namespace.key, namespace.spec)
        .await
        .unwrap();
    f.access(Access::Revoked, "revoke").await;
    assert!(matches!(
        f.store.lifecycle_plan(&f.scope, &f.claim).await,
        Err(Error::RecoveryRequired)
    ));
    for statement in [
        "UPDATE manager.instances SET drain_started_at=drain_started_at+interval '1 minute',drain_deadline_at=drain_deadline_at+interval '1 minute'",
        "UPDATE manager.instances SET drain_deadline_at=drain_deadline_at+interval '1 minute'",
        "UPDATE manager.instances SET drain_generation=NULL,drain_epoch=NULL,drain_started_at=NULL,drain_deadline_at=NULL,drain_completed_at=NULL",
        "UPDATE manager.instances SET drain_generation=NULL",
    ] {
        assert!(sqlx::query(statement).execute(&pool).await.is_err());
    }
    // Fixture evidence of an earlier completed drain. Production exposes no
    // completion method until native-child and execution-drain proofs exist.
    sqlx::query("UPDATE manager.instances SET drain_completed_at=clock_timestamp()")
        .execute(&pool)
        .await
        .unwrap();
    f.access(Access::Enabled, "enable-after-drain").await;
    f.access(Access::Suspended, "new-drain-cycle").await;
    let generation: i64 = sqlx::query_scalar("SELECT drain_generation FROM manager.instances")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(generation, f.claim.generation);
}
