//! Administrator grant commands.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::envelope::{Applied, Command, CommandContext};
use super::error::{AppError, Failure};
use super::ports::{AccessReader, Tx};
use crate::domain::access::{self, AccessRejected, Grant, Permission};
use crate::domain::identity::Identity;
use crate::domain::installation::Installation;

/// Actions any administrator may perform, as published in the contract.
pub const ADMINISTER_ACTIONS: &[&str] = &["administer"];

/// The additional action of grants that may manage other grants.
pub const MANAGE_ACTION: &str = "administrator.manage";

/// Signs in an administrator: records the login, then requires an active grant.
pub async fn admit<R: AccessReader>(
    reader: &R,
    installation: &Installation,
    identity: &Identity,
) -> Result<Grant, AppError> {
    identity
        .validate(&installation.issuer)
        .map_err(|_| AppError::forbidden())?;
    reader.record_login(identity).await?;
    current_grant(reader, identity).await
}

/// The identity's grant if it permits administration now.
pub async fn current_grant<R: AccessReader>(
    reader: &R,
    identity: &Identity,
) -> Result<Grant, AppError> {
    match reader.grant_of(identity).await? {
        Some(grant) if grant.permits(Permission::Administer) => Ok(grant),
        _ => Err(AppError::forbidden()),
    }
}

/// What the dashboard may offer a grant holder.
pub fn permitted_actions(grant: &Grant) -> Vec<&'static str> {
    let mut actions = Vec::new();
    if grant.permits(Permission::ManageAdministrators) {
        actions.push(MANAGE_ACTION);
    }
    if grant.permits(Permission::Administer) {
        actions.extend_from_slice(ADMINISTER_ACTIONS);
    }
    actions
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrantReceipt {
    pub identity_id: Uuid,
    pub version: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GrantChange {
    active: bool,
    can_manage_administrators: bool,
    version: i64,
}

impl From<Grant> for GrantChange {
    fn from(grant: Grant) -> Self {
        Self {
            active: grant.active,
            can_manage_administrators: grant.can_manage_administrators,
            version: grant.version,
        }
    }
}

/// Operator command: the first administrator, or recovery of one.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapAdministrator {
    pub identity: Identity,
    pub recover: bool,
}

#[async_trait]
impl Command for BootstrapAdministrator {
    type Receipt = GrantReceipt;
    const ACTION: &'static str = "administrator.bootstrap";

    fn permission(&self) -> Permission {
        Permission::Operator
    }

    async fn apply<T: Tx>(
        &self,
        tx: &mut T,
        context: &CommandContext<'_>,
    ) -> Result<Applied<GrantReceipt>, Failure> {
        self.identity.validate(&context.installation().issuer)?;
        let existing = tx.grant_count().await?;
        let mut grant = access::bootstrap(existing, self.recover)?;
        let record = tx.ensure_identity(&self.identity).await?;
        if let Some(previous) = tx.grant(record.id).await? {
            grant.version = previous.version + 1;
        }
        tx.put_grant(record.id, grant).await?;
        let receipt = GrantReceipt {
            identity_id: record.id,
            version: grant.version,
        };
        Ok(Applied::new(
            receipt,
            "administrator",
            record.id,
            &GrantChange::from(grant),
        ))
    }
}

/// Grants administration to an identity that has signed in before.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GrantAdministrator {
    pub identity: Identity,
    pub can_manage_administrators: bool,
}

#[async_trait]
impl Command for GrantAdministrator {
    type Receipt = GrantReceipt;
    const ACTION: &'static str = "administrator.grant";

    fn permission(&self) -> Permission {
        Permission::ManageAdministrators
    }

    async fn apply<T: Tx>(
        &self,
        tx: &mut T,
        context: &CommandContext<'_>,
    ) -> Result<Applied<GrantReceipt>, Failure> {
        self.identity.validate(&context.installation().issuer)?;
        let record = tx
            .identity(&self.identity)
            .await?
            .ok_or(AccessRejected::IdentityNotVerified)?;
        let existing = tx.grant(record.id).await?;
        let grant = access::grant(record.verified, existing, self.can_manage_administrators)?;
        tx.put_grant(record.id, grant).await?;
        let receipt = GrantReceipt {
            identity_id: record.id,
            version: grant.version,
        };
        Ok(Applied::new(
            receipt,
            "administrator",
            record.id,
            &GrantChange::from(grant),
        ))
    }
}

/// Changes or revokes a grant, keeping at least one active manager.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateAdministrator {
    pub identity_id: Uuid,
    pub expected_version: i64,
    pub active: bool,
    pub can_manage_administrators: bool,
}

#[async_trait]
impl Command for UpdateAdministrator {
    type Receipt = GrantReceipt;
    const ACTION: &'static str = "administrator.update";

    fn permission(&self) -> Permission {
        Permission::ManageAdministrators
    }

    async fn apply<T: Tx>(
        &self,
        tx: &mut T,
        _: &CommandContext<'_>,
    ) -> Result<Applied<GrantReceipt>, Failure> {
        let managers = tx.lock_active_managers().await?;
        let current = tx
            .grant(self.identity_id)
            .await?
            .ok_or(AccessRejected::NotFound)?;
        let grant = access::change(
            current,
            self.expected_version,
            self.active,
            self.can_manage_administrators,
            managers,
        )?;
        tx.put_grant(self.identity_id, grant).await?;
        let receipt = GrantReceipt {
            identity_id: self.identity_id,
            version: grant.version,
        };
        Ok(Applied::new(
            receipt,
            "administrator",
            self.identity_id,
            &GrantChange::from(grant),
        ))
    }
}
