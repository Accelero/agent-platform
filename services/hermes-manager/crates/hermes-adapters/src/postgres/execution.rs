//! Short SQL-only transactions for reviewed execution configuration.
use async_trait::async_trait;
use hermes_application::{
    Error, InstallationScope,
    execution::{
        AssignmentExecution, ExecutionAccepted, ExecutionCommand, ExecutionMutation,
        ExecutionPolicyView, ExecutionRepository, PolicyClaim, PolicyCondition, PullSecretBinding,
        ResolvedMount, ResolvedPolicy, ResolvedRuntime, RuntimeDefinition, StorageBinding,
    },
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;

use super::{PgStore, db_error, gate, lease_duration};

async fn owner(
    tx: &mut Transaction<'_, Postgres>,
    scope: &InstallationScope,
    instance: Uuid,
) -> Result<(String, String), Error> {
    let row = sqlx::query("SELECT i.namespace,i.namespace_uid FROM manager.instances i JOIN manager.assignments a ON a.id=i.assignment_id WHERE i.id=$1 AND a.installation_id=$2 FOR UPDATE OF a,i")
        .bind(instance).bind(scope.id).fetch_optional(&mut **tx).await.map_err(db_error)?.ok_or(Error::NotFound)?;
    Ok((
        row.get("namespace"),
        row.get::<Option<String>, _>("namespace_uid")
            .ok_or(Error::PreconditionFailed)?,
    ))
}
async fn check_owner(
    tx: &mut Transaction<'_, Postgres>,
    scope: &InstallationScope,
    instance: Uuid,
    namespace: &str,
    uid: &str,
) -> Result<(), Error> {
    if owner(tx, scope, instance).await? != (namespace.to_owned(), uid.to_owned()) {
        return Err(Error::OwnershipConflict);
    }
    Ok(())
}

#[async_trait]
impl ExecutionRepository for PgStore {
    async fn runtime_catalog(
        &self,
        scope: &InstallationScope,
        after: Option<&str>,
        limit: u16,
    ) -> Result<Vec<RuntimeDefinition>, Error> {
        if let Some(after) = after {
            hermes_domain::validate_name(after)?;
        }
        if !(1..=100).contains(&limit) {
            return Err(Error::Invalid);
        }
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        gate(&mut tx, scope).await?;
        let rows = sqlx::query("SELECT DISTINCT ON(id) definition,content_digest FROM manager.terminal_runtime_versions WHERE ($1::text IS NULL OR id>$1) ORDER BY id,version DESC LIMIT $2")
            .bind(after).bind(i64::from(limit)).fetch_all(&mut *tx).await.map_err(db_error)?;
        let result = rows
            .iter()
            .map(|row| {
                let definition: RuntimeDefinition = serde_json::from_value(row.get("definition"))
                    .map_err(|_| Error::RecoveryRequired)?;
                if definition.digest()? != row.get::<String, _>("content_digest") {
                    return Err(Error::OwnershipConflict);
                }
                Ok(definition)
            })
            .collect::<Result<Vec<_>, Error>>()?;
        tx.commit().await.map_err(db_error)?;
        Ok(result)
    }
    async fn storage_binding(
        &self,
        scope: &InstallationScope,
        id: Uuid,
    ) -> Result<StorageBinding, Error> {
        if id.is_nil() {
            return Err(Error::Invalid);
        }
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        gate(&mut tx, scope).await?;
        let row = sqlx::query("SELECT v.definition,v.instance_id FROM manager.storage_binding_versions v JOIN manager.instances i ON i.id=v.instance_id JOIN manager.assignments a ON a.id=i.assignment_id WHERE v.id=$1 AND a.installation_id=$2 AND v.namespace=i.namespace AND v.namespace_uid=i.namespace_uid ORDER BY v.version DESC LIMIT 1")
            .bind(id).bind(scope.id).fetch_optional(&mut *tx).await.map_err(db_error)?.ok_or(Error::NotFound)?;
        let binding: StorageBinding =
            serde_json::from_value(row.get("definition")).map_err(|_| Error::RecoveryRequired)?;
        binding.validate()?;
        if binding.id != id || binding.instance_id != row.get::<Uuid, _>("instance_id") {
            return Err(Error::OwnershipConflict);
        }
        tx.commit().await.map_err(db_error)?;
        Ok(binding)
    }
    async fn runtime_versions(
        &self,
        scope: &InstallationScope,
        runtime_id: &str,
        after_version: i64,
        limit: u16,
    ) -> Result<Vec<RuntimeDefinition>, Error> {
        hermes_domain::validate_name(runtime_id)?;
        if after_version < 0 || !(1..=100).contains(&limit) {
            return Err(Error::Invalid);
        }
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        gate(&mut tx, scope).await?;
        let rows=sqlx::query("SELECT definition,content_digest FROM manager.terminal_runtime_versions WHERE id=$1 AND version>$2 ORDER BY version LIMIT $3")
            .bind(runtime_id).bind(after_version).bind(i64::from(limit)).fetch_all(&mut *tx).await.map_err(db_error)?;
        let result = rows
            .iter()
            .map(|row| {
                let definition: RuntimeDefinition = serde_json::from_value(row.get("definition"))
                    .map_err(|_| Error::RecoveryRequired)?;
                if definition.digest()? != row.get::<String, _>("content_digest") {
                    return Err(Error::OwnershipConflict);
                }
                Ok(definition)
            })
            .collect::<Result<Vec<_>, Error>>()?;
        tx.commit().await.map_err(db_error)?;
        Ok(result)
    }
    async fn owner_storage_bindings(
        &self,
        scope: &InstallationScope,
        instance_id: Uuid,
        after: Option<Uuid>,
        limit: u16,
    ) -> Result<Vec<StorageBinding>, Error> {
        if instance_id.is_nil() || !(1..=100).contains(&limit) {
            return Err(Error::Invalid);
        }
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        gate(&mut tx, scope).await?;
        let (namespace, namespace_uid) = owner(&mut tx, scope, instance_id).await?;
        let rows=sqlx::query("SELECT DISTINCT ON(id) definition FROM manager.storage_binding_versions WHERE instance_id=$1 AND namespace=$2 AND namespace_uid=$3 AND ($4::uuid IS NULL OR id>$4) ORDER BY id,version DESC LIMIT $5")
            .bind(instance_id).bind(namespace).bind(namespace_uid).bind(after).bind(i64::from(limit)).fetch_all(&mut *tx).await.map_err(db_error)?;
        let result = rows
            .iter()
            .map(|row| {
                let binding: StorageBinding = serde_json::from_value(row.get("definition"))
                    .map_err(|_| Error::RecoveryRequired)?;
                binding.validate()?;
                Ok(binding)
            })
            .collect::<Result<Vec<_>, Error>>()?;
        tx.commit().await.map_err(db_error)?;
        Ok(result)
    }
    async fn mutate_execution(
        &self,
        mutation: ExecutionMutation,
    ) -> Result<ExecutionAccepted, Error> {
        mutation.command.validate()?;
        mutation.actor.validate(&mutation.scope.issuer)?;
        if mutation.request_id.is_nil()
            || mutation.idempotency_key.is_empty()
            || mutation.idempotency_key.len() > 128
            || !mutation
                .idempotency_key
                .bytes()
                .all(|c| c.is_ascii_graphic())
        {
            return Err(Error::Invalid);
        }
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        gate(&mut tx, &mutation.scope).await?;
        if matches!(mutation.command, ExecutionCommand::SetPolicy { .. }) {
            sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,184712))")
                .bind(mutation.scope.id.to_string())
                .execute(&mut *tx)
                .await
                .map_err(db_error)?;
        }

        let action = mutation.command.action();
        let digest =
            Sha256::digest(serde_json::to_vec(&mutation.command).map_err(|_| Error::Invalid)?)
                .to_vec();
        sqlx::query("INSERT INTO manager.mutation_requests(installation_id,actor_issuer,actor_subject,scope,idempotency_key,body_digest) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT DO NOTHING")
            .bind(mutation.scope.id).bind(&mutation.actor.issuer).bind(&mutation.actor.subject).bind(action).bind(&mutation.idempotency_key).bind(&digest).execute(&mut *tx).await.map_err(db_error)?;
        let prior=sqlx::query("SELECT body_digest,result FROM manager.mutation_requests WHERE installation_id=$1 AND actor_issuer=$2 AND actor_subject=$3 AND scope=$4 AND idempotency_key=$5 FOR UPDATE")
            .bind(mutation.scope.id).bind(&mutation.actor.issuer).bind(&mutation.actor.subject).bind(action).bind(&mutation.idempotency_key).fetch_one(&mut *tx).await.map_err(db_error)?;
        if prior.get::<Vec<u8>, _>("body_digest") != digest {
            return Err(Error::IdempotencyConflict);
        }
        if let Some(result) = prior.get::<Option<Value>, _>("result") {
            let accepted = serde_json::from_value(result).map_err(|_| Error::RecoveryRequired)?;
            tx.commit().await.map_err(db_error)?;
            return Ok(accepted);
        }
        let mut accepted = ExecutionAccepted {
            event_id: Uuid::new_v4(),
            assignment_version: None,
            policy_generation: None,
            policy_digest: None,
        };
        let change = match &mutation.command {
            ExecutionCommand::RegisterRuntime { definition } => {
                let digest = definition.digest()?;
                sqlx::query("INSERT INTO manager.terminal_runtime_versions(id,version,schema_version,content_digest,definition) VALUES($1,$2,$3,$4,$5)")
                    .bind(&definition.runtime.id).bind(definition.runtime.version).bind(i32::from(definition.schema_version)).bind(&digest).bind(serde_json::to_value(definition).map_err(|_|Error::Invalid)?).execute(&mut *tx).await.map_err(db_error)?;
                json!({"runtime":definition.runtime,"digest":digest})
            }
            ExecutionCommand::RegisterStorage { binding }
            | ExecutionCommand::UpdateStorage { binding, .. } => {
                check_owner(
                    &mut tx,
                    &mutation.scope,
                    binding.instance_id,
                    &binding.namespace,
                    &binding.namespace_uid,
                )
                .await?;
                sqlx::query("INSERT INTO manager.storage_binding_owners(id,instance_id) VALUES($1,$2) ON CONFLICT DO NOTHING").bind(binding.id).bind(binding.instance_id).execute(&mut *tx).await.map_err(db_error)?;
                let registered: Uuid = sqlx::query_scalar(
                    "SELECT instance_id FROM manager.storage_binding_owners WHERE id=$1 FOR UPDATE",
                )
                .bind(binding.id)
                .fetch_one(&mut *tx)
                .await
                .map_err(db_error)?;
                if registered != binding.instance_id {
                    return Err(Error::OwnershipConflict);
                }
                let latest: Option<i64> = sqlx::query_scalar(
                    "SELECT max(version) FROM manager.storage_binding_versions WHERE id=$1",
                )
                .bind(binding.id)
                .fetch_one(&mut *tx)
                .await
                .map_err(db_error)?;
                match &mutation.command {
                    ExecutionCommand::RegisterStorage { .. } => {
                        if binding.version != 1 {
                            return Err(Error::Invalid);
                        }
                        if latest.is_some() {
                            return Err(Error::AlreadyExists);
                        }
                    }
                    ExecutionCommand::UpdateStorage {
                        expected_version, ..
                    } => {
                        if latest != Some(*expected_version) {
                            return Err(Error::PreconditionFailed);
                        }
                    }
                    _ => unreachable!(),
                }
                sqlx::query("INSERT INTO manager.storage_binding_versions(id,version,instance_id,namespace,namespace_uid,pvc_name,pvc_uid,definition) VALUES($1,$2,$3,$4,$5,$6,$7,$8)")
                    .bind(binding.id).bind(binding.version).bind(binding.instance_id).bind(&binding.namespace).bind(&binding.namespace_uid).bind(&binding.pvc_name).bind(&binding.pvc_uid).bind(serde_json::to_value(binding).map_err(|_|Error::Invalid)?).execute(&mut *tx).await.map_err(db_error)?;
                json!({"storageBindingId":binding.id,"version":binding.version,"instanceId":binding.instance_id})
            }
            ExecutionCommand::RegisterPullSecret { binding } => {
                check_owner(
                    &mut tx,
                    &mutation.scope,
                    binding.instance_id,
                    &binding.namespace,
                    &binding.namespace_uid,
                )
                .await?;
                sqlx::query("INSERT INTO manager.execution_pull_secrets(instance_id,namespace,namespace_uid,name,uid) VALUES($1,$2,$3,$4,$5)")
                    .bind(binding.instance_id).bind(&binding.namespace).bind(&binding.namespace_uid).bind(&binding.name).bind(&binding.uid).execute(&mut *tx).await.map_err(db_error)?;
                json!({"instanceId":binding.instance_id,"name":binding.name})
            }
            ExecutionCommand::SetPolicy {
                assignment_id,
                expected_version,
                policy,
            } => {
                let row=sqlx::query("SELECT a.version,a.access,i.id,i.namespace,i.namespace_uid FROM manager.assignments a JOIN manager.instances i ON i.assignment_id=a.id WHERE a.id=$1 AND a.installation_id=$2 FOR UPDATE OF a,i")
                    .bind(assignment_id).bind(mutation.scope.id).fetch_optional(&mut *tx).await.map_err(db_error)?.ok_or(Error::NotFound)?;
                if row.get::<i64, _>("version") != *expected_version {
                    return Err(Error::PreconditionFailed);
                }
                if row.get::<String, _>("access") != "enabled" {
                    return Err(Error::Forbidden);
                }
                let instance: Uuid = row.get("id");
                let namespace: String = row.get("namespace");
                let namespace_uid: String = row
                    .get::<Option<String>, _>("namespace_uid")
                    .ok_or(Error::PreconditionFailed)?;
                let generation:i64=sqlx::query_scalar("SELECT coalesce(max(generation),0)+1 FROM manager.execution_policy_versions WHERE instance_id=$1").bind(instance).fetch_one(&mut *tx).await.map_err(db_error)?;
                let resolved = resolve(
                    &mut tx,
                    &mutation.scope,
                    instance,
                    namespace,
                    namespace_uid,
                    generation,
                    policy,
                )
                .await?;
                let digest = resolved.digest()?;
                sqlx::query("INSERT INTO manager.execution_policy_versions(instance_id,generation,content_digest,policy) VALUES($1,$2,$3,$4)").bind(instance).bind(generation).bind(&digest).bind(serde_json::to_value(&resolved).map_err(|_|Error::Invalid)?).execute(&mut *tx).await.map_err(db_error)?;
                sqlx::query("INSERT INTO manager.execution_policy_status(instance_id,desired_generation,state,epoch) VALUES($1,$2,'Pending',$3) ON CONFLICT(instance_id) DO UPDATE SET desired_generation=EXCLUDED.desired_generation,state='Pending',applied_generation=CASE WHEN manager.execution_policy_status.epoch<>EXCLUDED.epoch THEN 0 ELSE manager.execution_policy_status.applied_generation END,epoch=EXCLUDED.epoch,fence=manager.execution_policy_status.fence+1,lease_owner=NULL,lease_until=NULL,next_attempt_at=clock_timestamp(),condition=NULL")
                    .bind(instance).bind(generation).bind(mutation.scope.epoch).execute(&mut *tx).await.map_err(db_error)?;
                // New work is already fenced by desired != applied. Commit the
                // old-generation drain deadline in the acknowledgement TX too.
                sqlx::query("UPDATE manager.execution_environments SET state='Draining',drain_deadline=COALESCE(drain_deadline,clock_timestamp()+interval '5 minutes'),lease_owner=NULL,lease_until=NULL WHERE instance_id=$1 AND (policy_generation<>$2 OR epoch<>$3) AND state NOT IN ('Draining','Retired')")
                    .bind(instance).bind(generation).bind(mutation.scope.epoch).execute(&mut *tx).await.map_err(db_error)?;
                let version = expected_version.checked_add(1).ok_or(Error::Invalid)?;
                sqlx::query("UPDATE manager.assignments SET version=$2,updated_at=clock_timestamp() WHERE id=$1").bind(assignment_id).bind(version).execute(&mut *tx).await.map_err(db_error)?;
                accepted.assignment_version = Some(version);
                accepted.policy_generation = Some(generation);
                accepted.policy_digest = Some(digest.clone());
                json!({"assignmentId":assignment_id,"generation":generation,"digest":digest})
            }
        };
        let result = serde_json::to_value(&accepted).map_err(|_| Error::Invalid)?;
        sqlx::query("INSERT INTO manager.execution_configuration_events(id,installation_id,actor_issuer,actor_subject,action,request_id,change,result) VALUES($1,$2,$3,$4,$5,$6,$7,$8)")
            .bind(accepted.event_id).bind(mutation.scope.id).bind(&mutation.actor.issuer).bind(&mutation.actor.subject).bind(action).bind(mutation.request_id).bind(change).bind(&result).execute(&mut *tx).await.map_err(db_error)?;
        sqlx::query("UPDATE manager.mutation_requests SET result=$6 WHERE installation_id=$1 AND actor_issuer=$2 AND actor_subject=$3 AND scope=$4 AND idempotency_key=$5")
            .bind(mutation.scope.id).bind(&mutation.actor.issuer).bind(&mutation.actor.subject).bind(action).bind(&mutation.idempotency_key).bind(result).execute(&mut *tx).await.map_err(db_error)?;
        tx.commit().await.map_err(db_error)?;
        Ok(accepted)
    }

    async fn execution_policy(
        &self,
        scope: &InstallationScope,
        assignment_id: Uuid,
    ) -> Result<ExecutionPolicyView, Error> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        gate(&mut tx, scope).await?;
        let row=sqlx::query("SELECT a.id,a.version,s.desired_generation,s.applied_generation,s.state,p.policy,p.content_digest FROM manager.assignments a JOIN manager.instances i ON i.assignment_id=a.id JOIN manager.execution_policy_status s ON s.instance_id=i.id JOIN manager.execution_policy_versions p ON p.instance_id=i.id AND p.generation=s.desired_generation WHERE a.id=$1 AND a.installation_id=$2")
            .bind(assignment_id).bind(scope.id).fetch_optional(&mut *tx).await.map_err(db_error)?.ok_or(Error::NotFound)?;
        let desired = decode_policy(row.get("policy"), &row.get::<String, _>("content_digest"))?;
        let view = ExecutionPolicyView {
            assignment_id: row.get("id"),
            assignment_version: row.get("version"),
            desired_generation: row.get("desired_generation"),
            applied_generation: row.get("applied_generation"),
            state: row.get("state"),
            desired,
            digest: row.get("content_digest"),
        };
        tx.commit().await.map_err(db_error)?;
        Ok(view)
    }

    async fn claim_policy(
        &self,
        scope: &InstallationScope,
        worker: Uuid,
        lease_seconds: u16,
    ) -> Result<Option<PolicyClaim>, Error> {
        if worker.is_nil() {
            return Err(Error::Invalid);
        }
        let seconds = lease_duration(lease_seconds)?;
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        gate(&mut tx, scope).await?;
        // Lock owner before status consistently with all administrative writes.
        let row=sqlx::query("SELECT a.version,s.instance_id,s.desired_generation,p.content_digest FROM manager.assignments a JOIN manager.instances i ON i.assignment_id=a.id JOIN manager.execution_policy_status s ON s.instance_id=i.id JOIN manager.execution_policy_versions p ON p.instance_id=i.id AND p.generation=s.desired_generation WHERE a.installation_id=$1 AND a.access='enabled' AND s.epoch=$2 AND s.next_attempt_at<=clock_timestamp() AND (s.lease_until IS NULL OR s.lease_until<=clock_timestamp()) ORDER BY s.next_attempt_at,s.instance_id FOR UPDATE OF a,i,s SKIP LOCKED LIMIT 1")
            .bind(scope.id).bind(scope.epoch).fetch_optional(&mut *tx).await.map_err(db_error)?;
        let Some(row) = row else {
            tx.commit().await.map_err(db_error)?;
            return Ok(None);
        };
        let instance: Uuid = row.get("instance_id");
        let fence:i64=sqlx::query_scalar("UPDATE manager.execution_policy_status SET state='Reconciling',fence=fence+1,lease_owner=$2,lease_until=clock_timestamp()+make_interval(secs=>$3) WHERE instance_id=$1 RETURNING fence")
            .bind(instance).bind(worker).bind(seconds).fetch_one(&mut *tx).await.map_err(db_error)?;
        let claim = PolicyClaim {
            instance_id: instance,
            assignment_version: row.get("version"),
            generation: row.get("desired_generation"),
            epoch: scope.epoch,
            worker,
            fence,
            digest: row.get("content_digest"),
        };
        tx.commit().await.map_err(db_error)?;
        Ok(Some(claim))
    }

    async fn policy_plan(
        &self,
        scope: &InstallationScope,
        claim: &PolicyClaim,
    ) -> Result<ResolvedPolicy, Error> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        lock_policy(&mut tx, scope, claim).await?;
        let body:Value=sqlx::query_scalar("SELECT policy FROM manager.execution_policy_versions WHERE instance_id=$1 AND generation=$2 AND content_digest=$3")
            .bind(claim.instance_id).bind(claim.generation).bind(&claim.digest).fetch_optional(&mut *tx).await.map_err(db_error)?.ok_or(Error::StaleFence)?;
        let plan = decode_policy(body, &claim.digest)?;
        if plan.installation_id != scope.id
            || plan.instance_id != claim.instance_id
            || plan.generation != claim.generation
        {
            return Err(Error::OwnershipConflict);
        }
        tx.commit().await.map_err(db_error)?;
        Ok(plan)
    }

    async fn defer_policy(
        &self,
        scope: &InstallationScope,
        claim: &PolicyClaim,
        condition: PolicyCondition,
        seconds: u16,
    ) -> Result<(), Error> {
        let seconds = lease_duration(seconds)?;
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        lock_policy(&mut tx, scope, claim).await?;
        let condition = serde_json::to_value(condition)
            .map_err(|_| Error::Invalid)?
            .as_str()
            .ok_or(Error::Invalid)?
            .to_owned();
        sqlx::query("UPDATE manager.execution_policy_status SET state='Waiting',lease_owner=NULL,lease_until=NULL,next_attempt_at=clock_timestamp()+make_interval(secs=>$2),condition=$3 WHERE instance_id=$1")
            .bind(claim.instance_id).bind(seconds).bind(condition).execute(&mut *tx).await.map_err(db_error)?;
        tx.commit().await.map_err(db_error)
    }
}

pub(super) async fn lock_policy(
    tx: &mut Transaction<'_, Postgres>,
    scope: &InstallationScope,
    claim: &PolicyClaim,
) -> Result<(), Error> {
    gate(tx, scope).await?;
    if scope.epoch != claim.epoch {
        return Err(Error::StaleFence);
    }
    let owner=sqlx::query("SELECT a.access,a.version FROM manager.assignments a JOIN manager.instances i ON i.assignment_id=a.id WHERE i.id=$1 AND a.installation_id=$2 FOR UPDATE OF a,i")
        .bind(claim.instance_id).bind(scope.id).fetch_optional(&mut **tx).await.map_err(db_error)?;
    if owner.is_none_or(|row| {
        row.get::<String, _>("access") != "enabled"
            || row.get::<i64, _>("version") != claim.assignment_version
    }) {
        return Err(Error::StaleFence);
    }
    let valid:Option<bool>=sqlx::query_scalar("SELECT s.lease_until>clock_timestamp() FROM manager.execution_policy_status s JOIN manager.execution_policy_versions p ON p.instance_id=s.instance_id AND p.generation=s.desired_generation WHERE s.instance_id=$1 AND s.desired_generation=$2 AND s.epoch=$3 AND s.lease_owner=$4 AND s.fence=$5 AND s.state='Reconciling' AND p.content_digest=$6 FOR UPDATE OF s")
        .bind(claim.instance_id).bind(claim.generation).bind(claim.epoch).bind(claim.worker).bind(claim.fence).bind(&claim.digest).fetch_optional(&mut **tx).await.map_err(db_error)?;
    if valid != Some(true) {
        return Err(Error::StaleFence);
    }
    Ok(())
}

fn decode_policy(value: Value, digest: &str) -> Result<ResolvedPolicy, Error> {
    let policy: ResolvedPolicy =
        serde_json::from_value(value).map_err(|_| Error::RecoveryRequired)?;
    if policy.digest()? != digest {
        return Err(Error::OwnershipConflict);
    }
    Ok(policy)
}

async fn resolve(
    tx: &mut Transaction<'_, Postgres>,
    scope: &InstallationScope,
    instance_id: Uuid,
    namespace: String,
    namespace_uid: String,
    generation: i64,
    selection: &AssignmentExecution,
) -> Result<ResolvedPolicy, Error> {
    let mut runtimes = Vec::new();
    for allowed in &selection.allowed {
        let row=sqlx::query("SELECT definition,content_digest FROM manager.terminal_runtime_versions WHERE id=$1 AND version=$2 AND schema_version=1")
            .bind(&allowed.runtime.id).bind(allowed.runtime.version).fetch_optional(&mut **tx).await.map_err(db_error)?.ok_or(Error::TemplateUnavailable)?;
        let definition: RuntimeDefinition =
            serde_json::from_value(row.get("definition")).map_err(|_| Error::RecoveryRequired)?;
        let definition_digest: String = row.get("content_digest");
        if definition.runtime != allowed.runtime || definition.digest()? != definition_digest {
            return Err(Error::OwnershipConflict);
        }
        if allowed.resources.cpu_millis > definition.resources.cpu_millis
            || allowed.resources.memory_mib > definition.resources.memory_mib
            || allowed.resources.ephemeral_storage_mib > definition.resources.ephemeral_storage_mib
        {
            return Err(Error::Invalid);
        }
        let mut mounts = Vec::new();
        for selected in &allowed.mounts {
            let value:Value=sqlx::query_scalar("SELECT definition FROM manager.storage_binding_versions WHERE id=$1 AND version=$2 AND instance_id=$3 AND namespace=$4 AND namespace_uid=$5")
                .bind(selected.binding_id).bind(selected.binding_version).bind(instance_id).bind(&namespace).bind(&namespace_uid).fetch_optional(&mut **tx).await.map_err(db_error)?.ok_or(Error::OwnershipConflict)?;
            let binding: StorageBinding =
                serde_json::from_value(value).map_err(|_| Error::RecoveryRequired)?;
            binding.validate()?;
            if binding.instance_id != instance_id
                || binding.id != selected.binding_id
                || binding.version != selected.binding_version
                || binding.namespace != namespace
                || binding.namespace_uid != namespace_uid
                || !binding.mounts.contains(&selected.mount)
            {
                return Err(Error::OwnershipConflict);
            }
            mounts.push(ResolvedMount {
                binding,
                mount: selected.mount.clone(),
            });
        }
        let mut pull_secrets = Vec::new();
        for name in &definition.pull_secret_names {
            let row=sqlx::query("SELECT namespace,namespace_uid,name,uid FROM manager.execution_pull_secrets WHERE instance_id=$1 AND namespace=$2 AND namespace_uid=$3 AND name=$4")
                .bind(instance_id).bind(&namespace).bind(&namespace_uid).bind(name).fetch_optional(&mut **tx).await.map_err(db_error)?.ok_or(Error::OwnershipConflict)?;
            pull_secrets.push(PullSecretBinding {
                instance_id,
                namespace: row.get("namespace"),
                namespace_uid: row.get("namespace_uid"),
                name: row.get("name"),
                uid: row.get("uid"),
            });
        }
        runtimes.push(ResolvedRuntime {
            definition,
            definition_digest,
            resources: allowed.resources.clone(),
            limits: allowed.limits.clone(),
            mounts,
            pull_secrets,
        });
    }
    Ok(ResolvedPolicy {
        installation_id: scope.id,
        instance_id,
        namespace,
        namespace_uid,
        generation,
        default_runtime_id: selection.default_runtime_id.clone(),
        runtimes,
    })
}
