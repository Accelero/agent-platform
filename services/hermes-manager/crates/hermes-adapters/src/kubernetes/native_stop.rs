//! Retained Deployment scale subresource only. Never apply a replacement spec.
use super::*;
use hermes_application::{
    OperationClaim,
    native_stop::{NativeStopInspection, NativeStopObservation, PreparedNativeStop},
};
use hermes_domain::resources::valid_uid;
use serde_json::json;

impl KubernetesDriver {
    pub async fn inspect_native_stop(
        &self,
        owned: &OwnedResource,
    ) -> Result<NativeStopInspection, Error> {
        timeout(Duration::from_secs(20), async {
            let object = self.native_stop_object(owned).await?;
            inspection(&object)
        })
        .await
        .map_err(|_| Error::Unavailable)?
    }
    async fn native_stop_object(&self, owned: &OwnedResource) -> Result<DynamicObject, Error> {
        owned.creation.spec.validate()?;
        if owned.creation.spec.kind != ResourceKind::Deployment
            || owned.creation.spec.name != "hermes"
            || owned.state != "Bound"
            || owned.uid.is_none()
        {
            return Err(Error::OwnershipConflict);
        }
        self.namespace(&owned.creation).await?;
        let object = timeout(
            Duration::from_secs(6),
            self.api(&owned.creation.spec).get_opt("hermes"),
        )
        .await
        .map_err(|_| Error::Unavailable)?
        .map_err(api_error)?
        .ok_or(Error::OwnershipConflict)?;
        self.verify(owned, &object)?;
        Ok(object)
    }
    /// An uncertain successful Scale reply is recovered by observing zero
    /// replicas and the SAME frozen parent spec. No stale RV write is replayed.
    pub async fn stop_native(
        &self,
        prepared: &PreparedNativeStop,
        claim: &OperationClaim,
    ) -> Result<NativeStopObservation, Error> {
        prepared.validate(claim)?;
        timeout(Duration::from_secs(20), self.native_stop_inner(prepared))
            .await
            .map_err(|_| Error::Unavailable)?
    }
    async fn native_stop_inner(
        &self,
        prepared: &PreparedNativeStop,
    ) -> Result<NativeStopObservation, Error> {
        self.native_stop_fence(prepared).await?;
        let object = self.native_stop_object(&prepared.deployment).await?;
        let before = inspection(&object)?;
        if before.preserved_spec_digest != prepared.input.preserved_spec_digest {
            return Err(Error::OwnershipConflict);
        }
        if replicas(&object)? == 0 {
            return observation(prepared, &object);
        }
        if before.resource_version != prepared.input.resource_version {
            return Err(Error::PreconditionFailed);
        }
        let patch = json!({"metadata":{"uid":prepared.input.deployment_uid,"resourceVersion":prepared.input.resource_version},"spec":{"replicas":0}});
        timeout(
            Duration::from_secs(6),
            self.api(&prepared.deployment.creation.spec).patch_scale(
                "hermes",
                &PatchParams::default(),
                &Patch::Merge(patch),
            ),
        )
        .await
        .map_err(|_| Error::Unavailable)?
        .map_err(api_error)?;
        // Scale responses don't contain the parent Pod template/selector. Read
        // the actual parent before acknowledging preservation or zero replicas.
        self.native_stop_fence(prepared).await?;
        let after = self.native_stop_object(&prepared.deployment).await?;
        observation(prepared, &after)
    }
    async fn native_stop_fence(&self, prepared: &PreparedNativeStop) -> Result<(), Error> {
        let (version, state) = self.current_namespace_fence(&prepared.namespace).await?;
        if version.uid != prepared.input.namespace_uid
            || state.as_ref() != Some(&prepared.input.fence_state)
        {
            return Err(Error::OwnershipConflict);
        }
        Ok(())
    }
}
fn replicas(object: &DynamicObject) -> Result<u64, Error> {
    object
        .data
        .pointer("/spec/replicas")
        .and_then(Value::as_u64)
        .filter(|n| *n <= i32::MAX as u64)
        .ok_or(Error::OwnershipConflict)
}
fn inspection(object: &DynamicObject) -> Result<NativeStopInspection, Error> {
    replicas(object)?;
    let spec = object
        .data
        .get("spec")
        .and_then(Value::as_object)
        .ok_or(Error::OwnershipConflict)?;
    let mut fields = serde_json::Map::new();
    for key in ["template", "selector", "strategy"] {
        let value = spec
            .get(key)
            .filter(|v| v.is_object())
            .ok_or(Error::OwnershipConflict)?;
        fields.insert(key.into(), value.clone());
    }
    let bytes = serde_json::to_vec(&fields).map_err(|_| Error::OwnershipConflict)?;
    if bytes.len() > 65_536 {
        return Err(Error::OwnershipConflict);
    }
    let inspection = NativeStopInspection {
        uid: object
            .metadata
            .uid
            .clone()
            .filter(|v| valid_uid(v))
            .ok_or(Error::OwnershipConflict)?,
        resource_version: object
            .metadata
            .resource_version
            .clone()
            .filter(|v| valid_uid(v))
            .ok_or(Error::OwnershipConflict)?,
        preserved_spec_digest: Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
    };
    inspection.validate()?;
    Ok(inspection)
}
fn observation(
    prepared: &PreparedNativeStop,
    object: &DynamicObject,
) -> Result<NativeStopObservation, Error> {
    let actual = inspection(object)?;
    if actual.uid != prepared.input.deployment_uid
        || actual.preserved_spec_digest != prepared.input.preserved_spec_digest
        || replicas(object)? != 0
    {
        return Err(Error::OwnershipConflict);
    }
    Ok(NativeStopObservation {
        uid: actual.uid,
        resource_version: actual.resource_version,
        input_digest: prepared.input.digest()?,
        preserved_spec_digest: actual.preserved_spec_digest,
    })
}
