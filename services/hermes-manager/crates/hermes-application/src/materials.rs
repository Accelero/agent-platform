//! Opaque create-once bootstrap records. No secret values in command/audit APIs.
use crate::{Error, InstallationScope, OperationClaim};
use async_trait::async_trait;
use hermes_domain::validate_name;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MaterialContext {
    pub id: Uuid,
    pub installation_id: Uuid,
    pub instance_id: Uuid,
    pub epoch: i64,
    pub operation_id: Uuid,
    pub generation: i64,
    pub purpose: String,
}
impl MaterialContext {
    pub fn validate(&self) -> Result<(), Error> {
        validate_name(&self.purpose)?;
        if self.id.is_nil()
            || self.installation_id.is_nil()
            || self.instance_id.is_nil()
            || self.operation_id.is_nil()
            || self.epoch < 1
            || self.generation < 1
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }
}
/// No Debug/Serialize representation, including for encrypted credential blobs.
#[derive(Clone)]
pub struct Ciphertext {
    pub key_id: String,
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
}
#[derive(Clone)]
pub struct MaterialRecord {
    pub context: MaterialContext,
    pub sealed: Ciphertext,
}
#[async_trait]
pub trait MaterialRepository: Send + Sync {
    async fn find_material(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
        purpose: &str,
    ) -> Result<Option<MaterialRecord>, Error>;
    /// Read existing material only for current fenced suspend/revoke work or a
    /// fully pinned disabled-instance resync. Input and assignment must agree.
    /// Returns the original creation context; never reserves or replaces bytes.
    async fn find_retained_material(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
        purpose: &str,
    ) -> Result<Option<MaterialRecord>, Error>;
    /// One winner per instance/purpose. Concurrent proposals return the original
    /// committed record, and never replace its ciphertext or creation identity.
    async fn reserve_material(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
        candidate: MaterialRecord,
    ) -> Result<MaterialRecord, Error>;
}
