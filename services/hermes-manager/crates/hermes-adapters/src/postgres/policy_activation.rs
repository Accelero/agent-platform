//! SQL-only effective policy work. Kubernetes calls never hold these locks.
use super::{PgStore, db_error, execution::lock_policy, gate, resources};
use async_trait::async_trait;
use hermes_application::{
    Error, InstallationScope,
    execution::{PolicyClaim, ResolvedPolicy},
    resources::OwnedResource,
    terminal::activation::*,
};
use hermes_domain::resources::ResourceKind;
use serde_json::Value;
use sqlx::{Postgres, Row, Transaction};

type Tx<'a> = Transaction<'a, Postgres>;
async fn lock(
    tx: &mut Tx<'_>,
    scope: &InstallationScope,
    claim: &PolicyClaim,
) -> Result<(), Error> {
    gate(tx, scope).await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,184712))")
        .bind(scope.id.to_string())
        .execute(&mut **tx)
        .await
        .map_err(db_error)?;
    lock_policy(tx, scope, claim).await
}
async fn bound(
    tx: &mut Tx<'_>,
    scope: &InstallationScope,
    claim: &PolicyClaim,
    kind: ResourceKind,
    namespace: &str,
    name: &str,
) -> Result<OwnedResource, Error> {
    let row=sqlx::query("SELECT * FROM manager.owned_resources WHERE installation_id=$1 AND instance_id=$2 AND kind=$3 AND namespace=$4 AND name=$5")
        .bind(scope.id).bind(claim.instance_id).bind(kind.as_str()).bind(namespace).bind(name).fetch_optional(&mut **tx).await.map_err(db_error)?.ok_or(Error::PreconditionFailed)?;
    let owned = resources::resource(&row)?;
    if owned.state != "Bound" || owned.uid.is_none() {
        return Err(Error::PreconditionFailed);
    }
    if owned.creation.installation_id != scope.id
        || owned.creation.instance_id != claim.instance_id
        || owned.creation.epoch > scope.epoch
    {
        return Err(Error::OwnershipConflict);
    }
    Ok(owned)
}
async fn no_prior_workers(tx: &mut Tx<'_>, claim: &PolicyClaim) -> Result<(), Error> {
    let old:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM manager.execution_environments WHERE instance_id=$1 AND (policy_generation<>$2 OR epoch<>$3) AND state<>'Retired')")
        .bind(claim.instance_id).bind(claim.generation).bind(claim.epoch).fetch_one(&mut **tx).await.map_err(db_error)?;
    if old {
        return Err(Error::PreconditionFailed);
    }
    Ok(())
}
async fn plan(
    tx: &mut Tx<'_>,
    scope: &InstallationScope,
    claim: &PolicyClaim,
) -> Result<ActivationPlan, Error> {
    let row=sqlx::query("SELECT i.namespace,i.namespace_uid,i.desired_generation,i.drain_generation,i.drain_completed_at IS NOT NULL AS drain_complete,p.policy,p.content_digest FROM manager.instances i JOIN manager.execution_policy_versions p ON p.instance_id=i.id AND p.generation=$2 WHERE i.id=$1")
        .bind(claim.instance_id).bind(claim.generation).fetch_one(&mut **tx).await.map_err(db_error)?;
    if row.get::<Option<i64>, _>("drain_generation").is_some()
        && !row.get::<bool, _>("drain_complete")
    {
        return Err(Error::PreconditionFailed);
    }
    let policy: ResolvedPolicy =
        serde_json::from_value(row.get("policy")).map_err(|_| Error::RecoveryRequired)?;
    let namespace: String = row.get("namespace");
    let uid: String = row
        .get::<Option<String>, _>("namespace_uid")
        .ok_or(Error::PreconditionFailed)?;
    let assignment_generation = row.get("desired_generation");
    if policy.installation_id != scope.id
        || policy.instance_id != claim.instance_id
        || policy.generation != claim.generation
        || policy.namespace != namespace
        || policy.namespace_uid != uid
        || policy.digest()? != claim.digest
        || row.get::<String, _>("content_digest") != claim.digest
    {
        return Err(Error::OwnershipConflict);
    }
    crate::execution_workloads::validate_policy(&policy)?;
    // Service egress needs live Service/Endpoint identity resolution before its
    // NetworkPolicy can be approved. Never report that unsupported path applied.
    if policy
        .runtimes
        .iter()
        .any(|runtime| !runtime.definition.egress.is_empty())
    {
        return Err(Error::TemplateUnavailable);
    }

    if !super::terminal::leases::current_open(
        tx,
        scope,
        claim.instance_id,
        assignment_generation,
        &uid,
    )
    .await?
    {
        return Err(Error::PreconditionFailed);
    }
    no_prior_workers(tx, claim).await?;
    let namespace_owned = bound(tx, scope, claim, ResourceKind::Namespace, "", &namespace).await?;
    let parameters = bound(
        tx,
        scope,
        claim,
        ResourceKind::InstancePolicy,
        &namespace,
        "instance-policy",
    )
    .await?;
    let worker_account = bound(
        tx,
        scope,
        claim,
        ResourceKind::ServiceAccount,
        &namespace,
        "worker",
    )
    .await?;
    let authorized_keys = bound(
        tx,
        scope,
        claim,
        ResourceKind::Secret,
        &namespace,
        crate::execution_workloads::AUTHORIZED_KEYS,
    )
    .await?;
    if namespace_owned.uid.as_ref() != Some(&uid)
        || [&parameters, &worker_account, &authorized_keys]
            .iter()
            .any(|r| r.creation.spec.namespace_uid.as_ref() != Some(&uid))
    {
        return Err(Error::OwnershipConflict);
    }
    let candidate = crate::admission::worker_parameters(&policy)?["spec"]["worker"].clone();
    let candidate_digest = public_digest(&candidate)?;
    sqlx::query("INSERT INTO manager.execution_policy_renderings(instance_id,generation,epoch,policy_digest,rendering_digest,worker) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT DO NOTHING")
        .bind(claim.instance_id).bind(claim.generation).bind(scope.epoch).bind(&claim.digest).bind(candidate_digest).bind(candidate).execute(&mut **tx).await.map_err(db_error)?;
    let frozen=sqlx::query("SELECT policy_digest,rendering_digest,worker FROM manager.execution_policy_renderings WHERE instance_id=$1 AND generation=$2 AND epoch=$3")
        .bind(claim.instance_id).bind(claim.generation).bind(scope.epoch).fetch_one(&mut **tx).await.map_err(db_error)?;
    let worker: Value = frozen.get("worker");
    let rendering_digest: String = frozen.get("rendering_digest");
    if frozen.get::<String, _>("policy_digest") != claim.digest
        || public_digest(&worker)? != rendering_digest
    {
        return Err(Error::OwnershipConflict);
    }
    Ok(ActivationPlan {
        policy,
        assignment_generation,
        namespace: namespace_owned,
        parameters,
        worker_account,
        authorized_keys,
        worker,
        rendering_digest,
    })
}
fn check_input(
    scope: &InstallationScope,
    claim: &PolicyClaim,
    plan: &ActivationPlan,
    input: &PolicyAdmissionInput,
) -> Result<(), Error> {
    input.digest()?;
    if input.installation_id != scope.id
        || input.instance_id != claim.instance_id
        || input.generation != claim.generation
        || input.epoch != scope.epoch
        || input.assignment_generation != plan.assignment_generation
        || input.namespace != plan.policy.namespace
        || input.namespace_uid != plan.policy.namespace_uid
        || input.resource_id != plan.parameters.creation.id
        || plan.parameters.uid.as_ref() != Some(&input.policy_uid)
        || input.policy_digest != claim.digest
        || input.rendering_digest != plan.rendering_digest
    {
        return Err(Error::OwnershipConflict);
    }
    Ok(())
}
#[async_trait]
impl PolicyActivationRepository for PgStore {
    async fn renew_policy_activation(
        &self,
        scope: &InstallationScope,
        claim: &PolicyClaim,
    ) -> Result<(), Error> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        lock(&mut tx, scope, claim).await?;
        sqlx::query("UPDATE manager.execution_policy_status SET lease_until=clock_timestamp()+interval '30 seconds' WHERE instance_id=$1").bind(claim.instance_id).execute(&mut *tx).await.map_err(db_error)?;
        tx.commit().await.map_err(db_error)
    }

    async fn activation_plan(
        &self,
        scope: &InstallationScope,
        claim: &PolicyClaim,
    ) -> Result<ActivationPlan, Error> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        lock(&mut tx, scope, claim).await?;
        let result = plan(&mut tx, scope, claim).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(result)
    }
    async fn prepare_policy_admission(
        &self,
        scope: &InstallationScope,
        claim: &PolicyClaim,
        input: &PolicyAdmissionInput,
    ) -> Result<PreparedPolicyAdmission, Error> {
        let digest = input.digest()?;
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        lock(&mut tx, scope, claim).await?;
        let plan = plan(&mut tx, scope, claim).await?;
        check_input(scope, claim, &plan, input)?;
        sqlx::query("INSERT INTO manager.execution_policy_admission_inputs(instance_id,generation,epoch,input_digest,input,fence)VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT DO NOTHING")
            .bind(claim.instance_id).bind(claim.generation).bind(scope.epoch).bind(&digest).bind(serde_json::to_value(input).map_err(|_|Error::Invalid)?).bind(claim.fence).execute(&mut *tx).await.map_err(db_error)?;
        let row=sqlx::query("UPDATE manager.execution_policy_admission_inputs SET fence=$5 WHERE instance_id=$1 AND generation=$2 AND epoch=$3 AND input_digest=$4 RETURNING input")
            .bind(claim.instance_id).bind(claim.generation).bind(scope.epoch).bind(digest).bind(claim.fence).fetch_one(&mut *tx).await.map_err(db_error)?;
        let original: PolicyAdmissionInput =
            serde_json::from_value(row.get("input")).map_err(|_| Error::RecoveryRequired)?;
        if original != *input {
            return Err(Error::OwnershipConflict);
        }
        tx.commit().await.map_err(db_error)?;
        Ok(PreparedPolicyAdmission {
            plan,
            input: original,
        })
    }
    async fn pending_policy_admissions(
        &self,
        scope: &InstallationScope,
        claim: &PolicyClaim,
    ) -> Result<Vec<PreparedPolicyAdmission>, Error> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        lock(&mut tx, scope, claim).await?;
        let digests:Vec<String>=sqlx::query_scalar("SELECT input_digest FROM manager.execution_policy_admission_inputs WHERE instance_id=$1 AND generation=$2 AND epoch=$3 AND verified_at IS NULL ORDER BY created_at,input_digest LIMIT 33")
            .bind(claim.instance_id).bind(claim.generation).bind(scope.epoch).fetch_all(&mut *tx).await.map_err(db_error)?;
        if digests.len() > 32 {
            return Err(Error::OwnershipConflict);
        }
        let rows=sqlx::query("UPDATE manager.execution_policy_admission_inputs SET fence=$4 WHERE instance_id=$1 AND generation=$2 AND epoch=$3 AND input_digest=ANY($5) RETURNING input,input_digest")
            .bind(claim.instance_id).bind(claim.generation).bind(scope.epoch).bind(claim.fence).bind(digests).fetch_all(&mut *tx).await.map_err(db_error)?;
        let mut result = Vec::new();
        for row in rows {
            let input: PolicyAdmissionInput =
                serde_json::from_value(row.get("input")).map_err(|_| Error::RecoveryRequired)?;
            let plan = plan(&mut tx, scope, claim).await?;
            check_input(scope, claim, &plan, &input)?;
            if input.digest()? != row.get::<String, _>("input_digest") {
                return Err(Error::OwnershipConflict);
            }
            result.push(PreparedPolicyAdmission { plan, input });
        }
        tx.commit().await.map_err(db_error)?;
        Ok(result)
    }
    async fn record_policy_admission(
        &self,
        scope: &InstallationScope,
        claim: &PolicyClaim,
        observation: &PolicyAdmissionObservation,
    ) -> Result<(), Error> {
        hermes_domain::validate_sha256(&observation.input_digest)?;
        if !hermes_domain::resources::valid_uid(&observation.resource_version) {
            return Err(Error::Invalid);
        }
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        lock(&mut tx, scope, claim).await?;
        let row=sqlx::query("SELECT input,result,fence FROM manager.execution_policy_admission_inputs WHERE instance_id=$1 AND generation=$2 AND epoch=$3 AND input_digest=$4 FOR UPDATE")
            .bind(claim.instance_id).bind(claim.generation).bind(scope.epoch).bind(&observation.input_digest).fetch_optional(&mut *tx).await.map_err(db_error)?.ok_or(Error::NotFound)?;
        if row.get::<i64, _>("fence") != claim.fence {
            return Err(Error::StaleFence);
        }
        let input: PolicyAdmissionInput =
            serde_json::from_value(row.get("input")).map_err(|_| Error::RecoveryRequired)?;
        let plan = plan(&mut tx, scope, claim).await?;
        check_input(scope, claim, &plan, &input)?;
        if input.digest()? != observation.input_digest
            || observation.policy_uid != input.policy_uid
            || observation.namespace_uid != input.namespace_uid
            || observation.rendering_digest != input.rendering_digest
        {
            return Err(Error::OwnershipConflict);
        }
        if let Some(old) = row.get::<Option<Value>, _>("result") {
            let old: PolicyAdmissionObservation =
                serde_json::from_value(old).map_err(|_| Error::RecoveryRequired)?;
            if old.input_digest != observation.input_digest
                || old.policy_uid != observation.policy_uid
                || old.namespace_uid != observation.namespace_uid
                || old.rendering_digest != observation.rendering_digest
            {
                return Err(Error::OwnershipConflict);
            }
        } else {
            sqlx::query("UPDATE manager.execution_policy_admission_inputs SET result=$5,verified_at=clock_timestamp() WHERE instance_id=$1 AND generation=$2 AND epoch=$3 AND input_digest=$4")
                .bind(claim.instance_id).bind(claim.generation).bind(scope.epoch).bind(&observation.input_digest).bind(serde_json::to_value(observation).map_err(|_|Error::Invalid)?).execute(&mut *tx).await.map_err(db_error)?;
        }
        tx.commit().await.map_err(db_error)
    }
    async fn activate_execution_policy(
        &self,
        scope: &InstallationScope,
        claim: &PolicyClaim,
        input_digest: &str,
    ) -> Result<(), Error> {
        hermes_domain::validate_sha256(input_digest)?;
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        lock(&mut tx, scope, claim).await?;
        let _plan = plan(&mut tx, scope, claim).await?;
        let ready:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM manager.execution_policy_admission_inputs WHERE instance_id=$1 AND generation=$2 AND epoch=$3 AND input_digest=$4 AND fence=$5 AND verified_at IS NOT NULL) AND NOT EXISTS(SELECT 1 FROM manager.execution_policy_admission_inputs WHERE instance_id=$1 AND generation=$2 AND epoch=$3 AND verified_at IS NULL)")
            .bind(claim.instance_id).bind(claim.generation).bind(scope.epoch).bind(input_digest).bind(claim.fence).fetch_one(&mut *tx).await.map_err(db_error)?;
        if !ready {
            return Err(Error::PreconditionFailed);
        }
        sqlx::query("INSERT INTO manager.execution_effective_policy_receipts(instance_id,generation,epoch,content_digest)VALUES($1,$2,$3,$4) ON CONFLICT DO NOTHING")
            .bind(claim.instance_id).bind(claim.generation).bind(scope.epoch).bind(&claim.digest).execute(&mut *tx).await.map_err(db_error)?;
        sqlx::query("UPDATE manager.execution_policy_status SET applied_generation=desired_generation,state='Applied',condition=NULL,lease_owner=NULL,lease_until=NULL,next_attempt_at=clock_timestamp()+interval '30 seconds' WHERE instance_id=$1")
            .bind(claim.instance_id).execute(&mut *tx).await.map_err(db_error)?;
        tx.commit().await.map_err(db_error)
    }
}
