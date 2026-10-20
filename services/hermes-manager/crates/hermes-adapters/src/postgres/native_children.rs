//! Native child identities and immutable external-action inputs.
use super::*;
use hermes_application::native_children::{
    NativeChildAction, NativeChildInput, NativeChildObservation, NativeChildRepository,
    NativeChildResult, PreparedNativeChild,
};
use hermes_domain::resources::ResourceKind;

async fn authorized(
    tx: &mut Transaction<'_, Postgres>,
    scope: &InstallationScope,
    claim: &OperationClaim,
    action: NativeChildAction,
    fence_key: &str,
) -> Result<(), Error> {
    let row=sqlx::query("SELECT a.access,o.kind,o.input,i.drain_started_at IS NOT NULL AND i.drain_completed_at IS NULL AS draining FROM manager.assignments a JOIN manager.instances i ON i.assignment_id=a.id JOIN manager.operations o ON o.id=$3 WHERE a.installation_id=$1 AND i.id=$2")
        .bind(scope.id).bind(claim.instance_id).bind(claim.id).fetch_one(&mut **tx).await.map_err(db_error)?;
    let access: String = row.get("access");
    let kind: String = row.get("kind");
    let valid_kind = match access.as_str() {
        "enabled" => matches!(kind.as_str(), "commission" | "resync"),
        "suspended" => matches!(kind.as_str(), "suspend" | "resync"),
        "revoked" => matches!(kind.as_str(), "revoke" | "resync"),
        _ => false,
    };
    if kind != claim.kind || !valid_kind || row.get::<Value, _>("input")["access"] != access {
        return Err(Error::StaleFence);
    }
    if action == NativeChildAction::Start && (access != "enabled" || row.get::<bool, _>("draining"))
    {
        return Err(Error::Forbidden);
    }
    if action == NativeChildAction::Stop && access == "enabled" && !row.get::<bool, _>("draining") {
        return Err(Error::Forbidden);
    }
    let admission = if action == NativeChildAction::Start {
        "open"
    } else {
        "closed"
    };
    let fence=sqlx::query("SELECT n.input,n.input_digest,s.result FROM manager.namespace_fence_inputs n JOIN manager.operation_steps s USING(operation_id,step_key) WHERE n.operation_id=$1 AND n.step_key=$2 AND s.verified_at IS NOT NULL")
        .bind(claim.id).bind(fence_key).fetch_optional(&mut **tx).await.map_err(db_error)?.ok_or(Error::OwnershipConflict)?;
    let original = checked_fence(&fence, scope, claim, &access, admission)?;
    let latest=sqlx::query("SELECT n.input,n.input_digest,s.result FROM manager.namespace_fence_inputs n JOIN manager.operation_steps s USING(operation_id,step_key) JOIN manager.owned_resources r ON r.id=n.resource_id WHERE r.installation_id=$1 AND r.instance_id=$2 AND s.verified_at IS NOT NULL ORDER BY s.verified_at DESC,s.recorded_at DESC,n.step_key DESC LIMIT 1")
        .bind(scope.id).bind(claim.instance_id).fetch_optional(&mut **tx).await.map_err(db_error)?.ok_or(Error::OwnershipConflict)?;
    let latest = checked_fence(&latest, scope, claim, &access, admission)?;
    let namespace_uid: Option<String> =
        sqlx::query_scalar("SELECT namespace_uid FROM manager.instances WHERE id=$1")
            .bind(claim.instance_id)
            .fetch_one(&mut **tx)
            .await
            .map_err(db_error)?;
    if latest.state != original.state
        || latest.namespace_uid != original.namespace_uid
        || namespace_uid.as_ref() != Some(&original.namespace_uid)
    {
        return Err(Error::OwnershipConflict);
    }
    Ok(())
}
fn checked_fence(
    row: &sqlx::postgres::PgRow,
    scope: &InstallationScope,
    claim: &OperationClaim,
    access: &str,
    admission: &str,
) -> Result<hermes_application::namespace_fence::NamespaceFenceInput, Error> {
    let fence: hermes_application::namespace_fence::NamespaceFenceInput =
        serde_json::from_value(row.get("input")).map_err(|_| Error::RecoveryRequired)?;
    fence.validate()?;
    let result: StepResult =
        serde_json::from_value(row.get("result")).map_err(|_| Error::RecoveryRequired)?;
    if fence.operation_id != claim.id
        || fence.installation_id != scope.id
        || fence.instance_id != claim.instance_id
        || fence.state.epoch != scope.epoch
        || fence.state.generation != claim.generation
        || fence.state.access.as_str() != access
        || fence.state.admission.as_str() != admission
        || fence.digest()? != row.get::<String, _>("input_digest")
        || !matches!(result,StepResult::Verified{ref uid,..} if uid==&fence.namespace_uid)
    {
        return Err(Error::OwnershipConflict);
    }
    Ok(fence)
}
async fn startable(
    tx: &mut Transaction<'_, Postgres>,
    scope: &InstallationScope,
    pod_uid: &str,
) -> Result<(), Error> {
    let terminal:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM manager.native_child_actions WHERE pod_uid=$1 AND (action='stop' OR outcome='absent') AND input->>'installationId'=$2)")
        .bind(pod_uid).bind(scope.id.to_string()).fetch_one(&mut **tx).await.map_err(db_error)?;
    if terminal {
        return Err(Error::OwnershipConflict);
    }
    Ok(())
}

async fn deployment(
    tx: &mut Transaction<'_, Postgres>,
    scope: &InstallationScope,
    claim: &OperationClaim,
) -> Result<hermes_application::resources::OwnedResource, Error> {
    let row=sqlx::query("SELECT r.* FROM manager.owned_resources r JOIN manager.instances i ON i.id=r.instance_id WHERE r.installation_id=$1 AND r.instance_id=$2 AND r.kind='Deployment' AND r.name='hermes' AND r.state<>'Deleted' AND r.namespace=i.namespace AND r.creation->'spec'->>'namespaceUid'=i.namespace_uid FOR UPDATE OF r")
        .bind(scope.id).bind(claim.instance_id).fetch_optional(&mut **tx).await.map_err(db_error)?.ok_or(Error::OwnershipConflict)?;
    let owned = resources::resource(&row)?;
    if owned.state != "Bound"
        || owned.uid.is_none()
        || owned.creation.spec.kind != ResourceKind::Deployment
        || !(1..=scope.epoch).contains(&owned.creation.epoch)
        || !(1..=claim.generation).contains(&owned.creation.generation)
    {
        return Err(Error::OwnershipConflict);
    }
    Ok(owned)
}
#[async_trait]
impl NativeChildRepository for PgStore {
    async fn load_pending_native_children(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
        action: NativeChildAction,
    ) -> Result<Vec<PreparedNativeChild>, Error> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        lock_claim(&mut tx, scope, claim).await?;
        let owned = deployment(&mut tx, scope, claim).await?;
        let rows=sqlx::query("SELECT input,input_digest FROM manager.native_child_actions WHERE operation_id=$1 AND action=$2 AND completed_at IS NULL ORDER BY pod_uid,input_digest LIMIT 32")
            .bind(claim.id).bind(action.as_str()).fetch_all(&mut *tx).await.map_err(db_error)?;
        let mut pending = Vec::with_capacity(rows.len());
        for row in rows {
            let input: NativeChildInput =
                serde_json::from_value(row.get("input")).map_err(|_| Error::RecoveryRequired)?;
            if input.digest()? != row.get::<String, _>("input_digest") || input.action != action {
                return Err(Error::OwnershipConflict);
            }
            authorized(&mut tx, scope, claim, action, &input.fence_key).await?;
            // A vanished Start can still be observed absent, but any DELETE
            // intent makes dispatching another start unsafe even before absence.
            if action == NativeChildAction::Start {
                let stopped:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM manager.native_child_actions WHERE pod_uid=$1 AND action='stop' AND input->>'installationId'=$2)").bind(&input.pod_uid).bind(scope.id.to_string()).fetch_one(&mut *tx).await.map_err(db_error)?;
                if stopped {
                    return Err(Error::OwnershipConflict);
                }
            }
            let prepared = PreparedNativeChild {
                deployment: owned.clone(),
                input,
                completed: false,
            };
            prepared.validate(claim)?;
            pending.push(prepared);
        }
        tx.commit().await.map_err(db_error)?;
        Ok(pending)
    }
    async fn prepare_native_child(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
        action: NativeChildAction,
        fence_key: &str,
        observation: NativeChildObservation,
    ) -> Result<PreparedNativeChild, Error> {
        observation.validate()?;
        validate_name(fence_key)?;
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        lock_claim(&mut tx, scope, claim).await?;
        authorized(&mut tx, scope, claim, action, fence_key).await?;
        if action == NativeChildAction::Start {
            startable(&mut tx, scope, &observation.pod_uid).await?;
        }
        let owned = deployment(&mut tx, scope, claim).await?;
        let input = NativeChildInput {
            installation_id: scope.id,
            instance_id: claim.instance_id,
            operation_id: claim.id,
            deployment_id: owned.creation.id,
            fence_key: fence_key.into(),
            namespace: owned
                .creation
                .spec
                .namespace
                .clone()
                .ok_or(Error::OwnershipConflict)?,
            namespace_uid: observation.namespace_uid,
            deployment_uid: observation.deployment_uid,
            replica_set_name: observation.replica_set_name,
            replica_set_uid: observation.replica_set_uid,
            pod_name: observation.pod_name,
            pod_uid: observation.pod_uid,
            resource_version: observation.resource_version,
            spec_digest: observation.spec_digest,
            generation: claim.generation,
            epoch: scope.epoch,
            action,
        };
        let existing=sqlx::query("SELECT instance_id,deployment_id,provenance FROM manager.native_children WHERE installation_id=$1 AND pod_uid=$2")
            .bind(scope.id).bind(&input.pod_uid).fetch_optional(&mut *tx).await.map_err(db_error)?;
        if let Some(row) = existing {
            let original: NativeChildInput = serde_json::from_value(row.get("provenance"))
                .map_err(|_| Error::RecoveryRequired)?;
            if row.get::<Uuid, _>("instance_id") != claim.instance_id
                || row.get::<Uuid, _>("deployment_id") != owned.creation.id
                || original.namespace_uid != input.namespace_uid
                || original.deployment_uid != input.deployment_uid
                || original.replica_set_name != input.replica_set_name
                || original.replica_set_uid != input.replica_set_uid
                || original.pod_name != input.pod_name
                || original.spec_digest != input.spec_digest
            {
                return Err(Error::OwnershipConflict);
            }
        } else {
            // An ungated Pod with no pre-dispatch registration could already be
            // running. Never silently turn it into an authorized start.
            if action == NativeChildAction::Start && !observation.gated {
                return Err(Error::OwnershipConflict);
            }
            sqlx::query("INSERT INTO manager.native_children(installation_id,instance_id,pod_uid,deployment_id,provenance) VALUES($1,$2,$3,$4,$5)")
                .bind(scope.id).bind(claim.instance_id).bind(&input.pod_uid).bind(owned.creation.id).bind(serde_json::to_value(&input).map_err(|_|Error::Invalid)?).execute(&mut *tx).await.map_err(db_error)?;
        }
        input.validate()?;
        sqlx::query("INSERT INTO manager.native_child_actions(operation_id,pod_uid,action,input,input_digest) VALUES($1,$2,$3,$4,$5) ON CONFLICT DO NOTHING")
            .bind(claim.id).bind(&input.pod_uid).bind(action.as_str()).bind(serde_json::to_value(&input).map_err(|_|Error::Invalid)?).bind(input.digest()?).execute(&mut *tx).await.map_err(db_error)?;
        let row=sqlx::query("SELECT input,input_digest,completed_at IS NOT NULL AS completed FROM manager.native_child_actions WHERE operation_id=$1 AND pod_uid=$2 AND action=$3 AND input_digest=$4")
            .bind(claim.id).bind(&input.pod_uid).bind(action.as_str()).bind(input.digest()?).fetch_one(&mut *tx).await.map_err(db_error)?;
        let saved: NativeChildInput =
            serde_json::from_value(row.get("input")).map_err(|_| Error::RecoveryRequired)?;
        if input != saved || saved.digest()? != row.get::<String, _>("input_digest") {
            return Err(Error::OwnershipConflict);
        }
        authorized(&mut tx, scope, claim, action, &saved.fence_key).await?;
        let prepared = PreparedNativeChild {
            deployment: owned,
            input: saved,
            completed: row.get("completed"),
        };
        prepared.validate(claim)?;
        tx.commit().await.map_err(db_error)?;
        Ok(prepared)
    }
    async fn complete_native_child(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
        input: &NativeChildInput,
        result: NativeChildResult,
    ) -> Result<(), Error> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        lock_claim(&mut tx, scope, claim).await?;
        authorized(&mut tx, scope, claim, input.action, &input.fence_key).await?;
        if input.action == NativeChildAction::Start && !result.absent {
            startable(&mut tx, scope, &input.pod_uid).await?;
        }
        let owned = deployment(&mut tx, scope, claim).await?;
        PreparedNativeChild {
            deployment: owned,
            input: input.clone(),
            completed: false,
        }
        .validate(claim)?;
        let row=sqlx::query("SELECT input,input_digest,outcome FROM manager.native_child_actions WHERE operation_id=$1 AND pod_uid=$2 AND action=$3 AND input_digest=$4 FOR UPDATE")
            .bind(claim.id).bind(&input.pod_uid).bind(input.action.as_str()).bind(input.digest()?).fetch_optional(&mut *tx).await.map_err(db_error)?.ok_or(Error::OwnershipConflict)?;
        if row.get::<Value, _>("input")
            != serde_json::to_value(input).map_err(|_| Error::Invalid)?
            || row.get::<String, _>("input_digest") != input.digest()?
            || result.input_digest != input.digest()?
            || result.pod_uid != input.pod_uid
            || (input.action == NativeChildAction::Stop && !result.absent)
        {
            return Err(Error::OwnershipConflict);
        }
        let outcome = if result.absent { "absent" } else { "started" };
        if row
            .get::<Option<String>, _>("outcome")
            .is_some_and(|old| old != outcome)
        {
            return Err(Error::OwnershipConflict);
        }
        sqlx::query("UPDATE manager.native_child_actions SET completed_at=COALESCE(completed_at,clock_timestamp()),outcome=COALESCE(outcome,$5) WHERE operation_id=$1 AND pod_uid=$2 AND action=$3 AND input_digest=$4")
            .bind(claim.id).bind(&input.pod_uid).bind(input.action.as_str()).bind(input.digest()?).bind(outcome).execute(&mut *tx).await.map_err(db_error)?;
        tx.commit().await.map_err(db_error)
    }
    async fn native_children(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
    ) -> Result<Vec<NativeChildObservation>, Error> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        lock_claim(&mut tx, scope, claim).await?;
        let rows=sqlx::query("SELECT c.provenance FROM manager.native_children c WHERE c.installation_id=$1 AND c.instance_id=$2 AND NOT EXISTS(SELECT 1 FROM manager.native_child_actions a WHERE a.pod_uid=c.pod_uid AND a.outcome='absent' AND a.input->>'installationId'=c.installation_id::text) ORDER BY c.created_at,c.pod_uid LIMIT 32")
            .bind(scope.id).bind(claim.instance_id).fetch_all(&mut *tx).await.map_err(db_error)?;
        let children = rows
            .into_iter()
            .map(|r| {
                let input: NativeChildInput = serde_json::from_value(r.get("provenance"))
                    .map_err(|_| Error::RecoveryRequired)?;
                input.validate()?;
                Ok(input.observation(false))
            })
            .collect::<Result<Vec<_>, Error>>()?;
        tx.commit().await.map_err(db_error)?;
        Ok(children)
    }
}
