//! Use cases and transactional ports shared by HTTP and controller adapters.

use std::collections::{BTreeMap, BTreeSet};

use async_trait::async_trait;
use hermes_domain::{Access, Identity, Invalid, TemplateVersion, validate_name, validate_sha256};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum Error {
    #[error("invalid request")]
    Invalid,
    #[error("administrator permission required")]
    Forbidden,
    #[error("invalid identity evidence")]
    InvalidIdentityEvidence,
    #[error("idempotency key was already used for different input")]
    IdempotencyConflict,
    #[error("assignment already exists")]
    AlreadyExists,
    #[error("resource not found")]
    NotFound,
    #[error("stale resource version")]
    PreconditionFailed,
    #[error("template is not approved or compatible")]
    TemplateUnavailable,
    #[error("registry is unavailable or requires recovery")]
    RecoveryRequired,
    #[error("installation is in maintenance")]
    Maintenance,
    #[error("operation claim is no longer current")]
    StaleFence,
    #[error("operation intent or resource ownership conflicts")]
    OwnershipConflict,
    #[error("dependency unavailable")]
    Unavailable,
}

impl From<Invalid> for Error {
    fn from(_: Invalid) -> Self {
        Self::Invalid
    }
}

/// Produced ONLY by a cryptographic authentication adapter. Never deserialize
/// this from HTTP headers/JSON, or populate it by decoding an unverified JWT.
#[derive(Debug, Clone)]
pub struct VerifiedPrincipal {
    pub identity: Identity,
    pub audiences: BTreeSet<String>,
    pub client_roles: BTreeMap<String, BTreeSet<String>>,
}

#[derive(Debug, Clone)]
pub struct AdminPolicy {
    pub issuer: String,
    pub audience: String,
    pub management_client: String,
    pub administrator_role: String,
}

impl AdminPolicy {
    pub fn authorize(&self, actor: &VerifiedPrincipal) -> Result<(), Error> {
        actor
            .identity
            .validate(&self.issuer)
            .map_err(|_| Error::Forbidden)?;
        if self.audience.is_empty()
            || self.management_client.is_empty()
            || self.administrator_role.is_empty()
            || !actor.audiences.contains(&self.audience)
            || !actor
                .client_roles
                .get(&self.management_client)
                .is_some_and(|roles| roles.contains(&self.administrator_role))
        {
            return Err(Error::Forbidden);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum AssignmentCommand {
    Create {
        identity: Identity,
        access: Access,
        template: TemplateVersion,
    },
    CreateDefaultTemplate {
        identity: Identity,
        access: Access,
        template_id: String,
    },
    SetAccess {
        assignment_id: Uuid,
        expected_version: i64,
        access: Access,
    },
    Retry {
        instance_id: Uuid,
        expected_version: i64,
    },
}

impl AssignmentCommand {
    pub fn scope(&self) -> &'static str {
        match self {
            Self::Create { .. } | Self::CreateDefaultTemplate { .. } => "assignment.create",
            Self::SetAccess { .. } => "assignment.set_access",
            Self::Retry { .. } => "instance.retry",
        }
    }

    fn validate(&self, issuer: &str) -> Result<(), Error> {
        match self {
            Self::CreateDefaultTemplate {
                identity,
                template_id,
                ..
            } => {
                identity.validate(issuer)?;
                validate_name(template_id)?;
            }
            Self::Create {
                identity, template, ..
            } => {
                identity.validate(issuer)?;
                template.validate()?;
            }
            Self::SetAccess {
                assignment_id,
                expected_version,
                ..
            } => {
                if assignment_id.is_nil() || *expected_version < 1 {
                    return Err(Error::Invalid);
                }
            }
            Self::Retry {
                instance_id,
                expected_version,
            } => {
                if instance_id.is_nil() || *expected_version < 1 {
                    return Err(Error::Invalid);
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct InstallationScope {
    pub id: Uuid,
    pub epoch: i64,
    pub issuer: String,
}

#[derive(Debug, Clone)]
pub struct Mutation {
    pub installation: InstallationScope,
    pub actor: Identity,
    pub request_id: Uuid,
    pub idempotency_key: String,
    pub body_digest: Vec<u8>,
    pub command: AssignmentCommand,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Accepted {
    pub assignment_id: Uuid,
    pub instance_id: Uuid,
    pub operation_id: Uuid,
    pub version: i64,
}

/// The entire mutation is one transaction: desired state, stable instance,
/// idempotency result, audit and outbox. Separate repository writes would break
/// the acknowledgement/restart boundary.
#[async_trait]
pub trait AssignmentStore: Send + Sync {
    async fn mutate(&self, mutation: Mutation) -> Result<Accepted, Error>;
}

pub struct Assignments<S> {
    store: S,
    installation: InstallationScope,
    policy: AdminPolicy,
}

impl<S: AssignmentStore> Assignments<S> {
    pub fn new(
        store: S,
        installation: InstallationScope,
        policy: AdminPolicy,
    ) -> Result<Self, Error> {
        hermes_domain::validate_issuer(&installation.issuer)?;
        if installation.id.is_nil()
            || installation.epoch < 1
            || installation.issuer != policy.issuer
        {
            return Err(Error::Invalid);
        }
        Ok(Self {
            store,
            installation,
            policy,
        })
    }

    pub async fn execute(
        &self,
        actor: &VerifiedPrincipal,
        request_id: Uuid,
        idempotency_key: &str,
        command: AssignmentCommand,
    ) -> Result<Accepted, Error> {
        self.policy.authorize(actor)?;
        command.validate(&self.installation.issuer)?;
        if request_id.is_nil()
            || idempotency_key.is_empty()
            || idempotency_key.len() > 128
            || !idempotency_key.bytes().all(|b| b.is_ascii_graphic())
        {
            return Err(Error::Invalid);
        }
        let body = serde_json::to_vec(&command).map_err(|_| Error::Invalid)?;
        self.store
            .mutate(Mutation {
                installation: self.installation.clone(),
                actor: actor.identity.clone(),
                request_id,
                idempotency_key: idempotency_key.into(),
                body_digest: Sha256::digest(body).to_vec(),
                command,
            })
            .await
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssignmentView {
    pub id: Uuid,
    pub identity: Identity,
    pub access: Access,
    pub template: TemplateVersion,
    pub version: i64,
    pub instance_id: Uuid,
    pub namespace: String,
    pub desired_generation: i64,
    pub observed_generation: i64,
    pub state: String,
}

#[async_trait]
pub trait AssignmentReader: Send + Sync {
    async fn get_assignment(
        &self,
        scope: &InstallationScope,
        id: Uuid,
    ) -> Result<AssignmentView, Error>;
    async fn list_assignments(
        &self,
        scope: &InstallationScope,
        after: Option<Uuid>,
        limit: u16,
    ) -> Result<Vec<AssignmentView>, Error>;
}

#[derive(Debug, Clone)]
pub struct OperationClaim {
    pub id: Uuid,
    pub instance_id: Uuid,
    pub generation: i64,
    pub epoch: i64,
    pub worker: Uuid,
    pub fence: i64,
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Apply,
    Delete,
    Initialize,
}

/// An immutable, committed external action. Bootstrap material is a reference
/// to encrypted create-once storage, never inline credentials.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StepIntent {
    pub action: Action,
    pub creation_id: Uuid,
    pub kind: String,
    pub namespace: Option<String>,
    pub name: String,
    pub namespace_uid: Option<String>,
    pub expected_uid: Option<String>,
    pub expected_resource_version: Option<String>,
    pub manifest_digest: String,
    pub encrypted_material_ref: Option<Uuid>,
}

impl StepIntent {
    pub fn validate(&self) -> Result<(), Error> {
        if self.creation_id.is_nil()
            || self.kind.is_empty()
            || self.kind.len() > 63
            || !self.kind.bytes().all(|b| b.is_ascii_alphanumeric())
        {
            return Err(Error::Invalid);
        }
        validate_name(&self.name)?;
        validate_sha256(&self.manifest_digest)?;
        if let Some(namespace) = &self.namespace {
            validate_name(namespace)?;
            if self.namespace_uid.as_ref().is_none_or(|u| u.is_empty()) {
                return Err(Error::Invalid);
            }
        } else if self.kind != "Namespace" {
            return Err(Error::Invalid);
        }
        if self.action == Action::Delete
            && (self.expected_uid.is_none() || self.expected_resource_version.is_none())
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum StepResult {
    Unknown,
    /// A bound UID was re-observed and a new immutable apply intent committed.
    /// This retains uncertainty about the original write rather than claiming
    /// it was never applied. Initializers and deletes cannot use this outcome.
    Reconciled {
        uid: String,
        resource_version: String,
        next_step: String,
    },
    Verified {
        uid: String,
        resource_version: String,
    },
    Absent {
        deleted_uid: String,
    },
    Blocked {
        reason: FailureReason,
    },
}

/// Bounded diagnostics prevent upstream errors, secrets or response bodies from
/// entering persistent audit/operation records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureReason {
    OwnershipConflict,
    DependencyUnavailable,
    InitializationUncertain,
    PolicyUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalStep {
    pub intent: StepIntent,
    pub result: StepResult,
}

#[async_trait]
pub trait OperationQueue: Send + Sync {
    async fn claim(
        &self,
        scope: &InstallationScope,
        worker: Uuid,
        lease_seconds: u16,
    ) -> Result<Option<OperationClaim>, Error>;
    async fn renew(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
        lease_seconds: u16,
    ) -> Result<(), Error>;
    async fn prepare_step(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
        key: &str,
        intent: StepIntent,
    ) -> Result<JournalStep, Error>;
    async fn record_result(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
        key: &str,
        result: StepResult,
    ) -> Result<(), Error>;
    async fn finish(&self, scope: &InstallationScope, claim: &OperationClaim) -> Result<(), Error>;
}

pub mod administration;
pub mod commissioning;
pub mod materials;
pub mod resources;
pub mod routing;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    const ISSUER: &str = "https://sso.example.org/realms/agents";

    #[derive(Clone, Default)]
    struct Store(Arc<AtomicUsize>);
    #[async_trait]
    impl AssignmentStore for Store {
        async fn mutate(&self, _: Mutation) -> Result<Accepted, Error> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(Error::Unavailable)
        }
    }

    #[tokio::test]
    async fn no_assignment_or_general_role_can_authorize_administration() {
        let store = Store::default();
        let policy = AdminPolicy {
            issuer: ISSUER.into(),
            audience: "manager-api".into(),
            management_client: "manager-admin".into(),
            administrator_role: "platform-administrator".into(),
        };
        let service = Assignments::new(
            store.clone(),
            InstallationScope {
                id: Uuid::new_v4(),
                epoch: 1,
                issuer: ISSUER.into(),
            },
            policy,
        )
        .unwrap();
        let command = AssignmentCommand::Create {
            identity: Identity {
                issuer: ISSUER.into(),
                subject: "owner-a".into(),
            },
            access: Access::Enabled,
            template: TemplateVersion {
                id: "personal-default".into(),
                version: 1,
            },
        };
        let mut actor = VerifiedPrincipal {
            identity: Identity {
                issuer: ISSUER.into(),
                subject: "admin-a".into(),
            },
            audiences: BTreeSet::from(["manager-api".into()]),
            client_roles: BTreeMap::new(),
        };
        assert_eq!(
            service
                .execute(&actor, Uuid::new_v4(), "request-1", command.clone())
                .await,
            Err(Error::Forbidden)
        );
        actor.client_roles.insert(
            "hermes".into(),
            BTreeSet::from(["platform-administrator".into()]),
        );
        assert_eq!(
            service
                .execute(&actor, Uuid::new_v4(), "request-1", command.clone())
                .await,
            Err(Error::Forbidden)
        );
        actor.client_roles.insert(
            "manager-admin".into(),
            BTreeSet::from(["platform-administrator".into()]),
        );
        actor.audiences.clear();
        assert_eq!(
            service
                .execute(&actor, Uuid::new_v4(), "request-1", command.clone())
                .await,
            Err(Error::Forbidden)
        );
        assert_eq!(store.0.load(Ordering::SeqCst), 0);
        actor.audiences.insert("manager-api".into());
        assert_eq!(
            service
                .execute(&actor, Uuid::new_v4(), "request-1", command)
                .await,
            Err(Error::Unavailable)
        );
        assert_eq!(store.0.load(Ordering::SeqCst), 1);
    }
}
