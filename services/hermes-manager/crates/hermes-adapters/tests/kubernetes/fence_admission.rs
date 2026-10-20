//! Real Namespace fence, Pod start and parameter-free scale admission proofs.
use hermes_application::{
    InstallationScope,
    namespace_fence::{ACCESS, ADMISSION, EPOCH, GENERATION},
};
use k8s_openapi::api::{
    apps::v1::Deployment,
    core::v1::{Namespace, PersistentVolumeClaim, Pod},
};
use kube::{
    Api, Client,
    api::{Patch, PatchParams, PostParams},
};
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use uuid::Uuid;

fn denied<T>(result: Result<T, kube::Error>) {
    assert!(
        matches!(result,Err(kube::Error::Api(error)) if error.code==403 || error.code==422),
        "unsafe lifecycle update was not denied"
    );
}
fn labels(scope: &InstallationScope, generation: i64, access: &str, admission: &str) -> Value {
    json!({(ACCESS):access,(ADMISSION):admission,(GENERATION):generation.to_string(),(EPOCH):scope.epoch.to_string()})
}
pub(super) async fn close_initial(client: &Client, scope: &InstallationScope, ns: &str) {
    let namespaces: Api<Namespace> = Api::all(client.clone());
    let dry = PatchParams {
        dry_run: true,
        ..Default::default()
    };
    // Neither an active nor a prepopulated closed fence may be supplied at
    // Namespace creation: the first live fence is a separate owned-UID action.
    for admission in ["closed", "open"] {
        let instance = Uuid::new_v4();
        let mut owner = labels(scope, 1, "enabled", admission);
        owner["agent-platform.io/installation"] = json!(scope.id);
        owner["agent-platform.io/instance"] = json!(instance);
        owner["pod-security.kubernetes.io/enforce"] = json!("restricted");
        denied(namespaces.create(&PostParams {dry_run:true,..Default::default()},&serde_json::from_value(json!({"metadata":{"name":hermes_domain::instance_namespace(scope.id,instance),"labels":owner}})).unwrap()).await);
    }
    denied(
        namespaces
            .patch(
                ns,
                &dry,
                &Patch::Merge(json!({"metadata":{"labels":labels(scope,1,"enabled","open")}})),
            )
            .await,
    );
    denied(namespaces.patch(ns,&dry,&Patch::Merge(json!({"metadata":{"labels":{(ACCESS):"enabled",(ADMISSION):"closed",(GENERATION):"1"}}}))).await);
    namespaces
        .patch(
            ns,
            &PatchParams::default(),
            &Patch::Merge(json!({"metadata":{"labels":labels(scope,1,"enabled","closed")}})),
        )
        .await
        .unwrap();
}
pub(super) async fn open(client: &Client, scope: &InstallationScope, ns: &str) {
    let namespaces: Api<Namespace> = Api::all(client.clone());
    namespaces
        .patch(
            ns,
            &PatchParams::default(),
            &Patch::Merge(json!({"metadata":{"labels":labels(scope,1,"enabled","open")}})),
        )
        .await
        .unwrap();
    let dry = PatchParams {
        dry_run: true,
        ..Default::default()
    };
    // Idempotence keeps current healthy native Pods running during resync.
    namespaces
        .patch(
            ns,
            &dry,
            &Patch::Merge(json!({"metadata":{"labels":labels(scope,1,"enabled","open")}})),
        )
        .await
        .unwrap();
    for fields in [
        json!({(ACCESS):null,(ADMISSION):null,(GENERATION):null,(EPOCH):null}),
        json!({(EPOCH):null}),
        json!({(GENERATION):"0"}),
        json!({(GENERATION):"01"}),
        json!({(ACCESS):"revoked"}),
        labels(scope, 2, "enabled", "open"),
        json!({(EPOCH):(scope.epoch+1).to_string(),(ADMISSION):"open"}),
    ] {
        denied(
            namespaces
                .patch(
                    ns,
                    &dry,
                    &Patch::Merge(json!({"metadata":{"labels":fields}})),
                )
                .await,
        );
    }
}

pub(super) async fn pod_updates(client: &Client, administrator: &Client, ns: &str, pod: &Value) {
    let pods: Api<Pod> = Api::namespaced(client.clone(), ns);
    let foreign: Api<Pod> = Api::namespaced(administrator.clone(), ns);
    // This real Pod has no backing storage in this unregistered fixture
    // Namespace. It cannot schedule even after the authorized gate removal.
    let name = "native-gate-fixture";
    let mut gated = pod.clone();
    gated["metadata"]["name"] = json!(name);
    pods.create(
        &PostParams::default(),
        &serde_json::from_value(gated).unwrap(),
    )
    .await
    .unwrap();
    let dry = PatchParams {
        dry_run: true,
        ..Default::default()
    };
    let removal = json!({"spec":{"schedulingGates":null}});
    pods.patch(name, &dry, &Patch::Merge(removal.clone()))
        .await
        .unwrap();
    denied(foreign.patch(name, &dry, &Patch::Merge(removal)).await);
    for spec in [
        json!({"schedulingGates":null,"activeDeadlineSeconds":60}),
        json!({"schedulingGates":null,"overhead":{"cpu":"1","memory":"1Gi"}}),
        json!({"schedulingGates":null,"nodeSelector":{"kubernetes.io/hostname":"foreign-node"}}),
        json!({"schedulingGates":[{"name":"foreign.example.org/start"}]}),
    ] {
        denied(
            pods.patch(name, &dry, &Patch::Merge(json!({"spec":spec})))
                .await,
        );
    }
    let namespaces: Api<Namespace> = Api::all(client.clone());
    for admission in ["closed", "open"] {
        namespaces
            .patch(
                ns,
                &PatchParams::default(),
                &Patch::Merge(json!({"metadata":{"labels":{(ADMISSION):admission}}})),
            )
            .await
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let result = pods
                .patch(
                    name,
                    &dry,
                    &Patch::Merge(json!({"spec":{"schedulingGates":null}})),
                )
                .await;
            let expected = if admission == "closed" {
                matches!(result, Err(kube::Error::Api(error)) if error.code == 403)
            } else {
                result.is_ok()
            };
            if expected {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "native gate removal did not follow Namespace admission state"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    let claims: Api<PersistentVolumeClaim> = Api::namespaced(client.clone(), ns);
    for claim in ["home", "workspace"] {
        assert!(claims.get_opt(claim).await.unwrap().is_none());
    }
    let before = pods.get(name).await.unwrap();
    let removed = pods
        .patch(
            name,
            &PatchParams::default(),
            &Patch::Merge(json!({"metadata":{"uid":before.metadata.uid,"resourceVersion":before.metadata.resource_version},"spec":{"schedulingGates":null}})),
        )
        .await
        .unwrap();
    assert_eq!(removed.metadata.uid, before.metadata.uid);
    assert!(removed.spec.as_ref().unwrap().scheduling_gates.is_none());
    let mut before = before.spec.unwrap();
    before.scheduling_gates = None;
    assert_eq!(removed.spec.as_ref(), Some(&before));
}

pub(super) async fn stop_without_parameters(
    client: &Client,
    administrator: &Client,
    scope: &InstallationScope,
    ns: &str,
    deployment: &Value,
    policies: &Api<kube::core::DynamicObject>,
    worker: &Value,
) {
    let deployments: Api<Deployment> = Api::namespaced(client.clone(), ns);
    let foreign: Api<Deployment> = Api::namespaced(administrator.clone(), ns);
    let namespaces: Api<Namespace> = Api::all(client.clone());
    let dry = PatchParams {
        dry_run: true,
        ..Default::default()
    };
    let deadline = Instant::now() + Duration::from_secs(20);
    let created = loop {
        match deployments
            .create(
                &PostParams::default(),
                &serde_json::from_value(deployment.clone()).unwrap(),
            )
            .await
        {
            Ok(created) => break created,
            Err(error) => {
                assert!(
                    Instant::now() < deadline,
                    "current rendered Deployment was not admitted: {error:?}"
                );
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    };
    let name = "hermes";
    let uid = created.metadata.uid.as_ref().unwrap();
    denied(
        deployments
            .patch_scale(name, &dry, &Patch::Merge(json!({"spec":{"replicas":0}})))
            .await,
    );
    namespaces
        .patch(
            ns,
            &PatchParams::default(),
            &Patch::Merge(json!({"metadata":{"labels":labels(scope,1,"enabled","closed")}})),
        )
        .await
        .unwrap();
    policies
        .delete("instance-policy", &Default::default())
        .await
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        // A replica-one metadata update is valid with the native parameter.
        // Its denial proves the API server now observes that parameter missing.
        let result = deployments
            .patch(
                name,
                &dry,
                &Patch::Merge(
                    json!({"metadata":{"annotations":{"fixture-observation":"missing-parameter"}}}),
                ),
            )
            .await;
        if matches!(&result,Err(kube::Error::Api(error)) if error.code==422 && error.message.contains("no params found for policy binding"))
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "missing InstancePolicy did not produce the expected fail-closed denial"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let workers: Api<Pod> = Api::namespaced(client.clone(), ns);
    assert!(
        matches!(workers.create(&PostParams{dry_run:true,..Default::default()}, &serde_json::from_value(worker.clone()).unwrap()).await,
        Err(kube::Error::Api(error)) if error.code == 422 && error.message.contains("no params found for policy binding"))
    );
    // The main Deployment VAP retains missing-parameter Deny; only its separate
    // scale subresource can stop the unchanged parent without that parameter.
    denied(
        deployments
            .patch(name, &dry, &Patch::Merge(json!({"spec":{"replicas":0}})))
            .await,
    );
    denied(
        deployments
            .patch_scale(name, &dry, &Patch::Merge(json!({"spec":{"replicas":1}})))
            .await,
    );
    denied(
        foreign
            .patch_scale(name, &dry, &Patch::Merge(json!({"spec":{"replicas":0}})))
            .await,
    );
    denied(
        deployments
            .patch_scale(
                "foreign-hermes",
                &dry,
                &Patch::Merge(json!({"spec":{"replicas":0}})),
            )
            .await,
    );
    let scale = deployments.get_scale(name).await.unwrap();
    assert_eq!(scale.metadata.uid.as_ref(), Some(uid));
    let rv = scale.metadata.resource_version.as_ref().unwrap();
    for meta in [
        json!({"uid":"foreign-uid","resourceVersion":rv}),
        json!({"uid":uid,"resourceVersion":"1"}),
    ] {
        assert!(
            matches!(deployments.patch_scale(name,&dry,&Patch::Merge(json!({"metadata":meta,"spec":{"replicas":0}}))).await,Err(kube::Error::Api(error)) if error.code==409),
            "scale UID/RV precondition was not enforced"
        );
    }
    let before = deployments.get(name).await.unwrap();
    let commit = PatchParams::default();
    for params in [&dry, &commit] {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let scale = deployments.get_scale(name).await.unwrap();
            let result=deployments.patch_scale(name,params,&Patch::Merge(json!({"metadata":{"uid":uid,"resourceVersion":scale.metadata.resource_version},"spec":{"replicas":0}}))).await;
            if result.is_ok() {
                break;
            }
            let diagnostic = match &result {
                Err(kube::Error::Api(error)) => format!(
                    "status={}, missing_parameter={}, native_policy={}, stop_policy={}",
                    error.code,
                    error.message.contains("no params found for policy binding"),
                    error.message.contains("-deployments'"),
                    error.message.contains("-scale-stop'"),
                ),
                _ => "transport failure".into(),
            };
            assert!(
                Instant::now() < deadline,
                "closed owned scale-zero update failed without InstancePolicy: {diagnostic}"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        if params.dry_run {
            assert_eq!(
                deployments.get(name).await.unwrap().spec.unwrap().replicas,
                Some(1)
            );
            // Admission sees the merged Scale, not this patch payload. Omitting
            // replicas while the parent is still at one must not authorize it.
            denied(deployments.patch_scale(name, &dry, &Patch::Merge(json!({"metadata":{"annotations":{"fixture-observation":"unchanged-one"}}}))).await);
        }
    }
    let stopped_scale = deployments.get_scale(name).await.unwrap();
    assert_eq!(
        stopped_scale.spec.and_then(|spec| spec.replicas),
        None,
        "typed Scale zero was not omitted by the real API server"
    );
    let after = deployments.get(name).await.unwrap();
    assert_eq!(after.metadata.uid, before.metadata.uid);
    let mut before = serde_json::to_value(before.spec.unwrap()).unwrap();
    let after = serde_json::to_value(after.spec.unwrap()).unwrap();
    before["replicas"] = json!(0);
    assert_eq!(
        before, after,
        "scale modified more than the parent's replica count"
    );
    namespaces
        .patch(
            ns,
            &PatchParams::default(),
            &Patch::Merge(json!({"metadata":{"labels":labels(scope,2,"revoked","closed")}})),
        )
        .await
        .unwrap();
    denied(
        namespaces
            .patch(
                ns,
                &dry,
                &Patch::Merge(json!({"metadata":{"labels":labels(scope,1,"enabled","closed")}})),
            )
            .await,
    );
    denied(
        namespaces
            .patch(
                ns,
                &dry,
                &Patch::Merge(json!({"metadata":{"labels":labels(scope,2,"enabled","closed")}})),
            )
            .await,
    );
    let mut future = scope.clone();
    future.epoch += 1;
    namespaces
        .patch(
            ns,
            &PatchParams::default(),
            &Patch::Merge(json!({"metadata":{"labels":labels(&future,2,"revoked","closed")}})),
        )
        .await
        .unwrap();
    denied(
        namespaces
            .patch(
                ns,
                &dry,
                &Patch::Merge(json!({"metadata":{"labels":labels(scope,2,"revoked","closed")}})),
            )
            .await,
    );
}
