//! Live constrained transport: desired database rows -> create/update/delete.
use hermes_adapters::{controller::Controller, postgres::PgStore, workloads::WorkloadConfig};
use hermes_application::InstallationScope;
use k8s_openapi::api::{
    apps::v1::Deployment,
    core::v1::{ConfigMap, PersistentVolumeClaim, Secret},
};
use kube::{
    Api, Client, Config,
    api::{Patch, PatchParams},
};
use serde_json::json;
use sqlx::PgPool;
use std::time::{Duration, Instant};
use uuid::Uuid;

async fn converge(controller: &Controller, pool: &PgPool, expected_image: Option<&str>) {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        controller.step().await.unwrap();
        let pending: i64 = if let Some(image) = expected_image {
            sqlx::query_scalar("SELECT count(*) FROM manager.assignments a JOIN manager.instances i ON i.assignment_id=a.id WHERE a.infrastructure_managed AND NOT EXISTS(SELECT 1 FROM manager.owned_resources r WHERE r.instance_id=i.id AND r.kind='Deployment' AND r.state='Bound' AND EXISTS(SELECT 1 FROM manager.operation_steps s WHERE s.operation_id IN (SELECT id FROM manager.operations WHERE instance_id=i.id AND generation=i.desired_generation) AND s.intent->>'name'='hermes' AND s.result->>'status'='verified')) AND EXISTS(SELECT 1 FROM manager.instance_templates t WHERE t.id=a.template_id AND t.version=a.template_version AND t.image=$1)")
                .bind(image).fetch_one(pool).await.unwrap()
        } else {
            sqlx::query_scalar("SELECT count(*) FROM manager.instances i JOIN manager.assignments a ON a.id=i.assignment_id WHERE a.infrastructure_managed AND a.access='revoked' AND i.state<>'Retained'")
                .fetch_one(pool).await.unwrap()
        };
        if pending == 0 {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "Infrastructure convergence deadline exceeded"
        );
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

pub(super) async fn verify(
    pool: &PgPool,
    scope: &InstallationScope,
    config: Config,
    cluster: &str,
    workload: &WorkloadConfig,
) {
    let client = Client::try_from(config.clone()).unwrap();
    let store = PgStore::connect_with(pool.connect_options().as_ref().clone())
        .await
        .unwrap();
    let image = format!("ghcr.io/example/hermes@sha256:{}", "b".repeat(64));
    let definition = json!({"schemaVersion":1,"image":image,"storageClass":"local-path","homeGib":1,"workspaceGib":1,"cpuMillis":1000,"memoryMib":1024,"adminConfig":{"agent":{"max_turns":12}}});
    sqlx::query("INSERT INTO manager.infrastructure_templates(installation_id,id,definition) VALUES($1,'infra-personal',$2)")
        .bind(scope.id)
        .bind(definition)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO manager.desired_users VALUES($1,'infra-owner-a','infra-personal'),($1,'infra-owner-b','infra-personal')").bind(scope.id).execute(pool).await.unwrap();
    let worker = super::controller::controller(
        store.clone(),
        scope,
        config.clone(),
        cluster,
        workload.clone(),
    );
    converge(&worker, pool, Some(&image)).await;
    let instances: Vec<(Uuid,String)> = sqlx::query_as("SELECT i.id,i.namespace FROM manager.instances i JOIN manager.assignments a ON a.id=i.assignment_id JOIN manager.identities k ON k.id=a.identity_id WHERE a.infrastructure_managed ORDER BY k.subject").fetch_all(pool).await.unwrap();
    assert_eq!(instances.len(), 2);
    let namespace = &instances[0].1;
    let deployments: Api<Deployment> = Api::namespaced(client.clone(), namespace);
    let secrets: Api<Secret> = Api::namespaced(client.clone(), namespace);
    let claims: Api<PersistentVolumeClaim> = Api::namespaced(client.clone(), namespace);
    let maps: Api<ConfigMap> = Api::namespaced(client.clone(), namespace);
    let original = deployments.get("hermes").await.unwrap();
    let credentials = secrets.get("native-bootstrap").await.unwrap();
    let home = claims.get("home").await.unwrap();
    let other: Api<Secret> = Api::namespaced(client.clone(), &instances[1].1);
    assert!(credentials.data != other.get("native-bootstrap").await.unwrap().data);
    let changed_image = format!("ghcr.io/example/hermes@sha256:{}", "c".repeat(64));
    sqlx::query("UPDATE manager.infrastructure_templates SET definition=jsonb_set(jsonb_set(definition,'{image}',to_jsonb($2::text)),'{adminConfig,agent,max_turns}','24') WHERE installation_id=$1")
        .bind(scope.id).bind(&changed_image).execute(pool).await.unwrap();
    let restarted = super::controller::controller(store, scope, config, cluster, workload.clone());
    converge(&restarted, pool, Some(&changed_image)).await;
    let updated = deployments.get("hermes").await.unwrap();
    assert_eq!(updated.metadata.uid, original.metadata.uid);
    assert_eq!(
        updated.spec.unwrap().template.spec.unwrap().containers[0]
            .image
            .as_deref(),
        Some(changed_image.as_str())
    );
    let managed = maps.get("native-trust").await.unwrap();
    let managed: serde_json::Value =
        serde_json::from_str(&managed.data.unwrap()["config.yaml"]).unwrap();
    assert_eq!(managed["agent"]["max_turns"], 24);
    deployments
        .patch(
            "hermes",
            &PatchParams::default(),
            &Patch::Merge(json!({"metadata":{"finalizers":["fixture.example.org/retain"]}})),
        )
        .await
        .unwrap();
    sqlx::query(
        "DELETE FROM manager.desired_users WHERE installation_id=$1 AND subject='infra-owner-a'",
    )
    .bind(scope.id)
    .execute(pool)
    .await
    .unwrap();
    // Perform the SQL transition first; the loop must reconstruct retirement
    // from durable records, with no dependency on an in-memory user object.
    let deadline = Instant::now() + Duration::from_secs(60);
    let terminating = loop {
        restarted.step().await.unwrap();
        let deployment = deployments.get("hermes").await.unwrap();
        if deployment.metadata.deletion_timestamp.is_some() {
            break deployment;
        }
        assert!(
            Instant::now() < deadline,
            "retirement did not reach finalizer barrier"
        );
        tokio::time::sleep(Duration::from_millis(150)).await;
    };
    let mut changed_containers = terminating
        .spec
        .as_ref()
        .unwrap()
        .template
        .spec
        .as_ref()
        .unwrap()
        .containers
        .clone();
    changed_containers[0].image = Some(image.clone());
    let denied = deployments
        .patch(
            "hermes",
            &PatchParams::default(),
            &Patch::Merge(json!({"spec":{"template":{"spec":{"containers":changed_containers}}}})),
        )
        .await;
    assert!(matches!(denied,Err(kube::Error::Api(response)) if response.code==403));
    let remaining: Vec<_> = terminating
        .metadata
        .finalizers
        .unwrap()
        .into_iter()
        .filter(|f| f != "fixture.example.org/retain")
        .collect();
    deployments.patch("hermes",&PatchParams::default(),&Patch::Merge(json!({"metadata":{"uid":terminating.metadata.uid,"resourceVersion":terminating.metadata.resource_version,"finalizers":remaining}}))).await.unwrap();
    converge(&restarted, pool, None).await;
    assert!(deployments.get_opt("hermes").await.unwrap().is_none());
    assert_eq!(
        claims.get("home").await.unwrap().metadata.uid,
        home.metadata.uid
    );
    assert!(secrets.get("native-bootstrap").await.unwrap().data == credentials.data);
    sqlx::query("INSERT INTO manager.desired_users VALUES($1,'infra-owner-a','infra-personal')")
        .bind(scope.id)
        .execute(pool)
        .await
        .unwrap();
    converge(&restarted, pool, Some(&changed_image)).await;
    assert_ne!(
        deployments.get("hermes").await.unwrap().metadata.uid,
        original.metadata.uid
    );
    assert_eq!(
        claims.get("home").await.unwrap().metadata.uid,
        home.metadata.uid
    );
    assert!(secrets.get("native-bootstrap").await.unwrap().data == credentials.data);
    let other: Api<Deployment> = Api::namespaced(client, &instances[1].1);
    assert_eq!(
        other.get("hermes").await.unwrap().spec.unwrap().replicas,
        Some(1)
    );
}
