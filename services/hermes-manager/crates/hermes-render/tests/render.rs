//! Rendered objects: typed Kubernetes schema, determinism, restart digests,
//! credential placement and golden files (`UPDATE_GOLDEN=1` rewrites them).

use std::collections::BTreeMap;

use hermes_core::app::controller::{AllowedBackend, WorkerEnvironment};
use hermes_core::app::controller::{DeliveredSecret, DesiredInstance, InstanceMaterials, Plain};
use hermes_core::app::testing::{identity, installation, settings};
use hermes_core::domain::assignments::{Access, LifetimeMode, TerminalGrant};
use hermes_core::domain::backends::BackendDefinition;
use hermes_core::domain::catalog::VersionRef;
use hermes_core::domain::execution::{EnvironmentState, EnvironmentView};
use hermes_core::domain::instances::Workload;
use hermes_core::domain::materials::MaterialPurpose;
use hermes_core::domain::secrets::{SecretKind, SecretReference};
use hermes_core::domain::storage::VolumeSpec;
use hermes_core::domain::templates::TemplateDefinition;
use hermes_render::{Object, Peers, RenderConfig, admission, instance, worker};
use k8s_openapi::api::admissionregistration::v1::{
    ValidatingAdmissionPolicy, ValidatingAdmissionPolicyBinding,
};
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::{
    ConfigMap, Namespace, PersistentVolume, PersistentVolumeClaim, Pod, Secret, Service,
    ServiceAccount,
};
use k8s_openapi::api::networking::v1::NetworkPolicy;
use k8s_openapi::api::rbac::v1::{ClusterRole, ClusterRoleBinding, Role, RoleBinding};
use serde_json::{Value, json};
use uuid::Uuid;

const CA: &str = "-----BEGIN CERTIFICATE-----\nMIIBfixture\n-----END CERTIFICATE-----\n";

fn config() -> RenderConfig {
    let settings = settings();
    let labels = |component: &str| {
        BTreeMap::from([(
            "app.kubernetes.io/component".to_owned(),
            component.to_owned(),
        )])
    };
    RenderConfig {
        installation: installation(),
        sources: settings.sources.clone(),
        data_identity: settings.data_identity,
        control_namespace: "hermes-system".into(),
        controller_service_account: "hermes-manager-controller".into(),
        instance_role: "hermes-manager-instance".into(),
        peers: Peers {
            gateway: labels("hermes-gateway"),
            controller: labels("hermes-controller"),
            api: labels("hermes-manager-api"),
        },
        execution_url: "https://hermes-manager-api.hermes-system.svc:8443/execution".into(),
        execution_port: 8443,
        execution_ca_pem: CA.into(),
        oidc_ca_pem: None,
        native_client_id: "hermes-native".into(),
        public_origin: "https://hermes.example.org/".into(),
        service_egress: Vec::new(),
        public_https: true,
    }
}

fn desired() -> DesiredInstance {
    let instance = Uuid::from_u128(0x0b7d_4c1e_2a3f_4e5d_8c6b_7a9f_0e1d_2c3b);
    let definition: TemplateDefinition = serde_json::from_value(json!({
        "schemaVersion": 2,
        "image": format!("ghcr.io/accelero/agent-platform/hermes@sha256:{}", "a".repeat(64)),
        "resources": {"cpuMillis": 1500, "memoryMib": 2048},
        "home": {"source": "nvme", "sizeGib": 5},
        "mounts": {"workspace": {"mountPath": "/workspace"}, "home": {"mountPath": "/data/home", "readOnly": true}},
        "adminConfig": {"model": {"provider": "custom"}},
        "secrets": [{"name": "llm-key", "env": "OPENAI_API_KEY"}],
    }))
    .unwrap();
    DesiredInstance {
        instance_id: instance,
        assignment_id: Uuid::from_u128(1),
        identity: identity("bob"),
        namespace: installation().instance_namespace(instance),
        namespace_uid: Some("namespace-uid".into()),
        access: Access::Enabled,
        desired_generation: 3,
        execution_generation: 2,
        template: VersionRef {
            id: "personal-default".into(),
            version: 1,
        },
        admin_config: definition.admin_config.clone(),
        definition,
        volumes: BTreeMap::from([
            (
                "workspace".to_owned(),
                VolumeSpec {
                    source: "nvme".into(),
                    path: None,
                    size_gib: Some(20),
                    read_only: false,
                },
            ),
            (
                "home".to_owned(),
                VolumeSpec {
                    source: "host-data".into(),
                    path: Some("bob/home".into()),
                    size_gib: None,
                    read_only: false,
                },
            ),
        ]),
        terminal: TerminalGrant {
            mode: LifetimeMode::PerProfileOnDemand,
            default: Some("python-tools".into()),
            allowed: BTreeMap::from([("python-tools".to_owned(), 1)]),
        },
        secrets: vec![
            SecretReference {
                name: "llm-key".into(),
                env: Some("OPENAI_API_KEY".into()),
                file: None,
            },
            SecretReference {
                name: "ca-bundle".into(),
                env: None,
                file: Some("ca-bundle".into()),
            },
        ],
        backends: Default::default(),
    }
}

fn materials() -> InstanceMaterials {
    InstanceMaterials {
        initialization_id: Uuid::from_u128(2),
        plain: MaterialPurpose::ALL
            .into_iter()
            .map(|p| {
                (
                    p,
                    Plain::new(format!("fixture-{}", p.as_str()).into_bytes()),
                )
            })
            .collect(),
        public: BTreeMap::from([
            (
                MaterialPurpose::SshClient,
                "ssh-ed25519 AAAAclient".to_owned(),
            ),
            (
                MaterialPurpose::WorkerHost,
                "ssh-ed25519 AAAAhost".to_owned(),
            ),
        ]),
    }
}

fn secrets(value: &str) -> Vec<DeliveredSecret> {
    vec![
        DeliveredSecret {
            name: "llm-key".into(),
            kind: SecretKind::Env,
            version: 2,
            value: Plain::new(value.as_bytes().to_vec()),
        },
        DeliveredSecret {
            name: "ca-bundle".into(),
            kind: SecretKind::File,
            version: 1,
            value: Plain::new(b"bundle".to_vec()),
        },
        DeliveredSecret {
            name: "registry".into(),
            kind: SecretKind::Registry,
            version: 1,
            value: Plain::new(br#"{"auths":{}}"#.to_vec()),
        },
    ]
}

fn render(workload: Workload) -> Vec<Object> {
    instance::render(
        &config(),
        &desired(),
        &materials(),
        &secrets("sk-fixture"),
        workload,
    )
    .unwrap()
}

fn find<'a>(objects: &'a [Object], kind: &str, name: &str) -> &'a Value {
    &objects
        .iter()
        .find(|o| o.kind == kind && o.name == name)
        .unwrap_or_else(|| panic!("{kind} {name}"))
        .body
}

fn typed<T: serde::de::DeserializeOwned>(body: &Value) -> T {
    serde_json::from_value(body.clone()).expect("valid Kubernetes object")
}

#[test]
fn every_object_is_a_valid_kubernetes_object_in_apply_order() {
    let objects = render(Workload::Run);
    let kinds: Vec<_> = objects.iter().map(|o| o.kind).collect();
    assert_eq!(kinds.first(), Some(&"Namespace"));
    assert_eq!(
        kinds.get(1),
        Some(&"RoleBinding"),
        "the controller's rights come right after the namespace"
    );
    assert_eq!(kinds.last(), Some(&"Deployment"));
    for object in &objects {
        let body = &object.body;
        assert_eq!(
            body["metadata"]["labels"]["agent-platform.io/installation"],
            installation().id.to_string()
        );
        match object.kind {
            "Namespace" => drop(typed::<Namespace>(body)),
            "RoleBinding" => drop(typed::<RoleBinding>(body)),
            "ServiceAccount" => drop(typed::<ServiceAccount>(body)),
            "NetworkPolicy" => drop(typed::<NetworkPolicy>(body)),
            "PersistentVolume" => drop(typed::<PersistentVolume>(body)),
            "PersistentVolumeClaim" => drop(typed::<PersistentVolumeClaim>(body)),
            "Secret" => drop(typed::<Secret>(body)),
            "ConfigMap" => drop(typed::<ConfigMap>(body)),
            "Service" => drop(typed::<Service>(body)),
            "Deployment" => drop(typed::<Deployment>(body)),
            other => panic!("unexpected kind {other}"),
        }
        assert_eq!(
            object.namespace.is_none(),
            matches!(object.kind, "Namespace" | "PersistentVolume")
        );
    }
}

#[test]
fn rendering_is_deterministic_and_restarts_only_for_pod_inputs() {
    let digest = |objects: &[Object]| {
        find(objects, "Deployment", "hermes")["spec"]["template"]["metadata"]["annotations"].clone()
    };
    let base = render(Workload::Run);
    assert_eq!(base, render(Workload::Run));
    let mut next = desired();
    next.desired_generation = 4;
    next.terminal.allowed.insert("other".into(), 1);
    let terminal_only = instance::render(
        &config(),
        &next,
        &materials(),
        &secrets("sk-fixture"),
        Workload::Run,
    )
    .unwrap();
    assert_eq!(
        digest(&base),
        digest(&terminal_only),
        "a terminal change does not restart Hermes"
    );
    assert_eq!(
        find(&terminal_only, "Deployment", "hermes")["metadata"]["annotations"]["agent-platform.io/desired-generation"],
        "4"
    );
    let rotated = instance::render(
        &config(),
        &desired(),
        &materials(),
        &secrets("sk-rotated"),
        Workload::Run,
    )
    .unwrap();
    assert_ne!(
        digest(&base),
        digest(&rotated),
        "a new secret version restarts Hermes"
    );
    let mut settings = desired();
    settings
        .admin_config
        .insert("agent".into(), json!({"max_turns": 30}));
    let reconfigured = instance::render(
        &config(),
        &settings,
        &materials(),
        &secrets("sk-fixture"),
        Workload::Run,
    )
    .unwrap();
    assert_ne!(
        digest(&base),
        digest(&reconfigured),
        "an admin setting restarts Hermes"
    );
    let stopped = render(Workload::Stop);
    assert_eq!(
        find(&stopped, "Deployment", "hermes")["spec"]["replicas"],
        0
    );
    assert_eq!(digest(&base), digest(&stopped));
}

#[test]
fn credentials_go_only_where_they_are_used() {
    let objects = render(Workload::Run);
    for object in &objects {
        let printed = format!("{object:?}");
        assert!(
            !printed.contains("fixture-") && !printed.contains("sk-fixture"),
            "{printed}"
        );
    }
    let pod = &find(&objects, "Deployment", "hermes")["spec"]["template"]["spec"];
    let main = &pod["containers"][0];
    let init = &pod["initContainers"][0];
    let mounted = |container: &Value| -> Vec<String> {
        container["volumeMounts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["name"].as_str().unwrap().to_owned())
            .collect()
    };
    assert!(mounted(init).contains(&"hermes-init".to_owned()));
    assert!(
        !mounted(main).contains(&"hermes-init".to_owned()),
        "the running container never sees the init key"
    );
    let credentials = pod["volumes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["name"] == "hermes-credentials")
        .unwrap();
    let items: Vec<_> = credentials["secret"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["key"].clone())
        .collect();
    assert!(!items.contains(&json!("init-key")));
    let env = main["env"].as_array().unwrap();
    assert!(env.iter().any(|e| e["name"] == "OPENAI_API_KEY"
        && e["valueFrom"]["secretKeyRef"]
            == json!({"name": "hermes-admin-secrets", "key": "llm-key"})));
    assert!(mounted(main).contains(&"hermes-admin-secrets".to_owned()));
    let managed: Value = serde_json::from_str(
        find(&objects, "ConfigMap", "hermes-trust")["data"]["config.yaml"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(managed["terminal"]["backend"], "managed_execution");
    assert_eq!(
        managed["plugins"]["entries"]["managed_execution"]["settings"]["mode"],
        "per_profile_on_demand"
    );
    assert_eq!(
        managed["plugins"]["entries"]["managed_execution"]["settings"]["runtime_id"],
        "python-tools"
    );
    assert_eq!(managed["model"]["provider"], "custom");
    assert!(
        !managed.to_string().contains("fixture-"),
        "no credential in the managed configuration"
    );
}

#[test]
fn volumes_use_their_sources_and_mounts_follow_the_template() {
    let objects = render(Workload::Run);
    assert_eq!(
        find(&objects, "PersistentVolumeClaim", "hermes-home")["spec"]["storageClassName"],
        "local-nvme"
    );
    assert_eq!(
        find(&objects, "PersistentVolumeClaim", "workspace")["spec"]["resources"]["requests"]["storage"],
        "20Gi"
    );
    let pv_name = instance::volume_name(&desired().namespace, "home");
    let pv = find(&objects, "PersistentVolume", &pv_name);
    assert_eq!(pv["spec"]["local"]["path"], "/srv/hermes-data/bob/home");
    assert_eq!(pv["spec"]["persistentVolumeReclaimPolicy"], "Retain");
    assert_eq!(
        pv["spec"]["claimRef"],
        json!({"namespace": desired().namespace, "name": "home"})
    );
    assert_eq!(
        pv["spec"]["nodeAffinity"]["required"]["nodeSelectorTerms"][0]["matchExpressions"][0]["values"],
        json!(["server-1"])
    );
    assert_eq!(
        find(&objects, "PersistentVolumeClaim", "home")["spec"]["volumeName"],
        pv_name
    );
    let pod = &find(&objects, "Deployment", "hermes")["spec"]["template"]["spec"];
    let mounts = pod["containers"][0]["volumeMounts"].as_array().unwrap();
    let home = mounts.iter().find(|m| m["name"] == "home").unwrap();
    assert_eq!(
        (home["mountPath"].clone(), home["readOnly"].clone()),
        (json!("/data/home"), json!(true))
    );
    assert_eq!(pod["securityContext"]["runAsUser"], 1000);
    assert_eq!(pod["automountServiceAccountToken"], false);
}

#[test]
fn foreign_namespaces_and_identities_are_refused() {
    let mut foreign = desired();
    foreign.namespace = "kube-system".into();
    assert!(instance::render(&config(), &foreign, &materials(), &[], Workload::Run).is_err());
    let mut other = desired();
    other.identity.issuer = "https://elsewhere.example.org/realms/x".into();
    assert!(instance::render(&config(), &other, &materials(), &[], Workload::Run).is_err());
    assert!(
        instance::render(
            &config(),
            &desired(),
            &materials(),
            &secrets("x")[..1],
            Workload::Run
        )
        .is_err(),
        "a referenced secret must be delivered"
    );
}

#[test]
fn rbac_and_policies_are_valid_objects() {
    for object in admission::rbac(&config()).unwrap() {
        match object.kind {
            "ClusterRole" => drop(typed::<ClusterRole>(&object.body)),
            "ClusterRoleBinding" => drop(typed::<ClusterRoleBinding>(&object.body)),
            "Role" => drop(typed::<Role>(&object.body)),
            "RoleBinding" => drop(typed::<RoleBinding>(&object.body)),
            other => panic!("unexpected {other}"),
        }
    }
    let policies = admission::policies(&config()).unwrap();
    assert_eq!(policies.len() % 2, 0);
    for pair in policies.chunks(2) {
        drop(typed::<ValidatingAdmissionPolicy>(&pair[0].body));
        drop(typed::<ValidatingAdmissionPolicyBinding>(&pair[1].body));
        assert_eq!(pair[0].kind, "ValidatingAdmissionPolicy");
        assert_eq!(pair[1].kind, "ValidatingAdmissionPolicyBinding");
        assert_eq!(pair[1].body["spec"]["policyName"], pair[0].name);
    }
    let text =
        serde_json::to_string(&policies.iter().map(|o| &o.body).collect::<Vec<_>>()).unwrap();
    assert!(
        text.contains("/srv/hermes-data/") && text.contains("nas.internal"),
        "approved roots are parameters"
    );
}

fn golden(name: &str, objects: &[Object]) {
    let path = format!("{}/tests/golden/{name}.json", env!("CARGO_MANIFEST_DIR"));
    let rendered =
        serde_json::to_string_pretty(&objects.iter().map(|o| &o.body).collect::<Vec<_>>()).unwrap()
            + "\n";
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(format!("{}/tests/golden", env!("CARGO_MANIFEST_DIR"))).unwrap();
        std::fs::write(&path, &rendered).unwrap();
    }
    let expected = std::fs::read_to_string(&path).expect("golden file; run with UPDATE_GOLDEN=1");
    assert_eq!(
        rendered, expected,
        "{name} changed; review and run with UPDATE_GOLDEN=1"
    );
}

#[test]
fn rendered_objects_match_their_golden_files() {
    golden("instance", &render(Workload::Run));
    golden("rbac", &admission::rbac(&config()).unwrap());
    golden("policies", &admission::policies(&config()).unwrap());
}

fn worker_fixture() -> (DesiredInstance, WorkerEnvironment, AllowedBackend) {
    let mut desired = desired();
    let mut definition: BackendDefinition = serde_json::from_value(json!({
        "image": format!("ghcr.io/accelero/agent-platform/agent-runtime@sha256:{}", "b".repeat(64)),
        "sshContract": "managed-ssh-v2",
        "uid": 1000,
        "gid": 1000,
        "resources": {"cpuMillis": 2000, "memoryMib": 4096, "ephemeralGib": 10},
        "limits": {"idleSeconds": 900, "maxLeaseSeconds": 3600, "maxEnvironments": 4},
        "mounts": {
            "workspace": {"mountPath": "/workspace"},
            "home": {"mountPath": "/home/worker/home", "optional": true},
            "scratch": {"mountPath": "/scratch", "optional": true},
        },
        "registrySecret": "registry",
    }))
    .unwrap();
    definition.mounts.get_mut("home").unwrap().read_only = false;
    let backend = AllowedBackend {
        version: 1,
        definition,
    };
    desired
        .backends
        .insert("python-tools".into(), backend.clone());
    let environment = WorkerEnvironment {
        id: Uuid::from_u128(7),
        view: EnvironmentView {
            state: EnvironmentState::Pending,
            mode: LifetimeMode::PerSession,
            backend: ("python-tools".into(), 1),
            execution_generation: 2,
            pod_uid: None,
            live_leases: 0,
            idle_seconds: 0,
            drain_expired: false,
        },
        invocation: None,
    };
    (desired, environment, backend)
}

#[test]
fn workers_mount_only_bound_slots_with_the_instance_keys() {
    let (desired, environment, backend) = worker_fixture();
    let pod = worker::render(&config(), &desired, &environment, &backend).unwrap();
    assert_eq!(pod.name, format!("hw-{}", environment.id.simple()));
    let typed: Pod = serde_json::from_value(pod.body.clone()).unwrap();
    let spec = typed.spec.unwrap();
    assert_eq!(spec.automount_service_account_token, Some(false));
    assert_eq!(spec.restart_policy.as_deref(), Some("Never"));
    assert_eq!(
        spec.image_pull_secrets.unwrap()[0].name,
        "hermes-registry-registry"
    );
    let container = &spec.containers[0];
    let mounts: BTreeMap<_, _> = container
        .volume_mounts
        .as_ref()
        .unwrap()
        .iter()
        .map(|m| {
            (
                m.mount_path.clone(),
                (m.name.clone(), m.read_only.unwrap_or(false)),
            )
        })
        .collect();
    assert_eq!(
        mounts["/workspace"],
        ("workspace".to_owned(), false),
        "the shared volume, at the same path"
    );
    assert_eq!(
        mounts["/home/worker/home"],
        ("home".to_owned(), true),
        "read-only where the template mounts it read-only"
    );
    assert!(
        !mounts.contains_key("/scratch"),
        "unbound optional slots stay empty"
    );
    assert!(
        !mounts.contains_key("/opt/data"),
        "the private home is never in a worker"
    );
    assert_eq!(mounts["/etc/ssh/authorized_keys/worker"].0, "worker-ssh");
    let volumes = spec.volumes.unwrap();
    assert!(volumes.iter().all(|v| {
        v.persistent_volume_claim
            .as_ref()
            .is_none_or(|c| c.claim_name != "hermes-home")
    }));
    let security = spec.security_context.unwrap();
    assert_eq!(
        (security.run_as_user, security.run_as_group),
        (Some(1000), Some(1000))
    );
    assert_eq!(container.ports.as_ref().unwrap()[0].container_port, 2222);

    let objects = render(Workload::Run);
    let keys = find(&objects, "Secret", "hermes-worker-keys");
    let decoded = |key: &str| {
        use base64::Engine;
        String::from_utf8(
            base64::engine::general_purpose::STANDARD
                .decode(keys["data"][key].as_str().unwrap())
                .unwrap(),
        )
        .unwrap()
    };
    assert_eq!(
        decoded("authorized-keys"),
        "ssh-ed25519 AAAAclient\n",
        "only the client's public key"
    );
    assert_eq!(decoded("host-key"), "fixture-worker-host");
    let registry = find(&objects, "Secret", "hermes-registry-registry");
    assert_eq!(registry["type"], "kubernetes.io/dockerconfigjson");
}

#[test]
fn workers_refuse_backends_outside_the_contract() {
    let (desired, environment, mut backend) = worker_fixture();
    backend.definition.uid = 0;
    assert!(worker::render(&config(), &desired, &environment, &backend).is_err());
    let (desired, environment, mut backend) = worker_fixture();
    backend.definition.ssh_contract = "managed-ssh-v1".into();
    assert!(worker::render(&config(), &desired, &environment, &backend).is_err());
    let (mut desired, environment, backend) = worker_fixture();
    desired.volumes.remove("workspace");
    assert!(
        worker::render(&config(), &desired, &environment, &backend).is_err(),
        "required slots must be bound"
    );
}
