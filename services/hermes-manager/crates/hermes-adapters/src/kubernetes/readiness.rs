//! Read-only native observations. Passing these checks never publishes a route.
use super::*;
use crate::{native_bootstrap::NativeBootstrap, native_receipt};
use hermes_application::{commissioning::ConditionReason, materials::MaterialContext};
use hermes_domain::Identity;
use k8s_openapi::api::{apps::v1::ReplicaSet, core::v1::Pod};
use kube::api::ListParams;

/// All inputs come from the current fenced plan and its bound resource records.
/// There is deliberately no boolean that callers can turn into route readiness.
pub struct NativeReadiness<'a> {
    pub namespace: &'a OwnedResource,
    pub storage: [(&'a OwnedResource, &'a Manifest); 2],
    pub deployment: (&'a OwnedResource, &'a Manifest),
    pub epoch: i64,
    pub generation: i64,
    pub material: &'a MaterialContext,
    pub bootstrap: &'a NativeBootstrap,
    pub started_children: &'a [hermes_application::native_children::NativeChildInput],
    pub owner: &'a Identity,
}

impl KubernetesDriver {
    pub async fn native_readiness(
        &self,
        input: NativeReadiness<'_>,
    ) -> Result<ConditionReason, Error> {
        // An observation cannot outlive the controller's 30-second claim lease.
        // Any subsequent persistence still has to compare its fence/generation.
        timeout(Duration::from_secs(20), self.native_readiness_inner(input))
            .await
            .map_err(|_| Error::Unavailable)?
    }

    pub(super) async fn readiness_object(
        &self,
        owned: &OwnedResource,
        manifest: Option<&Manifest>,
    ) -> Result<DynamicObject, Error> {
        if owned.uid.is_none() || owned.state != "Bound" {
            return Err(Error::OwnershipConflict);
        }
        self.namespace(&owned.creation).await?;
        let spec = &owned.creation.spec;
        let object = timeout(Duration::from_secs(6), self.api(spec).get_opt(&spec.name))
            .await
            .map_err(|_| Error::Unavailable)?
            .map_err(api_error)?
            .ok_or(Error::OwnershipConflict)?;
        self.verify(owned, &object)?;
        if let Some(manifest) = manifest {
            let digest = manifest.digest()?;
            let mut expected = spec.clone();
            expected.manifest_digest = digest.clone();
            manifest.validate(&expected)?;
            if owned.observed_digest.as_ref() != Some(&digest)
                || object
                    .metadata
                    .annotations
                    .as_ref()
                    .and_then(|a| a.get(INTENT))
                    != Some(&digest)
                || !contains_fields(
                    &serde_json::to_value(&object).map_err(|_| Error::Invalid)?,
                    manifest.value(),
                )
            {
                return Err(Error::OwnershipConflict);
            }
        }
        Ok(object)
    }

    async fn native_readiness_inner(
        &self,
        input: NativeReadiness<'_>,
    ) -> Result<ConditionReason, Error> {
        input.validate()?;
        self.readiness_object(input.namespace, None).await?;
        for (owned, manifest) in input.storage {
            let pvc = self.readiness_object(owned, Some(manifest)).await?;
            if pvc.data.pointer("/status/phase").and_then(Value::as_str) != Some("Bound") {
                return Ok(ConditionReason::StoragePending);
            }
        }
        let deployment = self
            .readiness_object(input.deployment.0, Some(input.deployment.1))
            .await?;
        if !deployment_ready(&deployment) {
            return Ok(ConditionReason::NativePending);
        }
        let namespace = input
            .deployment
            .0
            .creation
            .spec
            .namespace
            .as_deref()
            .ok_or(Error::Invalid)?;
        let pods: Api<Pod> = Api::namespaced(self.client.clone(), namespace);
        self.namespace(&input.deployment.0.creation).await?;
        let selector = format!(
            "{INSTANCE}={},app.kubernetes.io/component=hermes",
            input.material.instance_id
        );
        let listed = timeout(
            Duration::from_secs(6),
            pods.list(&ListParams::default().labels(&selector).limit(32)),
        )
        .await
        .map_err(|_| Error::Unavailable)?
        .map_err(api_error)?;
        if listed
            .metadata
            .continue_
            .as_ref()
            .is_some_and(|s| !s.is_empty())
        {
            return Err(Error::Unavailable);
        }
        // Recreate strategy means exactly one live Pod. Never accept a healthy
        // old generation while a second writer is terminating or being created.
        if listed.items.len() != 1 {
            return Ok(ConditionReason::NativePending);
        }
        let pod = &listed.items[0];
        if pod.metadata.deletion_timestamp.is_some() {
            return Ok(ConditionReason::NativePending);
        }
        let reference = controller_owner(&pod.metadata, "ReplicaSet")?;
        let sets: Api<ReplicaSet> = Api::namespaced(self.client.clone(), namespace);
        self.namespace(&input.deployment.0.creation).await?;
        let set = timeout(Duration::from_secs(6), sets.get_opt(&reference.name))
            .await
            .map_err(|_| Error::Unavailable)?
            .map_err(api_error)?
            .ok_or(Error::OwnershipConflict)?;
        if !pod_ready(pod, &set, &deployment, &input)? {
            return Ok(ConditionReason::NativePending);
        }
        // Native, storage and authenticated initialization are observed. This
        // says nothing about backend HTTP auth or effective execution policy.
        Ok(ConditionReason::ExecutionPolicyPending)
    }
}

impl NativeReadiness<'_> {
    fn validate(&self) -> Result<(), Error> {
        self.material.validate()?;
        self.owner.validate(&self.owner.issuer)?;
        let deployment = self.deployment.0;
        let namespace = self.namespace;
        if self.epoch < 1
            || self.generation < 1
            || namespace.creation.spec.kind != ResourceKind::Namespace
            || deployment.creation.spec.kind != ResourceKind::Deployment
            || deployment.creation.spec.name != "hermes"
            || namespace.creation.installation_id != deployment.creation.installation_id
            || namespace.creation.instance_id != deployment.creation.instance_id
            || namespace.creation.cluster_identity != deployment.creation.cluster_identity
            || deployment.creation.spec.namespace.as_ref() != Some(&namespace.creation.spec.name)
            || deployment.creation.spec.namespace_uid != namespace.uid
            || self.material.installation_id != deployment.creation.installation_id
            || self.material.instance_id != deployment.creation.instance_id
            || self.material.epoch > self.epoch
            || self.material.generation > self.generation
            || self.material.purpose != crate::native_bootstrap::PURPOSE
        {
            return Err(Error::OwnershipConflict);
        }
        // Creation provenance and home credentials retain their original epoch
        // through coordinated recovery; future-epoch records are never valid.
        for owned in [namespace, self.storage[0].0, self.storage[1].0, deployment] {
            owned.creation.spec.validate()?;
            if !(1..=self.epoch).contains(&owned.creation.epoch)
                || !(1..=self.generation).contains(&owned.creation.generation)
            {
                return Err(Error::OwnershipConflict);
            }
        }
        for ((claim, _), name) in self.storage.iter().zip(["home", "workspace"]) {
            if claim.creation.spec.kind != ResourceKind::PersistentVolumeClaim
                || claim.creation.spec.name != name
                || claim.creation.installation_id != deployment.creation.installation_id
                || claim.creation.instance_id != deployment.creation.instance_id
                || claim.creation.cluster_identity != deployment.creation.cluster_identity
                || claim.creation.spec.namespace != deployment.creation.spec.namespace
                || claim.creation.spec.namespace_uid != namespace.uid
            {
                return Err(Error::OwnershipConflict);
            }
        }
        Ok(())
    }
}

fn deployment_ready(deployment: &DynamicObject) -> bool {
    let Some(generation) = deployment.metadata.generation.filter(|g| *g > 0) else {
        return false;
    };
    deployment
        .data
        .pointer("/spec/replicas")
        .and_then(Value::as_i64)
        == Some(1)
        && deployment
            .data
            .pointer("/status/observedGeneration")
            .and_then(Value::as_i64)
            == Some(generation)
        && [
            "replicas",
            "updatedReplicas",
            "readyReplicas",
            "availableReplicas",
        ]
        .iter()
        .all(|key| deployment.data["status"][key].as_i64() == Some(1))
}

pub(super) fn controller_owner<'a>(
    meta: &'a kube::core::ObjectMeta,
    kind: &str,
) -> Result<&'a k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference, Error> {
    let references = meta
        .owner_references
        .as_deref()
        .ok_or(Error::OwnershipConflict)?;
    if references.len() != 1 {
        return Err(Error::OwnershipConflict);
    }
    let owner = &references[0];
    if owner.api_version != "apps/v1"
        || owner.kind != kind
        || owner.controller != Some(true)
        || !hermes_domain::resources::valid_uid(&owner.uid)
        || hermes_domain::validate_name(&owner.name).is_err()
    {
        return Err(Error::OwnershipConflict);
    }
    Ok(owner)
}

fn pod_ready(
    pod: &Pod,
    set: &ReplicaSet,
    deployment: &DynamicObject,
    input: &NativeReadiness<'_>,
) -> Result<bool, Error> {
    let owner = controller_owner(&pod.metadata, "ReplicaSet")?;
    let parent = controller_owner(&set.metadata, "Deployment")?;
    if pod.metadata.namespace != deployment.metadata.namespace
        || pod
            .metadata
            .uid
            .as_deref()
            .is_none_or(|uid| !hermes_domain::resources::valid_uid(uid))
        || set.metadata.namespace != deployment.metadata.namespace
        || set.metadata.name.as_ref() != Some(&owner.name)
        || set.metadata.uid.as_ref() != Some(&owner.uid)
        || parent.name != "hermes"
        || deployment.metadata.uid.as_ref() != Some(&parent.uid)
    {
        return Err(Error::OwnershipConflict);
    }
    let template = &deployment.data["spec"]["template"];
    let expected_generation = input.generation.to_string();
    if template["metadata"]["labels"]["agent-platform.io/policy-generation"].as_str()
        != Some(expected_generation.as_str())
    {
        return Err(Error::OwnershipConflict);
    }
    // Recreate is asynchronous. A correctly identified older writer can
    // remain briefly after the desired template advances; it supplies no
    // readiness and must disappear before the current generation is ready.
    let pod_generation = pod
        .metadata
        .labels
        .as_ref()
        .and_then(|l| l.get("agent-platform.io/policy-generation"))
        .ok_or(Error::OwnershipConflict)?;
    let parsed = pod_generation
        .parse::<i64>()
        .map_err(|_| Error::OwnershipConflict)?;
    if parsed < 1 || parsed.to_string() != *pod_generation || parsed > input.generation {
        return Err(Error::OwnershipConflict);
    }
    if parsed < input.generation {
        return Ok(false);
    }
    let set = serde_json::to_value(set).map_err(|_| Error::Invalid)?;
    let pod_value = serde_json::to_value(pod).map_err(|_| Error::Invalid)?;
    if !contains_fields(
        &set["spec"]["template"]["metadata"]["labels"],
        &template["metadata"]["labels"],
    ) || !contains_pod_spec_fields(&set["spec"]["template"]["spec"], &template["spec"])
    {
        return Err(Error::OwnershipConflict);
    }
    let mut approved = template["spec"].clone();
    approved
        .as_object_mut()
        .ok_or(Error::Invalid)?
        .remove("schedulingGates");
    if !contains_fields(
        &pod_value["metadata"]["labels"],
        &template["metadata"]["labels"],
    ) || !contains_pod_spec_fields(&pod_value["spec"], &approved)
        || pod
            .spec
            .as_ref()
            .and_then(|s| s.scheduling_gates.as_ref())
            .is_some_and(|g| !g.is_empty())
    {
        return Err(Error::OwnershipConflict);
    }
    // Only a start whose result committed under this generation can justify an
    // ungated Pod. A healthy foreign/unregistered child is never readiness.
    let digest = super::native_children::pod_digest(pod)?;
    if !input.started_children.iter().any(|proof| {
        proof.action == hermes_application::native_children::NativeChildAction::Start
            && proof.installation_id == input.material.installation_id
            && proof.instance_id == input.material.instance_id
            && proof.epoch == input.epoch
            && proof.generation == input.generation
            && Some(&proof.pod_uid) == pod.metadata.uid.as_ref()
            && proof.replica_set_uid == owner.uid
            && Some(&proof.deployment_uid) == deployment.metadata.uid.as_ref()
            && proof.namespace_uid
                == input
                    .deployment
                    .0
                    .creation
                    .spec
                    .namespace_uid
                    .as_deref()
                    .unwrap_or("")
            && proof.spec_digest == digest
    }) {
        return Err(Error::OwnershipConflict);
    }
    if pod.metadata.deletion_timestamp.is_some()
        || set["metadata"]
            .get("deletionTimestamp")
            .is_some_and(|v| !v.is_null())
    {
        return Ok(false);
    }
    let Some(status) = &pod.status else {
        return Ok(false);
    };
    let Some(init) = status
        .init_container_statuses
        .as_deref()
        .and_then(|s| s.iter().find(|s| s.name == "native-home"))
    else {
        return Ok(false);
    };
    let Some(termination) = init.state.as_ref().and_then(|s| s.terminated.as_ref()) else {
        return Ok(false);
    };
    if termination.exit_code != 0 {
        return Ok(false);
    }
    let receipt = termination
        .message
        .as_deref()
        .ok_or(Error::OwnershipConflict)?;
    native_receipt::verify(input.material, input.bootstrap, input.owner, receipt)?;
    Ok(status.phase.as_deref() == Some("Running")
        && status.conditions.as_deref().is_some_and(|conditions| {
            conditions
                .iter()
                .any(|c| c.type_ == "Ready" && c.status == "True")
        })
        && status
            .container_statuses
            .as_deref()
            .is_some_and(|containers| {
                containers.len() == 1
                    && containers[0].name == "hermes"
                    && containers[0].ready
                    && containers[0]
                        .state
                        .as_ref()
                        .is_some_and(|s| s.running.is_some())
            }))
}

#[cfg(test)]
mod tests;
