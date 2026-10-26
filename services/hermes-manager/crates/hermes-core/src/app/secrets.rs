//! Admin secret commands. Values are sealed before they reach a command, so
//! neither the idempotency digest nor the audit ever sees them.

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::actor::Actor;
use super::envelope::{Applied, Command, CommandContext, CommandEnvelope, IdempotencyKey};
use super::error::{AppError, Failure};
use super::ports::{SecretRecord, Store, Tx};
use crate::domain::access::Permission;
use crate::domain::names::validate_name;
use crate::domain::secrets::{SecretKind, SecretRejected, validate_value};

/// Seals values to the controller's public key; only the controller can open
/// them. The API process holds this port, never the opening key. The sealed
/// value is bound to the secret's name and kind.
pub trait SecretSealer: Send + Sync {
    fn seal(&self, name: &str, kind: SecretKind, value: &[u8]) -> Result<Vec<u8>, AppError>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretReceipt {
    pub name: String,
    pub version: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Change<'a> {
    kind: &'a str,
    version: i64,
}

/// Writes a new version. The kind is fixed when the secret is created.
/// `sealed` is excluded from the idempotency digest: a retry with the same key
/// is the same intent.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteSecret {
    pub name: String,
    pub kind: SecretKind,
    #[serde(skip)]
    pub sealed: Vec<u8>,
}

#[async_trait]
impl Command for WriteSecret {
    type Receipt = SecretReceipt;
    const ACTION: &'static str = "secret.write";

    fn permission(&self) -> Permission {
        Permission::Administer
    }

    async fn apply<T: Tx>(
        &self,
        tx: &mut T,
        _: &CommandContext<'_>,
    ) -> Result<Applied<SecretReceipt>, Failure> {
        validate_name(&self.name)?;
        let version = match tx.secret(&self.name).await? {
            Some(current) if current.kind != self.kind => {
                return Err(SecretRejected::InvalidValue.into());
            }
            Some(current) => current.version + 1,
            None => 1,
        };
        let record = SecretRecord {
            kind: self.kind,
            version,
        };
        tx.put_secret_version(&self.name, record, &self.sealed)
            .await?;
        let instances = tx.instances_using_secret(&self.name).await?;
        let mut applied = Applied::new(
            SecretReceipt {
                name: self.name.clone(),
                version,
            },
            "secret",
            &self.name,
            &Change {
                kind: self.kind.as_str(),
                version,
            },
        );
        for instance in instances {
            tx.advance_instance(instance, false).await?;
            applied = applied.notifying(instance);
        }
        Ok(applied)
    }
}

/// Removes every version of an unreferenced secret.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteSecret {
    pub name: String,
    pub expected_version: i64,
}

#[async_trait]
impl Command for DeleteSecret {
    type Receipt = SecretReceipt;
    const ACTION: &'static str = "secret.delete";

    fn permission(&self) -> Permission {
        Permission::Administer
    }

    async fn apply<T: Tx>(
        &self,
        tx: &mut T,
        _: &CommandContext<'_>,
    ) -> Result<Applied<SecretReceipt>, Failure> {
        let current = tx
            .secret(&self.name)
            .await?
            .ok_or(SecretRejected::Unknown)?;
        if current.version != self.expected_version {
            return Err(SecretRejected::StaleVersion.into());
        }
        if tx.secret_referenced(&self.name).await? {
            return Err(SecretRejected::InUse.into());
        }
        tx.delete_secret(&self.name).await?;
        let receipt = SecretReceipt {
            name: self.name.clone(),
            version: current.version,
        };
        let change = Change {
            kind: current.kind.as_str(),
            version: current.version,
        };
        Ok(Applied::new(receipt, "secret", &self.name, &change))
    }
}

/// A secret value as submitted, before sealing.
pub struct PlainSecret<'a> {
    pub name: &'a str,
    pub kind: SecretKind,
    pub value: &'a [u8],
}

/// Validates, seals and writes a secret value in one step.
pub async fn write<S: Store>(
    envelope: &CommandEnvelope<S>,
    sealer: &Arc<dyn SecretSealer>,
    actor: &Actor,
    request_id: Uuid,
    key: &IdempotencyKey,
    secret: PlainSecret<'_>,
) -> Result<SecretReceipt, AppError> {
    validate_value(secret.kind, secret.value)?;
    let command = WriteSecret {
        name: secret.name.to_owned(),
        kind: secret.kind,
        sealed: sealer.seal(secret.name, secret.kind, secret.value)?,
    };
    envelope.execute(actor, request_id, key, &command).await
}
