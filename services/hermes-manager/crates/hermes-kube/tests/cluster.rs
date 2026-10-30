//! The cluster adapter against a disposable Kubernetes API server (k3s),
//! with the rendered RBAC and admission policies installed and the
//! controller acting as its real ServiceAccount.
//!
//! Needs `HERMES_TEST_KUBECONFIG`; run through
//! `tests/personal_hermes/kubernetes.py --workspace next`, never against a
//! real cluster.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use hermes_core::app::controller::{
    ApplyRequest, Cluster, ClusterError, Controller, DesiredInstance, InstanceStore, Plain,
};
use hermes_core::app::faults::NoFaults;
use hermes_core::app::testing::contracts::{bob_assignment, set_access};
use hermes_core::app::testing::fakes::{FakeNative, FakeVault};
use hermes_core::app::testing::memory::MemStore;
use hermes_core::app::testing::{Inspect, settings};
use hermes_core::domain::assignments::Access;
use hermes_core::domain::instances::{Apply, ClaimPhase, Phase, Precondition, Workload};
use hermes_core::domain::materials::MaterialPurpose;
use hermes_core::domain::orphans::OrphanKind;
use hermes_kube::LeaderElection;
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::{ConfigMap, Namespace, PersistentVolume, Pod, Secret};
use k8s_openapi::api::rbac::v1::RoleBinding;
use kube::api::{Api, PostParams};
use serde_json::json;
use uuid::Uuid;

#[allow(dead_code)]
mod support;

use support::{Fixture, desired, diagnose, materials};

#[tokio::test]
#[ignore = "needs HERMES_TEST_KUBECONFIG from the disposable Kubernetes fixture"]
async fn dynamic_claims_bind_when_the_scheduler_places_the_hermes_pod() {
    let fixture = Fixture::new().await;
    let cluster = fixture.cluster();
    let mut desired = desired(&fixture);
    desired.definition.mounts.remove("shared");
    desired.volumes.remove("shared");
    let materials = materials();
    cluster
        .apply(&ApplyRequest {
            desired: &desired,
            apply: Apply::Everything(Workload::Run),
            materials: Some(&materials),
            secrets: &[],
        })
        .await
        .unwrap();
    // With WaitForFirstConsumer the scheduler annotates each claim with its
    // node; the installation's policies must let the cluster do that.
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    loop {
        let claims = cluster.observe(&desired).await.unwrap().observed.claims;
        if claims.len() == 2 && claims.values().all(|phase| *phase == ClaimPhase::Bound) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "claims never bound: {claims:?}"
        );
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

fn refused<T: std::fmt::Debug>(result: Result<T, kube::Error>, what: &str) {
    match result {
        Err(kube::Error::Api(status)) if matches!(status.code, 400 | 403 | 422) => {}
        other => panic!("{what} was not refused: {other:?}"),
    }
}

#[tokio::test]
#[ignore = "needs HERMES_TEST_KUBECONFIG from the disposable Kubernetes fixture"]
async fn the_controller_applies_an_instance_idempotently_under_admission() {
    let fixture = Fixture::new().await;
    let cluster = fixture.cluster();
    let desired = desired(&fixture);
    let materials = materials();
    let request = |apply| ApplyRequest {
        desired: &desired,
        apply,
        materials: Some(&materials),
        secrets: &[],
    };
    diagnose(&fixture, &desired, &materials).await;
    cluster
        .apply(&request(Apply::Everything(Workload::Run)))
        .await
        .unwrap();
    let view = cluster.observe(&desired).await.unwrap();
    let namespace = view.observed.namespace.clone().unwrap();
    assert!(namespace.owned && !namespace.terminating);
    let deployment = view.observed.deployment.clone().unwrap();
    assert_eq!(
        (deployment.replicas, deployment.applied_generation),
        (1, Some(1))
    );
    assert!(
        view.observed.claims.contains_key("hermes-home")
            && view.observed.claims.contains_key("shared")
    );
    // The controller's ServiceAccount lists what the orphan scan compares,
    // and its own objects carry their instance, so none is an orphan.
    let owned = cluster.owned_objects().await.unwrap();
    let mine = |kind: OrphanKind, name: &str| {
        owned
            .iter()
            .any(|o| o.kind == kind && o.name == name && o.instance == Some(desired.instance_id))
    };
    assert!(mine(OrphanKind::Namespace, &desired.namespace), "{owned:?}");
    assert!(
        mine(
            OrphanKind::PersistentVolume,
            &hermes_render::instance::volume_name(&desired.namespace, "shared")
        ),
        "{owned:?}"
    );
    let recorded = BTreeMap::from([(desired.instance_id, desired.namespace.clone())]);
    assert_eq!(
        hermes_core::domain::orphans::orphans(&owned, &recorded),
        vec![]
    );

    let deployments: Api<Deployment> = Api::namespaced(fixture.admin.clone(), &desired.namespace);
    let configs: Api<ConfigMap> = Api::namespaced(fixture.admin.clone(), &desired.namespace);
    let before = (
        deployments.get("hermes").await.unwrap().metadata.generation,
        configs
            .get("hermes-trust")
            .await
            .unwrap()
            .metadata
            .resource_version,
    );
    cluster
        .apply(&request(Apply::Everything(Workload::Run)))
        .await
        .unwrap();
    let after = (
        deployments.get("hermes").await.unwrap().metadata.generation,
        configs
            .get("hermes-trust")
            .await
            .unwrap()
            .metadata
            .resource_version,
    );
    assert_eq!(
        before, after,
        "re-applying unchanged objects changes nothing"
    );

    cluster
        .apply(&request(Apply::Scale(Workload::Stop)))
        .await
        .unwrap();
    let stopped = cluster
        .observe(&desired)
        .await
        .unwrap()
        .observed
        .deployment
        .unwrap();
    assert_eq!(stopped.replicas, 0);
    let stale = Precondition {
        uid: stopped.uid.clone(),
        resource_version: deployment.resource_version.clone(),
    };
    assert_eq!(
        cluster.delete_deployment(&desired.namespace, &stale).await,
        Err(ClusterError::Conflict)
    );
    // The Deployment controller may still update status after the scale
    // change; like the controller, re-observe and retry on a conflict.
    let mut deleted = false;
    for _ in 0..40 {
        let Some(current) = cluster.observe(&desired).await.unwrap().observed.deployment else {
            deleted = true;
            break;
        };
        let fresh = Precondition {
            uid: current.uid,
            resource_version: current.resource_version,
        };
        match cluster.delete_deployment(&desired.namespace, &fresh).await {
            Ok(()) | Err(ClusterError::Conflict) => {}
            Err(error) => panic!("delete failed: {error}"),
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    assert!(deleted, "the Deployment was deleted");
    assert!(
        cluster
            .observe(&desired)
            .await
            .unwrap()
            .observed
            .deployment
            .is_none()
    );
    assert!(
        cluster
            .observe(&desired)
            .await
            .unwrap()
            .observed
            .claims
            .contains_key("hermes-home"),
        "claims are kept"
    );
    assert_eq!(
        hermes_kube::owned_namespaces(&fixture.controller, fixture.installation().id)
            .await
            .unwrap(),
        vec![desired.namespace.clone()]
    );
    assert!(
        !hermes_kube::cluster_identity(&fixture.controller)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
#[ignore = "needs HERMES_TEST_KUBECONFIG from the disposable Kubernetes fixture"]
async fn admission_and_rbac_refuse_what_the_controller_must_never_write() {
    let fixture = Fixture::new().await;
    let cluster = fixture.cluster();
    let desired = desired(&fixture);
    let materials = materials();
    cluster
        .apply(&ApplyRequest {
            desired: &desired,
            apply: Apply::Everything(Workload::Run),
            materials: Some(&materials),
            secrets: &[],
        })
        .await
        .unwrap();
    let ns = desired.namespace.as_str();
    let controller = &fixture.controller;
    let post = PostParams::default();

    let namespaces: Api<Namespace> = Api::all(controller.clone());
    refused(
        namespaces
            .create(&post, &serde_json::from_value(json!({"metadata": {"name": format!("not-ours-{}", &Uuid::new_v4().simple().to_string()[..8])}})).unwrap())
            .await,
        "a namespace outside the prefix",
    );
    refused(
        namespaces.delete(ns, &Default::default()).await,
        "deleting an instance namespace",
    );

    let pods: Api<Pod> = Api::namespaced(controller.clone(), ns);
    let pod = |spec: serde_json::Value| -> Pod {
        serde_json::from_value(json!({"metadata": {"name": format!("probe-{}", &Uuid::new_v4().simple().to_string()[..8]),
            "labels": {"agent-platform.io/installation": fixture.installation().id.to_string(), "app.kubernetes.io/managed-by": "hermes-manager"}},
            "spec": spec}))
        .unwrap()
    };
    let safe = json!({
        "automountServiceAccountToken": false,
        "securityContext": {"runAsNonRoot": true, "runAsUser": 1000, "runAsGroup": 1000, "seccompProfile": {"type": "RuntimeDefault"}},
        "containers": [{"name": "c", "image": format!("example.org/x@sha256:{}", "b".repeat(64)),
            "securityContext": {"allowPrivilegeEscalation": false, "capabilities": {"drop": ["ALL"]}}}],
    });
    let mut host_path = safe.clone();
    host_path["volumes"] = json!([{"name": "host", "hostPath": {"path": "/"}}]);
    refused(pods.create(&post, &pod(host_path)).await, "a hostPath Pod");
    let mut tagged = safe.clone();
    tagged["containers"][0]["image"] = json!("example.org/x:latest");
    refused(pods.create(&post, &pod(tagged)).await, "an unpinned image");
    let mut root = safe.clone();
    root["securityContext"]["runAsUser"] = json!(0);
    refused(pods.create(&post, &pod(root)).await, "a root Pod");
    let mut token = safe.clone();
    token["automountServiceAccountToken"] = json!(true);
    refused(pods.create(&post, &pod(token)).await, "a Pod with a token");

    let volumes: Api<PersistentVolume> = Api::all(controller.clone());
    let volume = |spec: serde_json::Value| -> PersistentVolume {
        serde_json::from_value(json!({"metadata": {"name": format!("{ns}-{}", &Uuid::new_v4().simple().to_string()[..6])}, "spec": spec})).unwrap()
    };
    refused(
        volumes
            .create(&post, &volume(json!({"capacity": {"storage": "1Gi"}, "accessModes": ["ReadWriteOnce"], "persistentVolumeReclaimPolicy": "Retain",
                "storageClassName": "", "claimRef": {"namespace": ns, "name": "x"}, "local": {"path": "/etc"},
                "nodeAffinity": {"required": {"nodeSelectorTerms": [{"matchExpressions": [{"key": "kubernetes.io/hostname", "operator": "In", "values": ["server-1"]}]}]}}})))
            .await,
        "a volume outside the approved roots",
    );
    refused(
        volumes
            .create(&post, &volume(json!({"capacity": {"storage": "1Gi"}, "accessModes": ["ReadWriteMany"], "persistentVolumeReclaimPolicy": "Delete",
                "storageClassName": "", "claimRef": {"namespace": ns, "name": "x"}, "nfs": {"server": "nas.internal", "path": "/export/homes/x"}})))
            .await,
        "a volume that is not retained",
    );

    let bindings: Api<RoleBinding> = Api::namespaced(controller.clone(), ns);
    refused(
        bindings
            .create(&post, &serde_json::from_value(json!({"metadata": {"name": "escalate"},
                "roleRef": {"apiGroup": "rbac.authorization.k8s.io", "kind": "ClusterRole", "name": "cluster-admin"},
                "subjects": [{"kind": "ServiceAccount", "name": "hermes-controller", "namespace": fixture.config.control_namespace}]})).unwrap())
            .await,
        "binding cluster-admin",
    );
    let elsewhere: Api<ConfigMap> = Api::namespaced(controller.clone(), "default");
    refused(
        elsewhere
            .create(
                &post,
                &serde_json::from_value(json!({"metadata": {"name": "x"}})).unwrap(),
            )
            .await,
        "writing outside instance namespaces",
    );
    let admin_secrets: Api<Secret> = Api::namespaced(fixture.admin.clone(), ns);
    refused(
        admin_secrets
            .create(
                &post,
                &serde_json::from_value(
                    json!({"metadata": {"name": "planted"}, "stringData": {"k": "v"}}),
                )
                .unwrap(),
            )
            .await,
        "an administrator planting a Secret in an instance namespace",
    );
}

#[tokio::test]
#[ignore = "needs HERMES_TEST_KUBECONFIG from the disposable Kubernetes fixture"]
async fn leader_election_has_one_holder_at_a_time() {
    let fixture = Fixture::new().await;
    let namespace = &fixture.config.control_namespace;
    let first = LeaderElection::new(fixture.controller.clone(), namespace, "leader", "first");
    let second = LeaderElection::new(fixture.controller.clone(), namespace, "leader", "second");
    assert!(first.try_lead().await.unwrap());
    assert!(!second.try_lead().await.unwrap());
    assert!(first.try_lead().await.unwrap(), "the holder renews");
    first.release().await.unwrap();
    assert!(
        second.try_lead().await.unwrap(),
        "a released lease is taken at once"
    );
    assert!(!first.try_lead().await.unwrap());
}

#[tokio::test]
#[ignore = "needs HERMES_TEST_KUBECONFIG from the disposable Kubernetes fixture"]
async fn the_controller_commissions_and_stops_an_instance_through_the_real_api() {
    // The contract helpers commission under the fixture installation.
    let fixture = Fixture::with(hermes_core::app::testing::installation()).await;
    let store = MemStore::new(fixture.installation().clone());
    let receipt = bob_assignment(&store).await;
    let controller = Controller {
        store: store.clone(),
        cluster: fixture.cluster(),
        vault: FakeVault,
        native: FakeNative::default(),
        settings: Arc::new(hermes_core::app::settings::Settings {
            installation: fixture.installation().clone(),
            ..(*settings()).clone()
        }),
        faults: Arc::new(NoFaults),
    };
    let epoch = store.advance_epoch().await.unwrap();
    let first = controller
        .reconcile(epoch, receipt.instance_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.plan.status.phase, Phase::Provisioning);
    controller
        .reconcile(epoch, receipt.instance_id)
        .await
        .unwrap();
    let desired = store
        .desired_instance(receipt.instance_id)
        .await
        .unwrap()
        .unwrap();
    let namespaces: Api<Namespace> = Api::all(fixture.admin.clone());
    assert_eq!(
        namespaces
            .get(&desired.namespace)
            .await
            .unwrap()
            .metadata
            .uid,
        desired.namespace_uid,
        "bound on first sight"
    );

    set_access(&store, &receipt, Access::Suspended).await;
    let mut phase = Phase::Provisioning;
    for _ in 0..60 {
        phase = controller
            .reconcile(epoch, receipt.instance_id)
            .await
            .unwrap()
            .unwrap()
            .plan
            .status
            .phase;
        if phase == Phase::Retained {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert_eq!(phase, Phase::Retained);
    let (recorded, _, observed) = store.instance_status(receipt.instance_id).await;
    assert_eq!((recorded.as_str(), observed), ("retained", 2));
    let deployments: Api<Deployment> = Api::namespaced(fixture.admin.clone(), &desired.namespace);
    assert!(deployments.get_opt("hermes").await.unwrap().is_none());
}

/// Runs a command in a Pod as the cluster administrator, through the
/// disposable server's own kubectl.
fn exec_in(pod: &str, namespace: &str, script: &str, input: &str) -> (bool, String) {
    use std::io::Write;
    let container = std::env::var("HERMES_TEST_K3S_CONTAINER").expect("k3s container");
    let mut child = std::process::Command::new("docker")
        .args([
            "exec", "-i", &container, "kubectl", "exec", "-i", "-n", namespace, pod, "--", "sh",
            "-c", script,
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).trim().to_owned(),
    )
}

/// Bob's instance objects without claims, and one worker of `image` applied.
/// Returns the instance, the environment and the client and host keys.
async fn start_worker(
    fixture: &Fixture,
    image: &str,
) -> (
    DesiredInstance,
    hermes_core::app::controller::WorkerEnvironment,
    String,
    String,
) {
    use hermes_core::app::controller::{AllowedBackend, WorkerEnvironment, WorkerRequest};
    use hermes_core::domain::assignments::LifetimeMode;
    use hermes_core::domain::execution::{EnvironmentState, EnvironmentView};
    use hermes_native::materials::ed25519_pair;

    let cluster = fixture.cluster();
    let mut desired = desired(fixture);
    // No claims: the disposable cluster has no storage for them.
    desired.definition.mounts.clear();
    desired.volumes.clear();
    let (client_private, client_public) = ed25519_pair().unwrap();
    let (host_private, host_public) = ed25519_pair().unwrap();
    let mut materials = materials();
    materials.plain.insert(
        MaterialPurpose::SshClient,
        Plain::new(client_private.as_bytes().to_vec()),
    );
    materials.plain.insert(
        MaterialPurpose::WorkerHost,
        Plain::new(host_private.as_bytes().to_vec()),
    );
    materials
        .public
        .insert(MaterialPurpose::SshClient, client_public.clone());
    materials
        .public
        .insert(MaterialPurpose::WorkerHost, host_public.clone());
    cluster
        .apply(&ApplyRequest {
            desired: &desired,
            apply: Apply::Everything(Workload::Run),
            materials: Some(&materials),
            secrets: &[],
        })
        .await
        .unwrap();
    let backend = AllowedBackend {
        version: 1,
        definition: serde_json::from_value(json!({
            "image": image,
            "sshContract": "managed-ssh-v2",
            "uid": 1000,
            "gid": 1000,
            "resources": {"cpuMillis": 250, "memoryMib": 256, "ephemeralGib": 1},
            "limits": {"idleSeconds": 900, "maxLeaseSeconds": 3600, "maxEnvironments": 4},
        }))
        .unwrap(),
    };
    let environment = WorkerEnvironment {
        id: Uuid::new_v4(),
        view: EnvironmentView {
            state: EnvironmentState::Pending,
            mode: LifetimeMode::PerSession,
            backend: ("python-tools".into(), 1),
            execution_generation: 1,
            pod_uid: None,
            live_leases: 0,
            idle_seconds: 0,
            drain_expired: false,
        },
        invocation: None,
    };
    cluster
        .apply_worker(&WorkerRequest {
            desired: &desired,
            environment: &environment,
            backend: &backend,
        })
        .await
        .unwrap();
    (
        desired,
        environment,
        client_private.to_string(),
        host_public,
    )
}

#[tokio::test]
#[ignore = "needs HERMES_TEST_KUBECONFIG from the disposable Kubernetes fixture"]
async fn a_worker_whose_image_cannot_be_pulled_is_reported_as_unavailable() {
    use hermes_core::domain::execution::WorkerFailure;

    let fixture = Fixture::new().await;
    let cluster = fixture.cluster();
    let missing = format!(
        "docker.io/library/hermes-e2e-missing@sha256:{}",
        "0".repeat(64)
    );
    let (desired, environment, _, _) = start_worker(&fixture, &missing).await;
    let deadline = std::time::Instant::now() + Duration::from_secs(180);
    loop {
        let workers = cluster.observe_workers(&desired).await.unwrap();
        let worker = workers.get(&environment.id);
        if worker.and_then(|w| w.failure) == Some(WorkerFailure::ImageUnavailable) {
            assert!(worker.is_some_and(|w| w.ready.is_none()));
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the pull failure was never reported: {worker:?}"
        );
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

#[tokio::test]
#[ignore = "needs HERMES_TEST_KUBECONFIG and the imported worker image"]
async fn a_worker_starts_with_the_instance_keys_and_answers_ssh_as_the_data_identity() {
    let image = std::env::var("HERMES_TEST_WORKER_IMAGE").expect("worker image");
    let fixture = Fixture::new().await;
    let cluster = fixture.cluster();
    let (desired, environment, client_private, host_public) = start_worker(&fixture, &image).await;
    let ed25519_pair = hermes_native::materials::ed25519_pair;
    let mut observed = None;
    for _ in 0..120 {
        let workers = cluster.observe_workers(&desired).await.unwrap();
        if let Some(worker) = workers.get(&environment.id).filter(|w| w.ready.is_some()) {
            observed = Some(worker.clone());
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let worker = observed.expect("the worker Pod became ready under admission");
    let pod = hermes_render::worker::pod_name(environment.id);

    // The instance's client key logs in, the provisioned host key answers,
    // and the shell runs as the backend-declared UID.
    let known = format!("[127.0.0.1]:2222 {host_public}");
    let script = format!(
        "umask 077; cat > /tmp/key; printf '%s\\n' '{known}' > /tmp/known; \
         ssh -F /dev/null -p 2222 -i /tmp/key -o BatchMode=yes -o IdentitiesOnly=yes \
         -o StrictHostKeyChecking=yes -o UserKnownHostsFile=/tmp/known worker@127.0.0.1 id -u"
    );
    let (ok, uid) = exec_in(&pod, &desired.namespace, &script, &client_private);
    assert!(ok, "SSH with the instance key and pinned host key");
    assert_eq!(uid, "1000");
    let (_, other_public) = ed25519_pair().unwrap();
    let wrong = script.replace(&host_public, &other_public);
    assert!(
        !exec_in(&pod, &desired.namespace, &wrong, &client_private).0,
        "another host key is refused"
    );
    let (stranger, _) = ed25519_pair().unwrap();
    assert!(
        !exec_in(&pod, &desired.namespace, &script, &stranger).0,
        "another client key is refused"
    );

    let stale = Precondition {
        uid: "another-uid".into(),
        resource_version: worker.resource_version.clone(),
    };
    assert_eq!(
        cluster
            .delete_worker(&desired.namespace, environment.id, &stale)
            .await,
        Err(ClusterError::Conflict),
        "only the observed Pod is deleted"
    );
    // The kubelet keeps updating status; like the controller, re-observe and
    // retry on a conflict.
    for _ in 0..60 {
        let Some(current) = cluster
            .observe_workers(&desired)
            .await
            .unwrap()
            .remove(&environment.id)
        else {
            return;
        };
        let precondition = Precondition {
            uid: current.uid,
            resource_version: current.resource_version,
        };
        match cluster
            .delete_worker(&desired.namespace, environment.id, &precondition)
            .await
        {
            Ok(()) | Err(ClusterError::Conflict) => {}
            Err(error) => panic!("delete failed: {error}"),
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    panic!("the worker Pod was not deleted");
}
