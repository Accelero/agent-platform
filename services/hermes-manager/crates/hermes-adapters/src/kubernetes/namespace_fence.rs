//! CAS-only admission labels. Never overwrite creation proof or applied intent.
use super::*;
use hermes_application::{
    OperationClaim,
    namespace_fence::{
        ACCESS, ADMISSION, Admission, EPOCH, GENERATION, NamespaceFenceObservation,
        NamespaceFenceState, PreparedNamespaceFence,
    },
};
use hermes_domain::{Access, resources::valid_uid};
use serde_json::json;

impl KubernetesDriver {
    /// Read the current bound Namespace without changing admission or SQL state.
    /// A missing complete fence is closed by default; a partial one is a conflict.
    pub async fn current_namespace_fence(
        &self,
        owned: &OwnedResource,
    ) -> Result<(ResourceVersion, Option<NamespaceFenceState>), Error> {
        timeout(Duration::from_secs(20), async {
            let object = self.namespace_fence_object(owned).await?;
            let observed = state(&object)?;
            let version = ResourceVersion {
                uid: object.metadata.uid.ok_or(Error::OwnershipConflict)?,
                resource_version: object
                    .metadata
                    .resource_version
                    .filter(|v| valid_uid(v))
                    .ok_or(Error::OwnershipConflict)?,
            };
            Ok((version, observed))
        })
        .await
        .map_err(|_| Error::Unavailable)?
    }

    async fn namespace_fence_object(&self, owned: &OwnedResource) -> Result<DynamicObject, Error> {
        owned.creation.spec.validate()?;
        if owned.creation.spec.kind != ResourceKind::Namespace
            || owned.state != "Bound"
            || owned.uid.is_none()
        {
            return Err(Error::OwnershipConflict);
        }
        self.namespace(&owned.creation).await?;
        let object = timeout(
            Duration::from_secs(6),
            self.api(&owned.creation.spec)
                .get_opt(&owned.creation.spec.name),
        )
        .await
        .map_err(|_| Error::Unavailable)?
        .map_err(api_error)?
        .ok_or(Error::OwnershipConflict)?;
        self.verify(owned, &object)?;
        Ok(object)
    }
    /// Observe first, so a lost successful patch response can be recovered using
    /// the original immutable input even though its write RV is now stale.
    pub async fn apply_namespace_fence(
        &self,
        prepared: &PreparedNamespaceFence,
        claim: &OperationClaim,
    ) -> Result<NamespaceFenceObservation, Error> {
        prepared.validate(claim)?;
        timeout(
            Duration::from_secs(20),
            self.namespace_fence_inner(prepared),
        )
        .await
        .map_err(|_| Error::Unavailable)?
    }
    async fn namespace_fence_inner(
        &self,
        prepared: &PreparedNamespaceFence,
    ) -> Result<NamespaceFenceObservation, Error> {
        let api = self.api(&prepared.namespace.creation.spec);
        let object = self.namespace_fence_object(&prepared.namespace).await?;
        let before = state(&object)?;
        if before.as_ref() == Some(&prepared.input.state) {
            return self.fence_observation(prepared, &object);
        }
        transition(before.as_ref(), &prepared.input.state)?;
        if object.metadata.resource_version.as_ref() != Some(&prepared.input.resource_version) {
            return Err(Error::PreconditionFailed);
        }
        let desired = &prepared.input.state;
        let labels = BTreeMap::from([
            (ACCESS, desired.access.as_str().to_string()),
            (ADMISSION, desired.admission.as_str().to_string()),
            (GENERATION, desired.generation.to_string()),
            (EPOCH, desired.epoch.to_string()),
        ]);
        let patch = json!({"metadata":{"uid":prepared.input.namespace_uid,"resourceVersion":prepared.input.resource_version,"labels":labels}});
        let updated = timeout(
            Duration::from_secs(6),
            api.patch(
                &prepared.input.namespace,
                &PatchParams::default(),
                &Patch::Merge(patch),
            ),
        )
        .await
        .map_err(|_| Error::Unavailable)?
        .map_err(api_error)?;
        self.fence_observation(prepared, &updated)
    }
    fn fence_observation(
        &self,
        prepared: &PreparedNamespaceFence,
        object: &DynamicObject,
    ) -> Result<NamespaceFenceObservation, Error> {
        self.verify(&prepared.namespace, object)?;
        if state(object)?.as_ref() != Some(&prepared.input.state) {
            return Err(Error::OwnershipConflict);
        }
        let resource_version = object
            .metadata
            .resource_version
            .clone()
            .filter(|s| valid_uid(s))
            .ok_or(Error::OwnershipConflict)?;
        Ok(NamespaceFenceObservation {
            uid: object
                .metadata
                .uid
                .clone()
                .ok_or(Error::OwnershipConflict)?,
            resource_version,
            input_digest: prepared.input.digest()?,
            state: prepared.input.state.clone(),
        })
    }
}

fn state(object: &DynamicObject) -> Result<Option<NamespaceFenceState>, Error> {
    let Some(labels) = &object.metadata.labels else {
        return Ok(None);
    };
    if [ACCESS, ADMISSION, GENERATION, EPOCH]
        .iter()
        .all(|key| !labels.contains_key(*key))
    {
        return Ok(None);
    }
    let value = |key: &str| {
        labels
            .get(key)
            .map(String::as_str)
            .ok_or(Error::OwnershipConflict)
    };
    let number = |key: &str| -> Result<i64, Error> {
        let text = value(key)?;
        let parsed = text.parse::<i64>().map_err(|_| Error::OwnershipConflict)?;
        if parsed < 1 || parsed.to_string() != text {
            return Err(Error::OwnershipConflict);
        }
        Ok(parsed)
    };
    let state = NamespaceFenceState {
        access: match value(ACCESS)? {
            "enabled" => Access::Enabled,
            "suspended" => Access::Suspended,
            "revoked" => Access::Revoked,
            _ => return Err(Error::OwnershipConflict),
        },
        admission: match value(ADMISSION)? {
            "closed" => Admission::Closed,
            "open" => Admission::Open,
            _ => return Err(Error::OwnershipConflict),
        },
        generation: number(GENERATION)?,
        epoch: number(EPOCH)?,
    };
    state.validate().map_err(|_| Error::OwnershipConflict)?;
    Ok(Some(state))
}

fn transition(
    before: Option<&NamespaceFenceState>,
    desired: &NamespaceFenceState,
) -> Result<(), Error> {
    desired.validate()?;
    if let Some(before) = before
        && (desired.epoch < before.epoch
            || desired.generation < before.generation
            || (desired.generation == before.generation && desired.access != before.access))
    {
        return Err(Error::OwnershipConflict);
    }
    if desired.admission == Admission::Open {
        let Some(before) = before else {
            return Err(Error::OwnershipConflict);
        };
        if before.admission != Admission::Closed
            || before.access != Access::Enabled
            || before.generation != desired.generation
            || before.epoch != desired.epoch
        {
            return Err(Error::OwnershipConflict);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn closed() -> NamespaceFenceState {
        NamespaceFenceState {
            access: Access::Enabled,
            admission: Admission::Closed,
            generation: 2,
            epoch: 1,
        }
    }
    #[test]
    fn missing_fence_is_closed_but_opening_requires_current_explicit_close() {
        let object: DynamicObject = serde_json::from_value(
            json!({"apiVersion":"v1","kind":"Namespace","metadata":{"name":"fixture"}}),
        )
        .unwrap();
        assert!(state(&object).unwrap().is_none());
        let closed = closed();
        transition(None, &closed).unwrap();
        let open = NamespaceFenceState {
            admission: Admission::Open,
            ..closed.clone()
        };
        assert!(matches!(
            transition(None, &open),
            Err(Error::OwnershipConflict)
        ));
        transition(Some(&closed), &open).unwrap();
        let later = NamespaceFenceState {
            epoch: 2,
            ..open.clone()
        };
        assert!(matches!(
            transition(Some(&closed), &later),
            Err(Error::OwnershipConflict)
        ));
        let old = NamespaceFenceState {
            generation: 1,
            ..closed.clone()
        };
        assert!(matches!(
            transition(Some(&closed), &old),
            Err(Error::OwnershipConflict)
        ));
        let changed = NamespaceFenceState {
            access: Access::Revoked,
            ..closed.clone()
        };
        assert!(matches!(
            transition(Some(&closed), &changed),
            Err(Error::OwnershipConflict)
        ));
        let current = NamespaceFenceState {
            access: Access::Revoked,
            generation: 3,
            ..closed.clone()
        };
        transition(Some(&open), &current).unwrap();
    }
    #[test]
    fn partial_noncanonical_and_disabled_open_fences_fail_closed() {
        let mut object:DynamicObject=serde_json::from_value(json!({"apiVersion":"v1","kind":"Namespace","metadata":{"name":"fixture","labels":{(ACCESS):"enabled",(ADMISSION):"closed",(GENERATION):"2",(EPOCH):"1"}}})).unwrap();
        assert!(state(&object).unwrap() == Some(closed()));
        for (key, value) in [
            (GENERATION, "02"),
            (EPOCH, "0"),
            (EPOCH, "9223372036854775808"),
            (ACCESS, "unknown"),
            (ADMISSION, "unknown"),
        ] {
            let mut bad = object.clone();
            bad.metadata
                .labels
                .as_mut()
                .unwrap()
                .insert(key.into(), value.into());
            assert!(matches!(state(&bad), Err(Error::OwnershipConflict)));
        }
        object.metadata.labels.as_mut().unwrap().remove(EPOCH);
        assert!(matches!(state(&object), Err(Error::OwnershipConflict)));
        let mut object:DynamicObject=serde_json::from_value(json!({"apiVersion":"v1","kind":"Namespace","metadata":{"name":"fixture","labels":{(ACCESS):"revoked",(ADMISSION):"open",(GENERATION):"2",(EPOCH):"1"}}})).unwrap();
        assert!(matches!(state(&object), Err(Error::OwnershipConflict)));
        object
            .metadata
            .labels
            .as_mut()
            .unwrap()
            .insert(ADMISSION.into(), "closed".into());
        assert!(state(&object).unwrap().is_some());
    }
}
