//! Read-only recovery of an already reserved native creation.
use super::*;
use hermes_application::{resources::PreparedObservation, resync::ResyncInput};

async fn owner(
    tx: &mut Transaction<'_, Postgres>,
    scope: &InstallationScope,
    claim: &OperationClaim,
) -> Result<sqlx::postgres::PgRow, Error> {
    let row = sqlx::query("SELECT a.access,a.template_id,a.template_version,a.version AS assignment_version,i.namespace,i.namespace_uid,p.cluster_identity,o.kind,o.input FROM manager.instances i JOIN manager.assignments a ON a.id=i.assignment_id JOIN manager.installations p ON p.id=a.installation_id JOIN manager.operations o ON o.id=$3 WHERE i.id=$1 AND a.installation_id=$2")
        .bind(claim.instance_id).bind(scope.id).bind(claim.id).fetch_one(&mut **tx).await.map_err(db_error)?;
    let access = row.get::<String, _>("access");
    let kind = row.get::<String, _>("kind");
    let input = row.get::<Value, _>("input");
    if kind != claim.kind || input["access"] != access {
        return Err(Error::StaleFence);
    }
    match (kind.as_str(), access.as_str()) {
        ("commission" | "retry", "enabled") | ("suspend", "suspended") | ("revoke", "revoked") => {}
        ("resync", "enabled" | "suspended" | "revoked") => {
            let pinned: ResyncInput =
                serde_json::from_value(input.clone()).map_err(|_| Error::OwnershipConflict)?;
            let uid: Option<String> = row.get("namespace_uid");
            if pinned.assignment_version != row.get::<i64, _>("assignment_version")
                || pinned.generation != claim.generation
                || pinned.epoch != scope.epoch
                || pinned
                    .namespace_uid
                    .as_deref()
                    .is_some_and(|pinned| !valid_uid(pinned) || uid.as_deref() != Some(pinned))
                || (access == "enabled" && pinned.namespace_uid.is_none())
            {
                return Err(Error::OwnershipConflict);
            }
        }
        _ => return Err(Error::Forbidden),
    }
    if input["templateId"] != row.get::<String, _>("template_id")
        || input["templateVersion"] != row.get::<i64, _>("template_version")
        || row.get::<String, _>("namespace") != instance_namespace(scope.id, claim.instance_id)
    {
        return Err(Error::OwnershipConflict);
    }
    Ok(row)
}

fn context(
    owned: &OwnedResource,
    owner: &sqlx::postgres::PgRow,
    scope: &InstallationScope,
    claim: &OperationClaim,
) -> Result<(), Error> {
    let c = &owned.creation;
    if c.id.is_nil()
        || c.operation_id.is_nil()
        || c.generation < 1
        || c.epoch < 1
        || c.epoch > scope.epoch
        || c.generation > claim.generation
        || (owned.uid.is_none() && c.epoch != scope.epoch)
    {
        return Err(Error::RecoveryRequired);
    }
    let namespace: String = owner.get("namespace");
    let namespace_uid: Option<String> = owner.get("namespace_uid");
    if c.installation_id != scope.id
        || c.instance_id != claim.instance_id
        || c.cluster_identity != owner.get::<String, _>("cluster_identity")
        || !matches!(owned.state.as_str(), "Reserved" | "Bound")
        || namespace_uid.as_deref().is_some_and(|uid| !valid_uid(uid))
    {
        return Err(Error::OwnershipConflict);
    }
    if c.spec.kind == ResourceKind::Namespace {
        if c.spec.name != namespace || owned.uid != namespace_uid {
            return Err(Error::OwnershipConflict);
        }
    } else if c.spec.namespace.as_deref() != Some(namespace.as_str())
        || c.spec.namespace_uid != namespace_uid
    {
        return Err(Error::OwnershipConflict);
    }
    Ok(())
}

async fn original(tx: &mut Transaction<'_, Postgres>, owned: &OwnedResource) -> Result<(), Error> {
    let c = &owned.creation;
    let label = if c.spec.kind == ResourceKind::Namespace {
        "namespace"
    } else {
        "instance"
    };
    let purpose = format!("render-{label}-g{}", c.generation);
    let rendering: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM manager.bootstrap_materials WHERE installation_id=$1 AND instance_id=$2 AND purpose=$3 AND epoch=$4 AND creation_generation=$5 AND creation_operation=$6)")
        .bind(c.installation_id).bind(c.instance_id).bind(purpose).bind(c.epoch).bind(c.generation).bind(c.operation_id)
        .fetch_one(&mut **tx).await.map_err(db_error)?;
    let creation: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM manager.resource_steps b JOIN manager.operation_steps s ON s.operation_id=b.operation_id AND s.step_key=b.step_key JOIN manager.operations o ON o.id=b.operation_id WHERE b.resource_id=$1 AND o.id=$2 AND o.installation_id=$3 AND o.instance_id=$4 AND o.epoch=$5 AND o.generation=$6 AND s.intent->>'action'='apply' AND s.intent->>'creationId'=$7 AND s.intent->>'manifestDigest'=$8)")
        .bind(c.id).bind(c.operation_id).bind(c.installation_id).bind(c.instance_id).bind(c.epoch).bind(c.generation)
        .bind(c.id.to_string()).bind(&c.spec.manifest_digest).fetch_one(&mut **tx).await.map_err(db_error)?;
    if !rendering || !creation {
        return Err(Error::RecoveryRequired);
    }
    Ok(())
}

pub(super) async fn prepare(
    store: &PgStore,
    scope: &InstallationScope,
    claim: &OperationClaim,
    key: &str,
    resource_id: Uuid,
) -> Result<PreparedObservation, Error> {
    validate_name(key)?;
    let mut tx = store.pool.begin().await.map_err(db_error)?;
    lock_claim(&mut tx, scope, claim).await?;
    let owner = owner(&mut tx, scope, claim).await?;
    let row = sqlx::query("SELECT * FROM manager.owned_resources WHERE id=$1 AND installation_id=$2 AND instance_id=$3 FOR UPDATE")
        .bind(resource_id).bind(scope.id).bind(claim.instance_id).fetch_optional(&mut *tx).await.map_err(db_error)?.ok_or(Error::NotFound)?;
    let owned = resource(&row)?;
    context(&owned, &owner, scope, claim)?;
    original(&mut tx, &owned).await?;
    if owned.uid.is_some() {
        let replay: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM manager.operation_steps s JOIN manager.resource_steps b ON b.operation_id=s.operation_id AND b.step_key=s.step_key WHERE s.operation_id=$1 AND s.step_key=$2 AND b.resource_id=$3 AND s.intent->>'action'='observe' AND s.result->>'status'='verified')")
            .bind(claim.id).bind(key).bind(resource_id).fetch_one(&mut *tx).await.map_err(db_error)?;
        if !replay {
            return Err(Error::OwnershipConflict);
        }
    }
    let step = journal(
        &mut tx,
        claim,
        key,
        &owned,
        intent(&owned, Action::Observe, &owned.creation.spec),
    )
    .await?;
    let prepared = PreparedObservation {
        resource: owned,
        step,
    };
    prepared.validate()?;
    tx.commit().await.map_err(db_error)?;
    Ok(prepared)
}

pub(super) async fn bind(
    store: &PgStore,
    scope: &InstallationScope,
    claim: &OperationClaim,
    key: &str,
    observed: ResourceObservation,
) -> Result<(), Error> {
    if !valid_uid(&observed.uid) || !valid_uid(&observed.resource_version) {
        return Err(Error::Invalid);
    }
    validate_sha256(&observed.manifest_digest)?;
    let mut tx = store.pool.begin().await.map_err(db_error)?;
    lock_claim(&mut tx, scope, claim).await?;
    let owner = owner(&mut tx, scope, claim).await?;
    let (owned, intent, previous) = locked_resource_step(&mut tx, claim, key).await?;
    context(&owned, &owner, scope, claim)?;
    original(&mut tx, &owned).await?;
    PreparedObservation {
        resource: owned.clone(),
        step: JournalStep {
            intent: intent.clone(),
            result: previous.clone(),
        },
    }
    .validate()?;
    if intent.manifest_digest != observed.manifest_digest
        || owned.uid.as_ref().is_some_and(|uid| uid != &observed.uid)
    {
        return Err(Error::OwnershipConflict);
    }
    let result = match previous {
        StepResult::Unknown => StepResult::Verified {
            uid: observed.uid.clone(),
            resource_version: observed.resource_version.clone(),
        },
        StepResult::Verified { ref uid, .. } if uid == &observed.uid => previous,
        _ => return Err(Error::OwnershipConflict),
    };
    sqlx::query("UPDATE manager.owned_resources SET state='Bound',uid=$2,resource_version=$3,observed_digest=$4 WHERE id=$1")
        .bind(owned.creation.id).bind(&observed.uid).bind(&observed.resource_version).bind(&observed.manifest_digest).execute(&mut *tx).await.map_err(db_error)?;
    if owned.creation.spec.kind == ResourceKind::Namespace {
        let changed = sqlx::query("UPDATE manager.instances SET namespace_uid=$2 WHERE id=$1 AND (namespace_uid IS NULL OR namespace_uid=$2)")
            .bind(claim.instance_id).bind(&observed.uid).execute(&mut *tx).await.map_err(db_error)?.rows_affected();
        if changed != 1 {
            return Err(Error::OwnershipConflict);
        }
    }
    sqlx::query("UPDATE manager.operation_steps SET result=$3,verified_at=COALESCE(verified_at,clock_timestamp()) WHERE operation_id=$1 AND step_key=$2")
        .bind(claim.id).bind(key).bind(serde_json::to_value(result).map_err(|_|Error::Invalid)?).execute(&mut *tx).await.map_err(db_error)?;
    tx.commit().await.map_err(db_error)
}

pub(super) async fn missing(
    store: &PgStore,
    scope: &InstallationScope,
    claim: &OperationClaim,
    key: &str,
) -> Result<(), Error> {
    let mut tx = store.pool.begin().await.map_err(db_error)?;
    lock_claim(&mut tx, scope, claim).await?;
    let owner = owner(&mut tx, scope, claim).await?;
    let (owned, intent, previous) = locked_resource_step(&mut tx, claim, key).await?;
    context(&owned, &owner, scope, claim)?;
    original(&mut tx, &owned).await?;
    PreparedObservation {
        resource: owned.clone(),
        step: JournalStep {
            intent,
            result: previous.clone(),
        },
    }
    .validate()?;
    if owned.uid.is_some() || !matches!(previous, StepResult::Unknown | StepResult::ObservedMissing)
    {
        return Err(Error::OwnershipConflict);
    }
    sqlx::query("UPDATE manager.operation_steps SET result=$3,verified_at=COALESCE(verified_at,clock_timestamp()) WHERE operation_id=$1 AND step_key=$2")
        .bind(claim.id).bind(key).bind(serde_json::to_value(StepResult::ObservedMissing).map_err(|_|Error::Invalid)?).execute(&mut *tx).await.map_err(db_error)?;
    tx.commit().await.map_err(db_error)
}
