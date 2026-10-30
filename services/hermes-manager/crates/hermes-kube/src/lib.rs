//! Kubernetes implementation of the controller's cluster port: bounded
//! observation, server-side apply with one field manager, preconditioned
//! deletes, cluster identity and leader election. Never reads a workstation
//! kubeconfig implicitly.

mod leader;
mod observe;

pub use kube::Client;
pub use leader::{LEASE_DURATION, LeaderElection, RENEW_DEADLINE, RETRY_PERIOD};

use std::collections::BTreeMap;
use std::time::Duration;

use async_trait::async_trait;
use hermes_core::app::controller::{
    ApplyRequest, Cluster, ClusterError, ClusterView, DesiredInstance, WorkerRequest,
};
use hermes_core::domain::execution::ObservedWorker;
use hermes_core::domain::instances::{Apply, Precondition, Workload};
use hermes_core::domain::orphans::{OrphanKind, OwnedObject};
use hermes_render::{INSTALLATION_LABEL, INSTANCE_LABEL, Object, RenderConfig, instance, worker};
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::{Namespace, PersistentVolume, Pod};
use kube::api::{DeleteParams, ListParams, Patch, PatchParams, Preconditions, PropagationPolicy};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::core::{ApiResource, DynamicObject, GroupVersionKind};
use kube::{Api, Config};
use serde_json::json;
use uuid::Uuid;

/// The one field manager the controller applies with.
pub const FIELD_MANAGER: &str = "hermes-manager";
/// Deadline of one Kubernetes call; below the leader renew deadline.
pub const CALL_DEADLINE: Duration = Duration::from_secs(5);

pub(crate) async fn timed<T>(
    call: impl Future<Output = Result<T, kube::Error>>,
) -> Result<T, ClusterError> {
    tokio::time::timeout(CALL_DEADLINE, call)
        .await
        .map_err(|_| ClusterError::Unavailable)?
        .map_err(cluster_error)
}

pub(crate) fn cluster_error(error: kube::Error) -> ClusterError {
    match error {
        kube::Error::Api(status) => match status.code {
            400 | 403 | 422 => ClusterError::Rejected,
            409 => ClusterError::Conflict,
            _ => ClusterError::Unavailable,
        },
        _ => ClusterError::Unavailable,
    }
}

/// A client from explicit configuration only: HTTPS, no exec or provider
/// plugins, no impersonation, bounded timeouts and no hidden retries.
pub fn client(mut config: Config) -> Result<Client, ClusterError> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let auth = &config.auth_info;
    if config.cluster_url.scheme_str() != Some("https")
        || config.accept_invalid_certs
        || config.proxy_url.is_some()
        || auth.exec.is_some()
        || auth.auth_provider.is_some()
        || auth.impersonate.is_some()
        || auth.impersonate_groups.is_some()
        || auth.impersonate_uid.is_some()
        || auth.impersonate_user_extra.is_some()
        || !config.headers.is_empty()
    {
        return Err(ClusterError::Rejected);
    }
    config.connect_timeout = Some(Duration::from_secs(2));
    config.read_timeout = Some(CALL_DEADLINE);
    config.write_timeout = Some(CALL_DEADLINE);
    // Uncertain writes are resolved by the next observation, never retried
    // invisibly.
    config.default_retry = false;
    Client::try_from(config).map_err(|_| ClusterError::Unavailable)
}

/// The in-cluster ServiceAccount configuration.
pub fn incluster() -> Result<Client, ClusterError> {
    client(Config::incluster().map_err(|_| ClusterError::Unavailable)?)
}

/// A kubeconfig file given explicitly (tests and operator tools), with its
/// context named. Refused before any client exists, and without running
/// anything: a missing or ambiguous context, cluster or user, exec and
/// provider plugins, impersonation, insecure TLS, proxies and relative
/// credential paths.
pub async fn from_kubeconfig(
    path: &std::path::Path,
    context: &str,
) -> Result<Client, ClusterError> {
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|_| ClusterError::Unavailable)?;
    if bytes.len() > 256 * 1024 {
        return Err(ClusterError::Rejected);
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| ClusterError::Rejected)?;
    let kubeconfig = Kubeconfig::from_yaml(text).map_err(|_| ClusterError::Rejected)?;
    if !explicit_kubeconfig(&kubeconfig, context) {
        return Err(ClusterError::Rejected);
    }
    let options = KubeConfigOptions {
        context: Some(context.to_owned()),
        ..KubeConfigOptions::default()
    };
    let config = Config::from_custom_kubeconfig(kubeconfig, &options)
        .await
        .map_err(|_| ClusterError::Rejected)?;
    client(config)
}

/// Exactly one context named `context`, whose cluster and user each exist
/// exactly once and use nothing beyond a token or certificate.
pub fn explicit_kubeconfig(config: &Kubeconfig, context: &str) -> bool {
    fn one<T>(items: &[T], name: impl Fn(&T) -> bool) -> Option<&T> {
        let mut matching = items.iter().filter(|item| name(item));
        match (matching.next(), matching.next()) {
            (Some(item), None) => Some(item),
            _ => None,
        }
    }
    let Some(named) = one(&config.contexts, |c| c.name == context) else {
        return false;
    };
    let Some(selected) = named.context.as_ref() else {
        return false;
    };
    let Some(user_name) = selected.user.as_deref() else {
        return false;
    };
    let cluster =
        one(&config.clusters, |c| c.name == selected.cluster).and_then(|c| c.cluster.as_ref());
    let user = one(&config.auth_infos, |u| u.name == user_name).and_then(|u| u.auth_info.as_ref());
    let (Some(cluster), Some(user)) = (cluster, user) else {
        return false;
    };
    let plain = cluster.insecure_skip_tls_verify != Some(true)
        && cluster.proxy_url.is_none()
        && user.exec.is_none()
        && user.auth_provider.is_none()
        && user.impersonate.is_none()
        && user.impersonate_uid.is_none()
        && user.impersonate_groups.is_none()
        && user.impersonate_user_extra.is_none();
    // Credential references never depend on the working directory.
    let absolute = [
        cluster.certificate_authority.as_deref(),
        user.token_file.as_deref(),
        user.client_certificate.as_deref(),
        user.client_key.as_deref(),
    ]
    .into_iter()
    .flatten()
    .all(|path| std::path::Path::new(path).is_absolute());
    plain && absolute
}

/// The cluster's identity: the UID of `kube-system`.
pub async fn cluster_identity(client: &Client) -> Result<String, ClusterError> {
    let namespaces: Api<Namespace> = Api::all(client.clone());
    timed(namespaces.get_metadata("kube-system"))
        .await?
        .metadata
        .uid
        .ok_or(ClusterError::Unavailable)
}

fn resource(object: &Object) -> Result<ApiResource, ClusterError> {
    let (group, version) = object
        .api_version
        .split_once('/')
        .unwrap_or(("", object.api_version));
    let plural = match object.kind {
        "Namespace" => "namespaces",
        "RoleBinding" => "rolebindings",
        "Role" => "roles",
        "ClusterRole" => "clusterroles",
        "ClusterRoleBinding" => "clusterrolebindings",
        "ServiceAccount" => "serviceaccounts",
        "NetworkPolicy" => "networkpolicies",
        "PersistentVolume" => "persistentvolumes",
        "PersistentVolumeClaim" => "persistentvolumeclaims",
        "Secret" => "secrets",
        "ConfigMap" => "configmaps",
        "Service" => "services",
        "Deployment" => "deployments",
        "Pod" => "pods",
        "ValidatingAdmissionPolicy" => "validatingadmissionpolicies",
        "ValidatingAdmissionPolicyBinding" => "validatingadmissionpolicybindings",
        _ => return Err(ClusterError::Rejected),
    };
    Ok(ApiResource::from_gvk_with_plural(
        &GroupVersionKind::gvk(group, version, object.kind),
        plural,
    ))
}

fn api(client: &Client, object: &Object) -> Result<Api<DynamicObject>, ClusterError> {
    let resource = resource(object)?;
    Ok(match &object.namespace {
        Some(namespace) => Api::namespaced_with(client.clone(), namespace, &resource),
        None => Api::all_with(client.clone(), &resource),
    })
}

/// Server-side applies each object in order with the controller's field
/// manager. PersistentVolumes are created once and never re-applied: their
/// binding is the cluster's.
pub async fn apply_objects(client: &Client, objects: &[Object]) -> Result<(), ClusterError> {
    let params = PatchParams::apply(FIELD_MANAGER).force();
    for object in objects {
        let api = api(client, object)?;
        if object.kind == "PersistentVolume" && timed(api.get_opt(&object.name)).await?.is_some() {
            continue;
        }
        timed(api.patch(&object.name, &params, &Patch::Apply(&object.body))).await?;
    }
    Ok(())
}

pub struct KubeCluster {
    client: Client,
    render: RenderConfig,
}

impl KubeCluster {
    pub fn new(client: Client, render: RenderConfig) -> Result<Self, ClusterError> {
        render.validate().map_err(|_| ClusterError::Rejected)?;
        Ok(Self { client, render })
    }

    pub fn client(&self) -> &Client {
        &self.client
    }
}

/// Namespaces carrying the installation's label.
/// Namespaces and PersistentVolumes with the installation label.
pub async fn owned_objects(
    client: &Client,
    installation: Uuid,
) -> Result<Vec<OwnedObject>, ClusterError> {
    let selector = format!("{INSTALLATION_LABEL}={installation}");
    let mut owned = labelled(
        Api::<Namespace>::all(client.clone()),
        &selector,
        OrphanKind::Namespace,
    )
    .await?;
    owned.extend(
        labelled(
            Api::<PersistentVolume>::all(client.clone()),
            &selector,
            OrphanKind::PersistentVolume,
        )
        .await?,
    );
    Ok(owned)
}

/// Every object matching `selector`, following list continuations.
async fn labelled<K>(
    api: Api<K>,
    selector: &str,
    kind: OrphanKind,
) -> Result<Vec<OwnedObject>, ClusterError>
where
    K: kube::Resource + Clone + k8s_openapi::serde::de::DeserializeOwned + std::fmt::Debug,
{
    let mut owned = Vec::new();
    let mut params = ListParams::default().labels(selector).limit(500);
    loop {
        let page = timed(api.list_metadata(&params)).await?;
        for item in page.items {
            let meta = item.metadata;
            let (Some(name), Some(uid)) = (meta.name, meta.uid) else {
                continue;
            };
            let instance = meta
                .labels
                .as_ref()
                .and_then(|labels| labels.get(INSTANCE_LABEL))
                .and_then(|value| Uuid::parse_str(value).ok());
            owned.push(OwnedObject {
                kind,
                name,
                uid,
                instance,
            });
        }
        match page.metadata.continue_.filter(|token| !token.is_empty()) {
            Some(token) => params = params.continue_token(&token),
            None => return Ok(owned),
        }
    }
}

pub async fn owned_namespaces(
    client: &Client,
    installation: uuid::Uuid,
) -> Result<Vec<String>, ClusterError> {
    let namespaces: Api<Namespace> = Api::all(client.clone());
    let selector = format!("{INSTALLATION_LABEL}={installation}");
    let list = timed(namespaces.list_metadata(&ListParams::default().labels(&selector).limit(500)))
        .await?;
    Ok(list
        .items
        .into_iter()
        .filter_map(|n| n.metadata.name)
        .collect())
}

#[async_trait]
impl Cluster for KubeCluster {
    async fn owned_objects(&self) -> Result<Vec<OwnedObject>, ClusterError> {
        owned_objects(&self.client, self.render.installation.id).await
    }

    async fn observe(&self, desired: &DesiredInstance) -> Result<ClusterView, ClusterError> {
        observe::observe(&self.client, &self.render, desired).await
    }

    async fn apply(&self, request: &ApplyRequest<'_>) -> Result<(), ClusterError> {
        match request.apply {
            Apply::Nothing => Ok(()),
            Apply::Everything(workload) => {
                let materials = request.materials.ok_or(ClusterError::Rejected)?;
                let objects = instance::render(
                    &self.render,
                    request.desired,
                    materials,
                    request.secrets,
                    workload,
                )
                .map_err(|_| ClusterError::Rejected)?;
                apply_objects(&self.client, &objects).await
            }
            Apply::Scale(workload) => {
                let deployments: Api<Deployment> =
                    Api::namespaced(self.client.clone(), &request.desired.namespace);
                let replicas = i32::from(workload == Workload::Run);
                let patch = Patch::Merge(json!({"spec": {"replicas": replicas}}));
                timed(deployments.patch("hermes", &PatchParams::default(), &patch))
                    .await
                    .map(|_| ())
            }
        }
    }

    async fn delete_deployment(
        &self,
        namespace: &str,
        precondition: &Precondition,
    ) -> Result<(), ClusterError> {
        let deployments: Api<Deployment> = Api::namespaced(self.client.clone(), namespace);
        let params = DeleteParams {
            preconditions: Some(Preconditions {
                uid: Some(precondition.uid.clone()),
                resource_version: Some(precondition.resource_version.clone()),
            }),
            propagation_policy: Some(PropagationPolicy::Background),
            ..DeleteParams::default()
        };
        match tokio::time::timeout(CALL_DEADLINE, deployments.delete("hermes", &params)).await {
            Err(_) => Err(ClusterError::Unavailable),
            Ok(Ok(_)) => Ok(()),
            Ok(Err(kube::Error::Api(status))) if status.code == 404 => Ok(()),
            Ok(Err(error)) => Err(cluster_error(error)),
        }
    }

    async fn observe_workers(
        &self,
        desired: &DesiredInstance,
    ) -> Result<BTreeMap<Uuid, ObservedWorker>, ClusterError> {
        observe::workers(&self.client, desired).await
    }

    async fn apply_worker(&self, request: &WorkerRequest<'_>) -> Result<(), ClusterError> {
        let pod = worker::render(
            &self.render,
            request.desired,
            request.environment,
            request.backend,
        )
        .map_err(|_| ClusterError::Rejected)?;
        apply_objects(&self.client, std::slice::from_ref(&pod)).await
    }

    async fn delete_worker(
        &self,
        namespace: &str,
        environment: Uuid,
        precondition: &Precondition,
    ) -> Result<(), ClusterError> {
        let pods: Api<Pod> = Api::namespaced(self.client.clone(), namespace);
        let params = DeleteParams {
            preconditions: Some(Preconditions {
                uid: Some(precondition.uid.clone()),
                resource_version: Some(precondition.resource_version.clone()),
            }),
            ..DeleteParams::default()
        };
        match tokio::time::timeout(
            CALL_DEADLINE,
            pods.delete(&worker::pod_name(environment), &params),
        )
        .await
        {
            Err(_) => Err(ClusterError::Unavailable),
            Ok(Ok(_)) => Ok(()),
            Ok(Err(kube::Error::Api(status))) if status.code == 404 => Ok(()),
            Ok(Err(error)) => Err(cluster_error(error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn explicit() -> Value {
        json!({
            "apiVersion": "v1", "kind": "Config",
            "current-context": "unselected-workstation-context",
            "contexts": [{"name": "controller", "context": {"cluster": "test", "user": "controller"}}],
            "clusters": [{"name": "test", "cluster": {"server": "https://127.0.0.1:1"}}],
            "users": [{"name": "controller", "user": {"token": "test-only-controller-token"}}],
        })
    }

    fn valid(config: &Value, context: &str) -> bool {
        explicit_kubeconfig(&serde_json::from_value(config.clone()).unwrap(), context)
    }

    #[test]
    fn explicit_context_and_credential_references_cannot_fall_back() {
        assert!(valid(&explicit(), "controller"));
        assert!(!valid(&explicit(), "missing"));
        assert!(!valid(&explicit(), "unselected-workstation-context"));
        let mut relative = explicit();
        relative["users"][0]["user"]["tokenFile"] = json!("relative-token-file");
        assert!(!valid(&relative, "controller"));
        let mut duplicate = explicit();
        let first = duplicate["contexts"][0].clone();
        duplicate["contexts"].as_array_mut().unwrap().push(first);
        assert!(!valid(&duplicate, "controller"));
        for (pointer, value) in [
            ("/clusters/0/cluster/insecure-skip-tls-verify", json!(true)),
            ("/clusters/0/cluster/proxy-url", json!("http://proxy:3128")),
            ("/users/0/user/as", json!("system:admin")),
            ("/users/0/user/as-groups", json!(["system:masters"])),
        ] {
            let mut refused = explicit();
            let (parent, key) = pointer.rsplit_once('/').unwrap();
            refused.pointer_mut(parent).unwrap()[key] = value;
            assert!(!valid(&refused, "controller"), "{pointer}");
        }
    }

    #[tokio::test]
    async fn the_client_refuses_what_could_redirect_or_widen_its_credential() {
        let config = || {
            let mut config = Config::new("https://127.0.0.1:1".parse().unwrap());
            config.auth_info.token = Some("test-only-controller-token".to_owned().into());
            config
        };
        assert!(client(config()).is_ok(), "the valid control builds");
        type Widen = fn(&mut Config);
        let cases: [(&str, Widen); 8] = [
            ("plain HTTP", |c| {
                c.cluster_url = "http://127.0.0.1:1".parse().unwrap()
            }),
            ("skipped certificate checks", |c| {
                c.accept_invalid_certs = true
            }),
            ("a proxy", |c| {
                c.proxy_url = Some("http://proxy:3128".parse().unwrap())
            }),
            ("impersonating a user", |c| {
                c.auth_info.impersonate = Some("system:admin".into())
            }),
            ("impersonating groups", |c| {
                c.auth_info.impersonate_groups = Some(vec!["system:masters".into()])
            }),
            ("impersonating a UID", |c| {
                c.auth_info.impersonate_uid = Some("0".into())
            }),
            ("an extra header", |c| {
                c.headers.push((
                    "x-forwarded-for".parse().unwrap(),
                    "10.0.0.1".parse().unwrap(),
                ))
            }),
            ("an executable credential plugin", |c| {
                c.auth_info.exec = Some(
                    serde_json::from_value(json!({
                        "apiVersion": "client.authentication.k8s.io/v1",
                        "command": "true",
                    }))
                    .unwrap(),
                )
            }),
        ];
        for (case, widen) in cases {
            let mut config = config();
            widen(&mut config);
            assert_eq!(client(config).err(), Some(ClusterError::Rejected), "{case}");
        }
    }

    #[tokio::test]
    async fn executable_kubeconfig_is_rejected_without_running_its_command() {
        let directory = std::env::temp_dir().join(format!("hermes-kube-{}", Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let marker = directory.join("auth-plugin-was-run");
        let mut config = explicit();
        config["users"][0]["user"]["exec"] = json!({
            "apiVersion": "client.authentication.k8s.io/v1",
            "command": "touch", "args": [marker], "interactiveMode": "Never",
        });
        let path = directory.join("config.json");
        std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
        let result = from_kubeconfig(&path, "controller").await;
        assert_eq!(result.err(), Some(ClusterError::Rejected));
        assert!(!marker.exists());
        std::fs::remove_dir_all(&directory).unwrap();
    }
}
