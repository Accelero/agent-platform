//! One instance's object set, in apply order: the namespace and the
//! controller's RoleBinding first, Pods last.

use std::collections::BTreeMap;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use hermes_core::app::controller::{
    DeliveredSecret, DesiredInstance, HOME_CLAIM, InstanceMaterials,
};
use hermes_core::domain::instances::Workload;
use hermes_core::domain::materials::MaterialPurpose;
use hermes_core::domain::secrets::SecretKind;
use hermes_core::domain::storage::{SourceKind, VolumeSpec};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::config::{self, CREDENTIALS_DIR, TRUST_DIR};
use crate::{
    COMPONENT_LABEL, DIGEST_ANNOTATION, GENERATION_ANNOTATION, INSTALLATION_LABEL, INSTANCE_LABEL,
    MANAGED_BY, MANAGER, Object, RenderConfig, RenderError,
};

/// The native dashboard port inside the Pod, and the Service port.
pub const NATIVE_PORT: u16 = 9119;
pub const SERVICE_PORT: u16 = 8080;
/// Terminal backends listen here.
pub const SSH_PORT: u16 = 2222;
pub const ADMIN_SECRETS_DIR: &str = "/run/hermes/admin-secrets";

const DEPLOYMENT: &str = "hermes";
const CREDENTIALS: &str = "hermes-credentials";
const ADMIN_SECRETS: &str = "hermes-admin-secrets";
const OWNER: &str = "hermes-owner";
const INIT: &str = "hermes-init";
const TRUST: &str = "hermes-trust";

/// Private address ranges excluded from public HTTPS egress.
const PRIVATE: &[&str] = &[
    "0.0.0.0/8",
    "10.0.0.0/8",
    "100.64.0.0/10",
    "127.0.0.0/8",
    "169.254.0.0/16",
    "172.16.0.0/12",
    "192.0.0.0/24",
    "192.168.0.0/16",
    "198.18.0.0/15",
    "224.0.0.0/4",
    "240.0.0.0/4",
];

/// Name of the PersistentVolume of one static volume: cluster-wide unique and
/// free of personal data.
pub fn volume_name(namespace: &str, volume: &str) -> String {
    format!("{namespace}-{volume}")
}

fn labels(config: &RenderConfig, desired: &DesiredInstance) -> Value {
    json!({
        MANAGED_BY: MANAGER,
        INSTALLATION_LABEL: config.installation.id.to_string(),
        INSTANCE_LABEL: desired.instance_id.to_string(),
    })
}

fn quantity_gib(gib: u32) -> String {
    format!("{gib}Gi")
}

fn cpu(millis: u32) -> String {
    if millis.is_multiple_of(1000) {
        (millis / 1000).to_string()
    } else {
        format!("{millis}m")
    }
}

fn memory(mib: u32) -> String {
    if mib.is_multiple_of(1024) {
        format!("{}Gi", mib / 1024)
    } else {
        format!("{mib}Mi")
    }
}

struct Builder<'a> {
    config: &'a RenderConfig,
    desired: &'a DesiredInstance,
    objects: Vec<Object>,
}

impl Builder<'_> {
    fn push(
        &mut self,
        api_version: &'static str,
        kind: &'static str,
        name: &str,
        mut body: Value,
        namespaced: bool,
    ) {
        body["apiVersion"] = json!(api_version);
        body["kind"] = json!(kind);
        body["metadata"]["name"] = json!(name);
        let mut merged = labels(self.config, self.desired);
        if let Some(extra) = body["metadata"]["labels"].as_object() {
            for (key, value) in extra {
                merged[key] = value.clone();
            }
        }
        body["metadata"]["labels"] = merged;
        let namespace = namespaced.then(|| self.desired.namespace.clone());
        if let Some(namespace) = &namespace {
            body["metadata"]["namespace"] = json!(namespace);
        }
        self.objects.push(Object {
            api_version,
            kind,
            namespace,
            name: name.to_owned(),
            body,
        });
    }
}

fn data(entries: impl IntoIterator<Item = (String, Vec<u8>)>) -> Value {
    Value::Object(
        entries
            .into_iter()
            .map(|(key, value)| (key, json!(STANDARD.encode(value))))
            .collect(),
    )
}

/// The last reference per target wins: assignment references override the
/// template's for the same variable or file.
fn deliveries(desired: &DesiredInstance) -> (BTreeMap<String, String>, BTreeMap<String, String>) {
    let mut env = BTreeMap::new();
    let mut files = BTreeMap::new();
    for reference in &desired.secrets {
        if let Some(variable) = &reference.env {
            env.insert(variable.clone(), reference.name.clone());
        }
        if let Some(file) = &reference.file {
            files.insert(file.clone(), reference.name.clone());
        }
    }
    (env, files)
}

/// Renders the whole object set. `materials` are the opened instance
/// material; `secrets` the opened admin secrets the instance references.
pub fn render(
    config: &RenderConfig,
    desired: &DesiredInstance,
    materials: &InstanceMaterials,
    secrets: &[DeliveredSecret],
    workload: Workload,
) -> Result<Vec<Object>, RenderError> {
    config.validate()?;
    if desired.namespace != config.installation.instance_namespace(desired.instance_id)
        || desired
            .identity
            .validate(&config.installation.issuer)
            .is_err()
    {
        return Err(RenderError::Invalid);
    }
    let material = |purpose| {
        materials
            .plain
            .get(&purpose)
            .map(|p| p.expose().to_vec())
            .ok_or(RenderError::Invalid)
    };
    let mut builder = Builder {
        config,
        desired,
        objects: Vec::new(),
    };
    let ns = desired.namespace.as_str();
    let instance = desired.instance_id.to_string();

    builder.push(
        "v1",
        "Namespace",
        ns,
        json!({"metadata": {"labels": {
            "pod-security.kubernetes.io/enforce": "restricted",
            "pod-security.kubernetes.io/enforce-version": "latest",
        }}}),
        false,
    );
    builder.push(
        "rbac.authorization.k8s.io/v1",
        "RoleBinding",
        "hermes-controller",
        json!({
            "metadata": {},
            "roleRef": {"apiGroup": "rbac.authorization.k8s.io", "kind": "ClusterRole", "name": config.instance_role},
            "subjects": [{"kind": "ServiceAccount", "name": config.controller_service_account, "namespace": config.control_namespace}],
        }),
        true,
    );
    for account in ["hermes", "worker"] {
        builder.push(
            "v1",
            "ServiceAccount",
            account,
            json!({"metadata": {}, "automountServiceAccountToken": false}),
            true,
        );
    }
    network_policies(&mut builder);

    // Storage: the private home, then every assigned volume.
    let home = config
        .sources
        .get(&desired.definition.home.source)
        .ok_or(RenderError::Invalid)?;
    let SourceKind::StorageClass { name: home_class } = &home.kind else {
        return Err(RenderError::Invalid);
    };
    builder.push(
        "v1",
        "PersistentVolumeClaim",
        HOME_CLAIM,
        json!({"metadata": {}, "spec": {
            "accessModes": ["ReadWriteOnce"],
            "storageClassName": home_class,
            "resources": {"requests": {"storage": quantity_gib(desired.definition.home.size_gib)}},
        }}),
        true,
    );
    for (name, volume) in &desired.volumes {
        storage(&mut builder, name, volume)?;
    }

    // Credentials, admin secrets and configuration.
    builder.push(
        "v1",
        "Secret",
        CREDENTIALS,
        json!({"metadata": {}, "type": "Opaque", "data": data([
            ("dashboard-session-token".to_owned(), material(MaterialPurpose::DashboardSession)?),
            ("execution-client-token".to_owned(), material(MaterialPurpose::ExecutionCredential)?),
            ("id_ed25519".to_owned(), material(MaterialPurpose::SshClient)?),
            ("init-key".to_owned(), material(MaterialPurpose::Initialization)?),
        ])}),
        true,
    );
    let (env_refs, file_refs) = deliveries(desired);
    let delivered: BTreeMap<&str, &DeliveredSecret> = secrets
        .iter()
        .filter(|s| matches!(s.kind, SecretKind::Env | SecretKind::File))
        .map(|s| (s.name.as_str(), s))
        .collect();
    for name in env_refs.values().chain(file_refs.values()) {
        if !delivered.contains_key(name.as_str()) {
            return Err(RenderError::Invalid);
        }
    }
    builder.push(
        "v1",
        "Secret",
        ADMIN_SECRETS,
        json!({"metadata": {}, "type": "Opaque", "data": data(
            delivered.iter().map(|(name, s)| ((*name).to_owned(), s.value.expose().to_vec()))
        )}),
        true,
    );
    // Workers present the instance's host key and accept only its client key.
    let client_public = materials
        .public
        .get(&MaterialPurpose::SshClient)
        .ok_or(RenderError::Invalid)?;
    builder.push(
        "v1",
        "Secret",
        crate::worker::KEYS,
        json!({"metadata": {}, "type": "Opaque", "data": data([
            ("authorized-keys".to_owned(), format!("{client_public}\n").into_bytes()),
            ("host-key".to_owned(), material(MaterialPurpose::WorkerHost)?),
        ])}),
        true,
    );
    // Pull credentials of private backend images.
    for secret in secrets.iter().filter(|s| s.kind == SecretKind::Registry) {
        builder.push(
            "v1",
            "Secret",
            &crate::worker::registry_secret(&secret.name),
            json!({"metadata": {}, "type": "kubernetes.io/dockerconfigjson", "data": data([
                (".dockerconfigjson".to_owned(), secret.value.expose().to_vec()),
            ])}),
            true,
        );
    }
    let owner = json!({
        "schemaVersion": 1,
        "installationId": config.installation.id.to_string(),
        "instanceId": instance,
        "materialId": materials.initialization_id.to_string(),
        "owner": {"issuer": desired.identity.issuer, "subject": desired.identity.subject},
        "template": {"id": desired.template.id, "version": desired.template.version},
    });
    builder.push(
        "v1",
        "ConfigMap",
        OWNER,
        json!({"metadata": {}, "data": {"context.json": owner.to_string()}}),
        true,
    );
    builder.push(
        "v1",
        "ConfigMap",
        INIT,
        json!({"metadata": {}, "data": {"native-config.json": config::seed().to_string()}}),
        true,
    );
    let mut trust = json!({
        "ca.crt": config.execution_ca_pem,
        "config.yaml": config::managed(config, desired).to_string(),
        ".env": "TERMINAL_ENV=managed_execution\n",
    });
    if let Some(ca) = &config.oidc_ca_pem {
        trust["oidc-ca.crt"] = json!(ca);
    }
    builder.push(
        "v1",
        "ConfigMap",
        TRUST,
        json!({"metadata": {}, "data": trust}),
        true,
    );

    let selector = json!({COMPONENT_LABEL: "hermes", INSTANCE_LABEL: instance});
    builder.push(
        "v1",
        "Service",
        "hermes",
        json!({"metadata": {}, "spec": {
            "selector": selector,
            "ports": [{"name": "http", "port": SERVICE_PORT, "targetPort": "dashboard", "protocol": "TCP"}],
        }}),
        true,
    );

    // Every input whose change needs a restart, without any secret value.
    let digest_inputs: Vec<Value> = builder
        .objects
        .iter()
        .filter(|o| matches!(o.kind, "Secret" | "ConfigMap"))
        .map(|o| json!([o.name, hex(&Sha256::digest(o.body["data"].to_string()))]))
        .collect();
    let digest = hex(&Sha256::digest(
        json!([digest_inputs, desired.definition, desired.volumes]).to_string(),
    ));
    let deployment = deployment(config, desired, workload, &env_refs, &file_refs, &digest)?;
    builder.push("apps/v1", "Deployment", DEPLOYMENT, deployment, true);
    Ok(builder.objects)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn storage(builder: &mut Builder<'_>, name: &str, volume: &VolumeSpec) -> Result<(), RenderError> {
    let source = builder
        .config
        .sources
        .get(&volume.source)
        .ok_or(RenderError::Invalid)?;
    match (&source.kind, volume.size_gib, &volume.path) {
        (SourceKind::StorageClass { name: class }, Some(size), None) => {
            builder.push(
                "v1",
                "PersistentVolumeClaim",
                name,
                json!({"metadata": {}, "spec": {
                    "accessModes": ["ReadWriteOnce"],
                    "storageClassName": class,
                    "resources": {"requests": {"storage": quantity_gib(size)}},
                }}),
                true,
            );
        }
        (SourceKind::LocalPath { .. } | SourceKind::Nfs { .. }, _, Some(path)) => {
            let pv = volume_name(&builder.desired.namespace, name);
            // Static volumes have no real size; Kubernetes needs one.
            let capacity = quantity_gib(volume.size_gib.unwrap_or(1));
            let (access, source_spec) = match &source.kind {
                SourceKind::LocalPath { node, root } => (
                    "ReadWriteOnce",
                    json!({
                        "local": {"path": format!("{}/{path}", root.trim_end_matches('/'))},
                        "nodeAffinity": {"required": {"nodeSelectorTerms": [{"matchExpressions": [
                            {"key": "kubernetes.io/hostname", "operator": "In", "values": [node]}
                        ]}]}},
                    }),
                ),
                SourceKind::Nfs { server, root } => (
                    "ReadWriteMany",
                    json!({"nfs": {"server": server, "path": format!("{}/{path}", root.trim_end_matches('/'))}}),
                ),
                SourceKind::StorageClass { .. } => return Err(RenderError::Invalid),
            };
            let mut spec = json!({
                "capacity": {"storage": capacity},
                "accessModes": [access],
                "persistentVolumeReclaimPolicy": "Retain",
                "storageClassName": "",
                "volumeMode": "Filesystem",
                "claimRef": {"namespace": builder.desired.namespace, "name": name},
            });
            for (key, value) in source_spec.as_object().expect("object") {
                spec[key] = value.clone();
            }
            builder.push(
                "v1",
                "PersistentVolume",
                &pv,
                json!({"metadata": {}, "spec": spec}),
                false,
            );
            builder.push(
                "v1",
                "PersistentVolumeClaim",
                name,
                json!({"metadata": {}, "spec": {
                    "accessModes": [access],
                    "storageClassName": "",
                    "volumeName": pv,
                    "resources": {"requests": {"storage": capacity}},
                }}),
                true,
            );
        }
        _ => return Err(RenderError::Invalid),
    }
    Ok(())
}

fn network_policies(builder: &mut Builder<'_>) {
    let config = builder.config;
    let control = json!({"kubernetes.io/metadata.name": config.control_namespace});
    let from_control = |labels: &BTreeMap<String, String>| json!({"namespaceSelector": {"matchLabels": control}, "podSelector": {"matchLabels": labels}});
    builder.push(
        "networking.k8s.io/v1",
        "NetworkPolicy",
        "default-deny",
        json!({"metadata": {}, "spec": {"podSelector": {}, "policyTypes": ["Ingress", "Egress"]}}),
        true,
    );
    let mut egress = vec![
        json!({"to": [{"namespaceSelector": {"matchLabels": {"kubernetes.io/metadata.name": "kube-system"}},
                       "podSelector": {"matchLabels": {"k8s-app": "kube-dns"}}}],
               "ports": [{"protocol": "UDP", "port": 53}, {"protocol": "TCP", "port": 53}]}),
        json!({"to": [from_control(&config.peers.api)], "ports": [{"protocol": "TCP", "port": config.execution_port}]}),
        json!({"to": [{"podSelector": {"matchLabels": {COMPONENT_LABEL: "worker"}}}],
               "ports": [{"protocol": "TCP", "port": SSH_PORT}]}),
    ];
    for peer in &config.service_egress {
        egress.push(json!({"to": [{"namespaceSelector": {"matchLabels": {"kubernetes.io/metadata.name": peer.namespace}},
                                   "podSelector": {"matchLabels": peer.labels}}],
                           "ports": [{"protocol": "TCP", "port": peer.port}]}));
    }
    let public = json!({"to": [{"ipBlock": {"cidr": "0.0.0.0/0", "except": PRIVATE}}], "ports": [{"protocol": "TCP", "port": 443}]});
    if config.public_https {
        egress.push(public.clone());
    }
    builder.push(
        "networking.k8s.io/v1",
        "NetworkPolicy",
        "hermes",
        json!({"metadata": {}, "spec": {
            "podSelector": {"matchLabels": {COMPONENT_LABEL: "hermes"}},
            "policyTypes": ["Ingress", "Egress"],
            "ingress": [{"from": [from_control(&config.peers.gateway), from_control(&config.peers.controller)],
                         "ports": [{"protocol": "TCP", "port": NATIVE_PORT}]}],
            "egress": egress,
        }}),
        true,
    );
    let mut worker_egress = vec![
        json!({"to": [{"namespaceSelector": {"matchLabels": {"kubernetes.io/metadata.name": "kube-system"}},
                                                "podSelector": {"matchLabels": {"k8s-app": "kube-dns"}}}],
                                        "ports": [{"protocol": "UDP", "port": 53}, {"protocol": "TCP", "port": 53}]}),
    ];
    if config.public_https {
        worker_egress.push(public);
    }
    builder.push(
        "networking.k8s.io/v1",
        "NetworkPolicy",
        "worker",
        json!({"metadata": {}, "spec": {
            "podSelector": {"matchLabels": {COMPONENT_LABEL: "worker"}},
            "policyTypes": ["Ingress", "Egress"],
            "ingress": [{"from": [{"podSelector": {"matchLabels": {COMPONENT_LABEL: "hermes"}}}, from_control(&config.peers.controller)],
                         "ports": [{"protocol": "TCP", "port": SSH_PORT}]}],
            "egress": worker_egress,
        }}),
        true,
    );
}

fn deployment(
    config: &RenderConfig,
    desired: &DesiredInstance,
    workload: Workload,
    env_refs: &BTreeMap<String, String>,
    file_refs: &BTreeMap<String, String>,
    digest: &str,
) -> Result<Value, RenderError> {
    let instance = desired.instance_id.to_string();
    let identity = config.data_identity;
    let resources = json!({
        "requests": {"cpu": cpu(desired.definition.resources.cpu_millis), "memory": memory(desired.definition.resources.memory_mib)},
        "limits": {"cpu": cpu(desired.definition.resources.cpu_millis), "memory": memory(desired.definition.resources.memory_mib)},
    });
    let security = json!({"allowPrivilegeEscalation": false, "readOnlyRootFilesystem": true, "capabilities": {"drop": ["ALL"]}});
    let mut env = vec![
        json!({"name": "HOME", "value": "/opt/data"}),
        json!({"name": "HERMES_HOME", "value": "/opt/data"}),
        json!({"name": "HERMES_MANAGED_DIR", "value": TRUST_DIR}),
        json!({"name": "HERMES_DASHBOARD_OIDC_ISSUER", "value": config.installation.issuer}),
        json!({"name": "HERMES_DASHBOARD_OIDC_CLIENT_ID", "value": config.native_client_id}),
        json!({"name": "HERMES_DASHBOARD_PUBLIC_URL", "value": config.public_origin}),
        json!({"name": "HERMES_DASHBOARD_OIDC_SCOPES", "value": "openid profile email"}),
        json!({"name": "API_SERVER_ENABLED", "value": "false"}),
        json!({"name": "PYTHONDONTWRITEBYTECODE", "value": "1"}),
    ];
    if config.oidc_ca_pem.is_some() {
        env.push(json!({"name": "HERMES_PERSONAL_OIDC_CA_FILE", "value": format!("{TRUST_DIR}/oidc-ca.crt")}));
    }
    for (variable, secret) in env_refs {
        env.push(json!({"name": variable, "valueFrom": {"secretKeyRef": {"name": ADMIN_SECRETS, "key": secret}}}));
    }
    let mut volumes = vec![
        json!({"name": HOME_CLAIM, "persistentVolumeClaim": {"claimName": HOME_CLAIM}}),
        json!({"name": "hermes-tmp", "emptyDir": {"sizeLimit": "256Mi"}}),
        json!({"name": OWNER, "configMap": {"name": OWNER}}),
        json!({"name": TRUST, "configMap": {"name": TRUST}}),
        json!({"name": INIT, "projected": {"defaultMode": 288, "sources": [
            {"secret": {"name": CREDENTIALS, "items": [{"key": "init-key", "path": "init-key"}]}},
            {"configMap": {"name": INIT, "items": [{"key": "native-config.json", "path": "native-config.json"}]}},
        ]}}),
        json!({"name": CREDENTIALS, "secret": {"secretName": CREDENTIALS, "defaultMode": 288, "items": [
            {"key": "dashboard-session-token", "path": "dashboard-session-token"},
            {"key": "execution-client-token", "path": "execution-client-token"},
            {"key": "id_ed25519", "path": "id_ed25519"},
        ]}}),
    ];
    let mut mounts = vec![
        json!({"name": HOME_CLAIM, "mountPath": "/opt/data"}),
        json!({"name": "hermes-tmp", "mountPath": "/tmp"}),
        json!({"name": OWNER, "mountPath": "/run/hermes/owner", "readOnly": true}),
        json!({"name": TRUST, "mountPath": TRUST_DIR, "readOnly": true}),
        json!({"name": CREDENTIALS, "mountPath": CREDENTIALS_DIR, "readOnly": true}),
    ];
    if !file_refs.is_empty() {
        let items: Vec<Value> = file_refs
            .iter()
            .map(|(file, secret)| json!({"key": secret, "path": file}))
            .collect();
        volumes.push(json!({"name": ADMIN_SECRETS, "secret": {"secretName": ADMIN_SECRETS, "defaultMode": 288, "items": items}}));
        mounts
            .push(json!({"name": ADMIN_SECRETS, "mountPath": ADMIN_SECRETS_DIR, "readOnly": true}));
    }
    for (name, mount) in &desired.definition.mounts {
        let volume = desired.volumes.get(name).ok_or(RenderError::Invalid)?;
        volumes.push(json!({"name": name, "persistentVolumeClaim": {"claimName": name}}));
        mounts.push(json!({"name": name, "mountPath": mount.mount_path, "readOnly": mount.read_only || volume.read_only}));
    }
    let labels = json!({
        COMPONENT_LABEL: "hermes",
        INSTALLATION_LABEL: config.installation.id.to_string(),
        INSTANCE_LABEL: instance,
    });
    Ok(json!({
        "metadata": {"annotations": {GENERATION_ANNOTATION: desired.desired_generation.to_string()}},
        "spec": {
            "replicas": i32::from(workload == Workload::Run),
            "strategy": {"type": "Recreate"},
            "revisionHistoryLimit": 2,
            "selector": {"matchLabels": {COMPONENT_LABEL: "hermes", INSTANCE_LABEL: instance}},
            "template": {
                "metadata": {"labels": labels, "annotations": {DIGEST_ANNOTATION: digest}},
                "spec": {
                    "serviceAccountName": "hermes",
                    "automountServiceAccountToken": false,
                    "enableServiceLinks": false,
                    "terminationGracePeriodSeconds": 45,
                    "securityContext": {
                        "runAsNonRoot": true,
                        "runAsUser": identity.uid,
                        "runAsGroup": identity.gid,
                        "fsGroup": identity.gid,
                        "fsGroupChangePolicy": "OnRootMismatch",
                        "seccompProfile": {"type": "RuntimeDefault"},
                    },
                    "initContainers": [{
                        "name": "native-home",
                        "image": desired.definition.image,
                        "command": ["/opt/hermes/.venv/bin/python", "/usr/local/lib/personal-init.py"],
                        "args": ["--context", "/run/hermes/owner/context.json"],
                        "securityContext": security,
                        "resources": resources,
                        "terminationMessagePath": "/tmp/native-init-result.json",
                        "terminationMessagePolicy": "File",
                        "volumeMounts": [
                            {"name": HOME_CLAIM, "mountPath": "/opt/data"},
                            {"name": "hermes-tmp", "mountPath": "/tmp"},
                            {"name": OWNER, "mountPath": "/run/hermes/owner", "readOnly": true},
                            {"name": INIT, "mountPath": "/run/hermes/init", "readOnly": true},
                        ],
                    }],
                    "containers": [{
                        "name": "hermes",
                        "image": desired.definition.image,
                        "command": ["/opt/hermes/.venv/bin/python", "/usr/local/lib/personal-run.py"],
                        "env": env,
                        "securityContext": security,
                        "resources": resources,
                        "ports": [{"name": "dashboard", "containerPort": NATIVE_PORT, "protocol": "TCP"}],
                        "startupProbe": {"tcpSocket": {"port": "dashboard"}, "periodSeconds": 5, "failureThreshold": 60},
                        "readinessProbe": {"tcpSocket": {"port": "dashboard"}, "periodSeconds": 5},
                        "volumeMounts": mounts,
                    }],
                    "volumes": volumes,
                },
            },
        },
    }))
}
