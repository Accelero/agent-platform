//! Worker-only admission CAS. Creation proof and native parameters are retained;
//! neither labels nor an applied-generation annotation authorize activation.
use super::*;
use hermes_application::{
    namespace_fence::{Admission, NamespaceFenceState},
    terminal::activation::*,
};
use hermes_domain::Access;
use serde_json::json;

impl KubernetesDriver {
    async fn activation_namespace(&self, plan: &ActivationPlan, epoch: i64) -> Result<(), Error> {
        let (_, state) = self.current_namespace_fence(&plan.namespace).await?;
        if state
            != Some(NamespaceFenceState {
                access: Access::Enabled,
                admission: Admission::Open,
                generation: plan.assignment_generation,
                epoch,
            })
            || plan.namespace.uid.as_ref() != Some(&plan.policy.namespace_uid)
        {
            return Err(Error::OwnershipConflict);
        }
        Ok(())
    }
    async fn activation_parameters(&self, plan: &ActivationPlan) -> Result<DynamicObject, Error> {
        let owned = &plan.parameters;
        if owned.state != "Bound"
            || owned.uid.is_none()
            || owned.creation.spec.kind != ResourceKind::InstancePolicy
            || owned.creation.instance_id != plan.policy.instance_id
            || owned.creation.installation_id != plan.policy.installation_id
            || owned.creation.spec.namespace_uid.as_ref() != Some(&plan.policy.namespace_uid)
        {
            return Err(Error::OwnershipConflict);
        }
        self.namespace(&owned.creation).await?;
        let object = timeout(
            Duration::from_secs(6),
            self.api(&owned.creation.spec)
                .get(&owned.creation.spec.name),
        )
        .await
        .map_err(|_| Error::Unavailable)?
        .map_err(api_error)?;
        self.verify(owned, &object)?;
        let native_digest = owned
            .observed_digest
            .as_ref()
            .ok_or(Error::OwnershipConflict)?;
        if object
            .metadata
            .annotations
            .as_ref()
            .and_then(|a| a.get(INTENT))
            != Some(native_digest)
        {
            return Err(Error::OwnershipConflict);
        }
        // The native subtree is bound to the authenticated rendering/update
        // journal. Reconstruct its current public manifest, omitting the separately
        // journaled worker subtree and Kubernetes-owned metadata.
        let mut native = object
            .data
            .get("spec")
            .cloned()
            .ok_or(Error::OwnershipConflict)?;
        native
            .as_object_mut()
            .ok_or(Error::OwnershipConflict)?
            .remove("worker");
        let original = Manifest::new(
            json!({"apiVersion":owned.creation.spec.kind.api_version(),"kind":"InstancePolicy","metadata":{"name":owned.creation.spec.name,"namespace":owned.creation.spec.namespace},"spec":native}),
        )?;
        if original.digest() != Ok(native_digest.clone()) {
            return Err(Error::OwnershipConflict);
        }
        if object
            .data
            .pointer("/spec/generation")
            .and_then(Value::as_i64)
            != Some(plan.assignment_generation)
            || object
                .data
                .pointer("/spec/namespaceUID")
                .and_then(Value::as_str)
                != Some(plan.policy.namespace_uid.as_str())
        {
            return Err(Error::OwnershipConflict);
        }
        let old = object.data.pointer("/spec/worker");
        if let Some(old) = old {
            let generation = old
                .get("generation")
                .and_then(Value::as_i64)
                .ok_or(Error::OwnershipConflict)?;
            if generation > plan.policy.generation
                || (generation == plan.policy.generation && old != &plan.worker)
            {
                return Err(Error::OwnershipConflict);
            }
        }
        Ok(object)
    }
    async fn activation_dependencies(&self, plan: &ActivationPlan) -> Result<(), Error> {
        let account = crate::execution_workloads::service_account(&plan.policy)?;
        self.verify_bound_manifest(&plan.worker_account, &account.manifest)
            .await?;
        let keys = &plan.authorized_keys;
        if keys.state != "Bound"
            || keys.creation.spec.kind != ResourceKind::Secret
            || keys.creation.spec.name != crate::execution_workloads::AUTHORIZED_KEYS
            || keys.creation.installation_id != plan.policy.installation_id
            || keys.creation.instance_id != plan.policy.instance_id
            || keys.creation.spec.namespace_uid.as_ref() != Some(&plan.policy.namespace_uid)
        {
            return Err(Error::OwnershipConflict);
        }
        self.namespace(&keys.creation).await?;
        let object = timeout(
            Duration::from_secs(6),
            self.api(&keys.creation.spec).get(&keys.creation.spec.name),
        )
        .await
        .map_err(|_| Error::Unavailable)?
        .map_err(api_error)?;
        self.verify(keys, &object)?;
        if object.data.get("immutable") != Some(&json!(true))
            || object.data.get("type") != Some(&json!("Opaque"))
            || object
                .data
                .get("data")
                .and_then(Value::as_object)
                .is_none_or(|data| data.len() != 1 || !data.contains_key("worker"))
        {
            return Err(Error::OwnershipConflict);
        }
        let original = Manifest::new(
            json!({"apiVersion":"v1","kind":"Secret","metadata":{"name":keys.creation.spec.name,"namespace":keys.creation.spec.namespace},"immutable":true,"type":"Opaque","data":object.data["data"]}),
        )?;
        if original.digest() != Ok(keys.creation.spec.manifest_digest.clone()) {
            return Err(Error::OwnershipConflict);
        }
        for runtime in &plan.policy.runtimes {
            self.verify_execution_references(&plan.policy, &runtime.definition.runtime.id)
                .await?;
        }
        Ok(())
    }
    pub async fn inspect_policy_admission(
        &self,
        plan: &ActivationPlan,
        epoch: i64,
    ) -> Result<PolicyAdmissionInput, Error> {
        timeout(Duration::from_secs(25), async {
            self.activation_namespace(plan, epoch).await?;
            self.activation_dependencies(plan).await?;
            let object = self.activation_parameters(plan).await?;
            let input = PolicyAdmissionInput {
                installation_id: plan.policy.installation_id,
                instance_id: plan.policy.instance_id,
                generation: plan.policy.generation,
                epoch,
                assignment_generation: plan.assignment_generation,
                namespace: plan.policy.namespace.clone(),
                namespace_uid: plan.policy.namespace_uid.clone(),
                resource_id: plan.parameters.creation.id,
                policy_uid: object.metadata.uid.ok_or(Error::OwnershipConflict)?,
                resource_version: object
                    .metadata
                    .resource_version
                    .ok_or(Error::OwnershipConflict)?,
                policy_digest: plan.policy.digest()?,
                rendering_digest: plan.rendering_digest.clone(),
            };
            input.digest()?;
            Ok(input)
        })
        .await
        .map_err(|_| Error::Unavailable)?
    }
    /// Observe the desired subtree first. A lost successful patch response is
    /// recovered without replay, even after unrelated metadata changes its RV.
    pub async fn apply_policy_admission(
        &self,
        prepared: &PreparedPolicyAdmission,
    ) -> Result<PolicyAdmissionObservation, Error> {
        timeout(Duration::from_secs(25),async{
            let plan=&prepared.plan;
            let input=&prepared.input;
            input.digest()?;
            if input.installation_id!=plan.policy.installation_id || input.instance_id!=plan.policy.instance_id || input.generation!=plan.policy.generation || input.assignment_generation!=plan.assignment_generation || input.namespace!=plan.policy.namespace || input.namespace_uid!=plan.policy.namespace_uid || input.resource_id!=plan.parameters.creation.id || plan.parameters.uid.as_ref()!=Some(&input.policy_uid) || input.policy_digest!=plan.policy.digest()? || input.rendering_digest!=plan.rendering_digest || public_digest(&plan.worker)?!=input.rendering_digest {return Err(Error::OwnershipConflict);}
            self.activation_namespace(plan,input.epoch).await?;
            self.activation_dependencies(plan).await?;
            let object=self.activation_parameters(plan).await?;
            let object=if object.data.pointer("/spec/worker")==Some(&plan.worker){object}else{
                if object.metadata.resource_version.as_ref()!=Some(&input.resource_version){return Err(Error::PreconditionFailed);}
                let patch=json!({"metadata":{"uid":input.policy_uid,"resourceVersion":input.resource_version},"spec":{"worker":plan.worker}});
                timeout(Duration::from_secs(6),self.api(&plan.parameters.creation.spec).patch(&plan.parameters.creation.spec.name,&PatchParams::default(),&Patch::Merge(patch))).await.map_err(|_|Error::Unavailable)?.map_err(api_error)?
            };
            self.verify(&plan.parameters,&object)?;
            if object.data.pointer("/spec/worker")!=Some(&plan.worker){return Err(Error::OwnershipConflict);}
            // A patch reply is not the durable observation boundary. GET again
            // and verify original native fields, exact worker bytes and live refs.
            let observed=self.activation_parameters(plan).await?;
            if observed.data.pointer("/spec/worker")!=Some(&plan.worker){return Err(Error::OwnershipConflict);}
            self.activation_namespace(plan,input.epoch).await?;
            Ok(PolicyAdmissionObservation{input_digest:input.digest()?,policy_uid:observed.metadata.uid.ok_or(Error::OwnershipConflict)?,resource_version:observed.metadata.resource_version.ok_or(Error::OwnershipConflict)?,namespace_uid:plan.policy.namespace_uid.clone(),rendering_digest:plan.rendering_digest.clone()})
        }).await.map_err(|_|Error::Unavailable)?
    }
}
