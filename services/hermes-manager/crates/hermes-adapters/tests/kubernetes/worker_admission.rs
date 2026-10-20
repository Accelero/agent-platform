//! Real worker renderer through the unified owner Namespace admission boundary.
use hermes_adapters::{
    admission,
    execution_workloads::{self, WorkerContext},
    workloads::WorkloadConfig,
};
use hermes_application::{
    InstallationScope,
    execution::{
        BackupPolicy, PermittedMount, PullSecretBinding, ResolvedMount, ResolvedPolicy,
        ResolvedRuntime, RuntimeDefinition, RuntimeRef, StorageBinding, WorkerResources,
    },
};
use k8s_openapi::api::{
    apps::v1::Deployment,
    core::v1::{Namespace, PersistentVolumeClaim, Pod, ServiceAccount},
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
        matches!(result,Err(kube::Error::Api(error)) if error.code == 403 || error.code == 422),
        "unsafe worker was not rejected"
    );
}
fn policy(
    scope: &InstallationScope,
    instance: Uuid,
    ns: &str,
    uid: &str,
    tag: char,
) -> ResolvedPolicy {
    let mount = PermittedMount {
        path: "/workspace".into(),
        read_only: false,
        sub_path: Some("selected".into()),
    };
    let resources = WorkerResources {
        cpu_millis: 1000,
        memory_mib: 1024,
        ephemeral_storage_mib: 1024,
    };
    let definition = RuntimeDefinition {
        schema_version: 1,
        runtime: RuntimeRef {
            id: "python-tools".into(),
            version: 1,
        },
        image: format!(
            "ghcr.io/example/worker-{tag}@sha256:{}",
            tag.to_string().repeat(64)
        ),
        ssh_contract: "managed-ssh-v1".into(),
        uid: 1000,
        gid: 1000,
        service_account: "worker".into(),
        resources: resources.clone(),
        locale: "C.UTF-8".into(),
        egress: vec![],
        pull_secret_names: vec![format!("pull-{tag}")],
    };
    let runtime = ResolvedRuntime {
        definition_digest: definition.digest().unwrap(),
        definition,
        resources,
        limits: Default::default(),
        mounts: vec![ResolvedMount {
            binding: StorageBinding {
                id: Uuid::new_v4(),
                version: 1,
                instance_id: instance,
                namespace: ns.into(),
                namespace_uid: uid.into(),
                pvc_name: format!("worker-data-{tag}"),
                pvc_uid: format!("pvc-uid-{tag}"),
                backing_storage_id: Uuid::new_v4(),
                external: true,
                required: true,
                backup: BackupPolicy::External,
                mounts: vec![mount.clone()],
            },
            mount,
        }],
        pull_secrets: vec![PullSecretBinding {
            instance_id: instance,
            namespace: ns.into(),
            namespace_uid: uid.into(),
            name: format!("pull-{tag}"),
            uid: format!("pull-uid-{tag}"),
        }],
    };
    ResolvedPolicy {
        installation_id: scope.id,
        instance_id: instance,
        namespace: ns.into(),
        namespace_uid: uid.into(),
        generation: 7,
        default_runtime_id: "python-tools".into(),
        runtimes: vec![runtime],
    }
}
fn pod(policy: &ResolvedPolicy) -> Value {
    selected_pod(policy, "python-tools")
}
fn selected_pod(policy: &ResolvedPolicy, runtime_id: &str) -> Value {
    let context = WorkerContext {
        assignment_generation: 1,
        environment_id: Uuid::new_v4(),
        environment_generation: 3,
    };
    let rendered = execution_workloads::render(policy, runtime_id, &context).unwrap();
    let mut pod = serde_json::to_value(rendered.pod().unwrap()).unwrap();
    pod["metadata"]["labels"]["agent-platform.io/installation"] = json!(policy.installation_id);
    pod["metadata"]["labels"]["agent-platform.io/instance"] = json!(policy.instance_id);
    pod
}
async fn admitted(pods: &Api<Pod>, pod: &Value) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let result = pods
            .create(
                &PostParams {
                    dry_run: true,
                    ..Default::default()
                },
                &serde_json::from_value(pod.clone()).unwrap(),
            )
            .await;
        if result.is_ok() {
            break;
        }
        let code = if let Err(kube::Error::Api(error)) = &result {
            error.code
        } else {
            0
        };
        assert!(
            Instant::now() < deadline,
            "resolved worker was not admitted (HTTP{code})"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[allow(clippy::too_many_arguments)] // Both actual credentials and exact owner/policy fixture bindings.
pub(super) async fn verify(
    client: &Client,
    administrator: &Client,
    scope: &InstallationScope,
    config: &WorkloadConfig,
    ns: &str,
    uid: &str,
    instance: Uuid,
    native_pod: &Value,
    policies: &Api<DynamicObject>,
    driver_config: kube::Config,
) -> Value {
    let mut current = policy(scope, instance, ns, uid, 'a');
    bind_references(client, &mut current).await;
    let mut bare = current.runtimes[0].clone();
    bare.definition.runtime.id = "bare-tools".into();
    bare.definition.pull_secret_names.clear();
    bare.definition_digest = bare.definition.digest().unwrap();
    bare.mounts.clear();
    bare.pull_secrets.clear();
    current.runtimes.push(bare);

    let worker = pod(&current);
    let pods: Api<Pod> = Api::namespaced(client.clone(), ns);
    let dry = PostParams {
        dry_run: true,
        ..Default::default()
    };
    let accounts: Api<ServiceAccount> = Api::namespaced(client.clone(), ns);
    accounts
        .create(
            &PostParams::default(),
            &serde_json::from_value(
                json!({"metadata":{"name":"worker"},"automountServiceAccountToken":false}),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    // Native-only params cannot accidentally authorize a worker branch.
    rejected(
        pods.create(&dry, &serde_json::from_value(worker.clone()).unwrap())
            .await,
    );
    policies
        .patch(
            "instance-policy",
            &PatchParams::default(),
            &Patch::Merge(admission::worker_parameters(&current).unwrap()),
        )
        .await
        .unwrap();
    admitted(&pods, &worker).await;
    admitted(&pods, native_pod).await;
    let admin_pods: Api<Pod> = Api::namespaced(administrator.clone(), ns);
    rejected(
        admin_pods
            .create(&dry, &serde_json::from_value(worker.clone()).unwrap())
            .await,
    );
    let mut unknown = worker.clone();
    unknown["metadata"]["labels"]["app.kubernetes.io/component"] = json!("unknown");
    rejected(
        pods.create(&dry, &serde_json::from_value(unknown).unwrap())
            .await,
    );
    let mut native = worker.clone();
    native["metadata"]["labels"]["app.kubernetes.io/component"] = json!("hermes");
    rejected(
        pods.create(&dry, &serde_json::from_value(native).unwrap())
            .await,
    );
    let mutations = [
        (
            "/spec/containers/0/image",
            json!(format!(
                "ghcr.io/example/worker-b@sha256:{}",
                "b".repeat(64)
            )),
        ),
        (
            "/spec/containers/0/command",
            json!(["/bin/sh", "-c", "sleep infinity"]),
        ),
        ("/spec/containers/0/args", json!(["--unsafe"])),
        ("/spec/serviceAccountName", json!("hermes")),
        ("/spec/automountServiceAccountToken", json!(true)),
        ("/spec/securityContext/runAsUser", json!(0)),
        (
            "/spec/containers/0/securityContext/readOnlyRootFilesystem",
            json!(false),
        ),
        (
            "/spec/containers/0/securityContext/capabilities/add",
            json!(["SYS_ADMIN"]),
        ),
        (
            "/spec/containers/0/terminationMessagePath",
            json!("/workspace/private-data"),
        ),
        (
            "/spec/containers/0/readinessProbe",
            json!({"exec":{"command":["/bin/sh","-c","id"]}}),
        ),
        (
            "/spec/containers/0/env/0/valueFrom",
            json!({"secretKeyRef":{"name":"native-bootstrap","key":"execution-client-token"}}),
        ),
        (
            "/spec/containers/0/env",
            json!([{"name":"EXECUTION_TOKEN","value":"fixture-only"}]),
        ),
        ("/spec/containers/0/resources/limits/cpu", json!("64")),
        ("/spec/containers/0/volumeMounts/3/readOnly", json!(true)),
        (
            "/spec/containers/0/volumeMounts/3/subPath",
            json!("foreign"),
        ),
        (
            "/spec/volumes/3/persistentVolumeClaim/claimName",
            json!("worker-data-b"),
        ),
        (
            "/spec/volumes/2/secret/secretName",
            json!("native-bootstrap"),
        ),
        (
            "/spec/volumes/2/secret/items",
            json!([{"key":"id_ed25519","path":"worker"}]),
        ),
        ("/spec/imagePullSecrets", json!([{"name":"pull-b"}])),
        ("/spec/hostNetwork", json!(true)),
        (
            "/spec/hostAliases",
            json!([{"ip":"127.0.0.1","hostnames":["foreign.example.org"]}]),
        ),
        ("/spec/schedulingGates", Value::Null),
        ("/spec/schedulingGates", json!([])),
        (
            "/spec/schedulingGates",
            json!([{"name":"agent-platform.io/native-start"}]),
        ),
        (
            "/spec/schedulingGates",
            json!([{"name":"agent-platform.io/worker-start"},{"name":"foreign.example.org/start"}]),
        ),
        (
            "/metadata/labels/hermes.agent-platform.io~1assignment-generation",
            json!("2"),
        ),
        (
            "/metadata/labels/hermes.agent-platform.io~1execution-policy-generation",
            json!("8"),
        ),
        (
            "/metadata/labels/hermes.agent-platform.io~1environment-generation",
            json!("0"),
        ),
        (
            "/metadata/labels/hermes.agent-platform.io~1namespace-uid",
            json!("foreign-namespace"),
        ),
        (
            "/metadata/labels/hermes.agent-platform.io~1runtime-version",
            json!("2"),
        ),
        (
            "/metadata/annotations/hermes.agent-platform.io~1policy-digest",
            json!("c".repeat(64)),
        ),
        ("/metadata/name", json!("foreign-worker")),
        (
            "/metadata/ownerReferences",
            json!([{"apiVersion":"apps/v1","kind":"ReplicaSet","name":"forged","uid":"forged-uid","controller":true}]),
        ),
    ];
    for (path, value) in mutations {
        let mut bad = worker.clone();
        set(&mut bad, path, value);
        rejected(
            pods.create(&dry, &serde_json::from_value(bad).unwrap())
                .await,
        );
    }
    let deployments: Api<Deployment> = Api::namespaced(client.clone(), ns);
    rejected(deployments.create(&dry,&serde_json::from_value(json!({"metadata":{"name":"hermes"},"spec":{"replicas":1,"selector":{"matchLabels":{"app.kubernetes.io/component":"worker"}},"template":{"metadata":worker["metadata"],"spec":worker["spec"]}}})).unwrap()).await);

    // A second owner has an independent exact image, PVC and pull Secret policy.
    let namespaces: Api<Namespace> = Api::all(client.clone());
    let other_instance = Uuid::new_v4();
    let other_ns = hermes_domain::instance_namespace(scope.id, other_instance);
    let other = namespaces.create(&PostParams::default(),&serde_json::from_value(json!({"metadata":{"name":other_ns,"labels":{"agent-platform.io/installation":scope.id,"agent-platform.io/instance":other_instance,"pod-security.kubernetes.io/enforce":"restricted"}}})).unwrap()).await.unwrap();
    let bindings: Api<k8s_openapi::api::rbac::v1::RoleBinding> =
        Api::namespaced(client.clone(), &other_ns);
    bindings.create(&PostParams::default(),&serde_json::from_value(json!({"metadata":{"name":"instance-reconciler"},"roleRef":{"apiGroup":"rbac.authorization.k8s.io","kind":"ClusterRole","name":config.reconciler_role},"subjects":[{"kind":"ServiceAccount","name":config.controller_service_account,"namespace":config.control_namespace}]})).unwrap()).await.unwrap();
    let other_uid = other.metadata.uid.as_deref().unwrap();
    super::fence_admission::close_initial(client, scope, &other_ns).await;
    super::fence_admission::open(client, scope, &other_ns).await;
    let mut alternate = policy(scope, other_instance, &other_ns, other_uid, 'b');
    bind_references(client, &mut alternate).await;
    let other_worker = pod(&alternate);
    let other_accounts: Api<ServiceAccount> = Api::namespaced(client.clone(), &other_ns);
    other_accounts
        .create(
            &PostParams::default(),
            &serde_json::from_value(
                json!({"metadata":{"name":"worker"},"automountServiceAccountToken":false}),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    let resource = ApiResource::from_gvk_with_plural(
        &GroupVersionKind::gvk(admission::GROUP, "v1alpha1", "InstancePolicy"),
        "instancepolicies",
    );
    let other_policies: Api<DynamicObject> =
        Api::namespaced_with(client.clone(), &other_ns, &resource);
    let mut native_parameters =
        admission::parameters(scope, other_instance, other_uid, 1, &native_pod["spec"]).unwrap();
    native_parameters["spec"]["worker"] =
        admission::worker_parameters(&alternate).unwrap()["spec"]["worker"].clone();
    native_parameters["metadata"] = json!({"name":"instance-policy","namespace":other_ns});
    native_parameters["apiVersion"] = json!(admission::VERSION);
    native_parameters["kind"] = json!("InstancePolicy");
    other_policies
        .create(
            &PostParams::default(),
            &serde_json::from_value(native_parameters).unwrap(),
        )
        .await
        .unwrap();
    let other_pods: Api<Pod> = Api::namespaced(client.clone(), &other_ns);
    admitted(&other_pods, &other_worker).await;
    let mut copied = other_worker.clone();
    copied["metadata"]["namespace"] = json!(ns);
    rejected(
        pods.create(&dry, &serde_json::from_value(copied).unwrap())
            .await,
    );
    admitted(&pods, &worker).await;

    let worker = selected_pod(&current, "bare-tools");
    let name = worker["metadata"]["name"].as_str().unwrap();
    use hermes_adapters::{kubernetes::KubernetesDriver, ownership::OwnershipSigner};
    use hermes_application::terminal::leases::{EnvironmentClaim, WorkerIntent, WorkerStartIntent};
    let cluster = namespaces
        .get_metadata("kube-system")
        .await
        .unwrap()
        .metadata
        .uid
        .unwrap();
    let driver = KubernetesDriver::from_config(
        driver_config,
        OwnershipSigner::new(
            "worker-fixture".into(),
            std::collections::BTreeMap::from([("worker-fixture".into(), vec![42; 32])]),
        )
        .unwrap(),
        cluster,
    )
    .unwrap();
    let environment = Uuid::parse_str(
        worker["metadata"]["labels"]["hermes.agent-platform.io/environment-id"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    let context = WorkerContext {
        assignment_generation: 1,
        environment_id: environment,
        environment_generation: 3,
    };
    let rendered = execution_workloads::render(&current, "bare-tools", &context).unwrap();
    let intent = WorkerIntent {
        claim: EnvironmentClaim {
            installation_id: scope.id,
            instance_id: instance,
            environment_id: environment,
            generation: 3,
            epoch: scope.epoch,
            worker: Uuid::new_v4(),
            fence: 1,
            assignment_generation: 1,
            policy: current.clone(),
            runtime_id: "bare-tools".into(),
            pod_name: name.into(),
        },
        spec_digest: rendered.resource.spec.manifest_digest,
    };
    let binding = driver.ensure_execution_worker(&intent).await.unwrap();
    assert!(binding.gated);
    let recovered = driver.ensure_execution_worker(&intent).await.unwrap();
    assert_eq!(recovered.pod_uid, binding.pod_uid);
    assert_eq!(recovered.observed_spec_digest, binding.observed_spec_digest);
    let start = WorkerStartIntent {
        intent: intent.clone(),
        pod_uid: binding.pod_uid.clone(),
        resource_version: binding.resource_version,
        observed_spec_digest: binding.observed_spec_digest,
    };
    let mut forged = start.clone();
    forged.pod_uid = "foreign-uid".into();
    assert!(matches!(
        driver.start_execution_worker(&forged).await,
        Err(hermes_application::Error::OwnershipConflict)
    ));
    let original = pods.get(name).await.unwrap();
    let proof =
        original.metadata.annotations.as_ref().unwrap()["agent-platform.io/creation-proof"].clone();
    pods.patch(
        name,
        &PatchParams::default(),
        &Patch::Merge(
            json!({"metadata":{"annotations":{"agent-platform.io/creation-proof":"forged"}}}),
        ),
    )
    .await
    .unwrap();
    assert!(matches!(
        driver.ensure_execution_worker(&intent).await,
        Err(hermes_application::Error::OwnershipConflict)
    ));
    pods.patch(
        name,
        &PatchParams::default(),
        &Patch::Merge(
            json!({"metadata":{"annotations":{"agent-platform.io/creation-proof":proof}}}),
        ),
    )
    .await
    .unwrap();
    let patch = PatchParams {
        dry_run: true,
        ..Default::default()
    };
    let removal = json!({"spec":{"schedulingGates":null}});
    pods.patch(name, &patch, &Patch::Merge(removal.clone()))
        .await
        .unwrap();
    rejected(admin_pods.patch(name, &patch, &Patch::Merge(removal)).await);
    rejected(pods.patch(name,&patch,&Patch::Merge(json!({"metadata":{"labels":{"app.kubernetes.io/component":"hermes"}},"spec":{"schedulingGates":null}}))).await);
    rejected(
        pods.patch(
            name,
            &patch,
            &Patch::Merge(json!({"spec":{"schedulingGates":null,"activeDeadlineSeconds":60}})),
        )
        .await,
    );
    for state in ["closed", "open"] {
        namespaces
            .patch(
                ns,
                &PatchParams::default(),
                &Patch::Merge(json!({"metadata":{"labels":{"agent-platform.io/admission":state}}})),
            )
            .await
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let observed = pods
                .patch(
                    name,
                    &patch,
                    &Patch::Merge(json!({"spec":{"schedulingGates":null}})),
                )
                .await;
            let expected = if state == "closed" {
                matches!(observed,Err(kube::Error::Api(error)) if error.code == 403)
            } else {
                observed.is_ok()
            };
            if expected {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "worker gate removal ignored Namespace fence"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        if state == "closed" {
            assert!(matches!(
                driver.start_execution_worker(&start).await,
                Err(hermes_application::Error::OwnershipConflict)
            ));
        }
    }
    let claims: Api<PersistentVolumeClaim> = Api::namespaced(client.clone(), ns);
    assert!(
        claims
            .get("worker-data-a")
            .await
            .unwrap()
            .spec
            .unwrap()
            .volume_name
            .is_none()
    );
    let public_keys: Api<k8s_openapi::api::core::v1::Secret> = Api::namespaced(client.clone(), ns);
    assert!(
        public_keys
            .get_opt(execution_workloads::AUTHORIZED_KEYS)
            .await
            .unwrap()
            .is_none(),
        "mandatory missing public-key Secret must keep this fake-image worker from executing"
    );
    pods.patch(
        name,
        &PatchParams::default(),
        &Patch::Merge(
            json!({"metadata":{"annotations":{"worker-fixture":"changed-resource-version"}}}),
        ),
    )
    .await
    .unwrap();
    assert!(matches!(
        driver.start_execution_worker(&start).await,
        Err(hermes_application::Error::PreconditionFailed)
    ));
    let before = pods.get(name).await.unwrap();
    // Each observed version represents a new immutable journaled start input.
    // Keep the old record intact; PostgreSQL tests prove its persisted boundary.
    let mut starts = vec![start];
    let deadline = Instant::now() + Duration::from_secs(20);
    let removed = loop {
        let observed = driver.ensure_execution_worker(&intent).await.unwrap();
        starts.push(WorkerStartIntent {
            intent: intent.clone(),
            pod_uid: observed.pod_uid,
            resource_version: observed.resource_version,
            observed_spec_digest: observed.observed_spec_digest,
        });
        match driver.start_execution_worker(starts.last().unwrap()).await {
            Ok(binding) => break binding,
            Err(hermes_application::Error::PreconditionFailed) => assert!(
                Instant::now() < deadline,
                "worker status changes prevented a fresh committed start CAS"
            ),
            Err(_) => panic!("fresh committed worker start was not accepted"),
        }
    };
    assert_ne!(starts[0].resource_version, starts[1].resource_version);
    let start = starts.last().unwrap();
    assert!(!removed.gated);
    assert_eq!(removed.pod_uid, start.pod_uid);
    let replay = driver.start_execution_worker(start).await.unwrap();
    assert_eq!(replay.pod_uid, start.pod_uid);
    assert_eq!(replay.observed_spec_digest, start.observed_spec_digest);
    let mut observed = pods.get(name).await.unwrap();
    let mut expected = before.spec.unwrap();
    observed.spec.as_mut().unwrap().node_name = None;
    expected.node_name = None;
    expected.scheduling_gates = None;
    assert_eq!(observed.spec, Some(expected));
    assert!(
        driver
            .execution_worker_candidate(start)
            .await
            .unwrap()
            .is_none()
    );
    // Return a fresh identity for the later missing-entire-parameter proof.
    pod(&current)
}
async fn bind_references(client: &Client, policy: &mut ResolvedPolicy) {
    use k8s_openapi::api::core::v1::Secret;
    let claims: Api<PersistentVolumeClaim> = Api::namespaced(client.clone(), &policy.namespace);
    let secrets: Api<Secret> = Api::namespaced(client.clone(), &policy.namespace);
    for runtime in &mut policy.runtimes {
        for mount in &mut runtime.mounts {
            let claim = claims.create(&PostParams::default(),&serde_json::from_value(json!({"metadata":{"name":mount.binding.pvc_name},"spec":{"storageClassName":"","accessModes":["ReadWriteOnce"],"resources":{"requests":{"storage":"1Gi"}}}})).unwrap()).await.unwrap();
            mount.binding.pvc_uid = claim.metadata.uid.unwrap();
        }
        for pull in &mut runtime.pull_secrets {
            let secret = secrets.create(&PostParams::default(),&serde_json::from_value(json!({"metadata":{"name":pull.name},"type":"kubernetes.io/dockerconfigjson","data":{".dockerconfigjson":"eyJhdXRocyI6e319"}})).unwrap()).await.unwrap();
            pull.uid = secret.metadata.uid.unwrap();
        }
    }
}

fn set(value: &mut Value, path: &str, replacement: Value) {
    let mut parts = path
        .trim_start_matches('/')
        .split('/')
        .map(|p| p.replace("~1", "/").replace("~0", "~"))
        .peekable();
    let mut cursor = value;
    while let Some(part) = parts.next() {
        if parts.peek().is_none() {
            if let Some(array) = cursor.as_array_mut() {
                array[part.parse::<usize>().unwrap()] = replacement;
            } else {
                cursor[part] = replacement;
            }
            return;
        }
        cursor = if cursor.is_array() {
            &mut cursor[part.parse::<usize>().unwrap()]
        } else {
            &mut cursor[part]
        };
    }
}
