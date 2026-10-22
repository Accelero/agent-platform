use super::*;
use hermes_application::resources::{
    Creation, OwnedResource, PreparedResource, ResourceObservation, ResourceRepository,
    ResourceVersion,
};
use hermes_domain::resources::{ResourceKind, ResourceSpec, Retention, valid_uid};

mod observation;

pub(super) fn resource(row: &sqlx::postgres::PgRow) -> Result<OwnedResource, Error> {
    let creation: Creation =
        serde_json::from_value(row.get("creation")).map_err(|_| Error::RecoveryRequired)?;
    creation.spec.validate()?;
    if creation.id != row.get::<Uuid, _>("id")
        || creation.installation_id != row.get::<Uuid, _>("installation_id")
        || creation.instance_id != row.get::<Uuid, _>("instance_id")
        || creation.spec.kind.as_str() != row.get::<String, _>("kind")
        || creation.spec.name != row.get::<String, _>("name")
        || creation.spec.namespace.as_deref().unwrap_or("") != row.get::<String, _>("namespace")
        || creation.spec.encrypted_material_ref
            != row.get::<Option<Uuid>, _>("encrypted_material_ref")
    {
        return Err(Error::RecoveryRequired);
    }
    Ok(OwnedResource {
        creation,
        uid: row.get("uid"),
        resource_version: row.get("resource_version"),
        observed_digest: row.get("observed_digest"),
        state: row.get("state"),
    })
}
async fn journal(
    tx: &mut Transaction<'_, Postgres>,
    claim: &OperationClaim,
    key: &str,
    owned: &OwnedResource,
    intent: StepIntent,
) -> Result<JournalStep, Error> {
    intent.validate()?;
    sqlx::query("INSERT INTO manager.operation_steps(operation_id,step_key,intent,fence) VALUES($1,$2,$3,$4) ON CONFLICT DO NOTHING")
        .bind(claim.id).bind(key).bind(serde_json::to_value(&intent).map_err(|_|Error::Invalid)?).bind(claim.fence).execute(&mut **tx).await.map_err(db_error)?;
    let row=sqlx::query("SELECT intent,result FROM manager.operation_steps WHERE operation_id=$1 AND step_key=$2 FOR UPDATE")
        .bind(claim.id).bind(key).fetch_one(&mut **tx).await.map_err(db_error)?;
    let original: StepIntent =
        serde_json::from_value(row.get("intent")).map_err(|_| Error::RecoveryRequired)?;
    if original.action != intent.action
        || original.creation_id != intent.creation_id
        || original.kind != intent.kind
        || original.namespace != intent.namespace
        || original.name != intent.name
        || original.namespace_uid != intent.namespace_uid
        || original.manifest_digest != intent.manifest_digest
        || original.encrypted_material_ref != intent.encrypted_material_ref
    {
        return Err(Error::OwnershipConflict);
    }
    // Keep the first preconditions after a lost response or a new worker claim.
    // A new attempted write after a concurrent API mutation needs a new step key.
    sqlx::query(
        "UPDATE manager.operation_steps SET fence=$3 WHERE operation_id=$1 AND step_key=$2",
    )
    .bind(claim.id)
    .bind(key)
    .bind(claim.fence)
    .execute(&mut **tx)
    .await
    .map_err(db_error)?;
    sqlx::query("INSERT INTO manager.resource_steps(operation_id,step_key,resource_id) VALUES($1,$2,$3) ON CONFLICT DO NOTHING")
        .bind(claim.id).bind(key).bind(owned.creation.id).execute(&mut **tx).await.map_err(db_error)?;
    let bound: Uuid = sqlx::query_scalar(
        "SELECT resource_id FROM manager.resource_steps WHERE operation_id=$1 AND step_key=$2",
    )
    .bind(claim.id)
    .bind(key)
    .fetch_one(&mut **tx)
    .await
    .map_err(db_error)?;
    if bound != owned.creation.id {
        return Err(Error::OwnershipConflict);
    }
    Ok(JournalStep {
        intent: original,
        result: serde_json::from_value(row.get("result")).map_err(|_| Error::RecoveryRequired)?,
    })
}
fn intent(owned: &OwnedResource, action: Action, spec: &ResourceSpec) -> StepIntent {
    StepIntent {
        action,
        creation_id: owned.creation.id,
        kind: spec.kind.as_str().into(),
        namespace: spec.namespace.clone(),
        name: spec.name.clone(),
        namespace_uid: spec.namespace_uid.clone(),
        expected_uid: owned.uid.clone(),
        expected_resource_version: owned.resource_version.clone(),
        manifest_digest: spec.manifest_digest.clone(),
        encrypted_material_ref: spec.encrypted_material_ref,
    }
}
async fn locked_resource_step(
    tx: &mut Transaction<'_, Postgres>,
    claim: &OperationClaim,
    key: &str,
) -> Result<(OwnedResource, StepIntent, StepResult), Error> {
    let row=sqlx::query("SELECT r.*,s.intent,s.result,s.fence FROM manager.owned_resources r JOIN manager.resource_steps b ON b.resource_id=r.id JOIN manager.operation_steps s ON s.operation_id=b.operation_id AND s.step_key=b.step_key WHERE s.operation_id=$1 AND s.step_key=$2 AND r.instance_id=$3 FOR UPDATE OF r,s")
        .bind(claim.id).bind(key).bind(claim.instance_id).fetch_optional(&mut **tx).await.map_err(db_error)?.ok_or(Error::NotFound)?;
    if row.get::<i64, _>("fence") != claim.fence {
        return Err(Error::StaleFence);
    }
    Ok((
        resource(&row)?,
        serde_json::from_value(row.get("intent")).map_err(|_| Error::RecoveryRequired)?,
        serde_json::from_value(row.get("result")).map_err(|_| Error::RecoveryRequired)?,
    ))
}
#[async_trait]
impl ResourceRepository for PgStore {
    async fn prepare_observation(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
        key: &str,
        resource_id: Uuid,
    ) -> Result<hermes_application::resources::PreparedObservation, Error> {
        observation::prepare(self, scope, claim, key, resource_id).await
    }
    async fn bind_observation(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
        key: &str,
        observed: ResourceObservation,
    ) -> Result<(), Error> {
        observation::bind(self, scope, claim, key, observed).await
    }
    async fn observation_missing(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
        key: &str,
    ) -> Result<(), Error> {
        observation::missing(self, scope, claim, key).await
    }
    async fn find_resource(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
        spec: &ResourceSpec,
    ) -> Result<Option<OwnedResource>, Error> {
        spec.validate()?;
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        lock_claim(&mut tx, scope, claim).await?;
        let row=sqlx::query("SELECT * FROM manager.owned_resources WHERE installation_id=$1 AND instance_id=$2 AND kind=$3 AND namespace=$4 AND name=$5 AND state<>'Deleted'")
            .bind(scope.id).bind(claim.instance_id).bind(spec.kind.as_str()).bind(spec.namespace.as_deref().unwrap_or("")).bind(&spec.name).fetch_optional(&mut *tx).await.map_err(db_error)?;
        let owned = row.as_ref().map(resource).transpose()?;
        tx.commit().await.map_err(db_error)?;
        Ok(owned)
    }
    async fn observe_resource_version(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
        resource_id: Uuid,
        version: ResourceVersion,
    ) -> Result<(), Error> {
        if !valid_uid(&version.uid) || !valid_uid(&version.resource_version) {
            return Err(Error::Invalid);
        }
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        lock_claim(&mut tx, scope, claim).await?;
        let changed=sqlx::query("UPDATE manager.owned_resources SET resource_version=$5 WHERE id=$1 AND installation_id=$2 AND instance_id=$3 AND uid=$4 AND state='Bound'")
            .bind(resource_id).bind(scope.id).bind(claim.instance_id).bind(version.uid).bind(version.resource_version).execute(&mut *tx).await.map_err(db_error)?.rows_affected();
        if changed != 1 {
            return Err(Error::OwnershipConflict);
        }
        tx.commit().await.map_err(db_error)
    }
    async fn prepare_resource(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
        key: &str,
        spec: ResourceSpec,
    ) -> Result<PreparedResource, Error> {
        validate_name(key)?;
        spec.validate()?;
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        lock_claim(&mut tx, scope, claim).await?;
        let owner=sqlx::query("SELECT a.access,i.namespace,i.namespace_uid,p.cluster_identity FROM manager.instances i JOIN manager.assignments a ON a.id=i.assignment_id JOIN manager.installations p ON p.id=a.installation_id WHERE i.id=$1 AND a.installation_id=$2")
            .bind(claim.instance_id).bind(scope.id).fetch_one(&mut *tx).await.map_err(db_error)?;
        if owner.get::<String, _>("access") != "enabled" {
            return Err(Error::Forbidden);
        }
        let tombstone: Option<i64> = sqlx::query_scalar("SELECT max((creation->>'generation')::bigint) FROM manager.owned_resources WHERE installation_id=$1 AND kind=$2 AND namespace=$3 AND name=$4 AND state='Deleted'")
            .bind(scope.id).bind(spec.kind.as_str()).bind(spec.namespace.as_deref().unwrap_or("")).bind(&spec.name).fetch_one(&mut *tx).await.map_err(db_error)?;
        if let Some(generation) = tombstone {
            let managed: bool = sqlx::query_scalar("SELECT (a.infrastructure_managed OR a.dashboard_managed) FROM manager.assignments a JOIN manager.instances i ON i.assignment_id=a.id WHERE i.id=$1")
                .bind(claim.instance_id).fetch_one(&mut *tx).await.map_err(db_error)?;
            if !managed || claim.generation <= generation || spec.kind != ResourceKind::Deployment {
                return Err(Error::OwnershipConflict);
            }
        }
        let namespace: String = owner.get("namespace");
        if spec.kind == ResourceKind::Namespace {
            if spec.name != namespace {
                return Err(Error::OwnershipConflict);
            }
        } else if spec.namespace.as_deref() != Some(namespace.as_str())
            || spec.namespace_uid != owner.get::<Option<String>, _>("namespace_uid")
        {
            return Err(Error::OwnershipConflict);
        }
        if let Some(id) = spec.encrypted_material_ref {
            let valid:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM manager.bootstrap_materials WHERE id=$1 AND installation_id=$2 AND instance_id=$3 AND epoch<=$4)")
                .bind(id).bind(scope.id).bind(claim.instance_id).bind(scope.epoch).fetch_one(&mut *tx).await.map_err(db_error)?;
            if !valid {
                return Err(Error::OwnershipConflict);
            }
        }
        let creation = Creation {
            id: Uuid::new_v4(),
            installation_id: scope.id,
            instance_id: claim.instance_id,
            cluster_identity: owner.get("cluster_identity"),
            epoch: scope.epoch,
            generation: claim.generation,
            operation_id: claim.id,
            spec: spec.clone(),
        };
        sqlx::query("INSERT INTO manager.owned_resources(id,installation_id,instance_id,kind,namespace,name,creation,encrypted_material_ref) VALUES($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT DO NOTHING")
            .bind(creation.id).bind(scope.id).bind(claim.instance_id).bind(spec.kind.as_str()).bind(spec.namespace.as_deref().unwrap_or("")).bind(&spec.name).bind(serde_json::to_value(&creation).map_err(|_|Error::Invalid)?).bind(spec.encrypted_material_ref).execute(&mut *tx).await.map_err(db_error)?;
        let row=sqlx::query("SELECT * FROM manager.owned_resources WHERE installation_id=$1 AND kind=$2 AND namespace=$3 AND name=$4 AND state<>'Deleted' FOR UPDATE")
            .bind(scope.id).bind(spec.kind.as_str()).bind(spec.namespace.as_deref().unwrap_or("")).bind(&spec.name).fetch_one(&mut *tx).await.map_err(db_error)?;
        let owned = resource(&row)?;
        if owned.creation.instance_id != claim.instance_id
            || owned.creation.spec.namespace_uid != spec.namespace_uid
            || owned.creation.spec.purpose != spec.purpose
            || owned.creation.spec.retention != spec.retention
            || owned.creation.spec.encrypted_material_ref != spec.encrypted_material_ref
            || !matches!(owned.state.as_str(), "Reserved" | "Bound")
            || (owned.uid.is_none()
                && (owned.creation.epoch != scope.epoch
                    || owned.creation.spec.manifest_digest != spec.manifest_digest))
        {
            return Err(Error::OwnershipConflict);
        }
        let step = journal(
            &mut tx,
            claim,
            key,
            &owned,
            intent(&owned, Action::Apply, &spec),
        )
        .await?;
        if !matches!(step.result, StepResult::Reconciled { .. })
            && let (Some(uid), Some(version)) = (&owned.uid, &owned.resource_version)
        {
            let reconciled = StepResult::Reconciled {
                uid: uid.clone(),
                resource_version: version.clone(),
                next_step: key.into(),
            };
            // A bound UID can be re-observed before another idempotent apply.
            // Preserve uncertain writes as linked history; never replay an
            // initializer or infer that the previous write did not happen.
            sqlx::query("UPDATE manager.operation_steps s SET result=$5,verified_at=clock_timestamp() FROM manager.resource_steps b WHERE s.operation_id=b.operation_id AND s.step_key=b.step_key AND b.resource_id=$1 AND s.operation_id=$2 AND s.step_key<>$3 AND s.result->>'status'='unknown' AND s.intent->>'action'='apply' AND s.intent->>'manifestDigest'=$4")
                .bind(owned.creation.id).bind(claim.id).bind(key).bind(&spec.manifest_digest).bind(serde_json::to_value(reconciled).map_err(|_|Error::Invalid)?).execute(&mut *tx).await.map_err(db_error)?;
        }
        tx.commit().await.map_err(db_error)?;
        Ok(PreparedResource {
            resource: owned,
            step,
        })
    }

    async fn bind_resource(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
        key: &str,
        observation: ResourceObservation,
    ) -> Result<(), Error> {
        if !valid_uid(&observation.uid) || !valid_uid(&observation.resource_version) {
            return Err(Error::Invalid);
        }
        validate_sha256(&observation.manifest_digest)?;
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        lock_claim(&mut tx, scope, claim).await?;
        let (owned, intent, previous) = locked_resource_step(&mut tx, claim, key).await?;
        if matches!(previous, StepResult::Reconciled { .. }) {
            return Err(Error::StaleFence);
        }
        if intent.action != Action::Apply
            || !matches!(owned.state.as_str(), "Reserved" | "Bound")
            || owned
                .uid
                .as_ref()
                .is_some_and(|uid| uid != &observation.uid)
            || intent
                .expected_uid
                .as_ref()
                .is_some_and(|uid| uid != &observation.uid)
            || intent.manifest_digest != observation.manifest_digest
        {
            return Err(Error::OwnershipConflict);
        }
        let result = match previous {
            StepResult::Unknown => StepResult::Verified {
                uid: observation.uid.clone(),
                resource_version: observation.resource_version.clone(),
            },
            StepResult::Verified { ref uid, .. } if uid == &observation.uid => previous,
            _ => return Err(Error::OwnershipConflict),
        };
        sqlx::query("UPDATE manager.owned_resources SET state='Bound',uid=$2,resource_version=$3,observed_digest=$4 WHERE id=$1")
            .bind(owned.creation.id).bind(&observation.uid).bind(&observation.resource_version).bind(&observation.manifest_digest).execute(&mut *tx).await.map_err(db_error)?;
        if owned.creation.spec.kind == ResourceKind::Namespace {
            let changed=sqlx::query("UPDATE manager.instances SET namespace_uid=$2,state=CASE WHEN observed_generation<desired_generation THEN 'Provisioning' ELSE state END WHERE id=$1 AND (namespace_uid IS NULL OR namespace_uid=$2)")
                .bind(claim.instance_id).bind(&observation.uid).execute(&mut *tx).await.map_err(db_error)?.rows_affected();
            if changed != 1 {
                return Err(Error::OwnershipConflict);
            }
        }
        sqlx::query("UPDATE manager.operation_steps SET result=$3,verified_at=clock_timestamp() WHERE operation_id=$1 AND step_key=$2")
            .bind(claim.id).bind(key).bind(serde_json::to_value(result).map_err(|_|Error::Invalid)?).execute(&mut *tx).await.map_err(db_error)?;
        tx.commit().await.map_err(db_error)
    }

    async fn retire_resource(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
        key: &str,
        resource_id: Uuid,
        observation: ResourceObservation,
    ) -> Result<PreparedResource, Error> {
        validate_name(key)?;
        if !valid_uid(&observation.uid) || !valid_uid(&observation.resource_version) {
            return Err(Error::Invalid);
        }
        validate_sha256(&observation.manifest_digest)?;
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        lock_claim(&mut tx, scope, claim).await?;
        let row=sqlx::query("SELECT * FROM manager.owned_resources WHERE id=$1 AND installation_id=$2 AND instance_id=$3 FOR UPDATE")
            .bind(resource_id).bind(scope.id).bind(claim.instance_id).fetch_optional(&mut *tx).await.map_err(db_error)?.ok_or(Error::NotFound)?;
        let mut owned = resource(&row)?;
        if owned.creation.spec.retention == Retention::Retain {
            return Err(Error::Forbidden);
        }
        if owned.uid.as_ref() != Some(&observation.uid)
            || owned.observed_digest.as_ref() != Some(&observation.manifest_digest)
            || !matches!(owned.state.as_str(), "Bound" | "Retiring")
        {
            return Err(Error::OwnershipConflict);
        }
        owned.resource_version = Some(observation.resource_version);
        let mut spec = owned.creation.spec.clone();
        spec.manifest_digest = observation.manifest_digest;
        let step = journal(
            &mut tx,
            claim,
            key,
            &owned,
            intent(&owned, Action::Delete, &spec),
        )
        .await?;
        sqlx::query(
            "UPDATE manager.owned_resources SET state='Retiring',resource_version=$2 WHERE id=$1",
        )
        .bind(resource_id)
        .bind(&owned.resource_version)
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
        owned.state = "Retiring".into();
        tx.commit().await.map_err(db_error)?;
        Ok(PreparedResource {
            resource: owned,
            step,
        })
    }

    async fn resource_absent(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
        key: &str,
    ) -> Result<(), Error> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        lock_claim(&mut tx, scope, claim).await?;
        let (owned, intent, previous) = locked_resource_step(&mut tx, claim, key).await?;
        if intent.action != Action::Delete
            || !matches!(owned.state.as_str(), "Retiring" | "Deleted")
            || intent.expected_uid != owned.uid
        {
            return Err(Error::OwnershipConflict);
        }
        let result = StepResult::Absent {
            deleted_uid: owned.uid.ok_or(Error::OwnershipConflict)?,
        };
        if previous != StepResult::Unknown && previous != result {
            return Err(Error::OwnershipConflict);
        }
        sqlx::query("UPDATE manager.owned_resources SET state='Deleted' WHERE id=$1")
            .bind(owned.creation.id)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        sqlx::query("UPDATE manager.operation_steps s SET result=$3,verified_at=clock_timestamp() FROM manager.resource_steps b WHERE s.operation_id=b.operation_id AND s.step_key=b.step_key AND s.operation_id=$1 AND b.resource_id=$2 AND s.intent->>'action'='delete' AND s.intent->>'expectedUid'=$4 AND (s.result->>'status'='unknown' OR s.result=$3)")
            .bind(claim.id).bind(owned.creation.id).bind(serde_json::to_value(&result).map_err(|_|Error::Invalid)?).bind(match &result{StepResult::Absent{deleted_uid}=>deleted_uid,_=>return Err(Error::Invalid)}).execute(&mut *tx).await.map_err(db_error)?;
        tx.commit().await.map_err(db_error)
    }
}
