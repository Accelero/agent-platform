use hermes_adapters::{
    materials::MaterialVault,
    postgres::{FreshInventory, PgStore},
    session_crypto::Sealer,
};
use hermes_application::{
    AdminPolicy, AssignmentCommand, Assignments, Error, InstallationScope, OperationClaim,
    OperationQueue, VerifiedPrincipal,
    terminal::clients::{ExecutionClientRepository, ExecutionTokenHash, NATIVE_EXECUTION_PROFILE},
};
use hermes_domain::{Access, Identity, TemplateVersion};
use sqlx::PgPool;
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

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

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn scoped_clients_are_create_once_and_live_authorized(pool: PgPool) {
    let (store, scope, claim, service) = fixture(&pool).await;
    let vault = MaterialVault::new(store.clone(), cipher());
    let (material, bootstrap) = vault
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
    let token = || ExecutionTokenHash::from_token(&bootstrap.execution_client_token).unwrap();
    let first = store
        .register_under_current_claim(
            &scope,
            &claim,
            &material.context,
            token(),
            NATIVE_EXECUTION_PROFILE,
        )
        .await
        .unwrap();
    let second = store
        .register_under_current_claim(
            &scope,
            &claim,
            &material.context,
            token(),
            NATIVE_EXECUTION_PROFILE,
        )
        .await
        .unwrap();
    assert_eq!(first, second);
    assert_eq!(
        store
            .authenticate_execution_client(&scope, token())
            .await
            .unwrap(),
        first
    );
    assert!(matches!(
        store
            .register_under_current_claim(
                &scope,
                &claim,
                &material.context,
                ExecutionTokenHash::from_token(&"b".repeat(64)).unwrap(),
                NATIVE_EXECUTION_PROFILE
            )
            .await,
        Err(Error::OwnershipConflict)
    ));
    assert!(matches!(
        store
            .register_under_current_claim(&scope, &claim, &material.context, token(), "foreign")
            .await,
        Err(Error::Invalid)
    ));
    assert!(matches!(
        store
            .authenticate_execution_client(
                &scope,
                ExecutionTokenHash::from_token(&"c".repeat(64)).unwrap()
            )
            .await,
        Err(Error::Forbidden)
    ));
    // A second owner's persisted material cannot register under this owner claim.
    service
        .execute(
            &actor(&scope),
            Uuid::new_v4(),
            "create-b",
            AssignmentCommand::Create {
                identity: Identity {
                    issuer: scope.issuer.clone(),
                    subject: "owner-b".into(),
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
    let claim_b = store
        .claim(&scope, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    let (material_b, bootstrap_b) = vault
        .native_bootstrap(
            &scope,
            &claim_b,
            TemplateVersion {
                id: "personal-default".into(),
                version: 1,
            },
            "https://manager.example.org/execution",
        )
        .await
        .unwrap();
    assert!(matches!(
        store
            .register_under_current_claim(
                &scope,
                &claim_b,
                &material.context,
                token(),
                NATIVE_EXECUTION_PROFILE
            )
            .await,
        Err(Error::Invalid)
    ));
    let client_b = store
        .register_under_current_claim(
            &scope,
            &claim_b,
            &material_b.context,
            ExecutionTokenHash::from_token(&bootstrap_b.execution_client_token).unwrap(),
            NATIVE_EXECUTION_PROFILE,
        )
        .await
        .unwrap();
    assert_ne!(client_b.instance_id, first.instance_id);
    assert!(
        sqlx::query("UPDATE manager.execution_clients SET profile='other' WHERE id=$1")
            .bind(first.id)
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM manager.execution_clients WHERE id=$1")
            .bind(first.id)
            .execute(&pool)
            .await
            .is_err()
    );
    sqlx::query("UPDATE manager.assignments SET access='suspended',version=version+1 WHERE id=(SELECT assignment_id FROM manager.instances WHERE id=$1)").bind(first.instance_id).execute(&pool).await.unwrap();
    assert!(matches!(
        store.authenticate_execution_client(&scope, token()).await,
        Err(Error::Forbidden)
    ));
    assert!(
        store
            .authenticate_execution_client(
                &scope,
                ExecutionTokenHash::from_token(&bootstrap_b.execution_client_token).unwrap()
            )
            .await
            .is_ok()
    );
}
