//! Resource reservations, immutable provenance and exact-UID retirement in SQL.
use hermes_adapters::{
    ownership::OwnershipSigner,
    postgres::{FreshInventory, PgStore},
};
use hermes_application::{
    AdminPolicy, AssignmentCommand, Assignments, Error, InstallationScope, OperationClaim,
    OperationQueue, VerifiedPrincipal,
    resources::{Creation, ResourceObservation, ResourceRepository},
};
use hermes_domain::{
    Access, Identity, TemplateVersion,
    resources::{ResourceKind, ResourceSpec, Retention},
};
use sqlx::PgPool;
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

async fn fixture(
    pool: &PgPool,
) -> (
    PgStore,
    InstallationScope,
    OperationClaim,
    Assignments<PgStore>,
) {
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
                cluster_identity: "resource-test".into(),
                digest: "a".repeat(64),
                registered_instances: 0,
                unexplained_resources: 0,
            },
        )
        .await
        .unwrap();
    sqlx::query("INSERT INTO manager.instance_templates(id,version,schema_version,image,content_digest,approved) VALUES('personal-default',1,1,$1,$2,true)").bind(format!("ghcr.io/example/hermes@sha256:{}","a".repeat(64))).bind("a".repeat(64)).execute(pool).await.unwrap();
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
fn namespace(scope: &InstallationScope, claim: &OperationClaim) -> ResourceSpec {
    ResourceSpec {
        kind: ResourceKind::Namespace,
        namespace: None,
        namespace_uid: None,
        name: hermes_domain::instance_namespace(scope.id, claim.instance_id),
        manifest_digest: "a".repeat(64),
        purpose: "namespace".into(),
        retention: Retention::Retain,
        encrypted_material_ref: None,
    }
}
fn child(scope: &InstallationScope, claim: &OperationClaim) -> ResourceSpec {
    ResourceSpec {
        kind: ResourceKind::ConfigMap,
        namespace: Some(hermes_domain::instance_namespace(
            scope.id,
            claim.instance_id,
        )),
        namespace_uid: Some("namespace-uid".into()),
        name: "native-config".into(),
        manifest_digest: "b".repeat(64),
        purpose: "native-config".into(),
        retention: Retention::Delete,
        encrypted_material_ref: None,
    }
}
fn observed(uid: &str, rv: &str, digest: &str) -> ResourceObservation {
    ResourceObservation {
        uid: uid.into(),
        resource_version: rv.into(),
        manifest_digest: digest.into(),
    }
}
async fn bind_namespace(store: &PgStore, scope: &InstallationScope, claim: &OperationClaim) {
    let spec = namespace(scope, claim);
    store
        .prepare_resource(scope, claim, "namespace", spec.clone())
        .await
        .unwrap();
    store
        .bind_resource(
            scope,
            claim,
            "namespace",
            observed("namespace-uid", "1", &spec.manifest_digest),
        )
        .await
        .unwrap();
}

#[test]
fn authenticated_marker_binds_complete_provenance_and_supports_key_rotation() {
    let scope = InstallationScope {
        id: Uuid::new_v4(),
        epoch: 1,
        issuer: "https://sso.example.org".into(),
    };
    let claim = OperationClaim {
        id: Uuid::new_v4(),
        instance_id: Uuid::new_v4(),
        generation: 1,
        epoch: 1,
        worker: Uuid::new_v4(),
        fence: 1,
        kind: "commission".into(),
    };
    let creation = Creation {
        id: Uuid::new_v4(),
        installation_id: scope.id,
        instance_id: claim.instance_id,
        cluster_identity: "cluster-a".into(),
        epoch: 1,
        generation: 1,
        operation_id: claim.id,
        spec: child(&scope, &claim),
    };
    let signer = OwnershipSigner::new(
        "key-one".into(),
        BTreeMap::from([("key-one".into(), vec![42; 32])]),
    )
    .unwrap();
    let proof = signer.sign(&creation).unwrap();
    signer.verify(&creation, &proof).unwrap();
    let rotated = OwnershipSigner::new(
        "key-two".into(),
        BTreeMap::from([
            ("key-one".into(), vec![42; 32]),
            ("key-two".into(), vec![43; 32]),
        ]),
    )
    .unwrap();
    rotated.verify(&creation, &proof).unwrap();
    let mut variants = vec![];
    let mut other = creation.clone();
    other.installation_id = Uuid::new_v4();
    variants.push(other);
    let mut other = creation.clone();
    other.instance_id = Uuid::new_v4();
    variants.push(other);
    let mut other = creation.clone();
    other.id = Uuid::new_v4();
    variants.push(other);
    let mut other = creation.clone();
    other.operation_id = Uuid::new_v4();
    variants.push(other);
    let mut other = creation.clone();
    other.cluster_identity = "cluster-b".into();
    variants.push(other);
    let mut other = creation.clone();
    other.epoch += 1;
    variants.push(other);
    let mut other = creation.clone();
    other.generation += 1;
    variants.push(other);
    let mut other = creation.clone();
    other.spec.namespace_uid = Some("foreign-namespace".into());
    variants.push(other);
    let mut other = creation.clone();
    other.spec.manifest_digest = "c".repeat(64);
    variants.push(other);
    let mut other = creation.clone();
    other.spec.name = "foreign".into();
    variants.push(other);
    for other in variants {
        assert_eq!(signer.verify(&other, &proof), Err(Error::OwnershipConflict));
    }
    assert!(signer.verify(&creation, &format!("{proof}0")).is_err());
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn reservation_and_journal_commit_before_creation_and_reopen_preserves_identity(
    pool: PgPool,
) {
    let (store, scope, claim, _) = fixture(&pool).await;
    let spec = namespace(&scope, &claim);
    let prepared = store
        .prepare_resource(&scope, &claim, "namespace", spec.clone())
        .await
        .unwrap();
    assert!(prepared.resource.uid.is_none());
    assert_eq!(
        prepared.step.intent.creation_id,
        prepared.resource.creation.id
    );
    assert_eq!(
        prepared.step.result,
        hermes_application::StepResult::Unknown
    );
    drop(store);
    let reopened = PgStore::connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    let recovered = reopened
        .prepare_resource(&scope, &claim, "namespace", spec)
        .await
        .unwrap();
    assert_eq!(recovered.resource, prepared.resource);
    assert_eq!(recovered.step, prepared.step);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM manager.resource_steps")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
    reopened
        .bind_resource(
            &scope,
            &claim,
            "namespace",
            observed("namespace-uid", "1", &"a".repeat(64)),
        )
        .await
        .unwrap();
    let uid: String = sqlx::query_scalar("SELECT namespace_uid FROM manager.instances")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(uid, "namespace-uid");
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM manager.instance_routes")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn child_creation_requires_exact_bound_parent_namespace_and_registered_material(
    pool: PgPool,
) {
    let (store, scope, claim, _) = fixture(&pool).await;
    assert!(matches!(
        store
            .prepare_resource(&scope, &claim, "native", child(&scope, &claim))
            .await,
        Err(Error::OwnershipConflict)
    ));
    bind_namespace(&store, &scope, &claim).await;
    let mut wrong = child(&scope, &claim);
    wrong.namespace_uid = Some("foreign-uid".into());
    assert!(
        store
            .prepare_resource(&scope, &claim, "wrong", wrong)
            .await
            .is_err()
    );
    let mut wrong = child(&scope, &claim);
    wrong.namespace = Some("control-plane".into());
    assert!(
        store
            .prepare_resource(&scope, &claim, "wrong", wrong)
            .await
            .is_err()
    );
    let mut secret = child(&scope, &claim);
    secret.kind = ResourceKind::Secret;
    secret.encrypted_material_ref = Some(Uuid::new_v4());
    assert!(
        store
            .prepare_resource(&scope, &claim, "secret", secret)
            .await
            .is_err()
    );
    store
        .prepare_resource(&scope, &claim, "native", child(&scope, &claim))
        .await
        .unwrap();
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn first_uid_binding_is_immutable_and_namespace_callbacks_are_serialized(pool: PgPool) {
    let (store, scope, claim, _) = fixture(&pool).await;
    store
        .prepare_resource(&scope, &claim, "namespace", namespace(&scope, &claim))
        .await
        .unwrap();
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..12 {
        let store = store.clone();
        let scope = scope.clone();
        let claim = claim.clone();
        tasks.spawn(async move {
            store
                .bind_resource(
                    &scope,
                    &claim,
                    "namespace",
                    observed("namespace-uid", "1", &"a".repeat(64)),
                )
                .await
        });
    }
    while let Some(result) = tasks.join_next().await {
        result.unwrap().unwrap();
    }
    assert_eq!(
        store
            .bind_resource(
                &scope,
                &claim,
                "namespace",
                observed("foreign-uid", "2", &"a".repeat(64))
            )
            .await,
        Err(Error::OwnershipConflict)
    );
    // Status/defaulting may advance RV after the action was verified. Observe
    // the same UID again without rewriting its immutable original step receipt.
    store
        .bind_resource(
            &scope,
            &claim,
            "namespace",
            observed("namespace-uid", "2", &"a".repeat(64)),
        )
        .await
        .unwrap();
    let rv: String = sqlx::query_scalar("SELECT resource_version FROM manager.owned_resources")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rv, "2");
    assert!(
        sqlx::query("UPDATE manager.owned_resources SET uid='foreign'")
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM manager.owned_resources")
            .execute(&pool)
            .await
            .is_err()
    );
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn disable_or_lost_claim_fences_binding_and_cannot_create_more_resources(pool: PgPool) {
    let (store, scope, claim, service) = fixture(&pool).await;
    store
        .prepare_resource(&scope, &claim, "namespace", namespace(&scope, &claim))
        .await
        .unwrap();
    service
        .execute(
            &actor(&scope),
            Uuid::new_v4(),
            "disable",
            AssignmentCommand::SetAccess {
                assignment_id: sqlx::query_scalar("SELECT id FROM manager.assignments")
                    .fetch_one(&pool)
                    .await
                    .unwrap(),
                expected_version: 1,
                access: Access::Revoked,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .bind_resource(
                &scope,
                &claim,
                "namespace",
                observed("namespace-uid", "1", &"a".repeat(64))
            )
            .await,
        Err(Error::StaleFence)
    );
    let next = store
        .claim(&scope, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        store
            .prepare_resource(&scope, &next, "namespace", namespace(&scope, &next))
            .await,
        Err(Error::Forbidden)
    ));
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn creation_success_before_uid_save_recovers_original_intent_after_worker_expiry(
    pool: PgPool,
) {
    let (store, scope, claim, _) = fixture(&pool).await;
    let prepared = store
        .prepare_resource(&scope, &claim, "namespace", namespace(&scope, &claim))
        .await
        .unwrap();
    sqlx::query("UPDATE manager.operations SET lease_until=clock_timestamp()-interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    let next = store
        .claim(&scope, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    let recovered = store
        .prepare_resource(&scope, &next, "namespace", namespace(&scope, &next))
        .await
        .unwrap();
    assert_eq!(prepared.resource, recovered.resource);
    assert_eq!(prepared.step.intent, recovered.step.intent);
    assert_eq!(
        store
            .bind_resource(
                &scope,
                &claim,
                "namespace",
                observed("namespace-uid", "1", &"a".repeat(64))
            )
            .await,
        Err(Error::StaleFence)
    );
    store
        .bind_resource(
            &scope,
            &next,
            "namespace",
            observed("namespace-uid", "1", &"a".repeat(64)),
        )
        .await
        .unwrap();
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn retirement_requires_retention_decision_and_exact_uid_and_preserves_tombstone(
    pool: PgPool,
) {
    let (store, scope, claim, _) = fixture(&pool).await;
    bind_namespace(&store, &scope, &claim).await;
    let spec = child(&scope, &claim);
    let child = store
        .prepare_resource(&scope, &claim, "native", spec.clone())
        .await
        .unwrap();
    store
        .bind_resource(
            &scope,
            &claim,
            "native",
            observed("config-uid", "1", &spec.manifest_digest),
        )
        .await
        .unwrap();
    assert!(
        store
            .retire_resource(
                &scope,
                &claim,
                "delete",
                child.resource.creation.id,
                observed("foreign-uid", "1", &spec.manifest_digest)
            )
            .await
            .is_err()
    );
    let retired = store
        .retire_resource(
            &scope,
            &claim,
            "delete",
            child.resource.creation.id,
            observed("config-uid", "2", &spec.manifest_digest),
        )
        .await
        .unwrap();
    assert_eq!(
        retired.step.intent.expected_resource_version.as_deref(),
        Some("2")
    );
    assert!(store.finish(&scope, &claim).await.is_err());
    store
        .resource_absent(&scope, &claim, "delete")
        .await
        .unwrap();
    store
        .resource_absent(&scope, &claim, "delete")
        .await
        .unwrap();
    assert!(
        store
            .prepare_resource(&scope, &claim, "resurrect", spec)
            .await
            .is_err()
    );
    let namespace_id: Uuid =
        sqlx::query_scalar("SELECT id FROM manager.owned_resources WHERE kind='Namespace'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(matches!(
        store
            .retire_resource(
                &scope,
                &claim,
                "purge",
                namespace_id,
                observed("namespace-uid", "1", &"a".repeat(64))
            )
            .await,
        Err(Error::Forbidden)
    ));
    store.finish(&scope, &claim).await.unwrap();
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn reservation_rolls_back_if_journal_link_fails_and_generic_journal_cannot_bypass_binding(
    pool: PgPool,
) {
    let (store, scope, claim, _) = fixture(&pool).await;
    sqlx::raw_sql("CREATE FUNCTION manager.fail_resource_step() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'fixture transaction failure'; END; $$; CREATE TRIGGER fail_resource_step BEFORE INSERT ON manager.resource_steps FOR EACH ROW EXECUTE FUNCTION manager.fail_resource_step();").execute(&pool).await.unwrap();
    assert!(
        store
            .prepare_resource(&scope, &claim, "namespace", namespace(&scope, &claim))
            .await
            .is_err()
    );
    for query in [
        "SELECT count(*) FROM manager.owned_resources",
        "SELECT count(*) FROM manager.operation_steps",
    ] {
        let count: i64 = sqlx::query_scalar(query).fetch_one(&pool).await.unwrap();
        assert_eq!(count, 0);
    }
    sqlx::raw_sql("DROP TRIGGER fail_resource_step ON manager.resource_steps;DROP FUNCTION manager.fail_resource_step();").execute(&pool).await.unwrap();
    store
        .prepare_resource(&scope, &claim, "namespace", namespace(&scope, &claim))
        .await
        .unwrap();
    assert_eq!(
        store
            .record_result(
                &scope,
                &claim,
                "namespace",
                hermes_application::StepResult::Verified {
                    uid: "namespace-uid".into(),
                    resource_version: "1".into()
                }
            )
            .await,
        Err(Error::OwnershipConflict)
    );
    store
        .bind_resource(
            &scope,
            &claim,
            "namespace",
            observed("namespace-uid", "1", &"a".repeat(64)),
        )
        .await
        .unwrap();
    assert!(
        sqlx::query("UPDATE manager.instances SET namespace_uid='foreign'")
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("UPDATE manager.instances SET namespace='foreign'")
            .execute(&pool)
            .await
            .is_err()
    );
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn changed_versions_link_uncertain_apply_intents_without_rewriting_or_replaying_them(
    pool: PgPool,
) {
    let (store, scope, claim, _) = fixture(&pool).await;
    bind_namespace(&store, &scope, &claim).await;
    let mut spec = namespace(&scope, &claim);
    spec.manifest_digest = "c".repeat(64);
    let first = store
        .prepare_resource(&scope, &claim, "update-first", spec.clone())
        .await
        .unwrap();
    store
        .observe_resource_version(
            &scope,
            &claim,
            first.resource.creation.id,
            hermes_application::resources::ResourceVersion {
                uid: "namespace-uid".into(),
                resource_version: "3".into(),
            },
        )
        .await
        .unwrap();
    let next = store
        .prepare_resource(&scope, &claim, "update-next", spec.clone())
        .await
        .unwrap();
    assert_eq!(
        next.step.intent.expected_resource_version.as_deref(),
        Some("3")
    );
    let old = store
        .prepare_resource(&scope, &claim, "update-first", spec)
        .await
        .unwrap();
    assert_eq!(old.step.intent, first.step.intent);
    assert_eq!(
        old.step.result,
        hermes_application::StepResult::Reconciled {
            uid: "namespace-uid".into(),
            resource_version: "3".into(),
            next_step: "update-next".into()
        }
    );
    assert_eq!(
        store
            .bind_resource(
                &scope,
                &claim,
                "update-first",
                observed("namespace-uid", "4", &"c".repeat(64))
            )
            .await,
        Err(Error::StaleFence)
    );
    assert!(store.finish(&scope, &claim).await.is_err());
    store
        .bind_resource(
            &scope,
            &claim,
            "update-next",
            observed("namespace-uid", "4", &"c".repeat(64)),
        )
        .await
        .unwrap();
    store.finish(&scope, &claim).await.unwrap();
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn new_epoch_preserves_bound_provenance_but_requires_recovery_for_an_unbound_old_intent(
    pool: PgPool,
) {
    let (store, scope, claim, _) = fixture(&pool).await;
    bind_namespace(&store, &scope, &claim).await;
    let original = store
        .prepare_resource(&scope, &claim, "namespace", namespace(&scope, &claim))
        .await
        .unwrap();
    store
        .prepare_resource(&scope, &claim, "native-config", child(&scope, &claim))
        .await
        .unwrap();
    sqlx::query("UPDATE manager.installations SET epoch=2")
        .execute(&pool)
        .await
        .unwrap();
    let mut next_scope = scope.clone();
    next_scope.epoch = 2;
    let service = Assignments::new(
        store.clone(),
        next_scope.clone(),
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
            "retry-after-maintenance",
            AssignmentCommand::Retry {
                instance_id: claim.instance_id,
                expected_version: 1,
            },
        )
        .await
        .unwrap();
    let next = store
        .claim(&next_scope, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    let current = store
        .prepare_resource(
            &next_scope,
            &next,
            "namespace",
            namespace(&next_scope, &next),
        )
        .await
        .unwrap();
    assert_eq!(current.resource.creation, original.resource.creation);
    assert_eq!(current.resource.uid.as_deref(), Some("namespace-uid"));
    assert!(matches!(
        store
            .prepare_resource(
                &next_scope,
                &next,
                "native-config",
                child(&next_scope, &next)
            )
            .await,
        Err(Error::OwnershipConflict)
    ));
}
