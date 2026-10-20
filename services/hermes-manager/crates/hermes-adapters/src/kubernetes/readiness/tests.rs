use super::*;
use hermes_application::resources::Creation;
use hermes_domain::TemplateVersion;
use hmac::{Hmac, KeyInit, Mac};
use serde_json::json;
use uuid::Uuid;

struct Fixture {
    namespace: OwnedResource,
    home: OwnedResource,
    workspace: OwnedResource,
    deployment: OwnedResource,
    manifest: Manifest,
    material: MaterialContext,
    bootstrap: NativeBootstrap,
    owner: Identity,
    live: DynamicObject,
    pod: Pod,
    set: ReplicaSet,
    starts: Vec<hermes_application::native_children::NativeChildInput>,
}

impl Fixture {
    fn new() -> Self {
        let installation = Uuid::new_v4();
        let instance = Uuid::new_v4();
        let material = MaterialContext {
            id: Uuid::new_v4(),
            installation_id: installation,
            instance_id: instance,
            epoch: 1,
            operation_id: Uuid::new_v4(),
            generation: 1,
            purpose: crate::native_bootstrap::PURPOSE.into(),
        };
        let owner = Identity {
            issuer: "https://sso.example.org/realms/agents".into(),
            subject: "owner-a".into(),
        };
        let bootstrap = NativeBootstrap::generate(
            TemplateVersion {
                id: "personal-default".into(),
                version: 1,
            },
            "https://manager.example.org/execution",
        )
        .unwrap();
        let make = |kind: ResourceKind, name: &str, uid: &str| OwnedResource {
            creation: Creation {
                id: Uuid::new_v4(),
                installation_id: installation,
                instance_id: instance,
                cluster_identity: "cluster-uid".into(),
                epoch: 1,
                generation: 1,
                operation_id: material.operation_id,
                spec: ResourceSpec {
                    kind,
                    namespace: (kind != ResourceKind::Namespace).then(|| "personal-fixture".into()),
                    namespace_uid: (kind != ResourceKind::Namespace)
                        .then(|| "namespace-uid".into()),
                    name: name.into(),
                    manifest_digest: "a".repeat(64),
                    purpose: "readiness".into(),
                    retention: Retention::Retain,
                    encrypted_material_ref: None,
                },
            },
            uid: Some(uid.into()),
            resource_version: Some("1".into()),
            observed_digest: Some("a".repeat(64)),
            state: "Bound".into(),
        };
        let namespace = make(ResourceKind::Namespace, "personal-fixture", "namespace-uid");
        let home = make(ResourceKind::PersistentVolumeClaim, "home", "home-uid");
        let workspace = make(
            ResourceKind::PersistentVolumeClaim,
            "workspace",
            "workspace-uid",
        );
        let deployment = make(ResourceKind::Deployment, "hermes", "deployment-uid");
        let template = json!({"metadata":{"labels":{"agent-platform.io/installation":installation,"agent-platform.io/instance":instance,"agent-platform.io/namespace-uid":"namespace-uid","agent-platform.io/policy-generation":"2","app.kubernetes.io/component":"hermes"}},"spec":{"containers":[{"name":"hermes","image":"native@sha256:approved"}],"initContainers":[{"name":"native-home","image":"native@sha256:approved"}]}});
        let live = serde_json::from_value(json!({"apiVersion":"apps/v1","kind":"Deployment","metadata":{"name":"hermes","namespace":"personal-fixture","uid":"deployment-uid","generation":7},"spec":{"replicas":1,"template":template},"status":{"observedGeneration":7,"replicas":1,"updatedReplicas":1,"readyReplicas":1,"availableReplicas":1}})).unwrap();
        let reference = |kind: &str, name: &str, uid: &str| json!({"apiVersion":"apps/v1","kind":kind,"name":name,"uid":uid,"controller":true});
        let set = serde_json::from_value(json!({"apiVersion":"apps/v1","kind":"ReplicaSet","metadata":{"name":"hermes-template","namespace":"personal-fixture","uid":"replicaset-uid","ownerReferences":[reference("Deployment","hermes","deployment-uid")]},"spec":{"selector":{"matchLabels":{"app.kubernetes.io/component":"hermes"}},"template":template}})).unwrap();
        let owner_digest: String = Sha256::digest(serde_json::to_vec(&owner).unwrap())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let record = json!({"complete":true,"intent":{"binding":{"schemaVersion":1,"installationId":installation,"instanceId":instance,"materialId":material.id,"ownerDigest":owner_digest}}});
        let key: Vec<u8> = (0..64)
            .step_by(2)
            .map(|i| u8::from_str_radix(&bootstrap.initialization_key[i..i + 2], 16).unwrap())
            .collect();
        let mut mac = Hmac::<Sha256>::new_from_slice(&key).unwrap();
        mac.update(b"hermes-home-completion-v1\0");
        mac.update(&serde_json::to_vec(&record).unwrap());
        let signature: String = mac
            .finalize()
            .into_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let receipt =
            serde_json::to_string(&json!({"record":record,"signature":signature})).unwrap();
        let mut pod = template;
        pod["apiVersion"] = json!("v1");
        pod["kind"] = json!("Pod");
        pod["metadata"]["name"] = json!("hermes-template-pod");
        pod["metadata"]["namespace"] = json!("personal-fixture");
        pod["metadata"]["uid"] = json!("pod-uid");
        pod["metadata"]["ownerReferences"] =
            json!([reference("ReplicaSet", "hermes-template", "replicaset-uid")]);
        pod["status"] = json!({"phase":"Running","conditions":[{"type":"Ready","status":"True"}],"containerStatuses":[{"name":"hermes","ready":true,"restartCount":0,"image":"native","imageID":"native","state":{"running":{}}}],"initContainerStatuses":[{"name":"native-home","ready":true,"restartCount":0,"image":"native","imageID":"native","state":{"terminated":{"exitCode":0,"message":receipt}}}]});
        let pod: Pod = serde_json::from_value(pod).unwrap();
        let starts = vec![hermes_application::native_children::NativeChildInput {
            installation_id: installation,
            instance_id: instance,
            operation_id: Uuid::new_v4(),
            deployment_id: deployment.creation.id,
            fence_key: "open".into(),
            namespace: "personal-fixture".into(),
            namespace_uid: "namespace-uid".into(),
            deployment_uid: "deployment-uid".into(),
            replica_set_name: "hermes-template".into(),
            replica_set_uid: "replicaset-uid".into(),
            pod_name: "hermes-template-pod".into(),
            pod_uid: "pod-uid".into(),
            resource_version: "1".into(),
            spec_digest: super::super::native_children::pod_digest(&pod).unwrap(),
            generation: 2,
            epoch: 1,
            action: hermes_application::native_children::NativeChildAction::Start,
        }];
        Self {
            starts,
            namespace,
            home,
            workspace,
            deployment,
            manifest: Manifest::new(json!({})).unwrap(),
            material,
            bootstrap,
            owner,
            live,
            pod,
            set,
        }
    }
    fn input(&self) -> NativeReadiness<'_> {
        NativeReadiness {
            namespace: &self.namespace,
            storage: [
                (&self.home, &self.manifest),
                (&self.workspace, &self.manifest),
            ],
            deployment: (&self.deployment, &self.manifest),
            epoch: 1,
            generation: 2,
            material: &self.material,
            bootstrap: &self.bootstrap,
            owner: &self.owner,
            started_children: &self.starts,
        }
    }
}

#[test]
fn native_readiness_requires_current_deployment_observation_and_one_replica() {
    let fixture = Fixture::new();
    assert!(deployment_ready(&fixture.live));
    for (field, value) in [
        ("observedGeneration", 6),
        ("replicas", 2),
        ("updatedReplicas", 0),
        ("readyReplicas", 0),
        ("availableReplicas", 0),
    ] {
        let mut changed = fixture.live.clone();
        changed.data["status"][field] = json!(value);
        assert!(!deployment_ready(&changed));
    }
}

#[test]
fn recovery_keeps_original_material_but_rejects_future_epoch_and_foreign_storage() {
    let fixture = Fixture::new();
    let mut input = fixture.input();
    input.epoch = 2;
    input.validate().unwrap();
    let mut material = fixture.material.clone();
    material.epoch = 3;
    input.material = &material;
    assert!(matches!(input.validate(), Err(Error::OwnershipConflict)));
    let mut deployment = fixture.deployment.clone();
    deployment.creation.epoch = 3;
    let mut input = fixture.input();
    input.deployment = (&deployment, &fixture.manifest);
    assert!(matches!(input.validate(), Err(Error::OwnershipConflict)));
    let mut home = fixture.home.clone();
    home.creation.spec.namespace_uid = Some("foreign-namespace-uid".into());
    let mut input = fixture.input();
    input.storage[0] = (&home, &fixture.manifest);
    assert!(matches!(input.validate(), Err(Error::OwnershipConflict)));
}

#[test]
fn healthy_pod_needs_exact_owner_chain_template_and_signed_initialization() {
    let fixture = Fixture::new();
    fixture.input().validate().unwrap();
    assert!(pod_ready(&fixture.pod, &fixture.set, &fixture.live, &fixture.input()).unwrap());
    let mutate = |path: &str, value: Value| {
        let mut pod = serde_json::to_value(&fixture.pod).unwrap();
        *pod.pointer_mut(path).unwrap() = value;
        serde_json::from_value::<Pod>(pod).unwrap()
    };
    for pod in [
        mutate(
            "/metadata/ownerReferences/0/uid",
            json!("foreign-replicaset-uid"),
        ),
        mutate("/metadata/ownerReferences/0/controller", json!(false)),
        mutate(
            "/metadata/labels/agent-platform.io~1policy-generation",
            json!("3"),
        ),
        mutate("/spec/containers/0/image", json!("foreign-image")),
        mutate(
            "/status/initContainerStatuses/0/state/terminated/message",
            json!("{}"),
        ),
    ] {
        assert!(matches!(
            pod_ready(&pod, &fixture.set, &fixture.live, &fixture.input()),
            Err(Error::OwnershipConflict)
        ));
    }
    let mut foreign = fixture.set.clone();
    foreign.metadata.owner_references.as_mut().unwrap()[0].uid = "foreign-deployment-uid".into();
    assert!(matches!(
        pod_ready(&fixture.pod, &foreign, &fixture.live, &fixture.input()),
        Err(Error::OwnershipConflict)
    ));
    let mut input = fixture.input();
    input.generation = 3;
    assert!(matches!(
        pod_ready(&fixture.pod, &fixture.set, &fixture.live, &input),
        Err(Error::OwnershipConflict)
    ));
    let mut owner = fixture.owner.clone();
    owner.subject = "owner-b".into();
    let mut input = fixture.input();
    input.owner = &owner;
    assert!(matches!(
        pod_ready(&fixture.pod, &fixture.set, &fixture.live, &input),
        Err(Error::OwnershipConflict)
    ));
    for pod in [
        mutate(
            "/status/initContainerStatuses/0/state/terminated/exitCode",
            json!(1),
        ),
        mutate("/status/conditions/0/status", json!("False")),
        mutate("/status/containerStatuses/0/ready", json!(false)),
    ] {
        assert!(!pod_ready(&pod, &fixture.set, &fixture.live, &fixture.input()).unwrap());
    }
}

#[test]
fn readiness_needs_committed_start_for_exact_pod_uid_and_removed_gate() {
    let fixture = Fixture::new();
    let mut input = fixture.input();
    input.started_children = &[];
    assert!(matches!(
        pod_ready(&fixture.pod, &fixture.set, &fixture.live, &input),
        Err(Error::OwnershipConflict)
    ));
    let mut foreign = fixture.starts.clone();
    foreign[0].pod_uid = "old-pod-uid".into();
    input.started_children = &foreign;
    assert!(matches!(
        pod_ready(&fixture.pod, &fixture.set, &fixture.live, &input),
        Err(Error::OwnershipConflict)
    ));
    let mut pod = fixture.pod.clone();
    pod.spec.as_mut().unwrap().scheduling_gates =
        Some(vec![k8s_openapi::api::core::v1::PodSchedulingGate {
            name: hermes_application::native_children::START_GATE.into(),
        }]);
    assert!(matches!(
        pod_ready(&pod, &fixture.set, &fixture.live, &fixture.input()),
        Err(Error::OwnershipConflict)
    ));
    // The scheduler's binding must not invalidate an already committed start.
    pod.spec.as_mut().unwrap().scheduling_gates = None;
    pod.spec.as_mut().unwrap().node_name = Some("fixture-node".into());
    assert!(pod_ready(&pod, &fixture.set, &fixture.live, &fixture.input()).unwrap());
}

#[test]
fn recreate_waits_for_verified_old_generation_without_accepting_future_or_malformed_labels() {
    let fixture = Fixture::new();
    let mut pod = fixture.pod.clone();
    pod.metadata
        .labels
        .as_mut()
        .unwrap()
        .insert("agent-platform.io/policy-generation".into(), "1".into());
    assert!(!pod_ready(&pod, &fixture.set, &fixture.live, &fixture.input()).unwrap());
    for generation in ["3", "01", "0", "unknown"] {
        pod.metadata.labels.as_mut().unwrap().insert(
            "agent-platform.io/policy-generation".into(),
            generation.into(),
        );
        assert!(matches!(
            pod_ready(&pod, &fixture.set, &fixture.live, &fixture.input()),
            Err(Error::OwnershipConflict)
        ));
    }
}
