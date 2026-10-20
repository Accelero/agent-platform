//! A GET has its own action and completion ports; it cannot become an apply.
use super::*;
use crate::{Action, StepResult};

pub struct PreparedObservation {
    pub resource: OwnedResource,
    pub step: JournalStep,
}
impl PreparedObservation {
    pub fn validate(&self) -> Result<(), Error> {
        let creation = &self.resource.creation;
        let spec = &creation.spec;
        let intent = &self.step.intent;
        spec.validate()?;
        intent.validate()?;
        if intent.action != Action::Observe
            || intent.creation_id != creation.id
            || intent.kind != spec.kind.as_str()
            || intent.namespace != spec.namespace
            || intent.name != spec.name
            || intent.namespace_uid != spec.namespace_uid
            || intent.manifest_digest != spec.manifest_digest
            || intent.encrypted_material_ref != spec.encrypted_material_ref
            || intent
                .expected_uid
                .as_ref()
                .is_some_and(|uid| self.resource.uid.as_ref() != Some(uid))
            || !matches!(self.resource.state.as_str(), "Reserved" | "Bound")
        {
            return Err(Error::OwnershipConflict);
        }
        match &self.step.result {
            StepResult::Unknown => {}
            StepResult::ObservedMissing if self.resource.uid.is_none() => {}
            StepResult::Verified { uid, .. } if self.resource.uid.as_ref() == Some(uid) => {}
            _ => return Err(Error::OwnershipConflict),
        }
        Ok(())
    }
}

impl<R: ResourceRepository + crate::OperationQueue, D: ResourceDriver> ResourceReconciler<R, D> {
    /// Recover an outstanding CREATE using only GET and a narrow UID binding.
    /// Reusing a completed missing step returns that historical observation;
    /// fresh work needs a new step or periodic operation, never rewritten history.
    pub async fn observe_existing(
        &self,
        claim: &OperationClaim,
        key: &str,
        resource_id: Uuid,
        manifest: &D::Manifest,
    ) -> Result<Option<ResourceObservation>, Error> {
        let prepared = self
            .repository
            .prepare_observation(&self.scope, claim, key, resource_id)
            .await?;
        prepared.validate()?;
        if prepared.step.result == StepResult::ObservedMissing {
            return Ok(None);
        }
        self.repository.renew(&self.scope, claim, 30).await?;
        let observation = self.driver.observe(&prepared, manifest).await?;
        match &observation {
            Some(observation) => {
                self.repository
                    .bind_observation(
                        &self.scope,
                        claim,
                        key,
                        ResourceObservation {
                            uid: observation.uid.clone(),
                            resource_version: observation.resource_version.clone(),
                            manifest_digest: observation.manifest_digest.clone(),
                        },
                    )
                    .await?
            }
            None => {
                self.repository
                    .observation_missing(&self.scope, claim, key)
                    .await?
            }
        }
        Ok(observation)
    }
}
