//! Real database create-once credentials, encryption context and recovery gates.
#[path = "support/grants.rs"]
mod grant_fixture;
use hermes_adapters::{
    materials::MaterialVault,
    postgres::{FreshInventory, PgStore},
    session_crypto::{Purpose, Sealer, hash},
};
use hermes_application::{
    AdminPolicy, AssignmentCommand, Assignments, Error, InstallationScope, OperationClaim,
    OperationQueue, VerifiedPrincipal,
    materials::{MaterialContext, MaterialRepository},
};
use hermes_domain::{Access, Identity, TemplateVersion};
use sqlx::PgPool;
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;
use zeroize::Zeroizing;

fn cipher() -> Sealer {
    Sealer::new(
        "controller-key".into(),
        BTreeMap::from([("controller-key".into(), vec![42; 32])]),
    )
    .unwrap()
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
fn policy(scope: &InstallationScope) -> AdminPolicy {
    AdminPolicy {
        issuer: scope.issuer.clone(),
        audience: "manager-api".into(),
        management_client: "manager-admin".into(),
        administrator_role: "administrator".into(),
    }
}
async fn fixture(
    pool: &PgPool,
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
                cluster_identity: "material-test".into(),
                digest: "a".repeat(64),
                registered_instances: 0,
                unexplained_resources: 0,
            },
        )
        .await
        .unwrap();
    grant_fixture::grant(pool, &scope, &["admin"]).await;
    sqlx::query("INSERT INTO manager.instance_templates(id,version,schema_version,image,content_digest,approved) VALUES('personal-default',1,1,$1,$2,true)").bind(format!("ghcr.io/example/hermes@sha256:{}","a".repeat(64))).bind("a".repeat(64)).execute(pool).await.unwrap();
    let service = Assignments::new(store.clone(), scope.clone(), policy(&scope)).unwrap();
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
#[test]
fn material_aead_binds_owner_installation_record_operation_epoch_generation_and_purpose() {
    let c = MaterialContext {
        id: Uuid::new_v4(),
        installation_id: Uuid::new_v4(),
        instance_id: Uuid::new_v4(),
        epoch: 1,
        operation_id: Uuid::new_v4(),
        generation: 1,
        purpose: "native-bootstrap-v1".into(),
    };
    let cipher = cipher();
    let sealed = cipher
        .seal_material(&c, b"fixture-bootstrap-credential")
        .unwrap();
    assert!(&**cipher.open_material(&c, &sealed).unwrap() == b"fixture-bootstrap-credential");
    let mut variants = vec![];
    let mut d = c.clone();
    d.id = Uuid::new_v4();
    variants.push(d);
    let mut d = c.clone();
    d.installation_id = Uuid::new_v4();
    variants.push(d);
    let mut d = c.clone();
    d.instance_id = Uuid::new_v4();
    variants.push(d);
    let mut d = c.clone();
    d.operation_id = Uuid::new_v4();
    variants.push(d);
    let mut d = c.clone();
    d.epoch += 1;
    variants.push(d);
    let mut d = c.clone();
    d.generation += 1;
    variants.push(d);
    let mut d = c.clone();
    d.purpose = "other-credential".into();
    variants.push(d);
    for other in variants {
        assert!(cipher.open_material(&other, &sealed).is_err());
    }
    let scope = InstallationScope {
        id: c.installation_id,
        epoch: 1,
        issuer: "https://sso.example.org".into(),
    };
    assert!(
        cipher
            .open(
                &scope,
                Purpose::Administration,
                "session",
                &hash("id"),
                &sealed
            )
            .is_err()
    );
    let mut changed = sealed.clone();
    changed.ciphertext[0] ^= 1;
    assert!(cipher.open_material(&c, &changed).is_err());
}
#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn lost_response_and_reopen_reuse_original_encrypted_material_without_generation(
    pool: PgPool,
) {
    let (store, scope, claim, _) = fixture(&pool).await;
    let first = MaterialVault::new(store.clone(), cipher())
        .get_or_create(&scope, &claim, "native-bootstrap-v1", || {
            Ok(Zeroizing::new(b"fixture-bootstrap-credential".to_vec()))
        })
        .await
        .unwrap();
    let persisted: Vec<u8> =
        sqlx::query_scalar("SELECT ciphertext FROM manager.bootstrap_materials")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!persisted.windows(10).any(|p| p == b"credential"));
    drop(store);
    let store = PgStore::connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    let next = MaterialVault::new(store.clone(), cipher())
        .get_or_create(&scope, &claim, "native-bootstrap-v1", || {
            panic!("committed credential was regenerated")
        })
        .await
        .unwrap();
    assert!(first.value == next.value);
    assert!(first.context == next.context);
    assert!(
        sqlx::query("UPDATE manager.bootstrap_materials SET ciphertext=$1")
            .bind(vec![0u8; 32])
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM manager.bootstrap_materials")
            .execute(&pool)
            .await
            .is_err()
    );
    let audit: serde_json::Value = sqlx::query_scalar("SELECT change FROM manager.audit_events")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(!audit.to_string().contains("credential"));
}
#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn concurrent_candidates_converge_on_one_ciphertext_and_one_secret_value(pool: PgPool) {
    let (store, scope, claim, _) = fixture(&pool).await;
    let vault = MaterialVault::new(store, cipher());
    let invalid=sqlx::query("INSERT INTO manager.bootstrap_materials(id,installation_id,instance_id,epoch,purpose,key_id,nonce,ciphertext,creation_operation,creation_generation) VALUES($1,$2,$3,$4,'invalid-generation','controller-key',$5,$6,$7,$8)")
        .bind(Uuid::new_v4()).bind(scope.id).bind(claim.instance_id).bind(scope.epoch).bind(vec![0u8;24]).bind(vec![0u8;16]).bind(claim.id).bind(claim.generation+1).execute(&pool).await;
    assert!(invalid.is_err());
    let mut tasks = tokio::task::JoinSet::new();
    for i in 0..16 {
        let vault = vault.clone();
        let scope = scope.clone();
        let claim = claim.clone();
        tasks.spawn(async move {
            vault
                .get_or_create(&scope, &claim, "native-bootstrap-v1", || {
                    Ok(Zeroizing::new(format!("candidate-{i}").into_bytes()))
                })
                .await
                .unwrap()
        });
    }
    let first = tasks.join_next().await.unwrap().unwrap();
    while let Some(other) = tasks.join_next().await {
        let other = other.unwrap();
        assert!(first.context == other.context);
        assert!(first.value == other.value);
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM manager.bootstrap_materials")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
}
#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn unavailable_or_retired_key_blocks_instead_of_regenerating_native_credentials(
    pool: PgPool,
) {
    let (store, scope, claim, _) = fixture(&pool).await;
    MaterialVault::new(store.clone(), cipher())
        .get_or_create(&scope, &claim, "native-bootstrap-v1", || {
            Ok(Zeroizing::new(b"fixture-original".to_vec()))
        })
        .await
        .unwrap();
    let missing = Sealer::new(
        "new-key".into(),
        BTreeMap::from([("new-key".into(), vec![43; 32])]),
    )
    .unwrap();
    let result = MaterialVault::new(store, missing)
        .get_or_create(&scope, &claim, "native-bootstrap-v1", || {
            panic!("lost key caused credential replacement")
        })
        .await;
    assert!(matches!(result, Err(Error::InvalidIdentityEvidence)));
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM manager.bootstrap_materials")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn native_credentials_and_initial_seed_remain_original_across_template_changes(pool: PgPool) {
    let (store, scope, claim, _) = fixture(&pool).await;
    let vault = MaterialVault::new(store, cipher());
    let (first_material, first) = vault
        .native_bootstrap(
            &scope,
            &claim,
            TemplateVersion {
                id: "personal-default".into(),
                version: 1,
            },
            "https://manager.example.org/execution",
        )
        .await
        .unwrap();
    let (second_material, second) = vault
        .native_bootstrap(
            &scope,
            &claim,
            TemplateVersion {
                id: "personal-default".into(),
                version: 2,
            },
            "https://changed.example.org/execution",
        )
        .await
        .unwrap();
    assert!(first_material.context == second_material.context);
    assert!(first.dashboard_session_token == second.dashboard_session_token);
    assert!(first.execution_client_token == second.execution_client_token);
    assert!(first.initialization_key == second.initialization_key);
    assert!(first.ssh_private_key == second.ssh_private_key);
    assert_eq!(second.template.version, 1);
    assert_eq!(
        second.native_config["plugins"]["entries"]["managed_execution"]["settings"]["manager_url"],
        "https://manager.example.org/execution"
    );
}
#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn revoke_and_changed_epoch_fence_old_writers_while_original_material_survives(pool: PgPool) {
    let (store, scope, claim, service) = fixture(&pool).await;
    let first = MaterialVault::new(store.clone(), cipher())
        .get_or_create(&scope, &claim, "native-bootstrap-v1", || {
            Ok(Zeroizing::new(b"fixture-original".to_vec()))
        })
        .await
        .unwrap();
    sqlx::query("UPDATE manager.installations SET epoch=2")
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        store
            .find_material(&scope, &claim, "native-bootstrap-v1")
            .await,
        Err(Error::RecoveryRequired)
    ));
    let mut next_scope = scope.clone();
    next_scope.epoch = 2;
    drop(service);
    let service = Assignments::new(store.clone(), next_scope.clone(), policy(&scope)).unwrap();
    service
        .execute(
            &actor(&scope),
            Uuid::new_v4(),
            "retry",
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
    let existing = MaterialVault::new(store.clone(), cipher())
        .get_or_create(&next_scope, &next, "native-bootstrap-v1", || {
            panic!("maintenance regenerated credentials")
        })
        .await
        .unwrap();
    assert!(first.context == existing.context);
    assert!(first.value == existing.value);
    let assignment: Uuid = sqlx::query_scalar("SELECT id FROM manager.assignments")
        .fetch_one(&pool)
        .await
        .unwrap();
    service
        .execute(
            &actor(&scope),
            Uuid::new_v4(),
            "revoke",
            AssignmentCommand::SetAccess {
                assignment_id: assignment,
                expected_version: 2,
                access: Access::Revoked,
            },
        )
        .await
        .unwrap();
    let revoked = store
        .claim(&next_scope, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        store
            .find_material(&next_scope, &revoked, "native-bootstrap-v1")
            .await,
        Err(Error::Forbidden)
    ));
}
