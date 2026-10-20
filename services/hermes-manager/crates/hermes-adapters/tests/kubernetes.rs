//! Real API-server proof. Run explicitly with a disposable fixture kubeconfig.
//! This uses fixture administrator transport; scoped controller admission/RBAC
//! must be tested separately before commissioning is enabled.
#[path = "kubernetes/activation.rs"]
mod activation;
#[path = "kubernetes/bootstrap.rs"]
mod bootstrap;
#[path = "kubernetes/controller.rs"]
mod controller;
#[path = "kubernetes/fence_admission.rs"]
mod fence_admission;
#[path = "kubernetes/infrastructure.rs"]
mod infrastructure;
#[path = "kubernetes/process.rs"]
mod process;
#[path = "kubernetes/worker_admission.rs"]
mod worker_admission;
#[path = "kubernetes/workloads.rs"]
mod workloads;
use hermes_adapters::{
    kubernetes::{KubernetesDriver, Manifest},
    ownership::OwnershipSigner,
    postgres::{FreshInventory, PgStore},
};
use hermes_application::{
    AdminPolicy, AssignmentCommand, Assignments, Error, InstallationScope, OperationQueue,
    VerifiedPrincipal,
    resources::{ResourceObservation, ResourceReconciler, ResourceRepository},
};
use hermes_domain::{
    Access, Identity, TemplateVersion,
    resources::{ResourceKind, ResourceSpec, Retention},
};
use k8s_openapi::api::core::v1::{ConfigMap, Namespace};
use kube::{
    Api, Client, Config,
    api::{DeleteParams, Patch, PatchParams, PostParams},
    config::{KubeConfigOptions, Kubeconfig},
};
use serde_json::json;
use sqlx::PgPool;
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

fn observation(uid: &str, rv: &str, digest: &str) -> ResourceObservation {
    ResourceObservation {
        uid: uid.into(),
        resource_version: rv.into(),
        manifest_digest: digest.into(),
    }
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
#[ignore = "requires HERMES_TEST_KUBECONFIG from the disposable Kubernetes fixture"]
async fn real_kubernetes_recovers_create_before_uid_commit_and_refuses_foreign_replacement(
    pool: PgPool,
) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let path = std::env::var("HERMES_TEST_KUBECONFIG")
        .expect("explicit disposable fixture kubeconfig required");
    let kubeconfig = Kubeconfig::read_from(path).expect("fixture kubeconfig");
    let config = Config::from_custom_kubeconfig(
        kubeconfig,
        &KubeConfigOptions {
            context: Some("personal-hermes-manager-tests".into()),
            ..KubeConfigOptions::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        config.cluster_url.host(),
        Some("127.0.0.1"),
        "only isolated loopback fixture is supported"
    );
    let client = Client::try_from(config.clone()).unwrap();
    let namespaces: Api<Namespace> = Api::all(client.clone());
    let cluster_identity = namespaces
        .get_metadata("kube-system")
        .await
        .unwrap()
        .metadata
        .uid
        .unwrap();
    let driver = KubernetesDriver::from_config(
        config,
        OwnershipSigner::new(
            "fixture".into(),
            BTreeMap::from([("fixture".into(), vec![42; 32])]),
        )
        .unwrap(),
        cluster_identity.clone(),
    )
    .unwrap();
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
                cluster_identity,
                digest: "a".repeat(64),
                registered_instances: 0,
                unexplained_resources: 0,
            },
        )
        .await
        .unwrap();
    sqlx::query("INSERT INTO manager.instance_templates(id,version,schema_version,image,content_digest,approved) VALUES('personal-default',1,1,$1,$2,true)").bind(format!("ghcr.io/example/hermes@sha256:{}","a".repeat(64))).bind("a".repeat(64)).execute(&pool).await.unwrap();
    let policy = AdminPolicy {
        issuer: scope.issuer.clone(),
        audience: "manager-api".into(),
        management_client: "manager-admin".into(),
        administrator_role: "administrator".into(),
    };
    let actor = VerifiedPrincipal {
        identity: Identity {
            issuer: scope.issuer.clone(),
            subject: "admin".into(),
        },
        audiences: BTreeSet::from(["manager-api".into()]),
        client_roles: BTreeMap::from([(
            "manager-admin".into(),
            BTreeSet::from(["administrator".into()]),
        )]),
    };
    let service = Assignments::new(store.clone(), scope.clone(), policy).unwrap();
    let accepted = service
        .execute(
            &actor,
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
        .claim(&scope, Uuid::new_v4(), 120)
        .await
        .unwrap()
        .unwrap();
    let namespace = hermes_domain::instance_namespace(scope.id, accepted.instance_id);
    let manifest=Manifest::new(json!({"apiVersion":"v1","kind":"Namespace","metadata":{"name":namespace,"labels":{"pod-security.kubernetes.io/enforce":"restricted"}}})).unwrap();
    let spec = ResourceSpec {
        kind: ResourceKind::Namespace,
        namespace: None,
        namespace_uid: None,
        name: namespace.clone(),
        manifest_digest: manifest.digest().unwrap(),
        purpose: "namespace".into(),
        retention: Retention::Retain,
        encrypted_material_ref: None,
    };
    let prepared = store
        .prepare_resource(&scope, &claim, "namespace", spec.clone())
        .await
        .unwrap();
    assert!(
        driver
            .observe(&prepared, &manifest)
            .await
            .unwrap()
            .is_none()
    );
    let created = driver.apply(&prepared, &manifest).await.unwrap();
    // Deliberately lose the result before saving the UID and reclaim the work.
    sqlx::query("UPDATE manager.operations SET lease_until=clock_timestamp()-interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    let recovered_claim = store
        .claim(&scope, Uuid::new_v4(), 120)
        .await
        .unwrap()
        .unwrap();
    let recovered = store
        .prepare_resource(&scope, &recovered_claim, "namespace", spec)
        .await
        .unwrap();
    let adopted = driver
        .observe(&recovered, &manifest)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(created.uid, adopted.uid);
    assert_eq!(
        store
            .bind_resource(
                &scope,
                &claim,
                "namespace",
                observation(
                    &adopted.uid,
                    &adopted.resource_version,
                    &adopted.manifest_digest
                )
            )
            .await,
        Err(Error::StaleFence)
    );
    store
        .bind_resource(&scope, &recovered_claim, "namespace", adopted)
        .await
        .unwrap();
    let config_manifest=Manifest::new(json!({"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":"native-config","namespace":namespace},"data":{"owner":"fixture-a"}})).unwrap();
    let child = ResourceSpec {
        kind: ResourceKind::ConfigMap,
        namespace: Some(namespace.clone()),
        namespace_uid: Some(created.uid),
        name: "native-config".into(),
        manifest_digest: config_manifest.digest().unwrap(),
        purpose: "native-config".into(),
        retention: Retention::Delete,
        encrypted_material_ref: None,
    };
    let _prepared = store
        .prepare_resource(&scope, &recovered_claim, "native-config", child.clone())
        .await
        .unwrap();
    let reconciler = ResourceReconciler::new(store.clone(), driver.clone(), scope.clone());
    let created = reconciler
        .ensure(
            &recovered_claim,
            "native-config",
            child.clone(),
            &config_manifest,
        )
        .await
        .unwrap();
    let original_uid = created.uid.clone();
    let maps: Api<ConfigMap> = Api::namespaced(client.clone(), &namespace);
    maps.patch(
        "native-config",
        &PatchParams::default(),
        &Patch::Merge(json!({"metadata":{"annotations":{"fixture-status-change":"new-version"}}})),
    )
    .await
    .unwrap();
    let updated_manifest=Manifest::new(json!({"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":"native-config","namespace":namespace},"data":{"owner":"fixture-a-updated"}})).unwrap();
    let mut updated_spec = child.clone();
    updated_spec.manifest_digest = updated_manifest.digest().unwrap();
    let stale = store
        .prepare_resource(
            &scope,
            &recovered_claim,
            "stale-update",
            updated_spec.clone(),
        )
        .await
        .unwrap();
    maps.patch(
        "native-config",
        &PatchParams::default(),
        &Patch::Merge(json!({"metadata":{"annotations":{"fixture-second-change":"new-version"}}})),
    )
    .await
    .unwrap();
    assert!(matches!(
        driver.apply(&stale, &updated_manifest).await,
        Err(Error::PreconditionFailed)
    ));
    let updated = reconciler
        .ensure(
            &recovered_claim,
            "native-config-update",
            updated_spec,
            &updated_manifest,
        )
        .await
        .unwrap();
    assert_eq!(updated.uid, original_uid);
    let previous:serde_json::Value=sqlx::query_scalar("SELECT result FROM manager.operation_steps WHERE operation_id=$1 AND step_key='stale-update'").bind(recovered_claim.id).fetch_one(&pool).await.unwrap();
    assert_eq!(previous["status"], "reconciled");
    assert_eq!(
        maps.get("native-config").await.unwrap().data.unwrap()["owner"],
        "fixture-a-updated"
    );
    assert!(matches!(
        reconciler
            .ensure(&claim, "stale-work", child.clone(), &config_manifest)
            .await,
        Err(Error::StaleFence)
    ));
    // The same reserved name/labels and copied signed proof do not authorize a
    // different UID once the manager has bound the original resource.
    let original = maps.get("native-config").await.unwrap();
    maps.delete("native-config", &DeleteParams::default())
        .await
        .unwrap();
    for _ in 0..30 {
        if maps.get_opt("native-config").await.unwrap().is_none() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let mut replacement = original.clone();
    replacement.metadata.uid = None;
    replacement.metadata.resource_version = None;
    replacement.metadata.creation_timestamp = None;
    replacement.metadata.managed_fields = None;
    let foreign = maps
        .create(&PostParams::default(), &replacement)
        .await
        .unwrap();
    assert_ne!(foreign.metadata.uid.as_deref(), Some(original_uid.as_str()));
    let bound = store
        .prepare_resource(&scope, &recovered_claim, "native-config", child)
        .await
        .unwrap();
    assert!(matches!(
        driver.observe(&bound, &config_manifest).await,
        Err(Error::OwnershipConflict)
    ));
    assert!(matches!(
        driver.apply(&bound, &config_manifest).await,
        Err(Error::OwnershipConflict)
    ));
    // Missing authenticated provenance blocks first adoption even if every
    // intended user-visible field happens to match.
    let foreign_manifest=Manifest::new(json!({"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":"foreign-config","namespace":namespace},"data":{"owner":"fixture-a"}})).unwrap();
    let foreign:ConfigMap=serde_json::from_value(json!({"metadata":{"name":"foreign-config","namespace":namespace},"data":{"owner":"fixture-a"}})).unwrap();
    maps.create(&PostParams::default(), &foreign).await.unwrap();
    let foreign_spec = ResourceSpec {
        kind: ResourceKind::ConfigMap,
        namespace: Some(namespace.clone()),
        namespace_uid: bound.resource.creation.spec.namespace_uid.clone(),
        name: "foreign-config".into(),
        manifest_digest: foreign_manifest.digest().unwrap(),
        purpose: "foreign-config".into(),
        retention: Retention::Delete,
        encrypted_material_ref: None,
    };
    let reserved = store
        .prepare_resource(&scope, &recovered_claim, "foreign-config", foreign_spec)
        .await
        .unwrap();
    assert!(matches!(
        driver.apply(&reserved, &foreign_manifest).await,
        Err(Error::OwnershipConflict)
    ));
    let tampered_manifest=Manifest::new(json!({"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":"tampered","namespace":namespace},"data":{"owner":"fixture-a"}})).unwrap();
    let tampered_spec = ResourceSpec {
        kind: ResourceKind::ConfigMap,
        namespace: Some(namespace.clone()),
        namespace_uid: bound.resource.creation.spec.namespace_uid.clone(),
        name: "tampered".into(),
        manifest_digest: tampered_manifest.digest().unwrap(),
        purpose: "tampered".into(),
        retention: Retention::Delete,
        encrypted_material_ref: None,
    };
    let tampered = store
        .prepare_resource(&scope, &recovered_claim, "tampered", tampered_spec)
        .await
        .unwrap();
    driver.apply(&tampered, &tampered_manifest).await.unwrap();
    maps.patch(
        "tampered",
        &PatchParams::default(),
        &Patch::Merge(json!({"data":{"owner":"foreign"}})),
    )
    .await
    .unwrap();
    assert!(matches!(
        driver.observe(&tampered, &tampered_manifest).await,
        Err(Error::OwnershipConflict)
    ));
    assert!(matches!(
        driver.apply(&tampered, &tampered_manifest).await,
        Err(Error::OwnershipConflict)
    ));
    // Delete a separate owned temporary object only after the intent commits.
    let temp_manifest=Manifest::new(json!({"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":"temporary","namespace":namespace},"data":{"value":"temporary"}})).unwrap();
    let temp_spec = ResourceSpec {
        kind: ResourceKind::ConfigMap,
        namespace: Some(namespace.clone()),
        namespace_uid: bound.resource.creation.spec.namespace_uid.clone(),
        name: "temporary".into(),
        manifest_digest: temp_manifest.digest().unwrap(),
        purpose: "temporary".into(),
        retention: Retention::Delete,
        encrypted_material_ref: None,
    };
    let temp = store
        .prepare_resource(&scope, &recovered_claim, "temporary", temp_spec)
        .await
        .unwrap();
    let seen = driver.apply(&temp, &temp_manifest).await.unwrap();
    store
        .bind_resource(
            &scope,
            &recovered_claim,
            "temporary",
            observation(&seen.uid, &seen.resource_version, &seen.manifest_digest),
        )
        .await
        .unwrap();
    let retire = store
        .retire_resource(
            &scope,
            &recovered_claim,
            "retire-temporary",
            temp.resource.creation.id,
            seen,
        )
        .await
        .unwrap();
    // API-server resource-version changes defeat a stale destructive request.
    maps.patch(
        "temporary",
        &PatchParams::default(),
            &Patch::Merge(json!({"metadata":{"annotations":{"fixture-change":"new"},"finalizers":["fixture.agent-platform.io/hold"]}})),
    )
    .await
    .unwrap();
    assert_eq!(driver.delete(&retire).await, Err(Error::PreconditionFailed));
    assert!(maps.get_opt("temporary").await.unwrap().is_some());
    let latest = maps.get("temporary").await.unwrap();
    let retire = store
        .retire_resource(
            &scope,
            &recovered_claim,
            "retire-temporary-again",
            temp.resource.creation.id,
            observation(
                latest.metadata.uid.as_ref().unwrap(),
                latest.metadata.resource_version.as_ref().unwrap(),
                &temp_manifest.digest().unwrap(),
            ),
        )
        .await
        .unwrap();
    driver.delete(&retire).await.unwrap();
    assert!(
        !driver.absent(&retire).await.unwrap(),
        "finalizer must keep deletion pending"
    );
    let state: String = sqlx::query_scalar("SELECT state FROM manager.owned_resources WHERE id=$1")
        .bind(retire.resource.creation.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(state, "Retiring");
    maps.patch(
        "temporary",
        &PatchParams::default(),
        &Patch::Merge(json!({"metadata":{"finalizers":[]}})),
    )
    .await
    .unwrap();
    for _ in 0..30 {
        if driver.absent(&retire).await.unwrap() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(driver.absent(&retire).await.unwrap());
    store
        .resource_absent(&scope, &recovered_claim, "retire-temporary-again")
        .await
        .unwrap();
    let pending:i64=sqlx::query_scalar("SELECT count(*) FROM manager.operation_steps s JOIN manager.resource_steps b ON b.operation_id=s.operation_id AND b.step_key=s.step_key WHERE b.resource_id=$1 AND s.intent->>'action'='delete' AND s.verified_at IS NULL").bind(temp.resource.creation.id).fetch_one(&pool).await.unwrap();
    assert_eq!(pending, 0);
    let namespaces: Api<Namespace> = Api::all(client);
    // Cleanup is fixture administration, outside the production retention API.
    namespaces
        .delete(&namespace, &DeleteParams::default())
        .await
        .unwrap();
}
