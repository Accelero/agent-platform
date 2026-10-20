//! PostgreSQL outbox -> actual constrained Kubernetes commissioning, then reopen.
use hermes_adapters::{
    controller::Controller,
    kubernetes::KubernetesDriver,
    materials::MaterialVault,
    ownership::OwnershipSigner,
    postgres::{FreshInventory, PgStore},
    session_crypto::Sealer,
    workloads::{TemplateDefinition, WorkloadConfig},
};
use hermes_application::{
    AdminPolicy, AssignmentCommand, Assignments, Error, InstallationScope, VerifiedPrincipal,
};
use hermes_domain::{Access, Identity, TemplateVersion};
use k8s_openapi::api::{
    apps::v1::Deployment,
    core::v1::{Namespace, Secret},
};
use kube::{Api, Client, Config, api::ListParams};
use sqlx::PgPool;
use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};
use uuid::Uuid;

fn controller(
    store: PgStore,
    scope: &InstallationScope,
    config: Config,
    cluster: &str,
    workload: WorkloadConfig,
) -> Controller {
    let driver = KubernetesDriver::from_config(
        config,
        OwnershipSigner::new(
            "fixture".into(),
            BTreeMap::from([("fixture".into(), vec![41; 32])]),
        )
        .unwrap(),
        cluster.into(),
    )
    .unwrap();
    let vault = MaterialVault::new(
        store.clone(),
        Sealer::new(
            "fixture".into(),
            BTreeMap::from([("fixture".into(), vec![42; 32])]),
        )
        .unwrap(),
    );
    Controller::new(
        store,
        driver,
        vault,
        scope.clone(),
        workload,
        "https://manager.example.org/execution".into(),
    )
    .unwrap()
}
async fn reconcile(controller: &Controller, pool: &PgPool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        assert!(controller.step().await.unwrap());
        let state: String = sqlx::query_scalar("SELECT state FROM manager.operations")
            .fetch_one(pool)
            .await
            .unwrap();
        if state == "Waiting" {
            break;
        }
        if state != "RetryAt" || Instant::now() >= deadline {
            let resources: Vec<(String, String)> = sqlx::query_as(
                "SELECT kind,state FROM manager.owned_resources ORDER BY created_at,id",
            )
            .fetch_all(pool)
            .await
            .unwrap();
            let reason: Option<serde_json::Value> =
                sqlx::query_scalar("SELECT last_condition FROM manager.operations")
                    .fetch_one(pool)
                    .await
                    .unwrap();
            panic!(
                "commissioning state={state}, condition={reason:?}, resource states={resources:?}"
            );
        }
        sqlx::query(
            "UPDATE manager.operations SET next_attempt_at=clock_timestamp()-interval '1 second'",
        )
        .execute(pool)
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
pub(super) async fn verify(
    pool: &PgPool,
    scope: &InstallationScope,
    config: Config,
    workload: &WorkloadConfig,
) {
    let client = Client::try_from(config.clone()).unwrap();
    let namespaces: Api<Namespace> = Api::all(client.clone());
    let existing = namespaces
        .list(
            &ListParams::default().labels(&format!("agent-platform.io/installation={}", scope.id)),
        )
        .await
        .unwrap();
    assert!(
        existing.items.is_empty(),
        "fresh fixture must have no existing instance namespaces"
    );
    let cluster = namespaces
        .get_metadata("kube-system")
        .await
        .unwrap()
        .metadata
        .uid
        .unwrap();
    let store = PgStore::connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    store
        .initialize_fresh(
            scope,
            FreshInventory {
                cluster_identity: cluster.clone(),
                digest: "a".repeat(64),
                registered_instances: 0,
                unexplained_resources: 0,
            },
        )
        .await
        .unwrap();
    let definition = TemplateDefinition {
        schema_version: 1,
        image: format!("ghcr.io/example/hermes@sha256:{}", "a".repeat(64)),
        storage_class: "local-path".into(),
        home_gib: 1,
        workspace_gib: 1,
        cpu_millis: 1000,
        memory_mib: 1024,
    };
    sqlx::query("INSERT INTO manager.instance_templates(id,version,schema_version,image,content_digest,approved) VALUES('personal-default',1,1,$1,$2,true)").bind(&definition.image).bind(definition.digest().unwrap()).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO manager.template_definitions(template_id,template_version,definition) VALUES('personal-default',1,$1)").bind(serde_json::to_value(&definition).unwrap()).execute(pool).await.unwrap();
    let policy = AdminPolicy {
        issuer: scope.issuer.clone(),
        audience: "manager-api".into(),
        management_client: "manager-admin".into(),
        administrator_role: "administrator".into(),
    };
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
    let assignments = Assignments::new(store.clone(), scope.clone(), policy).unwrap();
    let accepted = assignments
        .execute(
            &actor,
            Uuid::new_v4(),
            "commission",
            AssignmentCommand::Create {
                identity: Identity {
                    issuer: scope.issuer.clone(),
                    subject: "owner-controller-fixture".into(),
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
    let worker = controller(
        store.clone(),
        scope,
        config.clone(),
        &cluster,
        workload.clone(),
    );
    let wrong_cluster = controller(
        store.clone(),
        scope,
        config.clone(),
        "different-cluster",
        workload.clone(),
    );
    assert!(matches!(
        wrong_cluster.verify_startup().await,
        Err(Error::OwnershipConflict)
    ));
    worker.verify_startup().await.unwrap();
    reconcile(&worker, pool).await;
    let ns = hermes_domain::instance_namespace(scope.id, accepted.instance_id);
    let secrets: Api<Secret> = Api::namespaced(client.clone(), &ns);
    let original = secrets.get("native-bootstrap").await.unwrap();
    let deployments: Api<Deployment> = Api::namespaced(client.clone(), &ns);
    let original_deployment = deployments.get("hermes").await.unwrap();
    assert_eq!(original_deployment.spec.as_ref().unwrap().replicas, Some(1));
    let route_count: i64 = sqlx::query_scalar("SELECT count(*) FROM manager.instance_routes")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(
        route_count, 0,
        "created resources are not a readiness proof"
    );
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM manager.owned_resources WHERE state='Bound'")
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(count, 14);
    let snapshots: i64 = sqlx::query_scalar("SELECT count(*) FROM manager.bootstrap_materials")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(snapshots, 3);
    let mut changed = workload.clone();
    changed.public_origin = "https://changed.example.org".into();
    let reopened = PgStore::connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    let restarted = controller(reopened, scope, config.clone(), &cluster, changed);
    restarted.verify_startup().await.unwrap();
    sqlx::query(
        "UPDATE manager.operations SET next_attempt_at=clock_timestamp()-interval '1 second'",
    )
    .execute(pool)
    .await
    .unwrap();
    reconcile(&restarted, pool).await;
    let current = secrets.get("native-bootstrap").await.unwrap();
    assert!(
        current.data == original.data,
        "reopening changed committed owner credentials"
    );
    assert_eq!(current.metadata.uid, original.metadata.uid);
    let current = deployments.get("hermes").await.unwrap();
    assert_eq!(current.metadata.uid, original_deployment.metadata.uid);
    assert_eq!(
        current
            .spec
            .as_ref()
            .unwrap()
            .template
            .spec
            .as_ref()
            .unwrap()
            .containers[0]
            .env,
        original_deployment
            .spec
            .as_ref()
            .unwrap()
            .template
            .spec
            .as_ref()
            .unwrap()
            .containers[0]
            .env
    );
    let snapshots_after: i64 =
        sqlx::query_scalar("SELECT count(*) FROM manager.bootstrap_materials")
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(snapshots_after, snapshots);
    let audit: i64 = sqlx::query_scalar("SELECT count(*) FROM manager.audit_events")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(audit, 1);
    super::process::verify(pool, scope, config, workload, &cluster).await;
    // Use the same constrained controller, not administrator transport, for the
    // entire disable path. All state is retained while execution drain remains
    // an explicit unfinished barrier.
    let disabled = assignments
        .execute(
            &actor,
            Uuid::new_v4(),
            "revoke-controller",
            AssignmentCommand::SetAccess {
                assignment_id: accepted.assignment_id,
                expected_version: accepted.version,
                access: Access::Revoked,
            },
        )
        .await
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        restarted.step().await.unwrap();
        let condition: Option<serde_json::Value> =
            sqlx::query_scalar("SELECT last_condition FROM manager.operations WHERE id=$1")
                .bind(disabled.operation_id)
                .fetch_one(pool)
                .await
                .unwrap();
        if condition
            .as_ref()
            .is_some_and(|v| v["reason"] == "draining")
        {
            break;
        }
        if Instant::now() >= deadline {
            let steps:Vec<(String,String,String)>=sqlx::query_as("SELECT intent->>'action',intent->>'name',result->>'status' FROM manager.operation_steps WHERE operation_id=$1 ORDER BY recorded_at")
                .bind(disabled.operation_id).fetch_all(pool).await.unwrap();
            panic!("retained suspension did not reach native stop: {condition:?}, steps={steps:?}");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let stopped = deployments.get("hermes").await.unwrap();
    assert_eq!(stopped.metadata.uid, original_deployment.metadata.uid);
    assert_eq!(stopped.spec.as_ref().unwrap().replicas, Some(0));
    assert_eq!(
        stopped.spec.as_ref().unwrap().template,
        original_deployment.spec.as_ref().unwrap().template
    );
    let retained = secrets.get("native-bootstrap").await.unwrap();
    assert_eq!(retained.metadata.uid, original.metadata.uid);
    assert!(retained.data == original.data);
    let ns = namespaces.get(&ns).await.unwrap();
    let labels = ns.metadata.labels.unwrap();
    assert_eq!(labels["agent-platform.io/access"], "revoked");
    assert_eq!(labels["agent-platform.io/admission"], "closed");
    assert_eq!(labels["agent-platform.io/fence-generation"], "2");
    let (resources,draining):(i64,bool)=sqlx::query_as("SELECT (SELECT count(*) FROM manager.owned_resources WHERE instance_id=$1),drain_completed_at IS NULL FROM manager.instances WHERE id=$1")
        .bind(accepted.instance_id).fetch_one(pool).await.unwrap();
    assert_eq!(resources, 14);
    assert!(draining);
}
