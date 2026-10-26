//! Bounded reads of one instance namespace: the namespace, the Hermes
//! Deployment and its current ReplicaSet and Pod, worker Pods, claims and the
//! mount failures of a Hermes Pod that has not started.

use std::collections::BTreeMap;

use hermes_core::domain::execution::{ObservedWorker, WorkerFailure};
use hermes_render::worker::ENVIRONMENT_LABEL;
use uuid::Uuid;

use hermes_core::app::controller::{ClusterError, ClusterView, DesiredInstance};
use hermes_core::domain::instances::{
    ClaimPhase, NativeCheck, Observed, ObservedDeployment, ObservedNamespace,
};
use hermes_render::{
    COMPONENT_LABEL, GENERATION_ANNOTATION, INSTALLATION_LABEL, INSTANCE_LABEL, MANAGED_BY,
    MANAGER, RenderConfig,
};
use k8s_openapi::api::apps::v1::{Deployment, ReplicaSet};
use k8s_openapi::api::core::v1::{Event, Namespace, PersistentVolumeClaim, Pod};
use kube::api::ListParams;
use kube::{Api, Client};

use crate::timed;

const REVISION: &str = "deployment.kubernetes.io/revision";
/// Lists are bounded; an instance namespace never holds more.
const LIST_LIMIT: u32 = 200;
const INIT_CONTAINER: &str = "native-home";

fn list() -> ListParams {
    ListParams::default().limit(LIST_LIMIT)
}

pub(crate) async fn observe(
    client: &Client,
    render: &RenderConfig,
    desired: &DesiredInstance,
) -> Result<ClusterView, ClusterError> {
    let namespaces: Api<Namespace> = Api::all(client.clone());
    let Some(namespace) = timed(namespaces.get_opt(&desired.namespace)).await? else {
        return Ok(ClusterView {
            observed: Observed::absent(),
            receipt: None,
        });
    };
    let labels = namespace.metadata.labels.unwrap_or_default();
    let instance = desired.instance_id.to_string();
    let owned = labels.get(INSTALLATION_LABEL) == Some(&render.installation.id.to_string())
        && labels.get(INSTANCE_LABEL) == Some(&instance)
        && labels.get(MANAGED_BY).map(String::as_str) == Some(MANAGER);
    let observed_namespace = ObservedNamespace {
        uid: namespace.metadata.uid.unwrap_or_default(),
        owned,
        terminating: namespace.metadata.deletion_timestamp.is_some(),
    };
    if !owned {
        // Nothing inside a namespace that is not ours is read or trusted.
        return Ok(ClusterView {
            observed: Observed {
                namespace: Some(observed_namespace),
                ..Observed::absent()
            },
            receipt: None,
        });
    }
    let ns = desired.namespace.as_str();
    let deployments: Api<Deployment> = Api::namespaced(client.clone(), ns);
    let replica_sets: Api<ReplicaSet> = Api::namespaced(client.clone(), ns);
    let pods: Api<Pod> = Api::namespaced(client.clone(), ns);
    let claims: Api<PersistentVolumeClaim> = Api::namespaced(client.clone(), ns);
    let hermes = format!("{COMPONENT_LABEL}=hermes,{INSTANCE_LABEL}={instance}");
    let workers = format!("{COMPONENT_LABEL}=worker");

    let deployment = timed(deployments.get_opt("hermes")).await?;
    let hermes_pods = timed(pods.list(&list().labels(&hermes))).await?.items;
    let worker_pods = timed(pods.list_metadata(&list().labels(&workers)))
        .await?
        .items
        .len();
    let claims: BTreeMap<String, ClaimPhase> = timed(claims.list(&list()))
        .await?
        .items
        .into_iter()
        .filter_map(|claim| {
            let name = claim.metadata.name?;
            let phase = match claim.status.and_then(|s| s.phase).as_deref() {
                Some("Bound") => ClaimPhase::Bound,
                Some("Lost") => ClaimPhase::Lost,
                _ => ClaimPhase::Pending,
            };
            Some((name, phase))
        })
        .collect();

    let mut current = None;
    let observed_deployment = match &deployment {
        None => None,
        Some(deployment) => {
            let replicas = deployment
                .spec
                .as_ref()
                .and_then(|s| s.replicas)
                .unwrap_or(1);
            let status = deployment.status.clone().unwrap_or_default();
            let rolled_out = status.observed_generation == deployment.metadata.generation
                && status.replicas.unwrap_or(0) == replicas
                && status.updated_replicas.unwrap_or(0) == replicas
                && status.available_replicas.unwrap_or(0) == replicas
                && status.ready_replicas.unwrap_or(0) == replicas
                && status.unavailable_replicas.unwrap_or(0) == 0;
            let applied_generation = deployment
                .metadata
                .annotations
                .as_ref()
                .and_then(|a| a.get(GENERATION_ANNOTATION))
                .and_then(|g| g.parse().ok());
            if rolled_out && replicas == 1 {
                let sets = timed(replica_sets.list(&list().labels(&hermes)))
                    .await?
                    .items;
                current = current_pod(deployment, &sets, &hermes_pods);
            }
            Some(ObservedDeployment {
                uid: deployment.metadata.uid.clone().unwrap_or_default(),
                resource_version: deployment
                    .metadata
                    .resource_version
                    .clone()
                    .unwrap_or_default(),
                replicas,
                applied_generation,
                rolled_out,
            })
        }
    };
    let mount_failed = if current.is_none() {
        mount_failed(client, ns, &hermes_pods).await?
    } else {
        false
    };
    let receipt = current.and_then(receipt);
    Ok(ClusterView {
        observed: Observed {
            namespace: Some(observed_namespace),
            deployment: observed_deployment,
            hermes_pods: u32::try_from(hermes_pods.len()).unwrap_or(u32::MAX),
            worker_pods: u32::try_from(worker_pods).unwrap_or(u32::MAX),
            current_pod_ready: current.is_some(),
            claims,
            native: NativeCheck::NotChecked,
            mount_failed,
        },
        receipt,
    })
}

/// Whether the kubelet reports a failed volume mount for a Hermes Pod that
/// is not running, as for a static directory that does not exist.
async fn mount_failed(client: &Client, ns: &str, pods: &[Pod]) -> Result<bool, ClusterError> {
    let starting: Vec<&str> = pods
        .iter()
        .filter(|pod| pod.metadata.deletion_timestamp.is_none())
        .filter(|pod| {
            pod.status
                .as_ref()
                .and_then(|s| s.conditions.as_ref())
                .is_none_or(|c| !c.iter().any(|c| c.type_ == "Ready" && c.status == "True"))
        })
        .filter_map(|pod| pod.metadata.uid.as_deref())
        .collect();
    if starting.is_empty() {
        return Ok(false);
    }
    let events: Api<Event> = Api::namespaced(client.clone(), ns);
    let failed = timed(events.list(&list().fields("involvedObject.kind=Pod,reason=FailedMount")))
        .await?
        .items;
    Ok(failed.iter().any(|event| {
        event
            .involved_object
            .uid
            .as_deref()
            .is_some_and(|uid| starting.contains(&uid))
    }))
}

fn controller_of(
    owners: Option<&Vec<k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference>>,
) -> Option<&k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference> {
    owners?.iter().find(|o| o.controller == Some(true))
}

/// The one ready Pod of the Deployment's current revision, reached through
/// the owner chain Deployment → ReplicaSet → Pod.
fn current_pod<'a>(
    deployment: &Deployment,
    sets: &[ReplicaSet],
    pods: &'a [Pod],
) -> Option<&'a Pod> {
    let uid = deployment.metadata.uid.as_deref()?;
    let revision = deployment.metadata.annotations.as_ref()?.get(REVISION)?;
    let [pod] = pods else {
        return None;
    };
    if pod.metadata.deletion_timestamp.is_some() {
        return None;
    }
    let owner = controller_of(pod.metadata.owner_references.as_ref())?;
    let set = sets
        .iter()
        .find(|s| s.metadata.uid.as_deref() == Some(owner.uid.as_str()))?;
    let set_owner = controller_of(set.metadata.owner_references.as_ref())?;
    if set_owner.uid != uid || set.metadata.annotations.as_ref()?.get(REVISION) != Some(revision) {
        return None;
    }
    let ready = pod
        .status
        .as_ref()?
        .conditions
        .as_ref()?
        .iter()
        .any(|c| c.type_ == "Ready" && c.status == "True");
    ready.then_some(pod)
}

/// The init container's completion message: the home receipt.
fn receipt(pod: &Pod) -> Option<String> {
    let statuses = pod.status.as_ref()?.init_container_statuses.as_ref()?;
    let status = statuses.iter().find(|s| s.name == INIT_CONTAINER)?;
    let terminated = status.state.as_ref()?.terminated.as_ref()?;
    (terminated.exit_code == 0)
        .then(|| terminated.message.clone())
        .flatten()
}

/// Worker Pods of the instance by their environment label.
pub(crate) async fn workers(
    client: &Client,
    desired: &DesiredInstance,
) -> Result<BTreeMap<Uuid, ObservedWorker>, ClusterError> {
    let pods: Api<Pod> = Api::namespaced(client.clone(), &desired.namespace);
    let selector = format!(
        "{COMPONENT_LABEL}=worker,{INSTANCE_LABEL}={}",
        desired.instance_id
    );
    let mut workers = BTreeMap::new();
    for pod in timed(pods.list(&list().labels(&selector))).await?.items {
        let Some(environment) = pod
            .metadata
            .labels
            .as_ref()
            .and_then(|l| l.get(ENVIRONMENT_LABEL))
            .and_then(|e| Uuid::parse_str(e).ok())
        else {
            continue;
        };
        let status = pod.status.as_ref();
        let phase = status.and_then(|s| s.phase.as_deref());
        let gone = pod.metadata.deletion_timestamp.is_some()
            || matches!(phase, Some("Failed" | "Succeeded"));
        let ready = status
            .and_then(|s| s.conditions.as_ref())
            .is_some_and(|c| c.iter().any(|c| c.type_ == "Ready" && c.status == "True"));
        let address = status
            .and_then(|s| s.pod_ip.clone())
            .filter(|_| ready && !gone);
        let failure = worker_failure(&pod, phase);
        workers.insert(
            environment,
            ObservedWorker {
                uid: pod.metadata.uid.clone().unwrap_or_default(),
                resource_version: pod.metadata.resource_version.clone().unwrap_or_default(),
                ready: address,
                gone,
                failure,
            },
        );
    }
    Ok(workers)
}

/// A definite start failure in the worker Pod's status. A first pull error
/// is retried by the kubelet and only counts once it backs off.
fn worker_failure(pod: &Pod, phase: Option<&str>) -> Option<WorkerFailure> {
    if pod.metadata.deletion_timestamp.is_some() {
        return None;
    }
    if matches!(phase, Some("Failed" | "Succeeded")) {
        return Some(WorkerFailure::StartFailed);
    }
    let statuses = pod.status.as_ref()?.container_statuses.as_ref()?;
    statuses.iter().find_map(|status| {
        let reason = status.state.as_ref()?.waiting.as_ref()?.reason.as_deref()?;
        match reason {
            "ImagePullBackOff" | "InvalidImageName" | "ErrImageNeverPull" => {
                Some(WorkerFailure::ImageUnavailable)
            }
            "CrashLoopBackOff" | "CreateContainerConfigError" | "CreateContainerError" => {
                Some(WorkerFailure::StartFailed)
            }
            _ => None,
        }
    })
}
