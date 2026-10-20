//! Real PostgreSQL authorization for read-only disabled-operation material use.
use hermes_adapters::{
    materials::{MaterialVault, PlainMaterial},
    postgres::{FreshInventory, PgStore},
    session_crypto::Sealer,
};
use hermes_application::{
    AdminPolicy, AssignmentCommand, Assignments, Error, InstallationScope, OperationClaim,
    OperationQueue, VerifiedPrincipal,
    materials::{MaterialContext, MaterialRecord, MaterialRepository},
    resync::{PeriodicResync, ResyncInput},
};
use hermes_domain::{Access, Identity, TemplateVersion};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;
use zeroize::Zeroizing;

fn cipher() -> Sealer {
    Sealer::new(
        "fixture".into(),
        BTreeMap::from([("fixture".into(), vec![42; 32])]),
    )
    .unwrap()
}

struct Fixture {
    store: PgStore,
    scope: InstallationScope,
    assignments: Assignments<PgStore>,
    actor: VerifiedPrincipal,
    assignment: Uuid,
    version: i64,
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
                    cluster_identity: "retained-material-fixture".into(),
                    digest: "a".repeat(64),
                    registered_instances: 0,
                    unexplained_resources: 0,
                },
            )
            .await
            .unwrap();
        sqlx::query("INSERT INTO manager.instance_templates(id,version,schema_version,image,content_digest,approved) VALUES('personal-default',1,1,$1,$2,true)")
            .bind(format!("ghcr.io/example/hermes@sha256:{}", "a".repeat(64)))
            .bind("a".repeat(64)).execute(pool).await.unwrap();
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
        let accepted = assignments
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
            store,
            scope,
            assignments,
            actor,
            assignment: accepted.assignment_id,
            version: 1,
            claim,
        }
    }
    fn vault(&self) -> MaterialVault<PgStore> {
        MaterialVault::new(self.store.clone(), cipher())
    }
    async fn material(&self) -> PlainMaterial {
        self.vault()
            .get_or_create(&self.scope, &self.claim, "native-bootstrap-v1", || {
                Ok(Zeroizing::new(b"fixture-original-material".to_vec()))
            })
            .await
            .unwrap()
    }
    async fn access(&mut self, access: Access) -> OperationClaim {
        self.assignments
            .execute(
                &self.actor,
                Uuid::new_v4(),
                &format!("change-access-{}", self.version),
                AssignmentCommand::SetAccess {
                    assignment_id: self.assignment,
                    expected_version: self.version,
                    access,
                },
            )
            .await
            .unwrap();
        self.version += 1;
        self.claim = self
            .store
            .claim(&self.scope, Uuid::new_v4(), 30)
            .await
            .unwrap()
            .unwrap();
        self.claim.clone()
    }
    async fn resync(&mut self, pool: &PgPool, completed_state: &str) -> OperationClaim {
        // Completed lifecycle fixtures exercise the SQL authorization port;
        // these state writes are not production readiness/namespace proofs.
        sqlx::query("UPDATE manager.operations SET state='Succeeded',lease_owner=NULL,lease_until=NULL WHERE id=$1")
            .bind(self.claim.id).execute(pool).await.unwrap();
        sqlx::query("UPDATE manager.instances SET state=$2,observed_generation=desired_generation,reconcile_after=clock_timestamp()-interval '1 second' WHERE id=$1")
            .bind(self.claim.instance_id).bind(completed_state).execute(pool).await.unwrap();
        assert_eq!(self.store.enqueue_due(&self.scope, 1).await.unwrap(), 1);
        self.claim = self
            .store
            .claim(&self.scope, Uuid::new_v4(), 30)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(self.claim.kind, "resync");
        self.claim.clone()
    }
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn suspend_and_revoke_can_read_original_material_but_cannot_create_or_replace(pool: PgPool) {
    let mut fixture = Fixture::new(&pool).await;
    let original = fixture.material().await;
    let original_ciphertext: Vec<u8> =
        sqlx::query_scalar("SELECT ciphertext FROM manager.bootstrap_materials")
            .fetch_one(&pool)
            .await
            .unwrap();
    let commissioning = fixture.claim.clone();
    for access in [Access::Suspended, Access::Revoked] {
        let old = fixture.claim.clone();
        let claim = fixture.access(access).await;
        assert!(matches!(
            fixture
                .vault()
                .read_retained(&fixture.scope, &old, "native-bootstrap-v1")
                .await,
            Err(Error::StaleFence)
        ));
        let retained = fixture
            .vault()
            .read_retained(&fixture.scope, &claim, "native-bootstrap-v1")
            .await
            .unwrap()
            .unwrap();
        assert!(retained.context == original.context);
        assert!(retained.value == original.value);
        assert!(retained.context.generation < claim.generation);
        assert!(matches!(
            fixture
                .vault()
                .get_or_create(&fixture.scope, &claim, "native-bootstrap-v1", || {
                    panic!("disabled operation generated bootstrap material")
                })
                .await,
            Err(Error::Forbidden)
        ));
        let context = MaterialContext {
            id: Uuid::new_v4(),
            installation_id: fixture.scope.id,
            instance_id: claim.instance_id,
            epoch: fixture.scope.epoch,
            operation_id: claim.id,
            generation: claim.generation,
            purpose: "forbidden-new-record".into(),
        };
        let sealed = cipher()
            .seal_material(&context, b"must-not-be-committed")
            .unwrap();
        assert!(matches!(
            fixture
                .store
                .reserve_material(&fixture.scope, &claim, MaterialRecord { context, sealed })
                .await,
            Err(Error::Forbidden)
        ));
    }
    assert!(matches!(
        fixture
            .store
            .find_retained_material(&fixture.scope, &commissioning, "native-bootstrap-v1")
            .await,
        Err(Error::StaleFence)
    ));
    let ciphertext: Vec<u8> =
        sqlx::query_scalar("SELECT ciphertext FROM manager.bootstrap_materials")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(ciphertext == original_ciphertext);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM manager.bootstrap_materials")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn absent_retained_material_stays_absent_without_generation_or_reservation(pool: PgPool) {
    let mut fixture = Fixture::new(&pool).await;
    let claim = fixture.access(Access::Suspended).await;
    assert!(
        fixture
            .vault()
            .read_retained(&fixture.scope, &claim, "native-bootstrap-v1")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        fixture
            .store
            .find_retained_material(&fixture.scope, &claim, "render-instance-g1")
            .await
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        fixture
            .vault()
            .get_or_create(&fixture.scope, &claim, "native-bootstrap-v1", || {
                panic!("retirement of an unstarted instance generated credentials")
            })
            .await,
        Err(Error::Forbidden)
    ));
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM manager.bootstrap_materials")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn enabled_wrong_kind_wrong_scope_and_unavailable_key_cannot_read_retained_material(
    pool: PgPool,
) {
    let mut fixture = Fixture::new(&pool).await;
    fixture.material().await;
    assert!(matches!(
        fixture
            .vault()
            .read_retained(&fixture.scope, &fixture.claim, "native-bootstrap-v1")
            .await,
        Err(Error::Forbidden)
    ));
    let mut forged = fixture.claim.clone();
    forged.kind = "suspend".into();
    assert!(matches!(
        fixture
            .store
            .find_retained_material(&fixture.scope, &forged, "native-bootstrap-v1")
            .await,
        Err(Error::Forbidden)
    ));
    let claim = fixture.access(Access::Suspended).await;
    for kind in ["commission", "retry", "resync", "revoke"] {
        let mut forged = claim.clone();
        forged.kind = kind.into();
        assert!(matches!(
            fixture
                .store
                .find_retained_material(&fixture.scope, &forged, "native-bootstrap-v1")
                .await,
            Err(Error::Forbidden)
        ));
    }
    let mut changed = fixture.scope.clone();
    changed.issuer = "https://other.example.org/realms/agents".into();
    assert!(matches!(
        fixture
            .store
            .find_retained_material(&changed, &claim, "native-bootstrap-v1")
            .await,
        Err(Error::RecoveryRequired)
    ));
    changed = fixture.scope.clone();
    changed.epoch += 1;
    assert!(matches!(
        fixture
            .store
            .find_retained_material(&changed, &claim, "native-bootstrap-v1")
            .await,
        Err(Error::RecoveryRequired)
    ));
    let mut stale = claim.clone();
    stale.epoch += 1;
    assert!(matches!(
        fixture
            .store
            .find_retained_material(&fixture.scope, &stale, "native-bootstrap-v1")
            .await,
        Err(Error::StaleFence)
    ));
    stale = claim.clone();
    stale.instance_id = Uuid::new_v4();
    assert!(matches!(
        fixture
            .store
            .find_retained_material(&fixture.scope, &stale, "native-bootstrap-v1")
            .await,
        Err(Error::StaleFence)
    ));
    let missing_key = Sealer::new(
        "missing".into(),
        BTreeMap::from([("missing".into(), vec![43; 32])]),
    )
    .unwrap();
    assert!(matches!(
        MaterialVault::new(fixture.store.clone(), missing_key)
            .read_retained(&fixture.scope, &claim, "native-bootstrap-v1")
            .await,
        Err(Error::InvalidIdentityEvidence)
    ));
    fixture.access(Access::Enabled).await;
    assert!(matches!(
        fixture
            .vault()
            .read_retained(&fixture.scope, &claim, "native-bootstrap-v1")
            .await,
        Err(Error::StaleFence)
    ));
    assert!(matches!(
        fixture
            .vault()
            .read_retained(&fixture.scope, &fixture.claim, "native-bootstrap-v1")
            .await,
        Err(Error::Forbidden)
    ));
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn actual_database_kind_and_pinned_access_must_match_current_disabled_assignment(
    pool: PgPool,
) {
    let mut fixture = Fixture::new(&pool).await;
    fixture.material().await;
    fixture.access(Access::Suspended).await;
    // Fixture-only insertion simulates a syntactically valid future queue
    // producer with an inappropriate kind/input. Existing identities/inputs
    // remain immutable, and the read port must reject this new current work.
    for (kind, access) in [
        ("retry", "suspended"),
        ("suspend", "enabled"),
        ("revoke", "revoked"),
    ] {
        let mut tx = pool.begin().await.unwrap();
        sqlx::query("UPDATE manager.operations SET state='Superseded',lease_owner=NULL,lease_until=NULL,fence=fence+1 WHERE instance_id=$1 AND state='Running'")
            .bind(fixture.claim.instance_id).execute(&mut *tx).await.unwrap();
        let generation: i64 = sqlx::query_scalar("UPDATE manager.instances SET desired_generation=desired_generation+1 WHERE id=$1 RETURNING desired_generation")
            .bind(fixture.claim.instance_id).fetch_one(&mut *tx).await.unwrap();
        sqlx::query("INSERT INTO manager.operations(id,installation_id,instance_id,kind,generation,epoch,input) VALUES($1,$2,$3,$4,$5,$6,$7)")
            .bind(Uuid::new_v4()).bind(fixture.scope.id).bind(fixture.claim.instance_id).bind(kind)
            .bind(generation).bind(fixture.scope.epoch)
            .bind(json!({"access":access,"templateId":"personal-default","templateVersion":1}))
            .execute(&mut *tx).await.unwrap();
        tx.commit().await.unwrap();
        fixture.claim = fixture
            .store
            .claim(&fixture.scope, Uuid::new_v4(), 30)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            fixture
                .store
                .find_retained_material(&fixture.scope, &fixture.claim, "native-bootstrap-v1")
                .await,
            Err(Error::Forbidden)
        ));
    }
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn genuine_disabled_resync_reads_original_material_and_tolerates_late_namespace_binding(
    pool: PgPool,
) {
    let mut fixture = Fixture::new(&pool).await;
    let original = fixture.material().await;
    fixture.access(Access::Suspended).await;
    let resync = fixture.resync(&pool, "Suspended").await;
    let pinned: ResyncInput = serde_json::from_value(
        sqlx::query_scalar::<_, Value>("SELECT input FROM manager.operations WHERE id=$1")
            .bind(resync.id)
            .fetch_one(&pool)
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(pinned.namespace_uid.is_none());
    let retained = fixture
        .vault()
        .read_retained(&fixture.scope, &resync, "native-bootstrap-v1")
        .await
        .unwrap()
        .unwrap();
    assert!(retained.context == original.context);
    assert!(retained.value == original.value);
    assert!(
        fixture
            .vault()
            .read_retained(&fixture.scope, &resync, "render-instance-g1")
            .await
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        fixture
            .vault()
            .get_or_create(&fixture.scope, &resync, "native-bootstrap-v1", || {
                panic!("periodic disabled observation generated credentials")
            })
            .await,
        Err(Error::Forbidden)
    ));
    // Model the result of a separately authenticated late namespace binding.
    // The periodic operation keeps its original immutable None snapshot.
    sqlx::query("UPDATE manager.instances SET namespace_uid=$2 WHERE id=$1")
        .bind(resync.instance_id)
        .bind(Uuid::new_v4().to_string())
        .execute(&pool)
        .await
        .unwrap();
    let retained = fixture
        .vault()
        .read_retained(&fixture.scope, &resync, "native-bootstrap-v1")
        .await
        .unwrap()
        .unwrap();
    assert!(retained.context == original.context);
    assert!(retained.value == original.value);
    fixture.access(Access::Revoked).await;
    let resync = fixture.resync(&pool, "Retained").await;
    let retained = fixture
        .vault()
        .read_retained(&fixture.scope, &resync, "native-bootstrap-v1")
        .await
        .unwrap()
        .unwrap();
    assert!(retained.context == original.context);
    assert!(retained.value == original.value);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM manager.bootstrap_materials")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
    fixture.access(Access::Enabled).await;
    let enabled = fixture.resync(&pool, "Ready").await;
    assert!(matches!(
        fixture
            .vault()
            .read_retained(&fixture.scope, &enabled, "native-bootstrap-v1")
            .await,
        Err(Error::Forbidden)
    ));
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn malformed_or_stale_disabled_resync_snapshot_cannot_read_retained_material(pool: PgPool) {
    let mut fixture = Fixture::new(&pool).await;
    fixture.material().await;
    fixture.access(Access::Suspended).await;
    let namespace_uid = Uuid::new_v4().to_string();
    sqlx::query("UPDATE manager.instances SET namespace_uid=$2 WHERE id=$1")
        .bind(fixture.claim.instance_id)
        .bind(&namespace_uid)
        .execute(&pool)
        .await
        .unwrap();
    fixture.resync(&pool, "Suspended").await;
    let valid = serde_json::to_value(ResyncInput {
        access: Access::Suspended,
        template_id: "personal-default".into(),
        template_version: 1,
        assignment_version: fixture.version,
        generation: fixture.claim.generation,
        epoch: fixture.scope.epoch,
        namespace_uid: Some(namespace_uid),
    })
    .unwrap();
    let mut variants = vec![(json!({"access":"suspended"}), Error::OwnershipConflict)];
    for (field, value, error) in [
        ("access", json!("enabled"), Error::Forbidden),
        ("access", json!("revoked"), Error::Forbidden),
        (
            "assignmentVersion",
            json!(fixture.version + 1),
            Error::OwnershipConflict,
        ),
        (
            "generation",
            json!(fixture.claim.generation + 1),
            Error::OwnershipConflict,
        ),
        (
            "epoch",
            json!(fixture.scope.epoch + 1),
            Error::OwnershipConflict,
        ),
        (
            "templateId",
            json!("other-template"),
            Error::OwnershipConflict,
        ),
        ("templateVersion", json!(2), Error::OwnershipConflict),
        (
            "namespaceUid",
            json!(Uuid::new_v4().to_string()),
            Error::OwnershipConflict,
        ),
        ("namespaceUid", json!(""), Error::OwnershipConflict),
        ("unknownField", json!(true), Error::OwnershipConflict),
    ] {
        let mut input = valid.clone();
        input[field] = value;
        variants.push((input, error));
    }
    for (input, error) in variants {
        // Insert a new, syntactically valid queue record to exercise the trust
        // boundary. Existing operation snapshots remain immutable history.
        sqlx::query("UPDATE manager.operations SET state='Superseded',lease_owner=NULL,lease_until=NULL,fence=fence+1 WHERE id=$1")
            .bind(fixture.claim.id).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO manager.operations(id,installation_id,instance_id,kind,generation,epoch,input) VALUES($1,$2,$3,'resync',$4,$5,$6)")
            .bind(Uuid::new_v4()).bind(fixture.scope.id).bind(fixture.claim.instance_id)
            .bind(fixture.claim.generation).bind(fixture.scope.epoch).bind(input).execute(&pool).await.unwrap();
        fixture.claim = fixture
            .store
            .claim(&fixture.scope, Uuid::new_v4(), 30)
            .await
            .unwrap()
            .unwrap();
        let result = fixture
            .vault()
            .read_retained(&fixture.scope, &fixture.claim, "native-bootstrap-v1")
            .await;
        assert!(matches!(result, Err(actual) if actual == error));
    }
}
