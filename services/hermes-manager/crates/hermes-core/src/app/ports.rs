//! Store ports. One transaction type implements every context's port, so a
//! command can read and write any aggregate inside one short transaction.

use async_trait::async_trait;
use uuid::Uuid;

use super::actor::Actor;
use super::catalog::Definition;
use super::error::StoreError;
use crate::domain::access::Grant;
use crate::domain::assignments::AssignmentSpec;
use crate::domain::catalog::{CatalogKind, VersionStatus};
use crate::domain::identity::Identity;
use crate::domain::installation::Installation;
use crate::domain::secrets::SecretKind;

/// Opens short transactions. Implementations must not hold a transaction open
/// across an external call; the application never asks them to.
#[async_trait]
pub trait Store: Send + Sync {
    type Tx: Tx;

    async fn begin(&self) -> Result<Self::Tx, StoreError>;
}

/// Every port a command may use inside one transaction.
pub trait Tx: KernelTx + AccessTx + CatalogTx + AssignmentTx + SecretTx {}

impl<T: KernelTx + AccessTx + CatalogTx + AssignmentTx + SecretTx> Tx for T {}

/// Result of reserving an idempotency key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reservation {
    /// First use of the key: the command runs.
    Fresh,
    /// The key was used with the same body: return the stored receipt.
    Replay(Vec<u8>),
    /// The key was used with a different body.
    Conflict,
}

/// What an audit event refers to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditTarget {
    pub kind: &'static str,
    pub id: String,
}

/// An immutable audit record. `change` is redacted canonical JSON produced by
/// the command; it never contains secret values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEvent {
    pub id: Uuid,
    pub actor: Actor,
    pub action: &'static str,
    pub target: AuditTarget,
    pub request_id: Uuid,
    pub change: String,
}

/// Installation, idempotency, audit and wake-up primitives.
#[async_trait]
pub trait KernelTx: Send {
    /// The registry's installation identity, locked against change until commit.
    async fn installation(&mut self) -> Result<Option<Installation>, StoreError>;

    /// Reserves `(actor, action, key)` for a body digest. Concurrent reservations
    /// of the same key wait for the first transaction to finish.
    async fn reserve_receipt(
        &mut self,
        actor: &Actor,
        action: &'static str,
        key: &str,
        digest: [u8; 32],
    ) -> Result<Reservation, StoreError>;

    /// Records the receipt of a reserved key. A stored receipt never changes.
    async fn store_receipt(
        &mut self,
        actor: &Actor,
        action: &'static str,
        key: &str,
        receipt: Vec<u8>,
    ) -> Result<(), StoreError>;

    async fn append_audit(&mut self, event: AuditEvent) -> Result<(), StoreError>;

    /// Wakes the controller for an instance after commit. An accelerator only:
    /// the controller also finds the work by resync.
    async fn notify_instance(&mut self, instance: Uuid) -> Result<(), StoreError>;

    async fn commit(self) -> Result<(), StoreError>
    where
        Self: Sized;
}

/// A stored identity and whether it has signed in at least once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdentityRecord {
    pub id: Uuid,
    pub verified: bool,
}

/// Identities and administrator grants.
#[async_trait]
pub trait AccessTx: Send {
    async fn identity(&mut self, identity: &Identity)
    -> Result<Option<IdentityRecord>, StoreError>;

    /// Returns the identity's record, creating an unverified one if missing.
    async fn ensure_identity(&mut self, identity: &Identity) -> Result<IdentityRecord, StoreError>;

    /// The grant of an identity, locked until commit.
    async fn grant(&mut self, identity: Uuid) -> Result<Option<Grant>, StoreError>;

    async fn grant_count(&mut self) -> Result<usize, StoreError>;

    /// Locks every active managing grant and returns their number. Holding
    /// these locks serializes concurrent demotions and revocations.
    async fn lock_active_managers(&mut self) -> Result<usize, StoreError>;

    async fn put_grant(&mut self, identity: Uuid, grant: Grant) -> Result<(), StoreError>;
}

/// Read side of access, used by authentication adapters on every request.
/// Each call is its own short transaction.
#[async_trait]
pub trait AccessReader: Send + Sync {
    /// The identity's current grant.
    async fn grant_of(&self, identity: &Identity) -> Result<Option<Grant>, StoreError>;

    /// Records a successful sign-in. Grant targets must have one; it is
    /// observation, not desired state, so it does not go through a command.
    async fn record_login(&self, identity: &Identity) -> Result<(), StoreError>;
}

/// One catalog version with its lifecycle state.
#[derive(Debug, Clone, PartialEq)]
pub struct CatalogEntry<D> {
    pub status: VersionStatus,
    pub definition: D,
}

/// Hermes templates and terminal backends, one table of versions.
#[async_trait]
pub trait CatalogTx: Send {
    /// One version, locked until commit.
    async fn catalog_version<D: Definition>(
        &mut self,
        id: &str,
        version: i64,
    ) -> Result<Option<CatalogEntry<D>>, StoreError>;

    /// The highest version of an ID (0 if none), serializing new versions of it.
    async fn last_catalog_version(
        &mut self,
        kind: CatalogKind,
        id: &str,
    ) -> Result<i64, StoreError>;

    /// Inserts or replaces one version.
    async fn put_catalog_version<D: Definition>(
        &mut self,
        id: &str,
        version: i64,
        entry: &CatalogEntry<D>,
    ) -> Result<(), StoreError>;
}

/// An assignment and its instance.
#[derive(Debug, Clone, PartialEq)]
pub struct AssignmentRecord {
    pub id: Uuid,
    pub identity_id: Uuid,
    pub instance_id: Uuid,
    pub spec: AssignmentSpec,
    pub version: i64,
}

#[async_trait]
pub trait AssignmentTx: Send {
    /// One assignment, locked until commit.
    async fn assignment(&mut self, id: Uuid) -> Result<Option<AssignmentRecord>, StoreError>;

    async fn assignment_of(&mut self, identity: Uuid) -> Result<Option<Uuid>, StoreError>;

    /// Inserts the assignment and creates its instance in `namespace`.
    async fn insert_assignment(
        &mut self,
        record: &AssignmentRecord,
        namespace: &str,
    ) -> Result<(), StoreError>;

    async fn update_assignment(&mut self, record: &AssignmentRecord) -> Result<(), StoreError>;

    /// Advances the instance's desired generation, and with `execution` its
    /// execution generation too.
    async fn advance_instance(&mut self, instance: Uuid, execution: bool)
    -> Result<(), StoreError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SecretRecord {
    pub kind: SecretKind,
    pub version: i64,
}

#[async_trait]
pub trait SecretTx: Send {
    /// The current version of a secret, locked until commit.
    async fn secret(&mut self, name: &str) -> Result<Option<SecretRecord>, StoreError>;

    async fn put_secret_version(
        &mut self,
        name: &str,
        record: SecretRecord,
        sealed: &[u8],
    ) -> Result<(), StoreError>;

    async fn delete_secret(&mut self, name: &str) -> Result<(), StoreError>;

    /// Whether any catalog version or assignment references the secret.
    async fn secret_referenced(&mut self, name: &str) -> Result<bool, StoreError>;

    /// Instances whose assignment or pinned template references the secret.
    async fn instances_using_secret(&mut self, name: &str) -> Result<Vec<Uuid>, StoreError>;
}
