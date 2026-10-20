//! Exercise the actual renderer through live admission, not a duplicate Pod spec.
use hermes_adapters::{
    admission,
    native_bootstrap::NativeBootstrap,
    workloads::{self, TemplateDefinition, WorkloadConfig},
};
use hermes_application::{AssignmentView, InstallationScope, materials::MaterialContext};
use hermes_domain::{Access, Identity, TemplateVersion, resources::ResourceKind};
use k8s_openapi::api::{
    apps::v1::Deployment,
    core::v1::{Namespace, Pod, ServiceAccount},
};
use kube::{
    Api, Client,
    api::{Patch, PatchParams, PostParams},
    core::{ApiResource, DynamicObject, GroupVersionKind},
};
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use uuid::Uuid;

fn rejected<T>(result: Result<T, kube::Error>) {
    assert!(
        matches!(result,Err(kube::Error::Api(error)) if error.code==403 || error.code==422),
        "unsafe workload was not rejected by admission"
    );
}
pub(super) async fn verify(
    client: &Client,
    administrator: &Client,
    scope: &InstallationScope,
    config: &WorkloadConfig,
    namespace: &Namespace,
) {
    let ns = namespace.metadata.name.as_deref().unwrap();
    let uid = namespace.metadata.uid.as_deref().unwrap();
    super::fence_admission::close_initial(client, scope, ns).await;
    let instance =
        Uuid::parse_str(&namespace.metadata.labels.as_ref().unwrap()["agent-platform.io/instance"])
            .unwrap();
    let template = TemplateVersion {
        id: "personal-default".into(),
        version: 1,
    };
    let assignment = AssignmentView {
        id: Uuid::new_v4(),
        identity: Identity {
            issuer: scope.issuer.clone(),
            subject: "fixture-owner".into(),
        },
        access: Access::Enabled,
        template: template.clone(),
        version: 1,
        instance_id: instance,
        namespace: ns.into(),
        desired_generation: 1,
        observed_generation: 0,
        state: "Pending".into(),
    };
    let material = MaterialContext {
        id: Uuid::new_v4(),
        installation_id: scope.id,
        instance_id: instance,
        epoch: scope.epoch,
        operation_id: Uuid::new_v4(),
        generation: 1,
        purpose: hermes_adapters::native_bootstrap::PURPOSE.into(),
    };
    let definition = TemplateDefinition {
        schema_version: 1,
        image: format!("ghcr.io/example/hermes@sha256:{}", "a".repeat(64)),
        storage_class: "local-path".into(),
        home_gib: 5,
        workspace_gib: 10,
        cpu_millis: 1000,
        memory_mib: 1024,
    };
    let bootstrap =
        NativeBootstrap::generate(template, "https://manager.example.org/execution").unwrap();
    let rendered = workloads::render(
        scope,
        &assignment,
        uid,
        &definition,
        config,
        &material,
        &bootstrap,
    )
    .unwrap();
    let deployment = rendered
        .iter()
        .find(|r| r.spec.kind == ResourceKind::Deployment)
        .unwrap()
        .deployment()
        .unwrap();
    let deployment = serde_json::to_value(deployment).unwrap();
    let mut pod = deployment["spec"]["template"].clone();
    pod["apiVersion"] = json!("v1");
    pod["kind"] = json!("Pod");
    pod["metadata"]["name"] = json!("native-fixture");
    pod["metadata"]["namespace"] = json!(ns);
    let deployments: Api<Deployment> = Api::namespaced(client.clone(), ns);
    let pods: Api<Pod> = Api::namespaced(client.clone(), ns);
    let dry = PostParams {
        dry_run: true,
        ..PostParams::default()
    };
    // No default or cross-namespace parameter can stand in for this owner.
    rejected(
        deployments
            .create(&dry, &serde_json::from_value(deployment.clone()).unwrap())
            .await,
    );
    let resource = ApiResource::from_gvk_with_plural(
        &GroupVersionKind::gvk(admission::GROUP, "v1alpha1", "InstancePolicy"),
        "instancepolicies",
    );
    let policies: Api<DynamicObject> = Api::namespaced_with(client.clone(), ns, &resource);
    let mut parameters = admission::parameters(scope, instance, uid, 1, &pod["spec"]).unwrap();
    parameters["apiVersion"] = json!(admission::VERSION);
    parameters["kind"] = json!("InstancePolicy");
    parameters["metadata"] = json!({"name":"instance-policy","namespace":ns});
    policies
        .create(
            &PostParams::default(),
            &serde_json::from_value(parameters.clone()).unwrap(),
        )
        .await
        .unwrap();
    let accounts: Api<ServiceAccount> = Api::namespaced(client.clone(), ns);
    accounts
        .create(
            &PostParams::default(),
            &serde_json::from_value(
                json!({"metadata":{"name":"hermes"},"automountServiceAccountToken":false}),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let result = deployments
            .create(&dry, &serde_json::from_value(deployment.clone()).unwrap())
            .await;
        if result.is_ok() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "approved native Deployment was not admitted: {result:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // Deployment staging is allowed while closed, but no Pod can start there.
    rejected(
        pods.create(&dry, &serde_json::from_value(pod.clone()).unwrap())
            .await,
    );
    super::fence_admission::open(client, scope, ns).await;
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if pods
            .create(&dry, &serde_json::from_value(pod.clone()).unwrap())
            .await
            .is_ok()
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "open owned Namespace did not admit gated native Pod"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    for gates in [
        Value::Null,
        json!([]),
        json!([{"name":"foreign.example.org/start"}]),
        json!([
            {"name":"agent-platform.io/native-start"},
            {"name":"agent-platform.io/native-start"}
        ]),
        json!([
            {"name":"agent-platform.io/native-start"},
            {"name":"foreign.example.org/start"}
        ]),
    ] {
        let mut bad = pod.clone();
        bad["spec"]["schedulingGates"] = gates;
        rejected(
            pods.create(&dry, &serde_json::from_value(bad).unwrap())
                .await,
        );
    }
    super::fence_admission::pod_updates(client, administrator, ns, &pod).await;
    let mut duplicated_environment = pod["spec"]["containers"][0]["env"].clone();
    duplicated_environment[1] = duplicated_environment[0].clone();
    let mutations = [
        (
            "/containers/0/image",
            json!(format!("ghcr.io/example/foreign@sha256:{}", "a".repeat(64))),
        ),
        (
            "/initContainers/0/image",
            json!(format!("ghcr.io/example/foreign@sha256:{}", "a".repeat(64))),
        ),
        ("/serviceAccountName", json!("worker")),
        ("/automountServiceAccountToken", json!(true)),
        ("/hostNetwork", json!(true)),
        ("/imagePullSecrets", json!([{"name":"foreign-registry"}])),
        ("/securityContext/runAsUser", json!(0)),
        (
            "/containers/0/securityContext/allowPrivilegeEscalation",
            json!(true),
        ),
        ("/containers/0/securityContext/runAsUser", json!(0)),
        (
            "/containers/0/command",
            json!(["/bin/sh", "-c", "sleep infinity"]),
        ),
        ("/containers/0/env/0/value", json!("/somewhere-else")),
        ("/containers/0/env", duplicated_environment),
        (
            "/containers/0/readinessProbe",
            json!({"exec":{"command":["/bin/sh","-c","id"]},"periodSeconds":5}),
        ),
        (
            "/containers/0/startupProbe",
            json!({"tcpSocket":{"port":"dashboard","host":"foreign.example.org"},"periodSeconds":5,"failureThreshold":60}),
        ),
        ("/initContainers/0/restartPolicy", json!("Always")),
        (
            "/initContainers/0/terminationMessagePath",
            json!("/opt/data/forged-receipt"),
        ),
        (
            "/containers/0/terminationMessagePath",
            json!("/run/hermes/credentials/execution-client-token"),
        ),
        (
            "/containers/0/terminationMessagePolicy",
            json!("FallbackToLogsOnError"),
        ),
        ("/securityContext/supplementalGroups", json!([0])),
        ("/priorityClassName", json!("system-cluster-critical")),
        ("/priority", json!(2_000_000_000)),
        ("/preemptionPolicy", json!("Never")),
        ("/runtimeClassName", json!("foreign-runtime")),
        (
            "/resources",
            json!({"requests":{"cpu":"8","memory":"8Gi"},"limits":{"cpu":"8","memory":"8Gi"}}),
        ),
        ("/containers/0/resources/limits/cpu", json!("8")),
        (
            "/volumes/0/persistentVolumeClaim/claimName",
            json!("foreign-home"),
        ),
        ("/volumes/3/configMap/name", json!("foreign-owner")),
        ("/volumes/6/secret/secretName", json!("foreign-bootstrap")),
        (
            "/volumes/6/secret/items",
            json!([{"key":"init-key","path":"init-key"}]),
        ),
        ("/containers/0/volumeMounts/0/mountPath", json!("/host")),
        ("/containers/0/volumeMounts/5/name", json!("init")),
        ("/containers/0/volumeMounts/5/subPath", json!("init-key")),
    ];
    for (path, value) in mutations {
        let mut spec = pod["spec"].clone();
        set(&mut spec, path, value);
        let mut bad_pod = pod.clone();
        bad_pod["spec"] = spec.clone();
        rejected(
            pods.create(&dry, &serde_json::from_value(bad_pod).unwrap())
                .await,
        );
        let mut bad_deployment = deployment.clone();
        bad_deployment["spec"]["template"]["spec"] = spec;
        rejected(
            deployments
                .create(&dry, &serde_json::from_value(bad_deployment).unwrap())
                .await,
        );
    }
    let mut stale = pod.clone();
    stale["metadata"]["labels"]["agent-platform.io/policy-generation"] = json!("2");
    rejected(
        pods.create(&dry, &serde_json::from_value(stale).unwrap())
            .await,
    );
    let mut copied = pod.clone();
    copied["metadata"]["labels"]["agent-platform.io/instance"] = json!(Uuid::new_v4());
    rejected(
        pods.create(&dry, &serde_json::from_value(copied).unwrap())
            .await,
    );
    policies
        .patch(
            "instance-policy",
            &PatchParams::default(),
            &Patch::Merge(json!({"spec":{"namespaceUID":"foreign-namespace-uid"}})),
        )
        .await
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if pods
            .create(&dry, &serde_json::from_value(pod.clone()).unwrap())
            .await
            .is_err()
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "foreign namespace UID did not revoke admission"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    policies
        .patch(
            "instance-policy",
            &PatchParams::default(),
            &Patch::Merge(json!({"spec":{"namespaceUID":uid}})),
        )
        .await
        .unwrap();
    super::fence_admission::stop_without_parameters(
        client,
        administrator,
        scope,
        ns,
        &deployment,
        &policies,
    )
    .await;
}

fn set(value: &mut Value, path: &str, replacement: Value) {
    let (parent, key) = path.rsplit_once('/').unwrap();
    value.pointer_mut(parent).unwrap()[key] = replacement;
}
