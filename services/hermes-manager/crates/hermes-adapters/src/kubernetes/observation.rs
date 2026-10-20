//! Read-only driver for a committed observation of an original creation.
use super::*;
use hermes_application::resources::PreparedObservation;

impl KubernetesDriver {
    /// GET only. In particular, a missing original target is never recreated
    /// and mismatching intended fields are never repaired to fit an intent.
    pub async fn observe_reserved(
        &self,
        prepared: &PreparedObservation,
        manifest: &Manifest,
    ) -> Result<Option<ResourceObservation>, Error> {
        prepared.validate()?;
        manifest.validate(&prepared.resource.creation.spec)?;
        // The shared observation path verifies the live cluster/namespace UID,
        // HMAC creation proof, reserved target and every intended original field.
        // It issues GET only; apply rejects this distinct Observe action.
        KubernetesDriver::observe(
            self,
            &PreparedResource {
                resource: prepared.resource.clone(),
                step: prepared.step.clone(),
            },
            manifest,
        )
        .await
    }
}
