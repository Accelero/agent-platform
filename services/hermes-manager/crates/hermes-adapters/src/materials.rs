//! Controller bootstrap vault. All generation is local; no SQL spans I/O.
use crate::session_crypto::Sealer;
use hermes_application::{
    Error, InstallationScope, OperationClaim,
    materials::{MaterialContext, MaterialRecord, MaterialRepository},
};
use uuid::Uuid;
use zeroize::Zeroizing;

pub struct PlainMaterial {
    pub context: MaterialContext,
    pub value: Zeroizing<Vec<u8>>,
}
#[derive(Clone)]
pub struct MaterialVault<R> {
    repository: R,
    cipher: Sealer,
}
impl<R: MaterialRepository> MaterialVault<R> {
    pub fn new(repository: R, cipher: Sealer) -> Self {
        Self { repository, cipher }
    }
    pub async fn get_or_create<F>(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
        purpose: &str,
        generate: F,
    ) -> Result<PlainMaterial, Error>
    where
        F: FnOnce() -> Result<Zeroizing<Vec<u8>>, Error> + Send,
    {
        hermes_domain::validate_name(purpose)?;
        if let Some(record) = self.repository.find_material(scope, claim, purpose).await? {
            return self.open(record);
        }
        let context = MaterialContext {
            id: Uuid::new_v4(),
            installation_id: scope.id,
            instance_id: claim.instance_id,
            epoch: scope.epoch,
            operation_id: claim.id,
            generation: claim.generation,
            purpose: purpose.into(),
        };
        // This closure generates cryptographic material locally. Providers or
        // command runners must not execute inside a persistence transaction.
        let plaintext = generate()?;
        let sealed = self.cipher.seal_material(&context, &plaintext)?;
        let record = self
            .repository
            .reserve_material(scope, claim, MaterialRecord { context, sealed })
            .await?;
        self.open(record)
    }
    fn open(&self, record: MaterialRecord) -> Result<PlainMaterial, Error> {
        let value = self.cipher.open_material(&record.context, &record.sealed)?;
        Ok(PlainMaterial {
            context: record.context,
            value,
        })
    }
}
