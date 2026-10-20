//! Mandatory scheduling gate and exact-UID native child lifecycle.
use super::*;
use hermes_application::{
    OperationClaim,
    namespace_fence::{ACCESS, ADMISSION, EPOCH, GENERATION},
    native_children::{
        NativeChildAction, NativeChildObservation, NativeChildResult, PreparedNativeChild,
        START_GATE,
    },
};
use k8s_openapi::api::{apps::v1::ReplicaSet, core::v1::Pod};
use kube::api::ListParams;
use serde_json::json;

pub(super) fn pod_digest(pod: &Pod) -> Result<String, Error> {
    let mut spec = serde_json::to_value(pod.spec.as_ref().ok_or(Error::OwnershipConflict)?)
        .map_err(|_| Error::Invalid)?;
    // The scheduler binds nodeName after the durable start. That server-owned
    // field is not part of the reviewed execution template.
    let map = spec.as_object_mut().ok_or(Error::Invalid)?;
    map.remove("schedulingGates");
    map.remove("nodeName");
    Ok(
        Sha256::digest(serde_json::to_vec(&spec).map_err(|_| Error::Invalid)?)
            .iter()
            .map(|v| format!("{v:02x}"))
            .collect(),
    )
}
fn gated(pod: &Pod) -> Result<bool, Error> {
    let gates = pod
        .spec
        .as_ref()
        .ok_or(Error::OwnershipConflict)?
        .scheduling_gates
        .as_deref()
        .unwrap_or(&[]);
    match gates {
        [] => Ok(false),
        [gate] if gate.name == START_GATE => Ok(true),
        _ => Err(Error::OwnershipConflict),
    }
}
impl KubernetesDriver {
    async fn child_namespace(
        &self,
        owned: &OwnedResource,
        claim: &OperationClaim,
        open: bool,
    ) -> Result<(), Error> {
        self.namespace(&owned.creation).await?;
        let api: Api<k8s_openapi::api::core::v1::Namespace> = Api::all(self.client.clone());
        let ns = timeout(
            Duration::from_secs(6),
            api.get(
                owned
                    .creation
                    .spec
                    .namespace
                    .as_deref()
                    .ok_or(Error::Invalid)?,
            ),
        )
        .await
        .map_err(|_| Error::Unavailable)?
        .map_err(api_error)?;
        if ns.metadata.uid != owned.creation.spec.namespace_uid {
            return Err(Error::OwnershipConflict);
        }
        let labels = ns.metadata.labels.ok_or(Error::OwnershipConflict)?;
        if labels.get(ADMISSION).map(String::as_str) != Some(if open { "open" } else { "closed" })
            || (open && labels.get(ACCESS).map(String::as_str) != Some("enabled"))
            || labels.get(GENERATION) != Some(&claim.generation.to_string())
            || labels.get(EPOCH) != Some(&claim.epoch.to_string())
        {
            return Err(Error::OwnershipConflict);
        }
        Ok(())
    }
    async fn child_observation(
        &self,
        owned: &OwnedResource,
        pod: &Pod,
    ) -> Result<NativeChildObservation, Error> {
        let owner = super::readiness::controller_owner(&pod.metadata, "ReplicaSet")?;
        let namespace = owned
            .creation
            .spec
            .namespace
            .as_deref()
            .ok_or(Error::Invalid)?;
        if pod.metadata.namespace.as_deref() != Some(namespace) {
            return Err(Error::OwnershipConflict);
        }
        let sets: Api<ReplicaSet> = Api::namespaced(self.client.clone(), namespace);
        let set = timeout(Duration::from_secs(6), sets.get_opt(&owner.name))
            .await
            .map_err(|_| Error::Unavailable)?
            .map_err(api_error)?
            .ok_or(Error::OwnershipConflict)?;
        let parent = super::readiness::controller_owner(&set.metadata, "Deployment")?;
        if set.metadata.uid.as_ref() != Some(&owner.uid)
            || set.metadata.namespace.as_deref() != Some(namespace)
            || parent.name != "hermes"
            || owned.uid.as_ref() != Some(&parent.uid)
        {
            return Err(Error::OwnershipConflict);
        }
        let observation = NativeChildObservation {
            namespace_uid: owned
                .creation
                .spec
                .namespace_uid
                .clone()
                .ok_or(Error::OwnershipConflict)?,
            deployment_uid: parent.uid.clone(),
            replica_set_name: owner.name.clone(),
            replica_set_uid: owner.uid.clone(),
            pod_name: pod.metadata.name.clone().ok_or(Error::OwnershipConflict)?,
            pod_uid: pod.metadata.uid.clone().ok_or(Error::OwnershipConflict)?,
            resource_version: pod
                .metadata
                .resource_version
                .clone()
                .ok_or(Error::OwnershipConflict)?,
            spec_digest: pod_digest(pod)?,
            gated: gated(pod)?,
        };
        observation.validate()?;
        Ok(observation)
    }
    /// For start, compare both child templates to the exact approved Deployment.
    /// For stop, the bound parent UID chain suffices to identify retained work.
    pub async fn discover_native_children(
        &self,
        owned: &OwnedResource,
        manifest: Option<&Manifest>,
        claim: &OperationClaim,
    ) -> Result<Vec<NativeChildObservation>, Error> {
        timeout(Duration::from_secs(20), async {
            let deployment = self.readiness_object(owned, manifest).await?;
            self.child_namespace(owned, claim, manifest.is_some())
                .await?;
            let namespace = owned
                .creation
                .spec
                .namespace
                .as_deref()
                .ok_or(Error::Invalid)?;
            let pods: Api<Pod> = Api::namespaced(self.client.clone(), namespace);
            let selector = format!(
                "{INSTANCE}={},app.kubernetes.io/component=hermes",
                claim.instance_id
            );
            let listed = timeout(
                Duration::from_secs(6),
                pods.list(&ListParams::default().labels(&selector).limit(32)),
            )
            .await
            .map_err(|_| Error::Unavailable)?
            .map_err(api_error)?;
            if listed.metadata.continue_.is_some_and(|s| !s.is_empty()) {
                return Err(Error::Unavailable);
            }
            let mut output = Vec::new();
            for pod in listed.items {
                if manifest.is_some() {
                    let generation = pod
                        .metadata
                        .labels
                        .as_ref()
                        .and_then(|labels| labels.get("agent-platform.io/policy-generation"))
                        .and_then(|raw| {
                            raw.parse::<i64>().ok().filter(|generation| {
                                *generation > 0 && generation.to_string() == *raw
                            })
                        })
                        .ok_or(Error::OwnershipConflict)?;
                    if generation > claim.generation {
                        return Err(Error::OwnershipConflict);
                    }
                    // Old writers and terminating children cannot receive a
                    // new start proof. Readiness still waits for them to leave;
                    // their ReplicaSet may already have been collected.
                    if generation < claim.generation || pod.metadata.deletion_timestamp.is_some() {
                        continue;
                    }
                    let value = serde_json::to_value(&pod).map_err(|_| Error::Invalid)?;
                    let template = &deployment.data["spec"]["template"];
                    let mut expected = template["spec"].clone();
                    if !gated(&pod)? {
                        expected
                            .as_object_mut()
                            .ok_or(Error::Invalid)?
                            .remove("schedulingGates");
                    }
                    if !contains_fields(
                        &value["metadata"]["labels"],
                        &template["metadata"]["labels"],
                    ) || !contains_pod_spec_fields(&value["spec"], &expected)
                    {
                        return Err(Error::OwnershipConflict);
                    }
                }
                output.push(self.child_observation(owned, &pod).await?);
            }
            Ok(output)
        })
        .await
        .map_err(|_| Error::Unavailable)?
    }
    /// Refresh an exact previously registered UID. Name reuse is absence of the
    /// old child, not permission to adopt its replacement.
    pub async fn observe_native_child(
        &self,
        owned: &OwnedResource,
        known: &NativeChildObservation,
    ) -> Result<Option<NativeChildObservation>, Error> {
        timeout(Duration::from_secs(20), async {
            self.readiness_object(owned, None).await?;
            let namespace = owned
                .creation
                .spec
                .namespace
                .as_deref()
                .ok_or(Error::Invalid)?;
            let pods: Api<Pod> = Api::namespaced(self.client.clone(), namespace);
            let found = timeout(Duration::from_secs(6), pods.get_opt(&known.pod_name))
                .await
                .map_err(|_| Error::Unavailable)?
                .map_err(api_error)?;
            let Some(pod) = found.filter(|p| p.metadata.uid.as_ref() == Some(&known.pod_uid))
            else {
                return Ok(None);
            };
            if pod_digest(&pod)? != known.spec_digest {
                return Err(Error::OwnershipConflict);
            }
            let mut current = known.clone();
            current.resource_version = pod
                .metadata
                .resource_version
                .clone()
                .ok_or(Error::OwnershipConflict)?;
            current.gated = gated(&pod)?;
            current.validate()?;
            Ok(Some(current))
        })
        .await
        .map_err(|_| Error::Unavailable)?
    }
    pub async fn apply_native_child(
        &self,
        prepared: &PreparedNativeChild,
        claim: &OperationClaim,
    ) -> Result<Option<NativeChildResult>, Error> {
        prepared.validate(claim)?;
        timeout(Duration::from_secs(20),async {
            let input=&prepared.input;
            let start=input.action==NativeChildAction::Start;
            self.readiness_object(&prepared.deployment,None).await?;
            self.child_namespace(&prepared.deployment,claim,start).await?;
            let pods:Api<Pod>=Api::namespaced(self.client.clone(),&input.namespace);
            let pod=timeout(Duration::from_secs(6),pods.get_opt(&input.pod_name)).await.map_err(|_|Error::Unavailable)?.map_err(api_error)?;
            let Some(pod)=pod else {
                return Ok(Some(NativeChildResult{pod_uid:input.pod_uid.clone(),input_digest:input.digest()?,absent:true}));
            };
            // A replacement with the same name is evidence of absence of the
            // exact old UID; it never authorizes deleting that replacement.
            if pod.metadata.uid.as_ref()!=Some(&input.pod_uid) {
                return Ok(Some(NativeChildResult{pod_uid:input.pod_uid.clone(),input_digest:input.digest()?,absent:true}));
            }
            // This exact UID is still terminating. Its ReplicaSet may already
            // be gone, and neither start nor absence has been established.
            if start && pod.metadata.deletion_timestamp.is_some() {return Ok(None);}
            let observed=if start {self.child_observation(&prepared.deployment,&pod).await?} else {
                // This immutable registered UID remains our child even after
                // its ReplicaSet has been garbage-collected. No name adoption.
                let mut known=input.observation(gated(&pod)?);
                known.resource_version=pod.metadata.resource_version.clone().ok_or(Error::OwnershipConflict)?;
                known.spec_digest=pod_digest(&pod)?;
                known.validate()?;known
            };
            if observed.replica_set_uid!=input.replica_set_uid || observed.replica_set_name!=input.replica_set_name
                || observed.spec_digest!=input.spec_digest {return Err(Error::OwnershipConflict);}
            if start {
                if observed.gated {
                    if observed.resource_version!=input.resource_version {return Err(Error::PreconditionFailed);}
                    let patch=json!({"metadata":{"uid":input.pod_uid,"resourceVersion":input.resource_version},"spec":{"schedulingGates":[]}});
                    let updated=timeout(Duration::from_secs(6),pods.patch(&input.pod_name,&PatchParams::default(),&Patch::Merge(patch))).await.map_err(|_|Error::Unavailable)?.map_err(api_error)?;
                    if updated.metadata.uid.as_ref()!=Some(&input.pod_uid) || gated(&updated)? || pod_digest(&updated)?!=input.spec_digest {return Err(Error::OwnershipConflict);}
                }
                return Ok(Some(NativeChildResult{pod_uid:input.pod_uid.clone(),input_digest:input.digest()?,absent:false}));
            }
            if pod.metadata.deletion_timestamp.is_none() {
                if observed.resource_version!=input.resource_version {return Err(Error::PreconditionFailed);}
                let params=DeleteParams{grace_period_seconds:Some(45),preconditions:Some(Preconditions{uid:Some(input.pod_uid.clone()),resource_version:Some(input.resource_version.clone())}),..Default::default()};
                timeout(Duration::from_secs(6),pods.delete(&input.pod_name,&params)).await.map_err(|_|Error::Unavailable)?.map_err(api_error)?;
            }
            let after=timeout(Duration::from_secs(6),pods.get_opt(&input.pod_name)).await.map_err(|_|Error::Unavailable)?.map_err(api_error)?;
            if after.is_none_or(|p|p.metadata.uid.as_ref()!=Some(&input.pod_uid)) {
                Ok(Some(NativeChildResult{pod_uid:input.pod_uid.clone(),input_digest:input.digest()?,absent:true}))
            } else {Ok(None)}
        }).await.map_err(|_|Error::Unavailable)?
    }
}
