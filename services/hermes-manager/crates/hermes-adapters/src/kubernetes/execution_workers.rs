//! Direct worker Pods: committed immutable rendering, authenticated creation,
//! exact UID binding before gate removal, and read-only readiness candidates.
use super::*;
use crate::execution_workloads::{self, WorkerContext};
use hermes_application::{
    namespace_fence::{ACCESS, ADMISSION, EPOCH, GENERATION},
    terminal::leases::{WorkerBinding, WorkerIntent, WorkerStartIntent},
};
use k8s_openapi::api::core::v1::{Namespace, PersistentVolumeClaim, Pod, Secret};
use serde_json::json;
use std::net::IpAddr;

/// Kubernetes readiness alone cannot authorize a lease. A caller must also
/// authenticate SSH with the owner's private key before committing WorkerReady.
pub struct ExecutionWorkerCandidate {
    pub binding: WorkerBinding,
    pub host: IpAddr,
}

impl KubernetesDriver {
    fn worker_creation(&self, intent: &WorkerIntent) -> Result<(OwnedResource, Manifest), Error> {
        let claim = &intent.claim;
        if claim.installation_id != claim.policy.installation_id
            || claim.instance_id != claim.policy.instance_id
            || claim.epoch < 1
            || claim.fence < 1
            || claim.worker.is_nil()
        {
            return Err(Error::Invalid);
        }
        let rendered = execution_workloads::render(
            &claim.policy,
            &claim.runtime_id,
            &WorkerContext {
                assignment_generation: claim.assignment_generation,
                environment_id: claim.environment_id,
                environment_generation: claim.generation,
            },
        )?
        .resource;
        if rendered.spec.manifest_digest != intent.spec_digest
            || rendered.spec.name != claim.pod_name
        {
            return Err(Error::OwnershipConflict);
        }
        // The environment/generation is the durable execution action identity,
        // separate from the assignment operation journal. Its creation identity
        // is stable across controller fences and uncertain CREATE responses.
        let creation = hermes_application::resources::Creation {
            id: claim.environment_id,
            installation_id: claim.installation_id,
            instance_id: claim.instance_id,
            cluster_identity: self.cluster_identity.clone(),
            epoch: claim.epoch,
            generation: claim.generation,
            operation_id: claim.environment_id,
            spec: rendered.spec,
        };
        Ok((
            OwnedResource {
                creation,
                uid: None,
                resource_version: None,
                observed_digest: None,
                state: "Reserved".into(),
            },
            rendered.manifest,
        ))
    }
    async fn execution_namespace(
        &self,
        owned: &OwnedResource,
        intent: &WorkerIntent,
    ) -> Result<(), Error> {
        self.namespace(&owned.creation).await?;
        let namespace: Api<Namespace> = Api::all(self.client.clone());
        let ns = timeout(
            Duration::from_secs(6),
            namespace.get(&intent.claim.policy.namespace),
        )
        .await
        .map_err(|_| Error::Unavailable)?
        .map_err(api_error)?;
        let labels = ns.metadata.labels.ok_or(Error::OwnershipConflict)?;
        if ns.metadata.uid.as_ref() != Some(&intent.claim.policy.namespace_uid)
            || labels.get(ACCESS).map(String::as_str) != Some("enabled")
            || labels.get(ADMISSION).map(String::as_str) != Some("open")
            || labels.get(GENERATION) != Some(&intent.claim.assignment_generation.to_string())
            || labels.get(EPOCH) != Some(&intent.claim.epoch.to_string())
        {
            return Err(Error::OwnershipConflict);
        }
        Ok(())
    }
    async fn execution_references(&self, intent: &WorkerIntent) -> Result<(), Error> {
        let runtime = intent
            .claim
            .policy
            .runtimes
            .iter()
            .find(|runtime| runtime.definition.runtime.id == intent.claim.runtime_id)
            .ok_or(Error::Forbidden)?;
        let claims: Api<PersistentVolumeClaim> =
            Api::namespaced(self.client.clone(), &intent.claim.policy.namespace);
        let secrets: Api<Secret> =
            Api::namespaced(self.client.clone(), &intent.claim.policy.namespace);
        for mount in &runtime.mounts {
            let claim = timeout(Duration::from_secs(6), claims.get(&mount.binding.pvc_name))
                .await
                .map_err(|_| Error::Unavailable)?
                .map_err(api_error)?;
            if claim.metadata.uid.as_ref() != Some(&mount.binding.pvc_uid)
                || claim.metadata.deletion_timestamp.is_some()
            {
                return Err(Error::OwnershipConflict);
            }
            if claim.status.as_ref().and_then(|s| s.phase.as_deref()) != Some("Bound") {
                return Err(Error::PreconditionFailed);
            }
        }
        for selected in &runtime.pull_secrets {
            // Credential values are unnecessary: Kubernetes consumes the exact
            // registered Secret reference, and this observer verifies its UID.
            let secret = timeout(Duration::from_secs(6), secrets.get_metadata(&selected.name))
                .await
                .map_err(|_| Error::Unavailable)?
                .map_err(api_error)?;
            if secret.metadata.uid.as_ref() != Some(&selected.uid)
                || secret.metadata.deletion_timestamp.is_some()
            {
                return Err(Error::OwnershipConflict);
            }
        }
        Ok(())
    }
    fn execution_binding(
        &self,
        owned: &OwnedResource,
        manifest: &Manifest,
        object: &DynamicObject,
    ) -> Result<WorkerBinding, Error> {
        self.verify(owned, object)?;
        if object
            .metadata
            .owner_references
            .as_ref()
            .is_some_and(|owners| !owners.is_empty())
            || object
                .metadata
                .annotations
                .as_ref()
                .and_then(|a| a.get(INTENT))
                != Some(&manifest.digest()?)
        {
            return Err(Error::OwnershipConflict);
        }
        let mut actual = serde_json::to_value(object).map_err(|_| Error::Invalid)?;
        let mut wanted = manifest.value().clone();
        for value in [&mut actual, &mut wanted] {
            value
                .get_mut("spec")
                .and_then(Value::as_object_mut)
                .ok_or(Error::OwnershipConflict)?
                .remove("schedulingGates");
        }
        if !contains_fields(&actual, &wanted) {
            return Err(Error::OwnershipConflict);
        }
        let pod: Pod =
            serde_json::from_value(serde_json::to_value(object).map_err(|_| Error::Invalid)?)
                .map_err(|_| Error::OwnershipConflict)?;
        let gates = pod
            .spec
            .as_ref()
            .ok_or(Error::OwnershipConflict)?
            .scheduling_gates
            .as_deref()
            .unwrap_or(&[]);
        let gated = match gates {
            [] => false,
            [gate] if gate.name == execution_workloads::START_GATE => true,
            _ => return Err(Error::OwnershipConflict),
        };
        let observed_spec_digest = native_children::pod_digest(&pod)?;
        Ok(WorkerBinding {
            pod_uid: pod.metadata.uid.ok_or(Error::OwnershipConflict)?,
            resource_version: pod
                .metadata
                .resource_version
                .filter(|v| hermes_domain::resources::valid_uid(v))
                .ok_or(Error::OwnershipConflict)?,
            observed_spec_digest,
            gated,
        })
    }
    /// Caller has committed prepare_execution_worker before this method. A
    /// matching name or labels never permit adoption without creation proof.
    pub async fn ensure_execution_worker(
        &self,
        intent: &WorkerIntent,
    ) -> Result<WorkerBinding, Error> {
        timeout(Duration::from_secs(25), async {
            let (owned, manifest) = self.worker_creation(intent)?;
            self.execution_namespace(&owned, intent).await?;
            self.execution_references(intent).await?;
            let api = self.api(&owned.creation.spec);
            let observed = timeout(Duration::from_secs(6), api.get_opt(&intent.claim.pod_name))
                .await
                .map_err(|_| Error::Unavailable)?
                .map_err(api_error)?;
            let object = if let Some(object) = observed {
                object
            } else {
                timeout(
                    Duration::from_secs(6),
                    api.create(
                        &PostParams::default(),
                        &self.decorated(&owned.creation, &manifest)?,
                    ),
                )
                .await
                .map_err(|_| Error::Unavailable)?
                .map_err(api_error)?
            };
            self.execution_binding(&owned, &manifest, &object)
        })
        .await
        .map_err(|_| Error::Unavailable)?
    }
    /// Caller has durably bound the exact UID and committed this exact start
    /// input. RV changes require a fresh journaled action before another CAS.
    pub async fn start_execution_worker(
        &self,
        start: &WorkerStartIntent,
    ) -> Result<WorkerBinding, Error> {
        timeout(Duration::from_secs(25), async {
            let (mut owned, manifest) = self.worker_creation(&start.intent)?;
            owned.uid = Some(start.pod_uid.clone());
            self.execution_namespace(&owned, &start.intent).await?;
            self.execution_references(&start.intent).await?;
            let api = self.api(&owned.creation.spec);
            let object = timeout(Duration::from_secs(6), api.get(&start.intent.claim.pod_name)).await.map_err(|_|Error::Unavailable)?.map_err(api_error)?;
            let before = self.execution_binding(&owned, &manifest, &object)?;
            if before.observed_spec_digest != start.observed_spec_digest { return Err(Error::OwnershipConflict); }
            if !before.gated { return Ok(before); }
            if before.resource_version != start.resource_version { return Err(Error::PreconditionFailed); }
            let patch = json!({"metadata":{"uid":start.pod_uid,"resourceVersion":start.resource_version},"spec":{"schedulingGates":[]}});
            let updated = timeout(Duration::from_secs(6), api.patch(&start.intent.claim.pod_name, &PatchParams::default(), &Patch::Merge(patch))).await.map_err(|_|Error::Unavailable)?.map_err(api_error)?;
            let result = self.execution_binding(&owned, &manifest, &updated)?;
            if result.gated || result.observed_spec_digest != start.observed_spec_digest { return Err(Error::OwnershipConflict); }
            Ok(result)
        }).await.map_err(|_|Error::Unavailable)?
    }
    pub async fn execution_worker_candidate(
        &self,
        start: &WorkerStartIntent,
    ) -> Result<Option<ExecutionWorkerCandidate>, Error> {
        timeout(Duration::from_secs(25), async {
            let (mut owned, manifest) = self.worker_creation(&start.intent)?;
            owned.uid = Some(start.pod_uid.clone());
            self.execution_namespace(&owned, &start.intent).await?;
            self.execution_references(&start.intent).await?;
            let object = timeout(
                Duration::from_secs(6),
                self.api(&owned.creation.spec)
                    .get(&start.intent.claim.pod_name),
            )
            .await
            .map_err(|_| Error::Unavailable)?
            .map_err(api_error)?;
            let binding = self.execution_binding(&owned, &manifest, &object)?;
            if binding.observed_spec_digest != start.observed_spec_digest {
                return Err(Error::OwnershipConflict);
            }
            let pod: Pod =
                serde_json::from_value(serde_json::to_value(object).map_err(|_| Error::Invalid)?)
                    .map_err(|_| Error::OwnershipConflict)?;
            let Some(status) = pod.status else {
                return Ok(None);
            };
            let ready = status
                .container_statuses
                .as_deref()
                .is_some_and(|containers| {
                    containers.len() == 1
                        && containers[0].name == "execution"
                        && containers[0].ready
                        && containers[0]
                            .state
                            .as_ref()
                            .is_some_and(|state| state.running.is_some())
                });
            if binding.gated || status.phase.as_deref() != Some("Running") || !ready {
                return Ok(None);
            }
            let host: IpAddr = status
                .pod_ip
                .ok_or(Error::OwnershipConflict)?
                .parse()
                .map_err(|_| Error::OwnershipConflict)?;
            if host.is_unspecified() || host.is_loopback() || host.is_multicast() {
                return Err(Error::OwnershipConflict);
            }
            Ok(Some(ExecutionWorkerCandidate { binding, host }))
        })
        .await
        .map_err(|_| Error::Unavailable)?
    }
}
