//! The mutation boundary. Every desired-state write runs through
//! [`CommandEnvelope::execute`], which commits in one transaction:
//!
//! 1. the installation check;
//! 2. the actor's current grant;
//! 3. the idempotency reservation (same key and body replays the receipt);
//! 4. the command's own reads and writes;
//! 5. the audit event and controller notifications;
//! 6. the receipt.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Serialize;
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::actor::Actor;
use super::error::{AppError, Failure, StoreError};
use super::faults::{FaultHook, FaultPoint};
use super::ports::{AuditEvent, AuditTarget, KernelTx, Reservation, Store, Tx};
use super::settings::Settings;
use crate::domain::access::Permission;
use crate::domain::installation::Installation;

/// How often a transaction is retried after a serialization conflict.
const ATTEMPTS: usize = 3;

/// A client-chosen key identifying one intended mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdempotencyKey(String);

impl IdempotencyKey {
    pub fn parse(value: &str) -> Result<Self, AppError> {
        let valid =
            !value.is_empty() && value.len() <= 128 && value.bytes().all(|b| b.is_ascii_graphic());
        valid
            .then(|| Self(value.to_owned()))
            .ok_or_else(AppError::invalid)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// What a command reads from its surroundings.
pub struct CommandContext<'a> {
    pub settings: &'a Settings,
    pub actor: &'a Actor,
}

impl CommandContext<'_> {
    pub fn installation(&self) -> &Installation {
        &self.settings.installation
    }
}

/// The result of applying a command, before the envelope records it.
pub struct Applied<R> {
    pub receipt: R,
    pub target: AuditTarget,
    /// Redacted canonical JSON describing the change.
    pub change: String,
    /// Instances whose desired state changed.
    pub notify: Vec<Uuid>,
}

impl<R> Applied<R> {
    pub fn new(receipt: R, kind: &'static str, id: impl ToString, change: &impl Serialize) -> Self {
        Self {
            receipt,
            target: AuditTarget {
                kind,
                id: id.to_string(),
            },
            change: serde_json::to_string(change).unwrap_or_else(|_| "{}".to_owned()),
            notify: Vec::new(),
        }
    }

    pub fn notifying(mut self, instance: Uuid) -> Self {
        self.notify.push(instance);
        self
    }
}

/// A desired-state change. The serialized command is the idempotency body.
#[async_trait]
pub trait Command: Serialize + Send + Sync {
    type Receipt: Serialize + DeserializeOwned + Send;

    /// Idempotency scope and audit action, for example `administrator.grant`.
    const ACTION: &'static str;

    fn permission(&self) -> Permission;

    /// Loads aggregates, applies domain transitions and writes the result.
    async fn apply<T: Tx>(
        &self,
        tx: &mut T,
        context: &CommandContext<'_>,
    ) -> Result<Applied<Self::Receipt>, Failure>;
}

pub struct CommandEnvelope<S> {
    store: S,
    settings: Arc<Settings>,
    faults: Arc<dyn FaultHook>,
}

impl<S: Store> CommandEnvelope<S> {
    pub fn new(store: S, settings: Arc<Settings>, faults: Arc<dyn FaultHook>) -> Self {
        Self {
            store,
            settings,
            faults,
        }
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub async fn execute<C: Command>(
        &self,
        actor: &Actor,
        request_id: Uuid,
        key: &IdempotencyKey,
        command: &C,
    ) -> Result<C::Receipt, AppError> {
        let digest = body_digest(command)?;
        let mut attempt = 0;
        loop {
            attempt += 1;
            match self.attempt(actor, request_id, key, command, digest).await {
                Err(Failure::Store(StoreError::Conflict)) if attempt < ATTEMPTS => continue,
                Err(Failure::Store(error)) => return Err(error.into()),
                Err(Failure::Rejected(error)) => return Err(error),
                Ok(receipt) => return Ok(receipt),
            }
        }
    }

    async fn attempt<C: Command>(
        &self,
        actor: &Actor,
        request_id: Uuid,
        key: &IdempotencyKey,
        command: &C,
        digest: [u8; 32],
    ) -> Result<C::Receipt, Failure> {
        let mut tx = self.store.begin().await?;
        let installation = &self.settings.installation;
        if tx.installation().await?.as_ref() != Some(installation) {
            return Err(AppError::registry_mismatch().into());
        }
        authorize(&mut tx, installation, actor, command.permission()).await?;
        match tx
            .reserve_receipt(actor, C::ACTION, key.as_str(), digest)
            .await?
        {
            Reservation::Fresh => {}
            Reservation::Conflict => return Err(AppError::idempotency_conflict().into()),
            Reservation::Replay(bytes) => {
                tx.commit().await?;
                return serde_json::from_slice(&bytes).map_err(|_| StoreError::Unavailable.into());
            }
        }
        let context = CommandContext {
            settings: &self.settings,
            actor,
        };
        let applied = command.apply(&mut tx, &context).await?;
        tx.append_audit(AuditEvent {
            id: Uuid::new_v4(),
            actor: actor.clone(),
            action: C::ACTION,
            target: applied.target,
            request_id,
            change: applied.change,
        })
        .await?;
        for instance in applied.notify {
            tx.notify_instance(instance).await?;
        }
        let receipt = serde_json::to_vec(&applied.receipt).map_err(|_| AppError::invalid())?;
        tx.store_receipt(actor, C::ACTION, key.as_str(), receipt)
            .await?;
        self.fault(FaultPoint::EnvelopeBeforeCommit)?;
        tx.commit().await?;
        self.fault(FaultPoint::EnvelopeAfterCommit)?;
        Ok(applied.receipt)
    }

    fn fault(&self, point: FaultPoint) -> Result<(), Failure> {
        self.faults
            .hit(point)
            .map_err(|_| AppError::fault_injected().into())
    }
}

/// The actor must hold the permission now, inside this transaction.
async fn authorize<T: Tx>(
    tx: &mut T,
    installation: &Installation,
    actor: &Actor,
    permission: Permission,
) -> Result<(), Failure> {
    let identity = match (actor, permission) {
        (Actor::Operator, Permission::Operator) => return Ok(()),
        (Actor::Operator, _) | (Actor::Administrator(_), Permission::Operator) => {
            return Err(AppError::forbidden().into());
        }
        (Actor::Administrator(identity), _) => identity,
    };
    if identity.validate(&installation.issuer).is_err() {
        return Err(AppError::forbidden().into());
    }
    let Some(record) = tx.identity(identity).await? else {
        return Err(AppError::forbidden().into());
    };
    match tx.grant(record.id).await? {
        Some(grant) if grant.permits(permission) => Ok(()),
        _ => Err(AppError::forbidden().into()),
    }
}

/// SHA-256 over the action and the command's canonical JSON.
fn body_digest<C: Command>(command: &C) -> Result<[u8; 32], AppError> {
    let body = serde_json::to_vec(command).map_err(|_| AppError::invalid())?;
    let mut hash = Sha256::new();
    hash.update(C::ACTION.as_bytes());
    hash.update([0]);
    hash.update(&body);
    Ok(hash.finalize().into())
}
