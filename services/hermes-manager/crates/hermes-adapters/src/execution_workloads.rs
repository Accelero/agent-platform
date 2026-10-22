//! Owner-scoped worker rendering from a single immutable resolved policy.
use crate::{kubernetes::Manifest, workloads::RenderedResource};
use hermes_application::{
    Error,
    execution::{ResolvedPolicy, ResolvedRuntime},
};
use hermes_domain::{
    instance_namespace,
    resources::{ResourceKind, ResourceSpec, Retention, valid_uid},
    validate_name,
};
use k8s_openapi::api::core::v1::Pod;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

pub const START_GATE: &str = "agent-platform.io/worker-start";
pub const AUTHORIZED_KEYS: &str = "worker-authorized-keys";
pub const HOST_KEYS: &str = "worker-host-key";
pub const LABEL_PREFIX: &str = "hermes.agent-platform.io";
pub(crate) const TOLERATION_SECONDS: i64 = 300;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerContext {
    pub assignment_generation: i64,
    pub environment_id: Uuid,
    pub environment_generation: i64,
}
pub struct RenderedWorker {
    pub resource: RenderedResource,
}
impl RenderedWorker {
    pub fn pod(&self) -> Result<Pod, Error> {
        serde_json::from_value(self.resource.manifest.value().clone()).map_err(|_| Error::Invalid)
    }
}
pub fn pod_name(environment: Uuid, generation: i64) -> Result<String, Error> {
    if environment.is_nil() || generation < 1 {
        return Err(Error::Invalid);
    }
    let name = format!("hw-{}-g{generation}", environment.simple());
    validate_name(&name)?;
    Ok(name)
}

/// Validate every resolved owner reference before either rendering or deriving
/// admission. Resolution cannot be inferred from a matching claim/Secret name.
pub fn validate_policy(policy: &ResolvedPolicy) -> Result<(), Error> {
    if policy.installation_id.is_nil()
        || policy.instance_id.is_nil()
        || policy.namespace != instance_namespace(policy.installation_id, policy.instance_id)
        || !valid_uid(&policy.namespace_uid)
        || policy.generation < 1
        || policy.runtimes.is_empty()
        || policy.runtimes.len() > 16
    {
        return Err(Error::Invalid);
    }
    let mut ids = BTreeSet::new();
    for runtime in &policy.runtimes {
        runtime.definition.validate()?;
        runtime.resources.validate()?;
        runtime.limits.validate()?;
        if !ids.insert(runtime.definition.runtime.id.as_str())
            || runtime.definition_digest != runtime.definition.digest()?
            || runtime.resources.cpu_millis > runtime.definition.resources.cpu_millis
            || runtime.resources.memory_mib > runtime.definition.resources.memory_mib
            || runtime.resources.ephemeral_storage_mib
                > runtime.definition.resources.ephemeral_storage_mib
            || runtime.mounts.len() > 16
            || runtime.pull_secrets.len() > 4
        {
            return Err(Error::Invalid);
        }
        let mut selected = Vec::new();
        let mut claims = BTreeMap::new();
        for mount in &runtime.mounts {
            mount.binding.validate()?;
            mount.mount.validate()?;
            if mount.binding.instance_id != policy.instance_id
                || mount.binding.namespace != policy.namespace
                || mount.binding.namespace_uid != policy.namespace_uid
                || !mount.binding.mounts.contains(&mount.mount)
                || [
                    "/home/worker",
                    "/tmp",
                    "/entrypoint.sh",
                    "/bin",
                    "/sbin",
                    "/usr",
                    "/lib",
                    "/lib64",
                ]
                .iter()
                .any(|reserved| overlaps(&mount.mount.path, reserved))
            {
                return Err(Error::OwnershipConflict);
            }
            if claims
                .insert(&mount.binding.pvc_name, &mount.binding.pvc_uid)
                .is_some_and(|uid| uid != &mount.binding.pvc_uid)
            {
                return Err(Error::OwnershipConflict);
            }
            selected.push(hermes_domain::Mount {
                storage_binding_id: mount.binding.id,
                mount_path: mount.mount.path.clone(),
                sub_path: mount.mount.sub_path.clone(),
                read_only: mount.mount.read_only,
            });
        }
        hermes_domain::validate_mounts(&selected)?;
        let mut pull_names = BTreeSet::new();
        for pull in &runtime.pull_secrets {
            pull.validate()?;
            if pull.instance_id != policy.instance_id
                || pull.namespace != policy.namespace
                || pull.namespace_uid != policy.namespace_uid
                || !pull_names.insert(pull.name.as_str())
            {
                return Err(Error::OwnershipConflict);
            }
        }
        if pull_names
            != runtime
                .definition
                .pull_secret_names
                .iter()
                .map(String::as_str)
                .collect()
        {
            return Err(Error::OwnershipConflict);
        }
    }
    if !ids.contains(policy.default_runtime_id.as_str()) {
        return Err(Error::Invalid);
    }
    Ok(())
}
fn overlaps(path: &str, reserved: &str) -> bool {
    path == reserved
        || path
            .strip_prefix(reserved)
            .is_some_and(|s| s.starts_with('/'))
        || reserved
            .strip_prefix(path)
            .is_some_and(|s| s.starts_with('/'))
}
fn binary(mut amount: u32) -> String {
    let mut index = 0;
    let units = ["Mi", "Gi", "Ti"];
    while amount.is_multiple_of(1024) && index + 1 < units.len() {
        amount /= 1024;
        index += 1;
    }
    format!("{amount}{}", units[index])
}

/// Public, credential-free runtime fields consumed by BOTH the renderer and
/// Namespace-local admission parameters. UID bindings remain in ResolvedPolicy
/// for the driver's live resource verification; Kubernetes volumes accept names.
pub(crate) fn template(runtime: &ResolvedRuntime) -> Value {
    let cpu = if runtime.resources.cpu_millis.is_multiple_of(1000) {
        (runtime.resources.cpu_millis / 1000).to_string()
    } else {
        format!("{}m", runtime.resources.cpu_millis)
    };
    let disk = binary(runtime.resources.ephemeral_storage_mib);
    let environment = BTreeMap::from([
        ("HOME", "/home/worker"),
        ("USER", "worker"),
        ("LOGNAME", "worker"),
        ("LANG", runtime.definition.locale.as_str()),
        ("LC_ALL", runtime.definition.locale.as_str()),
        ("TMPDIR", "/tmp"),
    ]);
    let mut volumes = vec![
        json!({"name":"worker-home","kind":"empty","sizeLimit":disk}),
        json!({"name":"worker-tmp","kind":"empty","sizeLimit":disk}),
        json!({"name":"worker-ssh","kind":"ssh","reference":AUTHORIZED_KEYS}),
    ];
    let mut mounts = vec![
        json!({"name":"worker-home","path":"/home/worker","readOnly":false}),
        json!({"name":"worker-tmp","path":"/tmp","readOnly":false}),
        json!({"name":"worker-ssh","path":"/etc/ssh/authorized_keys","readOnly":true}),
    ];
    if runtime.definition.ssh_contract == "managed-ssh-v2" {
        // Secret projection roots become group-writable under fsGroup. Mount
        // only the immutable file below the image's root-owned directory so
        // OpenSSH StrictModes can validate every parent without weakening it.
        mounts[2] = json!({"name":"worker-ssh","path":"/etc/ssh/authorized_keys/worker","subPath":"worker","readOnly":true});
        volumes.push(json!({"name":"worker-host","kind":"host","reference":HOST_KEYS}));
        mounts.push(json!({"name":"worker-host","path":"/etc/ssh/hostkeys","readOnly":true}));
    }
    if !runtime.mounts.iter().any(|m| m.mount.path == "/workspace") {
        volumes.push(json!({"name":"worker-workspace","kind":"empty","sizeLimit":disk}));
        mounts.push(json!({"name":"worker-workspace","path":"/workspace","readOnly":false}));
    }
    for (index, mount) in runtime.mounts.iter().enumerate() {
        let name = format!("storage-{index}");
        volumes.push(json!({"name":name,"kind":"pvc","reference":mount.binding.pvc_name,"readOnly":mount.mount.read_only}));
        let mut selected =
            json!({"name":name,"path":mount.mount.path,"readOnly":mount.mount.read_only});
        if let Some(subpath) = &mount.mount.sub_path {
            selected["subPath"] = json!(subpath);
        }
        mounts.push(selected);
    }
    json!({"version":runtime.definition.runtime.version,"digest":runtime.definition_digest,"image":runtime.definition.image,
        "uid":runtime.definition.uid,"gid":runtime.definition.gid,"cpu":cpu,"memory":binary(runtime.resources.memory_mib),"ephemeralStorage":disk,
        "environment":environment,"volumes":volumes,"mounts":mounts,
        "pullSecrets":runtime.pull_secrets.iter().map(|p| &p.name).collect::<Vec<_>>()})
}

pub fn render(
    policy: &ResolvedPolicy,
    runtime_id: &str,
    context: &WorkerContext,
) -> Result<RenderedWorker, Error> {
    validate_policy(policy)?;
    if context.assignment_generation < 1 {
        return Err(Error::Invalid);
    }
    let name = pod_name(context.environment_id, context.environment_generation)?;
    let runtime = policy
        .runtimes
        .iter()
        .find(|r| r.definition.runtime.id == runtime_id)
        .ok_or(Error::Forbidden)?;
    let t = template(runtime);
    let volumes = t["volumes"].as_array().ok_or(Error::Invalid)?.iter().map(|v| {
        Ok(match v["kind"].as_str() {
            Some("pvc") => {
                let mut source = json!({"name":v["name"],"persistentVolumeClaim":{"claimName":v["reference"]}});
                if v["readOnly"] == true { source["persistentVolumeClaim"]["readOnly"] = json!(true); }
                source
            },
            Some("empty") => json!({"name":v["name"],"emptyDir":{"sizeLimit":v["sizeLimit"]}}),
            Some("ssh") => json!({"name":v["name"],"secret":{"secretName":AUTHORIZED_KEYS,"defaultMode":292,"items":[{"key":"worker","path":"worker"}]}}),
            Some("host") => json!({"name":v["name"],"secret":{"secretName":HOST_KEYS,"defaultMode":288,"items":[{"key":"host-key","path":"ssh_host_ed25519_key"}]}}),
            _ => return Err(Error::Invalid),
        })
    }).collect::<Result<Vec<_>,Error>>()?;
    let mounts = t["mounts"]
        .as_array()
        .ok_or(Error::Invalid)?
        .iter()
        .map(|m| {
            let mut value = json!({"name":m["name"],"mountPath":m["path"]});
            if m["readOnly"] == true {
                value["readOnly"] = json!(true);
            }
            if let Some(subpath) = m.get("subPath") {
                value["subPath"] = subpath.clone();
            }
            value
        })
        .collect::<Vec<_>>();
    let env = t["environment"]
        .as_object()
        .ok_or(Error::Invalid)?
        .iter()
        .map(|(name, value)| json!({"name":name,"value":value}))
        .collect::<Vec<_>>();
    let resources =
        json!({"cpu":t["cpu"],"memory":t["memory"],"ephemeral-storage":t["ephemeralStorage"]});
    let labels = json!({"app.kubernetes.io/component":"worker",
        "hermes.agent-platform.io/namespace-uid":policy.namespace_uid,
        "hermes.agent-platform.io/assignment-generation":context.assignment_generation.to_string(),
        "hermes.agent-platform.io/execution-policy-generation":policy.generation.to_string(),
        "hermes.agent-platform.io/environment-id":context.environment_id.to_string(),
        "hermes.agent-platform.io/environment-generation":context.environment_generation.to_string(),
        "hermes.agent-platform.io/runtime-id":runtime_id,
        "hermes.agent-platform.io/runtime-version":runtime.definition.runtime.version.to_string()});
    let body = json!({"metadata":{"name":name,"namespace":policy.namespace,"labels":labels,"annotations":{
        "hermes.agent-platform.io/policy-digest":policy.digest()?,"hermes.agent-platform.io/runtime-digest":runtime.definition_digest}},"spec":{
        "restartPolicy":"Never","serviceAccountName":"worker","automountServiceAccountToken":false,"enableServiceLinks":false,
        "schedulingGates":[{"name":START_GATE}],"terminationGracePeriodSeconds":30,
        "tolerations":[{"key":"node.kubernetes.io/not-ready","operator":"Exists","effect":"NoExecute","tolerationSeconds":TOLERATION_SECONDS},{"key":"node.kubernetes.io/unreachable","operator":"Exists","effect":"NoExecute","tolerationSeconds":TOLERATION_SECONDS}],
        "securityContext":{"runAsNonRoot":true,"runAsUser":t["uid"],"runAsGroup":t["gid"],"fsGroup":t["gid"],"seccompProfile":{"type":"RuntimeDefault"}},
        "imagePullSecrets":runtime.pull_secrets.iter().map(|p| json!({"name":p.name})).collect::<Vec<_>>(),
        "volumes":volumes,"containers":[{"name":"execution","image":t["image"],"imagePullPolicy":"IfNotPresent","command":["/entrypoint.sh"],"args":[],"workingDir":"/workspace","env":env,
            "securityContext":{"allowPrivilegeEscalation":false,"readOnlyRootFilesystem":true,"capabilities":{"drop":["ALL"]}},
            "resources":{"requests":resources,"limits":resources},"ports":[{"name":"ssh","containerPort":2222,"protocol":"TCP"}],
            "readinessProbe":{"tcpSocket":{"port":"ssh"},"periodSeconds":2},
            "terminationMessagePath":"/dev/termination-log","terminationMessagePolicy":"File","volumeMounts":mounts}]}});
    Ok(RenderedWorker {
        resource: resource(
            policy,
            ResourceKind::Pod,
            &name,
            "worker-environment",
            Retention::Delete,
            None,
            body,
        )?,
    })
}
pub fn service_account(policy: &ResolvedPolicy) -> Result<RenderedResource, Error> {
    validate_policy(policy)?;
    resource(
        policy,
        ResourceKind::ServiceAccount,
        "worker",
        "worker-service-account",
        Retention::Retain,
        None,
        json!({"metadata":{},"automountServiceAccountToken":false}),
    )
}
pub fn authorized_keys(
    policy: &ResolvedPolicy,
    public_key: &str,
    material_id: Uuid,
) -> Result<RenderedResource, Error> {
    validate_policy(policy)?;
    let mut key =
        ssh_key::PublicKey::from_openssh(public_key.trim()).map_err(|_| Error::Invalid)?;
    if material_id.is_nil()
        || key.algorithm() != ssh_key::Algorithm::Ed25519
        || public_key.len() > 1024
    {
        return Err(Error::Invalid);
    }
    key.set_comment("");
    let public = format!("{}\n", key.to_openssh().map_err(|_| Error::Invalid)?);
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    resource(
        policy,
        ResourceKind::Secret,
        AUTHORIZED_KEYS,
        "worker-authorized-keys",
        Retention::Retain,
        Some(material_id),
        json!({"metadata":{},"immutable":true,"type":"Opaque","data":{"worker":STANDARD.encode(public.as_bytes())}}),
    )
}

pub fn host_keys(
    policy: &ResolvedPolicy,
    bootstrap: &crate::native_bootstrap::NativeBootstrap,
    material_id: Uuid,
) -> Result<RenderedResource, Error> {
    validate_policy(policy)?;
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    let key = bootstrap.worker_host_key()?;
    let private = key
        .to_openssh(ssh_key::LineEnding::LF)
        .map_err(|_| Error::Invalid)?;
    resource(
        policy,
        ResourceKind::Secret,
        HOST_KEYS,
        HOST_KEYS,
        Retention::Retain,
        Some(material_id),
        json!({"metadata":{},"immutable":true,"type":"Opaque","data":{"host-key":STANDARD.encode(private.as_bytes())}}),
    )
}

pub fn network(
    policy: &ResolvedPolicy,
    control_namespace: &str,
) -> Result<RenderedResource, Error> {
    validate_policy(policy)?;
    validate_name(control_namespace)?;
    resource(
        policy,
        ResourceKind::NetworkPolicy,
        "worker-access",
        "worker-access",
        Retention::Retain,
        None,
        json!({"metadata":{},"spec":{"podSelector":{"matchLabels":{"app.kubernetes.io/component":"worker"}},"policyTypes":["Ingress","Egress"],
            "ingress":[{"from":[{"podSelector":{"matchLabels":{"app.kubernetes.io/component":"hermes"}}},{"namespaceSelector":{"matchLabels":{"kubernetes.io/metadata.name":control_namespace}},"podSelector":{"matchLabels":{"app.kubernetes.io/component":"hermes-controller"}}}],"ports":[{"protocol":"TCP","port":2222}]}],"egress":[]}}),
    )
}
fn resource(
    policy: &ResolvedPolicy,
    kind: ResourceKind,
    name: &str,
    purpose: &str,
    retention: Retention,
    material: Option<Uuid>,
    mut body: Value,
) -> Result<RenderedResource, Error> {
    body["apiVersion"] = json!(kind.api_version());
    body["kind"] = json!(kind.as_str());
    body["metadata"]["name"] = json!(name);
    body["metadata"]["namespace"] = json!(policy.namespace);
    let manifest = Manifest::new(body)?;
    let spec = ResourceSpec {
        kind,
        namespace: Some(policy.namespace.clone()),
        namespace_uid: Some(policy.namespace_uid.clone()),
        name: name.into(),
        purpose: purpose.into(),
        manifest_digest: manifest.digest()?,
        retention,
        encrypted_material_ref: material,
    };
    spec.validate()?;
    manifest.validate(&spec)?;
    Ok(RenderedResource {
        key: name.into(),
        spec,
        manifest,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_application::execution::{RuntimeDefinition, RuntimeRef, WorkerResources};

    #[test]
    fn v2_pins_a_separate_instance_host_identity_without_exposing_client_private_key() {
        use base64::{Engine as _, engine::general_purpose::STANDARD};
        let mut policy = policy();
        policy.runtimes[0].definition.ssh_contract = "managed-ssh-v2".into();
        policy.runtimes[0].definition_digest = policy.runtimes[0].definition.digest().unwrap();
        let bootstrap = crate::native_bootstrap::NativeBootstrap::generate(
            hermes_domain::TemplateVersion {
                id: "personal".into(),
                version: 1,
            },
            "https://manager.example.org/execution",
        )
        .unwrap();
        let host = bootstrap.worker_host_key().unwrap();
        assert!(
            host.public_key()
                != &ssh_key::PublicKey::from_openssh(&bootstrap.ssh_public_key).unwrap()
        );
        let secret = host_keys(&policy, &bootstrap, Uuid::new_v4()).unwrap();
        let bytes = STANDARD
            .decode(
                secret.manifest.value()["data"]["host-key"]
                    .as_str()
                    .unwrap(),
            )
            .unwrap();
        assert!(bytes.as_slice() != bootstrap.ssh_private_key.as_bytes());
        let parsed = ssh_key::PrivateKey::from_openssh(&bytes).unwrap();
        assert_eq!(parsed.public_key(), host.public_key());
        let pod = render(
            &policy,
            "tools",
            &WorkerContext {
                assignment_generation: 1,
                environment_id: Uuid::new_v4(),
                environment_generation: 1,
            },
        )
        .unwrap()
        .pod()
        .unwrap();
        let pod = pod.spec.unwrap();
        let projected = pod
            .volumes
            .unwrap()
            .into_iter()
            .find(|v| v.name == "worker-host")
            .unwrap()
            .secret
            .unwrap();
        assert_eq!(projected.secret_name.as_deref(), Some(HOST_KEYS));
        assert_eq!(projected.default_mode, Some(288));
        let authorized = pod.containers[0]
            .volume_mounts
            .as_ref()
            .unwrap()
            .iter()
            .find(|m| m.name == "worker-ssh")
            .unwrap();
        assert_eq!(authorized.mount_path, "/etc/ssh/authorized_keys/worker");
        assert_eq!(authorized.sub_path.as_deref(), Some("worker"));
        assert_eq!(authorized.read_only, Some(true));
        assert!(
            pod.containers[0]
                .volume_mounts
                .as_ref()
                .unwrap()
                .iter()
                .any(|m| m.name == "worker-host"
                    && m.mount_path == "/etc/ssh/hostkeys"
                    && m.read_only == Some(true))
        );
    }

    fn policy() -> ResolvedPolicy {
        let instance_id = Uuid::new_v4();
        let installation_id = Uuid::new_v4();
        let resources = WorkerResources {
            cpu_millis: 1000,
            memory_mib: 1024,
            ephemeral_storage_mib: 1024,
        };
        let definition = RuntimeDefinition {
            schema_version: 1,
            runtime: RuntimeRef {
                id: "tools".into(),
                version: 1,
            },
            image: format!("ghcr.io/example/worker@sha256:{}", "a".repeat(64)),
            ssh_contract: "managed-ssh-v1".into(),
            uid: 1000,
            gid: 1000,
            service_account: "worker".into(),
            resources: resources.clone(),
            locale: "C.UTF-8".into(),
            egress: vec![],
            pull_secret_names: vec![],
        };
        ResolvedPolicy {
            installation_id,
            instance_id,
            namespace: instance_namespace(installation_id, instance_id),
            namespace_uid: "namespace-uid".into(),
            generation: 2,
            default_runtime_id: "tools".into(),
            runtimes: vec![ResolvedRuntime {
                definition_digest: definition.digest().unwrap(),
                definition,
                resources,
                mounts: vec![],
                pull_secrets: vec![],
                limits: Default::default(),
            }],
        }
    }
    #[test]
    fn worker_identity_is_generation_bound_and_runtime_selection_cannot_fallback() {
        let policy = policy();
        let context = WorkerContext {
            assignment_generation: 3,
            environment_id: Uuid::new_v4(),
            environment_generation: 1,
        };
        let first = render(&policy, "tools", &context).unwrap();
        let repeated = render(&policy, "tools", &context).unwrap();
        assert_eq!(first.resource.spec, repeated.resource.spec);
        let mut replacement = context.clone();
        replacement.environment_generation = 2;
        let replacement = render(&policy, "tools", &replacement).unwrap();
        assert_ne!(first.resource.spec.name, replacement.resource.spec.name);
        assert_ne!(
            first.resource.spec.manifest_digest,
            replacement.resource.spec.manifest_digest
        );
        assert!(matches!(
            render(&policy, "foreign", &context),
            Err(Error::Forbidden)
        ));
        let mut changed = policy.clone();
        changed.runtimes[0].resources.cpu_millis += 1;
        assert!(matches!(
            render(&changed, "tools", &context),
            Err(Error::Invalid)
        ));
        let mut changed = policy.clone();
        changed.runtimes[0].limits.ttl_seconds = 1;
        assert!(matches!(
            render(&changed, "tools", &context),
            Err(Error::Invalid)
        ));
    }
    #[test]
    fn worker_authorization_secret_accepts_only_public_key_and_bound_material() {
        use base64::{Engine as _, engine::general_purpose::STANDARD};
        let policy = policy();
        let native = crate::native_bootstrap::NativeBootstrap::generate(
            hermes_domain::TemplateVersion {
                id: "personal-default".into(),
                version: 1,
            },
            "https://manager.example.org/execution",
        )
        .unwrap();
        let material = Uuid::new_v4();
        assert!(authorized_keys(&policy, &native.ssh_public_key, Uuid::nil()).is_err());
        assert!(authorized_keys(&policy, &native.ssh_private_key, material).is_err());
        let public = format!("{} ignored-comment", native.ssh_public_key.trim());
        let rendered = authorized_keys(&policy, &public, material).unwrap();
        assert_eq!(rendered.spec.encrypted_material_ref, Some(material));
        let body = rendered.manifest.value();
        assert_eq!(body["data"].as_object().unwrap().len(), 1);
        let decoded = STANDARD
            .decode(body["data"]["worker"].as_str().unwrap())
            .unwrap();
        assert_eq!(decoded, native.ssh_public_key.as_bytes());
        assert!(
            !String::from_utf8(decoded)
                .unwrap()
                .contains("ignored-comment")
        );
    }
}
