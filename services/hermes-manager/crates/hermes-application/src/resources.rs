//! Durable resource creation/binding/retirement across uncertain external calls.
use crate::{Error, InstallationScope, JournalStep, OperationClaim};
use async_trait::async_trait;
use hermes_domain::resources::ResourceSpec;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Immutable creation provenance. Keep this separate from later update intents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Creation {
    pub id: Uuid,
    pub installation_id: Uuid,
    pub instance_id: Uuid,
    pub cluster_identity: String,
    pub epoch: i64,
    pub generation: i64,
    pub operation_id: Uuid,
    pub spec: ResourceSpec,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedResource {
    pub creation: Creation,
    pub uid: Option<String>,
    pub resource_version: Option<String>,
    pub observed_digest: Option<String>,
    pub state: String,
}
pub struct PreparedResource {
    pub resource: OwnedResource,
    pub step: JournalStep,
}
/// Only a trusted driver can produce this after authenticated provenance,
/// namespace UID and intended-field verification, never from an API payload.
pub struct ResourceObservation {
    pub uid: String,
    pub resource_version: String,
    pub manifest_digest: String,
}
pub struct ResourceVersion {
    pub uid: String,
    pub resource_version: String,
}

#[async_trait]
pub trait ResourceRepository: Send + Sync {
    async fn find_resource(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
        spec: &ResourceSpec,
    ) -> Result<Option<OwnedResource>, Error>;
    /// Refresh only a previously bound resource version after trusted UID/proof
    /// verification. This does not assert desired-spec readiness or first adopt.
    async fn observe_resource_version(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
        resource_id: Uuid,
        version: ResourceVersion,
    ) -> Result<(), Error>;
    /// Atomic owned-resource reservation plus immutable external-action intent.
    async fn prepare_resource(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
        key: &str,
        spec: ResourceSpec,
    ) -> Result<PreparedResource, Error>;
    /// Atomic first UID binding, journal result and namespace UID binding.
    async fn bind_resource(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
        key: &str,
        observation: ResourceObservation,
    ) -> Result<(), Error>;
    /// Commit exact-UID retirement intent before an external delete.
    async fn retire_resource(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
        key: &str,
        resource_id: Uuid,
        observation: ResourceObservation,
    ) -> Result<PreparedResource, Error>;
    /// A delete acceptance is insufficient: only observed absence may finish it.
    async fn resource_absent(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
        key: &str,
    ) -> Result<(), Error>;
}

#[async_trait]
pub trait ResourceDriver: Send + Sync {
    type Manifest: Send + Sync;
    async fn current_version(&self, owned: &OwnedResource) -> Result<ResourceVersion, Error>;
    /// Observe first, then create/update only with the committed intent's exact
    /// provenance and preconditions. Reconcile uncertain outcomes before writes.
    async fn apply(
        &self,
        prepared: &PreparedResource,
        manifest: &Self::Manifest,
    ) -> Result<ResourceObservation, Error>;
    async fn delete(&self, prepared: &PreparedResource) -> Result<(), Error>;
    async fn absent(&self, prepared: &PreparedResource) -> Result<bool, Error>;
}

pub struct ResourceReconciler<R, D> {
    repository: R,
    driver: D,
    scope: InstallationScope,
}
impl<R: ResourceRepository + crate::OperationQueue, D: ResourceDriver> ResourceReconciler<R, D> {
    pub fn new(repository: R, driver: D, scope: InstallationScope) -> Self {
        Self {
            repository,
            driver,
            scope,
        }
    }
    /// Every adapter call is outside the SQL transactions used by the repository.
    /// If the result commit fails, the next worker observes the same reservation.
    pub async fn ensure(
        &self,
        claim: &OperationClaim,
        key: &str,
        spec: ResourceSpec,
        manifest: &D::Manifest,
    ) -> Result<ResourceObservation, Error> {
        hermes_domain::validate_name(key)?;
        if key.len() > 46 {
            return Err(Error::Invalid);
        }
        let mut step_key = key.to_string();
        if let Some(owned) = self
            .repository
            .find_resource(&self.scope, claim, &spec)
            .await?
            && owned.uid.is_some()
        {
            self.repository.renew(&self.scope, claim, 30).await?;
            let version = self.driver.current_version(&owned).await?;
            let digest =
                Sha256::digest(format!("{}\0{}", version.uid, version.resource_version).as_bytes());
            step_key = format!(
                "{key}-{}",
                digest[..8]
                    .iter()
                    .map(|v| format!("{v:02x}"))
                    .collect::<String>()
            );
            self.repository
                .observe_resource_version(&self.scope, claim, owned.creation.id, version)
                .await?;
        }
        let prepared = self
            .repository
            .prepare_resource(&self.scope, claim, &step_key, spec)
            .await?;
        self.repository.renew(&self.scope, claim, 30).await?;
        let observation = self.driver.apply(&prepared, manifest).await?;
        self.repository
            .bind_resource(
                &self.scope,
                claim,
                &step_key,
                ResourceObservation {
                    uid: observation.uid.clone(),
                    resource_version: observation.resource_version.clone(),
                    manifest_digest: observation.manifest_digest.clone(),
                },
            )
            .await?;
        Ok(observation)
    }
    /// Return false while deletion is pending. Acceptance is not completion.
    pub async fn retire(
        &self,
        claim: &OperationClaim,
        key: &str,
        resource_id: Uuid,
        observation: ResourceObservation,
    ) -> Result<bool, Error> {
        let prepared = self
            .repository
            .retire_resource(&self.scope, claim, key, resource_id, observation)
            .await?;
        self.repository.renew(&self.scope, claim, 30).await?;
        self.driver.delete(&prepared).await?;
        if !self.driver.absent(&prepared).await? {
            return Ok(false);
        }
        self.repository
            .resource_absent(&self.scope, claim, key)
            .await?;
        Ok(true)
    }
}
