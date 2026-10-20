use super::*;
use uuid::Uuid;

fn fixture() -> (
    InstallationScope,
    AssignmentView,
    MaterialContext,
    TemplateDefinition,
    WorkloadConfig,
    NativeBootstrap,
) {
    let scope = InstallationScope {
        id: Uuid::new_v4(),
        epoch: 1,
        issuer: "https://sso.example.org/realms/agents".into(),
    };
    let instance_id = Uuid::new_v4();
    let template = hermes_domain::TemplateVersion {
        id: "personal-default".into(),
        version: 1,
    };
    let assignment = AssignmentView {
        id: Uuid::new_v4(),
        identity: hermes_domain::Identity {
            issuer: scope.issuer.clone(),
            subject: "owner-a".into(),
        },
        access: Access::Enabled,
        template: template.clone(),
        version: 1,
        instance_id,
        namespace: hermes_domain::instance_namespace(scope.id, instance_id),
        desired_generation: 1,
        observed_generation: 0,
        state: "Pending".into(),
    };
    let material = MaterialContext {
        id: Uuid::new_v4(),
        installation_id: scope.id,
        instance_id,
        epoch: 1,
        operation_id: Uuid::new_v4(),
        generation: 1,
        purpose: crate::native_bootstrap::PURPOSE.into(),
    };
    let definition = TemplateDefinition {
        schema_version: 1,
        image: format!("ghcr.io/example/hermes@sha256:{}", "a".repeat(64)),
        storage_class: "local-path".into(),
        home_gib: 5,
        workspace_gib: 10,
        cpu_millis: 500,
        memory_mib: 1024,
        admin_config: Default::default(),
    };
    let config = WorkloadConfig {
        control_namespace: "agent-platform".into(),
        controller_service_account: "hermes-controller".into(),
        reconciler_role: "hermes-instance".into(),
        public_origin: "https://hermes.example.org".into(),
        oidc_client: "native.hermes".into(),
        execution_port: 8443,
        execution_ca_pem: include_str!("../../tests/fixtures/template-ca.crt").into(),
        oidc_ca_pem: None,
        service_egress: vec![],
        public_https: false,
    };
    let bootstrap =
        NativeBootstrap::generate(template, "https://manager.example.org/execution").unwrap();
    (scope, assignment, material, definition, config, bootstrap)
}

#[test]
fn personal_workload_has_private_storage_scoped_credentials_and_always_running_non_root_gateway() {
    let (scope, assignment, material, definition, config, bootstrap) = fixture();
    let rendered = render(
        &scope,
        &assignment,
        "namespace-uid",
        &definition,
        &config,
        &material,
        &bootstrap,
    )
    .unwrap();
    let get = |purpose: &str| rendered.iter().find(|r| r.key == purpose).unwrap();
    for object in &rendered {
        assert_eq!(
            object.spec.namespace.as_deref(),
            Some(assignment.namespace.as_str())
        );
        assert_eq!(object.spec.namespace_uid.as_deref(), Some("namespace-uid"));
        assert_eq!(
            object.spec.manifest_digest,
            object.manifest.digest().unwrap()
        );
        assert!(
            object
                .manifest
                .value()
                .pointer("/metadata/ownerReferences")
                .is_none()
        );
    }
    let workload = get("hermes-workload").manifest.value();
    let policy = get("instance-policy").manifest.value();
    assert_eq!(policy["spec"]["namespaceUID"], "namespace-uid");
    assert_eq!(policy["spec"]["generation"], assignment.desired_generation);
    assert_eq!(policy["spec"]["native"]["image"], definition.image);
    assert_eq!(policy["spec"]["native"]["cpu"], "500m");
    let policy_text = serde_json::to_string(policy).unwrap();
    for secret in [
        &bootstrap.initialization_key,
        &bootstrap.execution_client_token,
        &bootstrap.dashboard_session_token,
        &bootstrap.ssh_private_key,
    ] {
        assert!(!policy_text.contains(secret));
    }
    assert!(
        rendered
            .iter()
            .position(|r| r.key == "instance-policy")
            .unwrap()
            < rendered
                .iter()
                .position(|r| r.key == "hermes-workload")
                .unwrap()
    );
    assert!(get("native-bootstrap").deployment().is_err());
    assert_eq!(workload["spec"]["replicas"], 1);
    assert_eq!(workload["spec"]["strategy"]["type"], "Recreate");
    let pod = &workload["spec"]["template"]["spec"];
    assert_eq!(pod["automountServiceAccountToken"], false);
    assert_eq!(pod["securityContext"]["runAsNonRoot"], true);
    assert_eq!(pod["securityContext"]["runAsUser"], 1000);
    assert_eq!(
        pod["securityContext"]["seccompProfile"]["type"],
        "RuntimeDefault"
    );
    let app = &pod["containers"][0];
    let init = &pod["initContainers"][0];
    assert_eq!(app["command"][1], "/usr/local/lib/personal-run.py");
    assert_eq!(init["command"][1], "/usr/local/lib/personal-init.py");
    for container in [app, init] {
        assert_eq!(container["image"], definition.image);
        assert_eq!(
            container["securityContext"]["allowPrivilegeEscalation"],
            false
        );
        assert_eq!(container["securityContext"]["readOnlyRootFilesystem"], true);
        assert_eq!(
            container["securityContext"]["capabilities"]["drop"],
            json!(["ALL"])
        );
    }
    assert!(
        init["volumeMounts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["name"] == "init")
    );
    assert!(
        !app["volumeMounts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["name"] == "init")
    );
    let volumes = pod["volumes"].as_array().unwrap();
    let credentials = volumes.iter().find(|v| v["name"] == "credentials").unwrap();
    assert_eq!(credentials["secret"]["secretName"], "native-bootstrap");
    let selected: Vec<_> = credentials["secret"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["key"].as_str().unwrap())
        .collect();
    assert_eq!(
        selected,
        vec![
            "dashboard-session-token",
            "execution-client-token",
            "id_ed25519"
        ]
    );
    assert!(!selected.contains(&"init-key"));
    assert_eq!(
        volumes
            .iter()
            .filter(|v| !v["persistentVolumeClaim"].is_null())
            .count(),
        2
    );
    for purpose in ["home-storage", "workspace-storage"] {
        assert_eq!(get(purpose).spec.retention, Retention::Retain);
        assert_eq!(
            get(purpose).manifest.value()["spec"]["storageClassName"],
            "local-path"
        );
    }
    let secret = get("native-bootstrap");
    assert_eq!(secret.spec.encrypted_material_ref, Some(material.id));
    assert_eq!(secret.manifest.value()["immutable"], true);
    let deployment = serde_json::to_string(workload).unwrap();
    for token in [
        &bootstrap.initialization_key,
        &bootstrap.execution_client_token,
        &bootstrap.dashboard_session_token,
        &bootstrap.ssh_private_key,
    ] {
        assert!(!deployment.contains(token));
    }
    let deny = get("network-default").manifest.value();
    assert_eq!(deny["spec"]["podSelector"], json!({}));
    assert_eq!(deny["spec"]["egress"], json!([]));
    assert_eq!(deny["spec"]["ingress"], json!([]));
    let network = get("hermes-network").manifest.value();
    assert!(
        !serde_json::to_string(network)
            .unwrap()
            .contains("0.0.0.0/0")
    );
    assert_eq!(
        network["spec"]["ingress"][0]["from"][0]["podSelector"]["matchLabels"]["app.kubernetes.io/component"],
        "hermes-router"
    );
    let probes = &network["spec"]["ingress"][0]["from"][1];
    assert_eq!(
        probes["podSelector"]["matchLabels"]["app.kubernetes.io/component"],
        "hermes-controller"
    );
    assert_eq!(
        probes["namespaceSelector"]["matchLabels"]["kubernetes.io/metadata.name"],
        config.control_namespace
    );
    assert_eq!(
        network["spec"]["ingress"][0]["from"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(network["spec"]["ingress"][0]["ports"][0]["port"], 9119);
    let context: String = get("native-owner").manifest.value()["data"]["context.json"]
        .as_str()
        .unwrap()
        .into();
    let context: Value = serde_json::from_str(&context).unwrap();
    assert_eq!(context["owner"]["subject"], "owner-a");
    assert_eq!(context["instanceId"], assignment.instance_id.to_string());
}

#[test]
fn renderer_refuses_foreign_namespace_material_generation_access_and_unapproved_shape() {
    let (scope, mut assignment, mut material, mut definition, mut config, bootstrap) = fixture();
    let check =
        |a: &AssignmentView, m: &MaterialContext, d: &TemplateDefinition, c: &WorkloadConfig| {
            render(&scope, a, "namespace-uid", d, c, m, &bootstrap).is_err()
        };
    assignment.access = Access::Suspended;
    assert!(check(&assignment, &material, &definition, &config));
    assignment.access = Access::Enabled;
    material.instance_id = Uuid::new_v4();
    assert!(check(&assignment, &material, &definition, &config));
    material.instance_id = assignment.instance_id;
    material.epoch = 2;
    assert!(check(&assignment, &material, &definition, &config));
    material.epoch = 1;
    material.generation = 2;
    assert!(check(&assignment, &material, &definition, &config));
    material.generation = 1;
    material.purpose = "foreign".into();
    assert!(check(&assignment, &material, &definition, &config));
    material.purpose = crate::native_bootstrap::PURPOSE.into();
    assignment.namespace = "somebody-else".into();
    assert!(check(&assignment, &material, &definition, &config));
    assignment.namespace = hermes_domain::instance_namespace(scope.id, assignment.instance_id);
    for image in [
        "ghcr.io/example/hermes:latest".into(),
        format!("ghcr.io/example/../hermes@sha256:{}", "a".repeat(64)),
        format!("https://example.org/hermes@sha256:{}", "a".repeat(64)),
        format!("ghcr.io/example/hermes@sha256:{}\n", "a".repeat(64)),
    ] {
        definition.image = image;
        assert!(check(&assignment, &material, &definition, &config));
    }
    definition.image = format!("ghcr.io/example/hermes@sha256:{}", "a".repeat(64));
    config.execution_ca_pem = "not a certificate".into();
    assert!(check(&assignment, &material, &definition, &config));
    config.execution_ca_pem = "-----BEGIN CERTIFICATE-----\nYWJj\n-----END CERTIFICATE-----".into();
    assert!(check(&assignment, &material, &definition, &config));
}

#[test]
fn fresh_owners_have_distinct_bootstrap_keys_and_only_generic_seed() {
    let (_, _, _, _, _, a) = fixture();
    let (_, _, _, _, _, b) = fixture();
    assert!(a.dashboard_session_token != b.dashboard_session_token);
    assert!(a.execution_client_token != b.execution_client_token);
    assert!(a.initialization_key != b.initialization_key);
    assert!(a.ssh_public_key != b.ssh_public_key);
    let key = ssh_key::PrivateKey::from_openssh(&a.ssh_private_key).unwrap();
    assert_eq!(
        key.public_key().to_openssh().unwrap(),
        a.ssh_public_key.trim()
    );
    assert_eq!(
        a.native_config["plugins"]["enabled"],
        json!(["managed_execution"])
    );
    assert_eq!(
        a.native_config["plugins"]["entries"]["managed_execution"]["settings"]["manager_url"],
        "https://manager.example.org/execution"
    );
    assert_eq!(a.native_config.as_object().unwrap().len(), 2);
}
