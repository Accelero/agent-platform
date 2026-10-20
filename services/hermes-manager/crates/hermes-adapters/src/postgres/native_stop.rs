//! A committed stop action is distinct from apply/delete and from drain completion.
use super::*;
use hermes_application::{
    lifecycle::{LifecyclePlan, LifecycleRepository},
    namespace_fence::{Admission, NamespaceFenceInput},
    native_stop::{
        NativeStopInput, NativeStopInspection, NativeStopObservation, NativeStopRepository,
        PreparedNativeStop,
    },
};
use hermes_domain::resources::valid_uid;

async fn guard(
    tx: &mut Transaction<'_, Postgres>,
    scope: &InstallationScope,
    claim: &OperationClaim,
    plan: &LifecyclePlan,
    closed_key: &str,
) -> Result<NamespaceFenceInput, Error> {
    lock_claim(tx, scope, claim).await?;
    let row=sqlx::query("SELECT a.access,a.version,i.namespace,i.namespace_uid,i.drain_generation,i.drain_epoch,i.drain_completed_at IS NULL AS active_drain FROM manager.assignments a JOIN manager.instances i ON i.assignment_id=a.id WHERE a.installation_id=$1 AND i.id=$2")
        .bind(scope.id).bind(claim.instance_id).fetch_one(&mut **tx).await.map_err(db_error)?;
    let drain = plan.drain.as_ref().ok_or(Error::Forbidden)?;
    if row.get::<String, _>("access") != plan.assignment.access.as_str()
        || row.get::<i64, _>("version") != plan.assignment.version
        || row.get::<String, _>("namespace") != plan.assignment.namespace
        || row.get::<Option<String>, _>("namespace_uid") != plan.namespace_uid
        || row.get::<Option<i64>, _>("drain_generation") != Some(drain.generation)
        || row.get::<Option<i64>, _>("drain_epoch") != Some(drain.epoch)
        || (plan.assignment.access == Access::Enabled
            && (!row.get::<bool, _>("active_drain") || drain.completed_at_millis.is_some()))
    {
        return Err(Error::StaleFence);
    }
    let row=sqlx::query("SELECT n.operation_id,n.step_key,n.input,n.input_digest,s.result FROM manager.namespace_fence_inputs n JOIN manager.operation_steps s ON s.operation_id=n.operation_id AND s.step_key=n.step_key JOIN manager.owned_resources r ON r.id=n.resource_id WHERE r.installation_id=$1 AND r.instance_id=$2 AND n.operation_id=$3 AND n.step_key=$4 AND s.verified_at IS NOT NULL")
        .bind(scope.id).bind(claim.instance_id).bind(claim.id).bind(closed_key).fetch_optional(&mut **tx).await.map_err(db_error)?.ok_or(Error::Forbidden)?;
    let input = validated_fence(&row, scope, claim, plan)?;
    // A newer idempotent CLOSE can have another key/RV. Preserve this action's
    // original input while requiring the latest committed state to agree.
    // Historical CLOSED evidence after a later OPEN is never sufficient.
    let latest=sqlx::query("SELECT n.operation_id,n.step_key,n.input,n.input_digest,s.result FROM manager.namespace_fence_inputs n JOIN manager.operation_steps s ON s.operation_id=n.operation_id AND s.step_key=n.step_key JOIN manager.owned_resources r ON r.id=n.resource_id WHERE r.installation_id=$1 AND r.instance_id=$2 AND s.verified_at IS NOT NULL ORDER BY s.verified_at DESC,s.recorded_at DESC,n.step_key DESC LIMIT 1")
        .bind(scope.id).bind(claim.instance_id).fetch_optional(&mut **tx).await.map_err(db_error)?.ok_or(Error::Forbidden)?;
    let latest = validated_fence(&latest, scope, claim, plan)?;
    if input.state != latest.state || input.namespace_uid != latest.namespace_uid {
        return Err(Error::OwnershipConflict);
    }
    Ok(input)
}
fn validated_fence(
    row: &sqlx::postgres::PgRow,
    scope: &InstallationScope,
    claim: &OperationClaim,
    plan: &LifecyclePlan,
) -> Result<NamespaceFenceInput, Error> {
    let input: NamespaceFenceInput =
        serde_json::from_value(row.get("input")).map_err(|_| Error::RecoveryRequired)?;
    input.validate()?;
    let result: StepResult =
        serde_json::from_value(row.get("result")).map_err(|_| Error::RecoveryRequired)?;
    if row.get::<Uuid, _>("operation_id") != claim.id
        || input.operation_id != claim.id
        || input.installation_id != scope.id
        || input.instance_id != claim.instance_id
        || input.namespace != plan.assignment.namespace
        || Some(&input.namespace_uid) != plan.namespace_uid.as_ref()
        || input.state.admission != Admission::Closed
        || input.state.access != plan.assignment.access
        || input.state.generation != claim.generation
        || input.state.epoch != scope.epoch
        || input.digest()? != row.get::<String, _>("input_digest")
        || !matches!(result,StepResult::Verified{ref uid,..} if uid==&input.namespace_uid)
    {
        return Err(Error::OwnershipConflict);
    }
    Ok(input)
}

async fn loaded(
    tx: &mut Transaction<'_, Postgres>,
    scope: &InstallationScope,
    claim: &OperationClaim,
    key: &str,
    plan: &LifecyclePlan,
) -> Result<PreparedNativeStop, Error> {
    let row=sqlx::query("SELECT r.*,n.input AS stop_input,n.input_digest,n.closed_fence_key,s.intent,s.result,s.fence,b.resource_id AS step_resource FROM manager.native_stop_inputs n JOIN manager.operation_steps s ON s.operation_id=n.operation_id AND s.step_key=n.step_key JOIN manager.resource_steps b ON b.operation_id=n.operation_id AND b.step_key=n.step_key JOIN manager.owned_resources r ON r.id=n.resource_id WHERE n.operation_id=$1 AND n.step_key=$2 AND r.instance_id=$3 FOR UPDATE OF r,s")
        .bind(claim.id).bind(key).bind(claim.instance_id).fetch_optional(&mut **tx).await.map_err(db_error)?.ok_or(Error::NotFound)?;
    if row.get::<i64, _>("fence") != claim.fence {
        return Err(Error::StaleFence);
    }
    let prepared = PreparedNativeStop {
        key: key.into(),
        namespace: plan
            .namespace
            .as_ref()
            .ok_or(Error::OwnershipConflict)?
            .owned
            .clone(),
        deployment: resources::resource(&row)?,
        input: serde_json::from_value(row.get("stop_input"))
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
        || prepared.input.digest()? != row.get::<String, _>("input_digest")
        || row.get::<Uuid, _>("step_resource") != prepared.deployment.creation.id
        || row.get::<String, _>("closed_fence_key") != prepared.input.closed_fence_key
    {
        return Err(Error::OwnershipConflict);
    }
    Ok(prepared)
}

#[async_trait]
impl NativeStopRepository for PgStore {
    async fn load_pending_native_stops(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
    ) -> Result<Vec<PreparedNativeStop>, Error> {
        let plan = self.lifecycle_plan(scope, claim).await?;
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        lock_claim(&mut tx, scope, claim).await?;
        let keys:Vec<String>=sqlx::query_scalar("SELECT n.step_key FROM manager.native_stop_inputs n JOIN manager.operation_steps s ON s.operation_id=n.operation_id AND s.step_key=n.step_key WHERE n.operation_id=$1 AND s.verified_at IS NULL ORDER BY s.recorded_at,n.step_key LIMIT 65")
            .bind(claim.id).fetch_all(&mut *tx).await.map_err(db_error)?;
        if keys.len() > 64 {
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
            let prepared = loaded(&mut tx, scope, claim, &key, &plan).await?;
            let closed = guard(
                &mut tx,
                scope,
                claim,
                &plan,
                &prepared.input.closed_fence_key,
            )
            .await?;
            let drain = plan.drain.as_ref().ok_or(Error::Forbidden)?;
            if prepared.input.closed_fence_digest != closed.digest()?
                || prepared.input.fence_state != closed.state
                || prepared.input.drain_epoch != drain.epoch
                || prepared.input.drain_generation != drain.generation
            {
                return Err(Error::OwnershipConflict);
            }
            pending.push(prepared);
        }
        tx.commit().await.map_err(db_error)?;
        Ok(pending)
    }
    async fn prepare_native_stop(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
        key: &str,
        closed_fence_key: &str,
        inspection: NativeStopInspection,
    ) -> Result<PreparedNativeStop, Error> {
        validate_name(key)?;
        validate_name(closed_fence_key)?;
        inspection.validate()?;
        let plan = self.lifecycle_plan(scope, claim).await?;
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        let closed = guard(&mut tx, scope, claim, &plan, closed_fence_key).await?;
        let retained = plan.deployment.as_ref().ok_or(Error::NotFound)?;
        if retained.owned.state != "Bound" || retained.owned.uid.as_ref() != Some(&inspection.uid) {
            return Err(Error::OwnershipConflict);
        }
        let drain = plan.drain.as_ref().ok_or(Error::Forbidden)?;
        let input = NativeStopInput {
            installation_id: scope.id,
            instance_id: claim.instance_id,
            operation_id: claim.id,
            namespace: plan.assignment.namespace.clone(),
            namespace_uid: closed.namespace_uid.clone(),
            deployment_uid: inspection.uid,
            resource_version: inspection.resource_version,
            preserved_spec_digest: inspection.preserved_spec_digest,
            closed_fence_key: closed_fence_key.into(),
            closed_fence_digest: closed.digest()?,
            fence_state: closed.state,
            drain_generation: drain.generation,
            drain_epoch: drain.epoch,
        };
        let intent = input.intent(retained.owned.creation.id)?;
        sqlx::query("INSERT INTO manager.operation_steps(operation_id,step_key,intent,fence) VALUES($1,$2,$3,$4) ON CONFLICT DO NOTHING")
            .bind(claim.id).bind(key).bind(serde_json::to_value(intent).map_err(|_|Error::Invalid)?).bind(claim.fence).execute(&mut *tx).await.map_err(db_error)?;
        sqlx::query("INSERT INTO manager.resource_steps(operation_id,step_key,resource_id) VALUES($1,$2,$3) ON CONFLICT DO NOTHING")
            .bind(claim.id).bind(key).bind(retained.owned.creation.id).execute(&mut *tx).await.map_err(db_error)?;
        sqlx::query("INSERT INTO manager.native_stop_inputs(operation_id,step_key,resource_id,closed_fence_key,input,input_digest) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT DO NOTHING")
            .bind(claim.id).bind(key).bind(retained.owned.creation.id).bind(closed_fence_key).bind(serde_json::to_value(&input).map_err(|_|Error::Invalid)?).bind(input.digest()?).execute(&mut *tx).await.map_err(db_error)?;
        sqlx::query(
            "UPDATE manager.operation_steps SET fence=$3 WHERE operation_id=$1 AND step_key=$2",
        )
        .bind(claim.id)
        .bind(key)
        .bind(claim.fence)
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
        let prepared = loaded(&mut tx, scope, claim, key, &plan).await?;
        let mut requested = input;
        requested.resource_version = prepared.input.resource_version.clone();
        if requested != prepared.input {
            return Err(Error::OwnershipConflict);
        }
        tx.commit().await.map_err(db_error)?;
        Ok(prepared)
    }
    async fn complete_native_stop(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
        key: &str,
        observation: NativeStopObservation,
    ) -> Result<(), Error> {
        if !valid_uid(&observation.uid) || !valid_uid(&observation.resource_version) {
            return Err(Error::Invalid);
        }
        let plan = self.lifecycle_plan(scope, claim).await?;
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        lock_claim(&mut tx, scope, claim).await?;
        let prepared = loaded(&mut tx, scope, claim, key, &plan).await?;
        let closed = guard(
            &mut tx,
            scope,
            claim,
            &plan,
            &prepared.input.closed_fence_key,
        )
        .await?;
        let drain = plan.drain.as_ref().ok_or(Error::Forbidden)?;
        if closed.digest()? != prepared.input.closed_fence_digest
            || closed.state != prepared.input.fence_state
            || drain.generation != prepared.input.drain_generation
            || drain.epoch != prepared.input.drain_epoch
            || observation.uid != prepared.input.deployment_uid
            || observation.input_digest != prepared.input.digest()?
            || observation.preserved_spec_digest != prepared.input.preserved_spec_digest
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
            .bind(prepared.deployment.creation.id).bind(claim.instance_id).bind(&observation.uid).bind(observation.resource_version).execute(&mut *tx).await.map_err(db_error)?.rows_affected();
        if changed != 1 {
            return Err(Error::OwnershipConflict);
        }
        sqlx::query("UPDATE manager.operation_steps SET result=$3,verified_at=COALESCE(verified_at,clock_timestamp()) WHERE operation_id=$1 AND step_key=$2")
            .bind(claim.id).bind(key).bind(serde_json::to_value(result).map_err(|_|Error::Invalid)?).execute(&mut *tx).await.map_err(db_error)?;
        tx.commit().await.map_err(db_error)
    }
}
