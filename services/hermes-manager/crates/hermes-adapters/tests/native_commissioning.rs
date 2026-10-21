//! Run inside the disposable cluster, never against a deployment kubecontext.
use hermes_adapters::{
    bootstrap::BootstrapPolicy, controller::Controller, kubernetes::KubernetesDriver,
    materials::MaterialVault, ownership::OwnershipSigner, postgres::PgStore,
    session_crypto::Sealer, workloads::WorkloadConfig,
};
use hermes_application::InstallationScope;
use k8s_openapi::api::{
    apps::v1::Deployment,
    authentication::v1::{TokenRequest, TokenRequestSpec},
    core::v1::{ConfigMap, Namespace, PersistentVolumeClaim, ServiceAccount},
};
use kube::{
    Api, Client, Config,
    api::{Patch, PatchParams, PostParams},
    core::{ApiResource, DynamicObject, GroupVersionKind},
};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};
use uuid::Uuid;
#[path = "native_commissioning/terminal.rs"]
mod terminal;

fn api(client: &Client, manifest: &Value) -> Api<DynamicObject> {
    let kind = manifest["kind"].as_str().unwrap();
    let plural = match kind {
        "ClusterRole" => "clusterroles",
        "ClusterRoleBinding" => "clusterrolebindings",
        "ValidatingAdmissionPolicy" => "validatingadmissionpolicies",
        "ValidatingAdmissionPolicyBinding" => "validatingadmissionpolicybindings",
        "CustomResourceDefinition" => "customresourcedefinitions",
        _ => panic!("unexpected bootstrap kind"),
    };
    let (group, version) = manifest["apiVersion"]
        .as_str()
        .unwrap()
        .split_once('/')
        .unwrap();
    Api::all_with(
        client.clone(),
        &ApiResource::from_gvk_with_plural(&GroupVersionKind::gvk(group, version, kind), plural),
    )
}

fn worker(
    store: PgStore,
    scope: &InstallationScope,
    config: Config,
    cluster: &str,
    workload: &WorkloadConfig,
) -> Controller {
    let driver = KubernetesDriver::from_config(
        config,
        OwnershipSigner::new(
            "fixture".into(),
            BTreeMap::from([("fixture".into(), vec![51; 32])]),
        )
        .unwrap(),
        cluster.into(),
    )
    .unwrap();
    let vault = MaterialVault::new(
        store.clone(),
        Sealer::new(
            "fixture".into(),
            BTreeMap::from([("fixture".into(), vec![52; 32])]),
        )
        .unwrap(),
    );
    Controller::new(
        store,
        driver,
        vault,
        scope.clone(),
        workload.clone(),
        std::env::var("HERMES_FIXTURE_EXECUTION_URL").unwrap(),
    )
    .unwrap()
}

async fn drive(worker: &Controller, pool: &PgPool, ready: i64, retained: i64) {
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        worker.step().await.unwrap();
        let counts:(i64,i64)=sqlx::query_as("SELECT count(*) FILTER(WHERE i.state='Ready' AND i.observed_generation=i.desired_generation),count(*) FILTER(WHERE i.state='Retained' AND i.observed_generation=i.desired_generation) FROM manager.instances i").fetch_one(pool).await.unwrap();
        let active:i64=sqlx::query_scalar("SELECT count(*) FROM manager.operations WHERE state IN ('Pending','Running','Waiting','RetryAt')").fetch_one(pool).await.unwrap();
        if counts == (ready, retained) && active == 0 {
            return;
        }
        if Instant::now() >= deadline {
            let states:Vec<(String,Option<Value>)>=sqlx::query_as("SELECT state,last_condition FROM manager.operations WHERE state NOT IN ('Succeeded','Superseded')").fetch_all(pool).await.unwrap();
            panic!("native commissioning did not converge: {states:?}");
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

async fn stage(maps: &Api<ConfigMap>, name: &str, namespace_a: &str, namespace_b: &str) {
    maps.patch(
        "fixture-progress",
        &PatchParams::default(),
        &Patch::Merge(
            json!({"data":{"stage":name,"namespaceA":namespace_a,"namespaceB":namespace_b}}),
        ),
    )
    .await
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let current = maps.get("fixture-progress").await.unwrap();
        if current.data.unwrap()["stage"] == format!("{name}-checked") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "fixture did not acknowledge stage {name}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
#[ignore = "requires the dedicated native commissioning fixture Pod"]
async fn real_native_instances_converge_from_database_and_preserve_owner_state(pool: PgPool) {
    assert_eq!(
        std::env::var("HERMES_NATIVE_COMMISSIONING_FIXTURE").as_deref(),
        Ok("isolated")
    );
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut config = Config::incluster().unwrap();
    config.default_retry = false;
    let administrator = Client::try_from(config.clone()).unwrap();
    let namespaces: Api<Namespace> = Api::all(administrator.clone());
    let cluster = namespaces
        .get("kube-system")
        .await
        .unwrap()
        .metadata
        .uid
        .unwrap();
    let control = std::env::var("FIXTURE_CONTROL_NAMESPACE").unwrap();
    assert!(control.starts_with("native-commissioning-"));
    let scope = InstallationScope {
        id: Uuid::new_v4(),
        epoch: 1,
        issuer: "https://sso.example.org/realms/agents".into(),
    };
    let store = PgStore::connect_with(pool.connect_options().as_ref().clone())
        .await
        .unwrap();
    let inventory_driver = KubernetesDriver::from_config(
        config.clone(),
        OwnershipSigner::new(
            "fixture".into(),
            BTreeMap::from([("fixture".into(), vec![51; 32])]),
        )
        .unwrap(),
        cluster.clone(),
    )
    .unwrap();
    let inventory = inventory_driver.fresh_inventory(&scope).await.unwrap();
    store.initialize_fresh(&scope, inventory).await.unwrap();
    let workload = WorkloadConfig {
        control_namespace: control.clone(),
        controller_service_account: "hermes-controller".into(),
        reconciler_role: BootstrapPolicy::reconciler_role(&scope),
        public_origin: "https://hermes.example.org".into(),
        oidc_client: "native-hermes".into(),
        execution_port: 8443,
        execution_ca_pem: std::fs::read_to_string("/fixture-tls/server.crt").unwrap(),
        oidc_ca_pem: None,
        service_egress: vec![hermes_adapters::workloads::ServiceEgress {
            namespace: control.clone(),
            labels: BTreeMap::from([(
                "app.kubernetes.io/component".into(),
                "hermes-controller".into(),
            )]),
            port: 8443,
        }],
        public_https: false,
    };
    let accounts: Api<ServiceAccount> = Api::namespaced(administrator.clone(), &control);
    accounts.create(&PostParams::default(),&serde_json::from_value(json!({"metadata":{"name":"hermes-controller"},"automountServiceAccountToken":false})).unwrap()).await.unwrap();
    let policy = BootstrapPolicy::new(&scope, &workload).unwrap();
    for manifest in policy.manifests() {
        let api = api(&administrator, manifest);
        if manifest["kind"] == "CustomResourceDefinition"
            && api
                .get_opt(manifest["metadata"]["name"].as_str().unwrap())
                .await
                .unwrap()
                .is_some()
        {
            continue;
        }
        api.create(
            &PostParams::default(),
            &serde_json::from_value(manifest.clone()).unwrap(),
        )
        .await
        .unwrap();
    }
    let token = accounts
        .create_token_request(
            "hermes-controller",
            &PostParams::default(),
            &TokenRequest {
                spec: Some(TokenRequestSpec {
                    expiration_seconds: Some(1200),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .status
        .unwrap()
        .token
        .unwrap();
    config.auth_info = Default::default();
    config.auth_info.token = Some(token.into());
    let controller = worker(store.clone(), &scope, config.clone(), &cluster, &workload);
    let listener = tokio::net::TcpListener::bind("0.0.0.0:8443").await.unwrap();
    let tls = hermes_adapters::server::tls_config(
        &std::fs::read("/fixture-tls/server.crt").unwrap(),
        &std::fs::read("/fixture-tls/server.key").unwrap(),
        None,
    )
    .unwrap();
    let router =
        hermes_adapters::execution_http::execution_router(store.clone(), scope.clone()).unwrap();
    let api_server = tokio::spawn(hermes_adapters::server::serve_until(
        listener,
        tls,
        Default::default(),
        router,
        std::future::pending::<()>(),
    ));
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if controller.verify_startup().await.is_ok() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "native fixture bootstrap unavailable"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let image = std::env::var("HERMES_NATIVE_FIXTURE_IMAGE").unwrap();
    let image_updated = std::env::var("HERMES_NATIVE_FIXTURE_UPDATED_IMAGE").unwrap();
    assert_ne!(image, image_updated);
    let definition = json!({"schemaVersion":1,"image":image,"storageClass":"local-path","homeGib":1,"workspaceGib":1,"cpuMillis":500,"memoryMib":1024,"adminConfig":{"agent":{"max_turns":12}}});
    sqlx::query("INSERT INTO manager.infrastructure_templates(installation_id,id,definition) VALUES($1,'native',$2)").bind(scope.id).bind(definition).execute(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO manager.desired_users VALUES($1,'owner-a','native'),($1,'owner-b','native')",
    )
    .bind(scope.id)
    .execute(&pool)
    .await
    .unwrap();
    drive(&controller, &pool, 2, 0).await;
    let bindings:Vec<(Uuid,String)>=sqlx::query_as("SELECT i.id,i.namespace FROM manager.instances i JOIN manager.assignments a ON a.id=i.assignment_id JOIN manager.identities k ON k.id=a.identity_id ORDER BY k.subject").fetch_all(&pool).await.unwrap();
    let namespace_a = &bindings[0].1;
    let namespace_b = &bindings[1].1;
    let deployments: Api<Deployment> = Api::namespaced(administrator.clone(), namespace_a);
    let claims: Api<PersistentVolumeClaim> = Api::namespaced(administrator.clone(), namespace_a);
    let original = deployments.get("hermes").await.unwrap();
    let home = claims.get("home").await.unwrap();
    let progress: Api<ConfigMap> = Api::namespaced(administrator, &control);
    stage(&progress, "ready", namespace_a, namespace_b).await;
    sqlx::query("UPDATE manager.infrastructure_templates SET definition=jsonb_set(jsonb_set(definition,'{image}',to_jsonb($2::text)),'{adminConfig,agent,max_turns}','24') WHERE installation_id=$1").bind(scope.id).bind(&image_updated).execute(&pool).await.unwrap();
    let reopened = PgStore::connect_with(pool.connect_options().as_ref().clone())
        .await
        .unwrap();
    let restarted = worker(reopened, &scope, config, &cluster, &workload);
    // The desired generation is ingested by the controller, then the same
    // native workload UID is updated and independently observed as ready.
    restarted.step().await.unwrap();
    drive(&restarted, &pool, 2, 0).await;
    let updated = deployments.get("hermes").await.unwrap();
    assert_eq!(updated.metadata.uid, original.metadata.uid);
    assert_eq!(
        updated.spec.unwrap().template.spec.unwrap().containers[0]
            .image
            .as_deref(),
        Some(image_updated.as_str())
    );
    stage(&progress, "updated", namespace_a, namespace_b).await;
    sqlx::query(
        "UPDATE manager.instances SET reconcile_after=clock_timestamp()-interval '1 second'",
    )
    .execute(&pool)
    .await
    .unwrap();
    restarted.step().await.unwrap();
    drive(&restarted, &pool, 2, 0).await;
    stage(&progress, "restarted", namespace_a, namespace_b).await;
    sqlx::query("DELETE FROM manager.desired_users WHERE installation_id=$1 AND subject='owner-a'")
        .bind(scope.id)
        .execute(&pool)
        .await
        .unwrap();
    restarted.step().await.unwrap();
    drive(&restarted, &pool, 1, 1).await;
    assert!(deployments.get_opt("hermes").await.unwrap().is_none());
    assert_eq!(
        claims.get("home").await.unwrap().metadata.uid,
        home.metadata.uid
    );
    stage(&progress, "deleted", namespace_a, namespace_b).await;
    sqlx::query("INSERT INTO manager.desired_users VALUES($1,'owner-a','native')")
        .bind(scope.id)
        .execute(&pool)
        .await
        .unwrap();
    restarted.step().await.unwrap();
    drive(&restarted, &pool, 2, 0).await;
    assert_ne!(
        deployments.get("hermes").await.unwrap().metadata.uid,
        original.metadata.uid
    );
    assert_eq!(
        claims.get("home").await.unwrap().metadata.uid,
        home.metadata.uid
    );
    stage(&progress, "readded", namespace_a, namespace_b).await;
    terminal::verify(&pool, &store, &scope, &restarted, &progress, &bindings).await;
    api_server.abort();
}
