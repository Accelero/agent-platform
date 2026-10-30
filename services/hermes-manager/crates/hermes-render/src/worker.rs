//! One terminal backend Pod per environment, implementing `managed-ssh-v2`:
//! the image's `worker` user with the backend-declared identity, SSH on port 2222, the
//! instance's client key as the only authorized key and the instance's host
//! key. It mounts the backend's slots that the assignment binds and nothing
//! else; the private Hermes home is never among them.

use hermes_core::app::controller::{AllowedBackend, DesiredInstance, WorkerEnvironment};
use hermes_core::domain::execution::SSH_PORT;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    COMPONENT_LABEL, INSTALLATION_LABEL, INSTANCE_LABEL, MANAGED_BY, MANAGER, Object, RenderConfig,
    RenderError,
};

/// The Secret holding `authorized-keys` and `host-key`.
pub const KEYS: &str = "hermes-worker-keys";
/// Pod label naming the environment.
pub const ENVIRONMENT_LABEL: &str = "agent-platform.io/environment";
pub const SSH_CONTRACT: &str = "managed-ssh-v2";

pub fn pod_name(environment: Uuid) -> String {
    format!("hw-{}", environment.simple())
}

pub fn registry_secret(name: &str) -> String {
    format!("hermes-registry-{name}")
}

fn quantity(mib: u32) -> String {
    if mib.is_multiple_of(1024) {
        format!("{}Gi", mib / 1024)
    } else {
        format!("{mib}Mi")
    }
}

/// The worker Pod of one environment.
pub fn render(
    config: &RenderConfig,
    desired: &DesiredInstance,
    environment: &WorkerEnvironment,
    backend: &AllowedBackend,
) -> Result<Object, RenderError> {
    config.validate()?;
    let definition = &backend.definition;
    if definition.ssh_contract != SSH_CONTRACT
        || environment.view.backend.1 != backend.version
        || desired.namespace != config.installation.instance_namespace(desired.instance_id)
        || !(1..u32::MAX).contains(&definition.uid)
        || !(1..u32::MAX).contains(&definition.gid)
    {
        return Err(RenderError::Invalid);
    }
    let resources = &definition.resources;
    let cpu = if resources.cpu_millis.is_multiple_of(1000) {
        (resources.cpu_millis / 1000).to_string()
    } else {
        format!("{}m", resources.cpu_millis)
    };
    let limits = json!({
        "cpu": cpu,
        "memory": quantity(resources.memory_mib),
        "ephemeral-storage": format!("{}Gi", resources.ephemeral_gib),
    });
    let scratch = format!("{}Gi", resources.ephemeral_gib);
    let mut volumes = vec![
        json!({"name": "worker-tmp", "emptyDir": {"sizeLimit": scratch}}),
        json!({"name": "worker-ssh", "secret": {"secretName": KEYS, "defaultMode": 292,
            "items": [{"key": "authorized-keys", "path": "worker"}]}}),
        json!({"name": "worker-host", "secret": {"secretName": KEYS, "defaultMode": 288,
            "items": [{"key": "host-key", "path": "ssh_host_ed25519_key"}]}}),
    ];
    let mut mounts = vec![
        json!({"name": "worker-tmp", "mountPath": "/tmp"}),
        // A file below the image's root-owned directory, so OpenSSH's strict
        // modes accept every parent.
        json!({"name": "worker-ssh", "mountPath": "/etc/ssh/authorized_keys/worker", "subPath": "worker", "readOnly": true}),
        json!({"name": "worker-host", "mountPath": "/etc/ssh/hostkeys", "readOnly": true}),
    ];
    let mut slot_paths = Vec::new();
    for (name, slot) in &definition.mounts {
        // Unbound optional slots stay empty; nothing is mounted in their place.
        let Some(volume) = desired.volumes.get(name) else {
            if slot.optional {
                continue;
            }
            return Err(RenderError::Invalid);
        };
        let template_read_only = desired
            .definition
            .mounts
            .get(name)
            .is_some_and(|m| m.read_only);
        volumes.push(json!({"name": name, "persistentVolumeClaim": {"claimName": name}}));
        mounts.push(json!({"name": name, "mountPath": slot.mount_path,
            "readOnly": slot.read_only || volume.read_only || template_read_only}));
        slot_paths.push(slot.mount_path.as_str());
    }
    for (name, path) in [
        ("worker-home", "/home/worker"),
        ("worker-workspace", "/workspace"),
    ] {
        if !slot_paths.contains(&path) {
            volumes.push(json!({"name": name, "emptyDir": {"sizeLimit": scratch}}));
            mounts.push(json!({"name": name, "mountPath": path}));
        }
    }
    let mut spec = json!({
        "restartPolicy": "Never",
        "serviceAccountName": "worker",
        "automountServiceAccountToken": false,
        "enableServiceLinks": false,
        "terminationGracePeriodSeconds": 30,
        "securityContext": {
            "runAsNonRoot": true,
            "runAsUser": definition.uid,
            "runAsGroup": definition.gid,
            "fsGroup": definition.gid,
            "fsGroupChangePolicy": "OnRootMismatch",
            "seccompProfile": {"type": "RuntimeDefault"},
        },
        "volumes": volumes,
        "containers": [{
            "name": "execution",
            "image": definition.image,
            "command": ["/entrypoint.sh"],
            "workingDir": "/workspace",
            "env": [
                {"name": "HOME", "value": "/home/worker"},
                {"name": "USER", "value": "worker"},
                {"name": "LOGNAME", "value": "worker"},
                {"name": "LANG", "value": "C.UTF-8"},
                {"name": "LC_ALL", "value": "C.UTF-8"},
                {"name": "TMPDIR", "value": "/tmp"},
            ],
            "securityContext": {"allowPrivilegeEscalation": false, "readOnlyRootFilesystem": true, "capabilities": {"drop": ["ALL"]}},
            "resources": {"requests": limits, "limits": limits},
            "ports": [{"name": "ssh", "containerPort": SSH_PORT, "protocol": "TCP"}],
            "readinessProbe": {"tcpSocket": {"port": "ssh"}, "periodSeconds": 2},
            "volumeMounts": mounts,
        }],
    });
    if let Some(secret) = &definition.registry_secret {
        spec["imagePullSecrets"] = json!([{"name": registry_secret(secret)}]);
    }
    let name = pod_name(environment.id);
    let body: Value = json!({
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": {
            "name": name,
            "namespace": desired.namespace,
            "labels": {
                MANAGED_BY: MANAGER,
                INSTALLATION_LABEL: config.installation.id.to_string(),
                INSTANCE_LABEL: desired.instance_id.to_string(),
                COMPONENT_LABEL: "worker",
                ENVIRONMENT_LABEL: environment.id.to_string(),
            },
        },
        "spec": spec,
    });
    Ok(Object {
        api_version: "v1",
        kind: "Pod",
        namespace: Some(desired.namespace.clone()),
        name,
        body,
    })
}
