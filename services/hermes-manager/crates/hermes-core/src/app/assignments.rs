//! Assignment commands: who gets a Hermes instance, on which template, with
//! which volumes, terminal backends, admin settings and secrets.

use std::collections::{BTreeMap, BTreeSet};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

use super::actor::Actor;
use super::catalog::{check_secret_uses, reference_uses};
use super::envelope::{Applied, Command, CommandContext};
use super::envelope::{CommandEnvelope, IdempotencyKey};
use super::error::{AppError, Failure};
use super::ports::{AssignmentRecord, AssignmentTx, CatalogTx, Store, Tx};
use super::settings::Settings;
use crate::domain::Problem;
use crate::domain::access::Permission;
use crate::domain::assignments::{
    Access, AssignmentRejected, AssignmentSpec, TerminalGrant, overlapping, storage_change,
};
use crate::domain::backends::BackendDefinition;
use crate::domain::catalog::{CatalogKind, CatalogRejected, VersionRef, selectable};
use crate::domain::identity::Identity;
use crate::domain::secrets::SecretReference;
use crate::domain::storage::VolumeSpec;
use crate::domain::templates::{self, TemplateDefinition};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssignmentReceipt {
    pub assignment_id: Uuid,
    pub instance_id: Uuid,
    pub version: i64,
}

/// Loads pinned definitions and checks `next` against them, the storage of
/// other owners and, for updates, the current assignment. All problems are
/// reported together.
async fn check<T: Tx>(
    tx: &mut T,
    settings: &Settings,
    current: Option<&AssignmentRecord>,
    next: &AssignmentSpec,
) -> Result<(), Failure> {
    let mut problems: Vec<Problem> = Vec::new();
    let current_spec = current.map(|record| &record.spec);

    let template = tx
        .catalog_version::<TemplateDefinition>(&next.template.id, next.template.version)
        .await?;
    // A newly selected version must be published; an existing pin stays usable
    // after the version is withdrawn.
    let newly_selected = current_spec.is_none_or(|spec| spec.template != next.template);
    let template = match template {
        Some(entry)
            if !newly_selected || selectable(CatalogKind::Template, Some(entry.status)).is_ok() =>
        {
            entry.definition
        }
        _ => return Err(CatalogRejected::Unavailable(CatalogKind::Template).into()),
    };

    let mut backends = BTreeMap::new();
    for (id, version) in &next.terminal.allowed {
        let entry = tx
            .catalog_version::<BackendDefinition>(id, *version)
            .await?;
        let pinned =
            current_spec.is_some_and(|spec| spec.terminal.allowed.get(id) == Some(version));
        let usable = entry.as_ref().is_some_and(|entry| {
            pinned || selectable(CatalogKind::TerminalBackend, Some(entry.status)).is_ok()
        });
        if usable {
            backends.insert(id.clone(), entry.expect("usable").definition);
        }
    }
    if let Err(found) = next.validate(&template, &backends, &settings.sources) {
        problems.extend(found);
    }
    problems.extend(check_secret_uses(tx, &reference_uses(&next.secrets, "/secrets")).await?);

    let sources: BTreeSet<String> = next
        .volumes
        .values()
        .filter(|volume| volume.path.is_some())
        .map(|volume| volume.source.clone())
        .collect();
    let sources: Vec<String> = sources.into_iter().collect();
    let others = tx
        .lock_source_volumes(&sources, current.map(|record| record.id))
        .await?;
    if let Err(found) = overlapping(&next.volumes, others.iter()) {
        problems.extend(found);
    }

    if let Some(current) = current_spec {
        if let Err(found) = storage_change(&current.volumes, &next.volumes, &settings.sources) {
            problems.extend(found);
        }
        if current.template != next.template {
            let previous = tx
                .catalog_version::<TemplateDefinition>(
                    &current.template.id,
                    current.template.version,
                )
                .await?
                .ok_or(AppError::unavailable())?;
            if let Err(found) = templates::rollout(&previous.definition, &template) {
                problems.extend(found);
            }
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(AppError::invalid_configuration(problems).into())
    }
}

/// What the audit records about an assignment change: never secret values,
/// which assignments do not hold anyway.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Change<'a> {
    version: i64,
    access: &'a str,
    template: &'a VersionRef,
    volumes: Vec<&'a String>,
    backends: Vec<&'a String>,
}

fn change(record: &AssignmentRecord) -> Change<'_> {
    Change {
        version: record.version,
        access: record.spec.access.as_str(),
        template: &record.spec.template,
        volumes: record.spec.volumes.keys().collect(),
        backends: record.spec.terminal.allowed.keys().collect(),
    }
}

/// Gives an identity a Hermes instance. Missing volumes default from the template.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateAssignment {
    pub identity: Identity,
    pub spec: AssignmentSpec,
}

#[async_trait]
impl Command for CreateAssignment {
    type Receipt = AssignmentReceipt;
    const ACTION: &'static str = "assignment.create";

    fn permission(&self) -> Permission {
        Permission::Administer
    }

    async fn apply<T: Tx>(
        &self,
        tx: &mut T,
        context: &CommandContext<'_>,
    ) -> Result<Applied<AssignmentReceipt>, Failure> {
        self.identity.validate(&context.installation().issuer)?;
        let identity = tx.ensure_identity(&self.identity).await?;
        if tx.assignment_of(identity.id).await?.is_some() {
            return Err(AssignmentRejected::Exists.into());
        }
        let template = tx
            .catalog_version::<TemplateDefinition>(
                &self.spec.template.id,
                self.spec.template.version,
            )
            .await?
            .ok_or(CatalogRejected::Unavailable(CatalogKind::Template))?;
        let spec = self
            .spec
            .clone()
            .with_template_defaults(&template.definition);
        check(tx, context.settings, None, &spec).await?;
        let record = AssignmentRecord {
            id: Uuid::new_v4(),
            identity_id: identity.id,
            instance_id: Uuid::new_v4(),
            spec,
            version: 1,
        };
        let namespace = context
            .installation()
            .instance_namespace(record.instance_id);
        tx.insert_assignment(&record, &namespace).await?;
        let receipt = AssignmentReceipt {
            assignment_id: record.id,
            instance_id: record.instance_id,
            version: record.version,
        };
        Ok(
            Applied::new(receipt, "assignment", record.id, &change(&record))
                .notifying(record.instance_id),
        )
    }
}

/// The fields an update replaces; omitted fields stay as they are.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssignmentChanges {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<Access>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<VersionRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volumes: Option<BTreeMap<String, VolumeSpec>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal: Option<TerminalGrant>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admin_config: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secrets: Option<Vec<SecretReference>>,
}

impl AssignmentChanges {
    pub fn apply_to(&self, spec: &AssignmentSpec) -> AssignmentSpec {
        AssignmentSpec {
            access: self.access.unwrap_or(spec.access),
            template: self
                .template
                .clone()
                .unwrap_or_else(|| spec.template.clone()),
            volumes: self.volumes.clone().unwrap_or_else(|| spec.volumes.clone()),
            terminal: self
                .terminal
                .clone()
                .unwrap_or_else(|| spec.terminal.clone()),
            admin_config: self
                .admin_config
                .clone()
                .unwrap_or_else(|| spec.admin_config.clone()),
            secrets: self.secrets.clone().unwrap_or_else(|| spec.secrets.clone()),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateAssignment {
    pub assignment_id: Uuid,
    pub expected_version: i64,
    pub changes: AssignmentChanges,
}

#[async_trait]
impl Command for UpdateAssignment {
    type Receipt = AssignmentReceipt;
    const ACTION: &'static str = "assignment.update";

    fn permission(&self) -> Permission {
        Permission::Administer
    }

    async fn apply<T: Tx>(
        &self,
        tx: &mut T,
        context: &CommandContext<'_>,
    ) -> Result<Applied<AssignmentReceipt>, Failure> {
        let current = tx
            .assignment(self.assignment_id)
            .await?
            .ok_or(AssignmentRejected::NotFound)?;
        if current.version != self.expected_version {
            return Err(AssignmentRejected::StaleVersion.into());
        }
        let spec = self.changes.apply_to(&current.spec);
        check(tx, context.settings, Some(&current), &spec).await?;
        let execution = current.spec.execution_changed(&spec);
        let record = AssignmentRecord {
            spec,
            version: current.version + 1,
            ..current
        };
        tx.update_assignment(&record).await?;
        tx.advance_instance(record.instance_id, execution).await?;
        let receipt = AssignmentReceipt {
            assignment_id: record.id,
            instance_id: record.instance_id,
            version: record.version,
        };
        Ok(
            Applied::new(receipt, "assignment", record.id, &change(&record))
                .notifying(record.instance_id),
        )
    }
}

/// Asks the controller to reconcile again, for example after a blocked
/// condition was fixed outside the manager. Changes no desired state.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RetryInstance {
    pub assignment_id: Uuid,
    pub expected_version: i64,
}

#[async_trait]
impl Command for RetryInstance {
    type Receipt = AssignmentReceipt;
    const ACTION: &'static str = "instance.retry";

    fn permission(&self) -> Permission {
        Permission::Administer
    }

    async fn apply<T: Tx>(
        &self,
        tx: &mut T,
        _: &CommandContext<'_>,
    ) -> Result<Applied<AssignmentReceipt>, Failure> {
        let current = tx
            .assignment(self.assignment_id)
            .await?
            .ok_or(AssignmentRejected::NotFound)?;
        if current.version != self.expected_version {
            return Err(AssignmentRejected::StaleVersion.into());
        }
        tx.advance_instance(current.instance_id, false).await?;
        let receipt = AssignmentReceipt {
            assignment_id: current.id,
            instance_id: current.instance_id,
            version: current.version,
        };
        Ok(
            Applied::new(receipt, "instance", current.instance_id, &change(&current))
                .notifying(current.instance_id),
        )
    }
}

/// What a proposed change would do, without committing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preview {
    pub problems: Vec<Problem>,
    /// The Hermes Pod would be recreated (Recreate rollout).
    pub restarts_hermes: bool,
    /// New terminal environments would use a new definition.
    pub recreates_terminals: bool,
}

/// Checks `changes` against the current assignment inside a transaction that
/// is rolled back.
pub async fn preview<S: Store>(
    store: &S,
    settings: &Settings,
    assignment: Uuid,
    changes: &AssignmentChanges,
) -> Result<Preview, AppError> {
    let mut tx = store.begin().await?;
    let current = tx
        .assignment(assignment)
        .await?
        .ok_or(AssignmentRejected::NotFound)?;
    let next = changes.apply_to(&current.spec);
    let problems = match check(&mut tx, settings, Some(&current), &next).await {
        Ok(()) => Vec::new(),
        Err(Failure::Rejected(error)) if error.code == "invalid_configuration" => error.problems,
        Err(Failure::Rejected(error)) => return Err(error),
        Err(Failure::Store(error)) => return Err(error.into()),
    };
    let mounted = match tx
        .catalog_version::<TemplateDefinition>(&next.template.id, next.template.version)
        .await?
    {
        Some(template) => template.definition.mounts.keys().cloned().collect(),
        None => BTreeSet::new(),
    };
    // Dropping the transaction rolls back the locks the check took.
    drop(tx);
    let spec = &current.spec;
    let mounted_changed = mounted
        .iter()
        .any(|name| spec.volumes.get(name) != next.volumes.get(name));
    Ok(Preview {
        problems,
        restarts_hermes: spec.access != next.access
            || spec.template != next.template
            || spec.admin_config != next.admin_config
            || spec.secrets != next.secrets
            || spec.terminal.mode != next.terminal.mode
            || mounted_changed,
        recreates_terminals: spec.execution_changed(&next),
    })
}

/// One member of a template rollout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RolloutMember {
    pub assignment_id: Uuid,
    pub expected_version: i64,
}

pub const MAX_ROLLOUT: usize = 50;

/// Applies one template version to each member as its own change. Each member
/// uses a key derived from `key`, so a retry repeats only what did not commit.
pub async fn rollout<S: Store>(
    envelope: &CommandEnvelope<S>,
    actor: &Actor,
    request_id: Uuid,
    key: &IdempotencyKey,
    template: &VersionRef,
    members: &[RolloutMember],
) -> Result<Vec<(Uuid, Result<AssignmentReceipt, AppError>)>, AppError> {
    if members.is_empty() || members.len() > MAX_ROLLOUT {
        return Err(AppError::invalid());
    }
    let mut results = Vec::with_capacity(members.len());
    for member in members {
        let digest = Sha256::digest(format!("{}\0{}", key.as_str(), member.assignment_id));
        let member_key = IdempotencyKey::parse(
            &digest
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>(),
        )?;
        let command = UpdateAssignment {
            assignment_id: member.assignment_id,
            expected_version: member.expected_version,
            changes: AssignmentChanges {
                template: Some(template.clone()),
                ..Default::default()
            },
        };
        let result = envelope
            .execute(actor, request_id, &member_key, &command)
            .await;
        results.push((member.assignment_id, result));
    }
    Ok(results)
}
