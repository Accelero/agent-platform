//! Catalog commands: Hermes templates and terminal backends share one draft,
//! publish and withdraw lifecycle.

use std::marker::PhantomData;

use async_trait::async_trait;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use super::envelope::{Applied, Command, CommandContext};
use super::error::{AppError, Failure};
use super::ports::{CatalogEntry, Store, Tx};
use super::settings::Settings;
use crate::domain::Problem;
use crate::domain::access::Permission;
use crate::domain::backends::BackendDefinition;
use crate::domain::catalog::{self, CatalogKind, CatalogRejected, VersionState, VersionStatus};
use crate::domain::names::validate_name;
use crate::domain::secrets::{SecretKind, SecretReference};
use crate::domain::templates::TemplateDefinition;

/// A catalog definition type and its audit actions.
pub trait Definition:
    Serialize + DeserializeOwned + Clone + PartialEq + Send + Sync + 'static
{
    const KIND: CatalogKind;
    const CREATE: &'static str;
    const EDIT: &'static str;
    const PUBLISH: &'static str;
    const WITHDRAW: &'static str;

    /// Checks the definition against installation settings, for publication.
    fn check(&self, settings: &Settings) -> Result<(), Vec<Problem>>;

    /// The secrets the definition names, with the kind each use requires.
    fn secret_uses(&self) -> Vec<SecretUse>;
}

/// A reference to an admin secret, at a pointer in its document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretUse {
    pub pointer: String,
    pub name: String,
    pub kind: SecretKind,
}

/// The uses of a list of secret references at `at`.
pub fn reference_uses(references: &[SecretReference], at: &str) -> Vec<SecretUse> {
    references
        .iter()
        .enumerate()
        .map(|(index, reference)| SecretUse {
            pointer: format!("{at}/{index}/name"),
            name: reference.name.clone(),
            kind: if reference.env.is_some() {
                SecretKind::Env
            } else {
                SecretKind::File
            },
        })
        .collect()
}

/// Every used secret must exist with the kind its use requires.
pub async fn check_secret_uses<T: Tx>(
    tx: &mut T,
    uses: &[SecretUse],
) -> Result<Vec<Problem>, Failure> {
    let mut problems = Vec::new();
    for secret_use in uses {
        let code = match tx.secret(&secret_use.name).await? {
            Some(secret) if secret.kind == secret_use.kind => continue,
            Some(_) => "wrong_secret_kind",
            None => "unknown_secret",
        };
        problems.push(Problem {
            pointer: secret_use.pointer.clone(),
            code,
        });
    }
    Ok(problems)
}

impl Definition for TemplateDefinition {
    const KIND: CatalogKind = CatalogKind::Template;
    const CREATE: &'static str = "template.create";
    const EDIT: &'static str = "template.edit";
    const PUBLISH: &'static str = "template.publish";
    const WITHDRAW: &'static str = "template.withdraw";

    fn check(&self, settings: &Settings) -> Result<(), Vec<Problem>> {
        self.validate(&settings.sources)
    }

    fn secret_uses(&self) -> Vec<SecretUse> {
        reference_uses(&self.secrets, "/secrets")
    }
}

impl Definition for BackendDefinition {
    const KIND: CatalogKind = CatalogKind::TerminalBackend;
    const CREATE: &'static str = "terminal_backend.create";
    const EDIT: &'static str = "terminal_backend.edit";
    const PUBLISH: &'static str = "terminal_backend.publish";
    const WITHDRAW: &'static str = "terminal_backend.withdraw";

    fn check(&self, _settings: &Settings) -> Result<(), Vec<Problem>> {
        self.validate()
    }

    fn secret_uses(&self) -> Vec<SecretUse> {
        self.registry_secret
            .iter()
            .map(|name| SecretUse {
                pointer: "/registrySecret".into(),
                name: name.clone(),
                kind: SecretKind::Registry,
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionReceipt {
    pub id: String,
    pub version: i64,
    pub revision: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Change<'a> {
    version: i64,
    state: &'a str,
    revision: i64,
}

fn applied(
    kind: CatalogKind,
    id: &str,
    version: i64,
    status: VersionStatus,
) -> Applied<VersionReceipt> {
    let receipt = VersionReceipt {
        id: id.to_owned(),
        version,
        revision: status.revision,
    };
    let change = Change {
        version,
        state: status.state.as_str(),
        revision: status.revision,
    };
    Applied::new(receipt, kind.as_str(), id, &change)
}

/// Starts the next version of `id` as a draft.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateDraft<D> {
    pub id: String,
    pub definition: D,
}

#[async_trait]
impl<D: Definition> Command for CreateDraft<D> {
    type Receipt = VersionReceipt;
    const ACTION: &'static str = D::CREATE;

    fn permission(&self) -> Permission {
        Permission::Administer
    }

    async fn apply<T: Tx>(
        &self,
        tx: &mut T,
        _: &CommandContext<'_>,
    ) -> Result<Applied<VersionReceipt>, Failure> {
        validate_name(&self.id)?;
        let version = tx.last_catalog_version(D::KIND, &self.id).await? + 1;
        let entry = CatalogEntry {
            status: VersionStatus {
                state: VersionState::Draft,
                revision: 1,
            },
            definition: self.definition.clone(),
        };
        tx.put_catalog_version(&self.id, version, &entry).await?;
        Ok(applied(D::KIND, &self.id, version, entry.status))
    }
}

/// Replaces a draft's definition.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditDraft<D> {
    pub id: String,
    pub version: i64,
    pub expected_revision: i64,
    pub definition: D,
}

#[async_trait]
impl<D: Definition> Command for EditDraft<D> {
    type Receipt = VersionReceipt;
    const ACTION: &'static str = D::EDIT;

    fn permission(&self) -> Permission {
        Permission::Administer
    }

    async fn apply<T: Tx>(
        &self,
        tx: &mut T,
        _: &CommandContext<'_>,
    ) -> Result<Applied<VersionReceipt>, Failure> {
        let current = tx
            .catalog_version::<D>(&self.id, self.version)
            .await?
            .ok_or(CatalogRejected::NotFound)?;
        let status = catalog::edit(current.status, self.expected_revision)?;
        let entry = CatalogEntry {
            status,
            definition: self.definition.clone(),
        };
        tx.put_catalog_version(&self.id, self.version, &entry)
            .await?;
        Ok(applied(D::KIND, &self.id, self.version, status))
    }
}

/// Validates and freezes a draft.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Publish<D> {
    pub id: String,
    pub version: i64,
    pub expected_revision: i64,
    #[serde(skip)]
    pub kind: PhantomData<D>,
}

#[async_trait]
impl<D: Definition> Command for Publish<D> {
    type Receipt = VersionReceipt;
    const ACTION: &'static str = D::PUBLISH;

    fn permission(&self) -> Permission {
        Permission::Administer
    }

    async fn apply<T: Tx>(
        &self,
        tx: &mut T,
        context: &CommandContext<'_>,
    ) -> Result<Applied<VersionReceipt>, Failure> {
        let current = tx
            .catalog_version::<D>(&self.id, self.version)
            .await?
            .ok_or(CatalogRejected::NotFound)?;
        let status = catalog::publish(current.status, self.expected_revision)?;
        let mut problems = current
            .definition
            .check(context.settings)
            .err()
            .unwrap_or_default();
        problems.extend(check_secret_uses(tx, &current.definition.secret_uses()).await?);
        if !problems.is_empty() {
            return Err(AppError::invalid_configuration(problems).into());
        }
        let entry = CatalogEntry {
            status,
            definition: current.definition,
        };
        tx.put_catalog_version(&self.id, self.version, &entry)
            .await?;
        Ok(applied(D::KIND, &self.id, self.version, status))
    }
}

/// Stops new selections of a published version. Existing pins keep it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Withdraw<D> {
    pub id: String,
    pub version: i64,
    pub expected_revision: i64,
    #[serde(skip)]
    pub kind: PhantomData<D>,
}

#[async_trait]
impl<D: Definition> Command for Withdraw<D> {
    type Receipt = VersionReceipt;
    const ACTION: &'static str = D::WITHDRAW;

    fn permission(&self) -> Permission {
        Permission::Administer
    }

    async fn apply<T: Tx>(
        &self,
        tx: &mut T,
        _: &CommandContext<'_>,
    ) -> Result<Applied<VersionReceipt>, Failure> {
        let current = tx
            .catalog_version::<D>(&self.id, self.version)
            .await?
            .ok_or(CatalogRejected::NotFound)?;
        let status = catalog::withdraw(current.status, self.expected_revision)?;
        let entry = CatalogEntry {
            status,
            definition: current.definition,
        };
        tx.put_catalog_version(&self.id, self.version, &entry)
            .await?;
        Ok(applied(D::KIND, &self.id, self.version, status))
    }
}

/// Validates a definition against installation settings and existing
/// secrets, storing nothing.
pub async fn validate<S: Store, D: Definition>(
    store: &S,
    settings: &Settings,
    definition: &D,
) -> Result<Vec<Problem>, AppError> {
    let mut problems = definition.check(settings).err().unwrap_or_default();
    let mut tx = store.begin().await?;
    match check_secret_uses(&mut tx, &definition.secret_uses()).await {
        Ok(found) => problems.extend(found),
        Err(Failure::Rejected(error)) => return Err(error),
        Err(Failure::Store(error)) => return Err(error.into()),
    }
    Ok(problems)
}
