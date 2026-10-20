use super::*;
use hermes_application::{
    namespace_fence::{
        Admission, NamespaceFenceInput, NamespaceFenceObservation, NamespaceFenceRepository,
        NamespaceFenceState, PreparedNamespaceFence,
    },
    resources::ResourceVersion,
};
use hermes_domain::resources::valid_uid;

fn access(value: &str) -> Result<Access, Error> {
    match value {
        "enabled" => Ok(Access::Enabled),
        "suspended" => Ok(Access::Suspended),
        "revoked" => Ok(Access::Revoked),
        _ => Err(Error::RecoveryRequired),
    }
}

async fn current_access(
    tx: &mut Transaction<'_, Postgres>,
    scope: &InstallationScope,
    claim: &OperationClaim,
    admission: Admission,
) -> Result<(Access, String, Option<String>), Error> {
    let row=sqlx::query("SELECT a.access,i.namespace,i.namespace_uid,o.kind,o.input FROM manager.assignments a JOIN manager.instances i ON i.assignment_id=a.id JOIN manager.operations o ON o.id=$3 WHERE a.installation_id=$1 AND i.id=$2")
        .bind(scope.id).bind(claim.instance_id).bind(claim.id).fetch_one(&mut **tx).await.map_err(db_error)?;
    let actual = access(&row.get::<String, _>("access"))?;
    let input: Value = row.get("input");
    if row.get::<String, _>("kind") != claim.kind
        || input["access"].as_str() != Some(actual.as_str())
    {
        return Err(Error::StaleFence);
    }
    match (admission, claim.kind.as_str()) {
        (Admission::Open, "commission" | "resync") if actual == Access::Enabled => {}
        (Admission::Closed, "commission" | "retry" | "suspend" | "revoke" | "resync") => {}
        _ => return Err(Error::Forbidden),
    }
    Ok((actual, row.get("namespace"), row.get("namespace_uid")))
}

async fn loaded(
    tx: &mut Transaction<'_, Postgres>,
    scope: &InstallationScope,
    claim: &OperationClaim,
    key: &str,
) -> Result<PreparedNamespaceFence, Error> {
    let row=sqlx::query("SELECT r.*,n.input AS fence_input,n.input_digest,s.intent,s.result,s.fence,b.resource_id AS step_resource FROM manager.namespace_fence_inputs n JOIN manager.operation_steps s ON s.operation_id=n.operation_id AND s.step_key=n.step_key JOIN manager.resource_steps b ON b.operation_id=n.operation_id AND b.step_key=n.step_key JOIN manager.owned_resources r ON r.id=n.resource_id WHERE n.operation_id=$1 AND n.step_key=$2 AND r.instance_id=$3 FOR UPDATE OF r,s")
        .bind(claim.id).bind(key).bind(claim.instance_id).fetch_optional(&mut **tx).await.map_err(db_error)?.ok_or(Error::NotFound)?;
    if row.get::<i64, _>("fence") != claim.fence {
        return Err(Error::StaleFence);
    }
    let prepared = PreparedNamespaceFence {
        key: key.into(),
        namespace: resources::resource(&row)?,
        input: serde_json::from_value(row.get("fence_input"))
            .map_err(|_| Error::RecoveryRequired)?,
        step: JournalStep {
            intent: serde_json::from_value(row.get("intent"))
                .map_err(|_| Error::RecoveryRequired)?,
            result: serde_json::from_value(row.get("result"))
                .map_err(|_| Error::RecoveryRequired)?,
        },
    };
    prepared.validate(claim)?;
    if prepared.input.installation_id != scope.id
        || prepared.input.state.epoch != scope.epoch
        || prepared.input.digest()? != row.get::<String, _>("input_digest")
        || row.get::<Uuid, _>("step_resource") != prepared.namespace.creation.id
    {
        return Err(Error::OwnershipConflict);
    }
    Ok(prepared)
}

#[async_trait]
impl NamespaceFenceRepository for PgStore {
    async fn load_pending_namespace_fences(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
        admission: Admission,
    ) -> Result<Vec<PreparedNamespaceFence>, Error> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        lock_claim(&mut tx, scope, claim).await?;
        let (access, namespace, namespace_uid) =
            current_access(&mut tx, scope, claim, admission).await?;
        let keys: Vec<String> = sqlx::query_scalar("SELECT n.step_key FROM manager.namespace_fence_inputs n JOIN manager.operation_steps s ON s.operation_id=n.operation_id AND s.step_key=n.step_key JOIN manager.owned_resources r ON r.id=n.resource_id WHERE n.operation_id=$1 AND r.installation_id=$2 AND r.instance_id=$3 AND n.input->'state'->>'admission'=$4 AND s.verified_at IS NULL ORDER BY s.recorded_at,n.step_key LIMIT 33")
            .bind(claim.id).bind(scope.id).bind(claim.instance_id).bind(admission.as_str())
            .fetch_all(&mut *tx).await.map_err(db_error)?;
        if keys.len() > 32 {
            return Err(Error::OwnershipConflict);
        }
        let mut pending = Vec::with_capacity(keys.len());
        for key in keys {
            sqlx::query(
                "UPDATE manager.operation_steps SET fence=$3 WHERE operation_id=$1 AND step_key=$2",
            )
            .bind(claim.id)
            .bind(&key)
            .bind(claim.fence)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
            let prepared = loaded(&mut tx, scope, claim, &key).await?;
            if prepared.input.state
                != (NamespaceFenceState {
                    access,
                    admission,
                    generation: claim.generation,
                    epoch: scope.epoch,
                })
                || prepared.input.namespace != namespace
                || namespace_uid.as_ref() != Some(&prepared.input.namespace_uid)
                || !matches!(prepared.step.result, StepResult::Unknown)
            {
                return Err(Error::OwnershipConflict);
            }
            pending.push(prepared);
        }
        tx.commit().await.map_err(db_error)?;
        Ok(pending)
    }

    async fn prepare_namespace_fence(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
        key: &str,
        admission: Admission,
        version: ResourceVersion,
    ) -> Result<PreparedNamespaceFence, Error> {
        validate_name(key)?;
        if !valid_uid(&version.uid) || !valid_uid(&version.resource_version) {
            return Err(Error::Invalid);
        }
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        lock_claim(&mut tx, scope, claim).await?;
        let (access, namespace, namespace_uid) =
            current_access(&mut tx, scope, claim, admission).await?;
        let row=sqlx::query("SELECT * FROM manager.owned_resources WHERE installation_id=$1 AND instance_id=$2 AND kind='Namespace' AND namespace='' AND name=$3 FOR UPDATE")
            .bind(scope.id).bind(claim.instance_id).bind(&namespace).fetch_optional(&mut *tx).await.map_err(db_error)?.ok_or(Error::OwnershipConflict)?;
        let owned = resources::resource(&row)?;
        if owned.state != "Bound"
            || owned.uid.as_ref() != Some(&version.uid)
            || namespace_uid.as_ref() != Some(&version.uid)
        {
            return Err(Error::OwnershipConflict);
        }
        let input = NamespaceFenceInput {
            installation_id: scope.id,
            instance_id: claim.instance_id,
            operation_id: claim.id,
            namespace,
            namespace_uid: version.uid,
            resource_version: version.resource_version,
            state: NamespaceFenceState {
                access,
                admission,
                generation: claim.generation,
                epoch: scope.epoch,
            },
        };
        let intent = input.intent(owned.creation.id)?;
        sqlx::query("INSERT INTO manager.operation_steps(operation_id,step_key,intent,fence) VALUES($1,$2,$3,$4) ON CONFLICT DO NOTHING")
            .bind(claim.id).bind(key).bind(serde_json::to_value(&intent).map_err(|_|Error::Invalid)?).bind(claim.fence).execute(&mut *tx).await.map_err(db_error)?;
        sqlx::query("INSERT INTO manager.resource_steps(operation_id,step_key,resource_id) VALUES($1,$2,$3) ON CONFLICT DO NOTHING")
            .bind(claim.id).bind(key).bind(owned.creation.id).execute(&mut *tx).await.map_err(db_error)?;
        sqlx::query("INSERT INTO manager.namespace_fence_inputs(operation_id,step_key,resource_id,input,input_digest) VALUES($1,$2,$3,$4,$5) ON CONFLICT DO NOTHING")
            .bind(claim.id).bind(key).bind(owned.creation.id).bind(serde_json::to_value(&input).map_err(|_|Error::Invalid)?).bind(input.digest()?).execute(&mut *tx).await.map_err(db_error)?;
        sqlx::query(
            "UPDATE manager.operation_steps SET fence=$3 WHERE operation_id=$1 AND step_key=$2",
        )
        .bind(claim.id)
        .bind(key)
        .bind(claim.fence)
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
        let prepared = loaded(&mut tx, scope, claim, key).await?;
        let mut same_input = input;
        same_input.resource_version = prepared.input.resource_version.clone();
        if prepared.input != same_input {
            return Err(Error::OwnershipConflict);
        }
        tx.commit().await.map_err(db_error)?;
        Ok(prepared)
    }

    async fn complete_namespace_fence(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
        key: &str,
        observation: NamespaceFenceObservation,
    ) -> Result<(), Error> {
        if !valid_uid(&observation.uid) || !valid_uid(&observation.resource_version) {
            return Err(Error::Invalid);
        }
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        lock_claim(&mut tx, scope, claim).await?;
        let prepared = loaded(&mut tx, scope, claim, key).await?;
        let (access, namespace, namespace_uid) =
            current_access(&mut tx, scope, claim, prepared.input.state.admission).await?;
        if access != prepared.input.state.access
            || namespace != prepared.input.namespace
            || namespace_uid.as_ref() != Some(&observation.uid)
            || observation.uid != prepared.input.namespace_uid
            || observation.input_digest != prepared.input.digest()?
            || observation.state != prepared.input.state
        {
            return Err(Error::OwnershipConflict);
        }
        let result = match prepared.step.result {
            StepResult::Unknown => StepResult::Verified {
                uid: observation.uid.clone(),
                resource_version: observation.resource_version.clone(),
            },
            previous @ StepResult::Verified { .. } => previous,
            _ => return Err(Error::OwnershipConflict),
        };
        let changed=sqlx::query("UPDATE manager.owned_resources SET resource_version=$4 WHERE id=$1 AND instance_id=$2 AND uid=$3 AND state='Bound'")
            .bind(prepared.namespace.creation.id).bind(claim.instance_id).bind(&observation.uid).bind(&observation.resource_version).execute(&mut *tx).await.map_err(db_error)?.rows_affected();
        if changed != 1 {
            return Err(Error::OwnershipConflict);
        }
        sqlx::query("UPDATE manager.operation_steps SET result=$3,verified_at=COALESCE(verified_at,clock_timestamp()) WHERE operation_id=$1 AND step_key=$2")
            .bind(claim.id).bind(key).bind(serde_json::to_value(result).map_err(|_|Error::Invalid)?).execute(&mut *tx).await.map_err(db_error)?;
        tx.commit().await.map_err(db_error)
    }
}
