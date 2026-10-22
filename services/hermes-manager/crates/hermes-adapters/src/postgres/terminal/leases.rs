use super::super::{PgStore, db_error, gate};
use async_trait::async_trait;
use hermes_application::{
    Error, InstallationScope,
    execution::ResolvedPolicy,
    terminal::{clients::ExecutionClient, leases::*},
};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;

type Tx<'a> = Transaction<'a, Postgres>;
fn mode(value: LifetimeMode) -> &'static str {
    match value {
        LifetimeMode::PerSession => "per_session",
        LifetimeMode::PerProfileOnDemand => "per_profile_on_demand",
        LifetimeMode::PerProfileAlwaysRunning => "per_profile_always_running",
    }
}
async fn lock_client(
    tx: &mut Tx<'_>,
    scope: &InstallationScope,
    client: &ExecutionClient,
) -> Result<sqlx::postgres::PgRow, Error> {
    gate(tx, scope).await?;
    // All execution mutations use one installation quota lock before owner locks.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,184712))")
        .bind(scope.id.to_string())
        .execute(&mut **tx)
        .await
        .map_err(db_error)?;
    let row=sqlx::query("SELECT i.*,a.access,i.drain_completed_at IS NOT NULL AS drain_complete FROM manager.execution_clients c JOIN manager.instances i ON i.id=c.instance_id JOIN manager.assignments a ON a.id=i.assignment_id WHERE c.id=$1 AND c.instance_id=$2 AND c.installation_id=$3 AND a.installation_id=$3 AND c.authorization_epoch=$4 AND c.revoked_at IS NULL AND a.access='enabled' FOR UPDATE OF i,a")
 .bind(client.id).bind(client.instance_id).bind(scope.id).bind(scope.epoch).fetch_optional(&mut **tx).await.map_err(db_error)?.ok_or(Error::Forbidden)?;
    if client.installation_id != scope.id || client.epoch != scope.epoch {
        return Err(Error::Forbidden);
    }
    Ok(row)
}
async fn policy(
    tx: &mut Tx<'_>,
    scope: &InstallationScope,
    instance: Uuid,
) -> Result<Option<(ResolvedPolicy, bool)>, Error> {
    let row=sqlx::query("SELECT v.policy,v.content_digest,s.applied_generation=s.desired_generation AND s.applied_generation>0 AND s.state='Applied' AND s.epoch=$2 AND EXISTS(SELECT 1 FROM manager.execution_effective_policy_receipts r WHERE r.instance_id=s.instance_id AND r.generation=s.desired_generation AND r.epoch=$2 AND r.content_digest=v.content_digest) AS effective FROM manager.execution_policy_status s JOIN manager.execution_policy_versions v ON v.instance_id=s.instance_id AND v.generation=s.desired_generation WHERE s.instance_id=$1")
 .bind(instance).bind(scope.epoch).fetch_optional(&mut **tx).await.map_err(db_error)?;
    row.map(|r| {
        let p: ResolvedPolicy =
            serde_json::from_value(r.get("policy")).map_err(|_| Error::RecoveryRequired)?;
        if p.instance_id != instance
            || p.installation_id != scope.id
            || p.digest()? != r.get::<String, _>("content_digest")
        {
            return Err(Error::RecoveryRequired);
        }
        Ok((p, r.get("effective")))
    })
    .transpose()
}
pub(in crate::postgres) async fn current_open(
    tx: &mut Tx<'_>,
    scope: &InstallationScope,
    instance: Uuid,
    generation: i64,
    namespace_uid: &str,
) -> Result<bool, Error> {
    use hermes_application::{
        StepResult,
        namespace_fence::{Admission, NamespaceFenceInput},
    };
    let row=sqlx::query("SELECT n.input,n.input_digest,s.result FROM manager.namespace_fence_inputs n JOIN manager.operation_steps s USING(operation_id,step_key) JOIN manager.owned_resources r ON r.id=n.resource_id WHERE r.installation_id=$1 AND r.instance_id=$2 AND s.verified_at IS NOT NULL ORDER BY s.verified_at DESC,s.recorded_at DESC,n.step_key DESC LIMIT 1").bind(scope.id).bind(instance).fetch_optional(&mut **tx).await.map_err(db_error)?;
    let Some(row) = row else { return Ok(false) };
    let input: NamespaceFenceInput =
        serde_json::from_value(row.get("input")).map_err(|_| Error::RecoveryRequired)?;
    let result: StepResult =
        serde_json::from_value(row.get("result")).map_err(|_| Error::RecoveryRequired)?;
    Ok(input.digest()? == row.get::<String, _>("input_digest")
        && input.installation_id == scope.id
        && input.instance_id == instance
        && input.namespace_uid == namespace_uid
        && input.state.epoch == scope.epoch
        && input.state.generation == generation
        && input.state.admission == Admission::Open
        && input.state.access == hermes_domain::Access::Enabled
        && matches!(result,StepResult::Verified{uid,..}if uid==namespace_uid))
}
async fn expire_owner(tx: &mut Tx<'_>, instance: Uuid) -> Result<(), Error> {
    sqlx::query("WITH due AS (UPDATE manager.execution_leases SET state='Expired',terminal_at=clock_timestamp(),fence=fence+1 WHERE instance_id=$1 AND state='Active' AND expires_at<=clock_timestamp() RETURNING environment_id) UPDATE manager.execution_environments SET state='Draining',drain_deadline=COALESCE(drain_deadline,clock_timestamp()+interval '5 minutes'),lease_owner=NULL,lease_until=NULL WHERE id IN(SELECT environment_id FROM due) AND state NOT IN ('Draining','Retired') AND NOT EXISTS(SELECT 1 FROM manager.execution_leases live WHERE live.environment_id=manager.execution_environments.id AND live.state='Active' AND live.expires_at>clock_timestamp())").bind(instance).execute(&mut **tx).await.map_err(db_error)?;
    Ok(())
}
fn limits(row: &sqlx::postgres::PgRow) -> Result<ExecutionLimits, Error> {
    let v: ExecutionLimits =
        serde_json::from_value(row.get("limits")).map_err(|_| Error::RecoveryRequired)?;
    v.validate()?;
    Ok(v)
}
async fn reply(
    tx: &mut Tx<'_>,
    row: &sqlx::postgres::PgRow,
    lease: Uuid,
    ttl: i64,
) -> Result<AcquireReply, Error> {
    let host_key: Option<String> = sqlx::query_scalar("SELECT host_key FROM manager.execution_worker_ready_receipts WHERE environment_id=$1 AND generation=$2")
        .bind(row.get::<Uuid,_>("id")).bind(row.get::<i64,_>("generation")).fetch_optional(&mut **tx).await.map_err(db_error)?.flatten();
    Ok(AcquireReply {
        environment: row.get("id"),
        generation: row.get::<i64, _>("generation").to_string(),
        lease,
        ttl,
        ssh: SshEndpoint {
            host: row.get("ssh_host"),
            port: 2222,
            user: "worker".into(),
            host_key,
        },
    })
}
#[async_trait]
impl ExecutionLeaseRepository for PgStore {
    async fn acquire_execution_lease(
        &self,
        scope: &InstallationScope,
        client: &ExecutionClient,
        request: &AcquireRequest,
    ) -> Result<AcquireOutcome, Error> {
        request.validate()?;
        let digest = request.digest()?;
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        let owner = lock_client(&mut tx, scope, client).await?;
        expire_owner(&mut tx, client.instance_id).await?;
        let previous = sqlx::query(
            "SELECT * FROM manager.execution_requests WHERE client_id=$1 AND request=$2",
        )
        .bind(client.id)
        .bind(&request.request)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_error)?;
        if previous
            .as_ref()
            .is_some_and(|r| r.get::<Vec<u8>, _>("body_digest") != digest)
        {
            return Err(Error::IdempotencyConflict);
        }
        let Some((approved, effective)) = policy(&mut tx, scope, client.instance_id).await? else {
            tx.commit().await.map_err(db_error)?;
            return Ok(AcquireOutcome::Pending);
        };
        let runtime_id = request
            .runtime_id
            .as_deref()
            .unwrap_or(&approved.default_runtime_id);
        let runtime = approved
            .runtimes
            .iter()
            .find(|r| r.definition.runtime.id == runtime_id)
            .ok_or(Error::Forbidden)?;
        runtime.limits.validate()?;
        if !effective
            || owner.get::<Option<String>, _>("namespace_uid").as_deref()
                != Some(&approved.namespace_uid)
            || owner.get::<Option<i64>, _>("drain_generation").is_some()
                && !owner.get::<bool, _>("drain_complete")
        {
            tx.commit().await.map_err(db_error)?;
            return Ok(AcquireOutcome::Pending);
        }
        if !current_open(
            &mut tx,
            scope,
            client.instance_id,
            owner.get("desired_generation"),
            &approved.namespace_uid,
        )
        .await?
        {
            tx.commit().await.map_err(db_error)?;
            return Ok(AcquireOutcome::Pending);
        }
        let identity = Sha256::digest(
            serde_json::to_vec(&json!([
                client.instance_id,
                mode(request.mode),
                if request.mode == LifetimeMode::PerSession {
                    request.session.as_str()
                } else {
                    "profile"
                },
                request.runtime,
                runtime_id,
                approved.generation
            ]))
            .map_err(|_| Error::Invalid)?,
        )
        .to_vec();
        if let Some(previous) = &previous {
            let retired: bool = sqlx::query_scalar(
                "SELECT state='Retired' FROM manager.execution_environments WHERE id=$1",
            )
            .bind(previous.get::<Uuid, _>("environment_id"))
            .fetch_one(&mut *tx)
            .await
            .map_err(db_error)?;
            if retired {
                return Err(Error::PreconditionFailed);
            }
        }
        let current=sqlx::query("SELECT * FROM manager.execution_environments WHERE instance_id=$1 AND identity_digest=$2 AND state<>'Retired' FOR UPDATE").bind(client.instance_id).bind(&identity).fetch_optional(&mut *tx).await.map_err(db_error)?;
        let env = if let Some(row) = current {
            row
        } else {
            let counts=sqlx::query("SELECT count(*) FILTER(WHERE instance_id=$1) AS owner,count(*) AS global FROM manager.execution_environments WHERE installation_id=$2 AND state<>'Retired'").bind(client.instance_id).bind(scope.id).fetch_one(&mut *tx).await.map_err(db_error)?;
            if counts.get::<i64, _>("owner") >= i64::from(runtime.limits.max_environments)
                || counts.get::<i64, _>("global") >= 1024
            {
                return Err(Error::Unavailable);
            }
            let id = Uuid::new_v4();
            sqlx::query("INSERT INTO manager.execution_environments(id,installation_id,instance_id,identity_digest,epoch,assignment_generation,policy_generation,policy_digest,runtime_id,session,mode,limits,invocation,pod_name) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14) RETURNING *")
 .bind(id).bind(scope.id).bind(client.instance_id).bind(identity).bind(scope.epoch).bind(owner.get::<i64,_>("desired_generation")).bind(approved.generation).bind(approved.digest()?).bind(runtime_id).bind(if request.mode==LifetimeMode::PerSession{request.session.as_str()}else{""}).bind(mode(request.mode)).bind(serde_json::to_value(&runtime.limits).map_err(|_|Error::Invalid)?).bind(serde_json::to_value(&request.runtime).map_err(|_|Error::Invalid)?).bind(worker_pod_name(id,1)?).fetch_one(&mut *tx).await.map_err(db_error)?
        };
        if env.get::<i64, _>("epoch") != scope.epoch
            || env.get::<i64, _>("assignment_generation")
                != owner.get::<i64, _>("desired_generation")
            || matches!(
                env.get::<String, _>("state").as_str(),
                "Draining" | "Retired"
            )
        {
            tx.commit().await.map_err(db_error)?;
            return Err(Error::PreconditionFailed);
        }
        if let Some(p) = &previous
            && p.get::<Uuid, _>("environment_id") != env.get::<Uuid, _>("id")
        {
            return Err(Error::IdempotencyConflict);
        }
        let lease = previous
            .as_ref()
            .map(|r| r.get::<Uuid, _>("lease_id"))
            .unwrap_or_else(Uuid::new_v4);
        if previous.is_none() {
            sqlx::query("INSERT INTO manager.execution_requests(client_id,request,body_digest,environment_id,environment_generation,lease_id)VALUES($1,$2,$3,$4,$5,$6)").bind(client.id).bind(&request.request).bind(&digest).bind(env.get::<Uuid,_>("id")).bind(env.get::<i64,_>("generation")).bind(lease).execute(&mut *tx).await.map_err(db_error)?;
        }
        let old=sqlx::query("SELECT state,GREATEST(0,floor(extract(epoch FROM expires_at-clock_timestamp())))::bigint AS ttl FROM manager.execution_leases WHERE id=$1").bind(lease).fetch_optional(&mut *tx).await.map_err(db_error)?;
        if let Some(old) = old {
            let ttl: i64 = old.get("ttl");
            if old.get::<String, _>("state") != "Active" || ttl < 1 {
                tx.commit().await.map_err(db_error)?;
                return Err(Error::PreconditionFailed);
            }
            let result = reply(&mut tx, &env, lease, ttl).await?;
            tx.commit().await.map_err(db_error)?;
            return Ok(AcquireOutcome::Granted(result));
        }
        let env_id: Uuid = env.get("id");
        if let Some(parent) = request.parent_lease {
            let live:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM manager.execution_leases WHERE id=$1 AND client_id=$2 AND instance_id=$3 AND environment_id=$4 AND environment_generation=$5 AND epoch=$6 AND state='Active' AND expires_at>clock_timestamp())").bind(parent).bind(client.id).bind(client.instance_id).bind(env_id).bind(env.get::<i64,_>("generation")).bind(scope.epoch).fetch_one(&mut *tx).await.map_err(db_error)?;
            if !live {
                return Err(Error::Forbidden);
            }
        }
        if env.get::<String, _>("state") != "Ready" {
            tx.commit().await.map_err(db_error)?;
            return Ok(AcquireOutcome::Pending);
        }
        let proof:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM manager.execution_worker_ready_receipts WHERE environment_id=$1 AND generation=$2 AND pod_uid=$3 AND spec_digest=$4 AND ssh_host=$5)").bind(env_id).bind(env.get::<i64,_>("generation")).bind(env.get::<String,_>("pod_uid")).bind(env.get::<String,_>("spec_digest")).bind(env.get::<String,_>("ssh_host")).fetch_one(&mut *tx).await.map_err(db_error)?;
        if !proof {
            return Err(Error::RecoveryRequired);
        }
        let bounds = limits(&env)?;
        let counts=sqlx::query("SELECT count(*) FILTER(WHERE instance_id=$1) AS owner,count(*) AS global,count(*) FILTER(WHERE environment_id=$2) AS environment,bool_or(exclusive) FILTER(WHERE environment_id=$2) AS exclusive FROM manager.execution_leases WHERE epoch=$3 AND state='Active' AND expires_at>clock_timestamp()").bind(client.instance_id).bind(env_id).bind(scope.epoch).fetch_one(&mut *tx).await.map_err(db_error)?;
        if counts.get::<i64, _>("owner") >= i64::from(bounds.max_leases)
            || counts.get::<i64, _>("global") >= 4096
        {
            return Err(Error::Unavailable);
        }
        if counts.get::<Option<bool>, _>("exclusive").unwrap_or(false)
            || (bounds.exclusive && counts.get::<i64, _>("environment") > 0)
        {
            tx.commit().await.map_err(db_error)?;
            return Ok(AcquireOutcome::Pending);
        }
        if bounds.serialize_profile {
            let other:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM manager.execution_environments WHERE instance_id=$1 AND id<>$2 AND state<>'Retired' AND (pod_uid IS NOT NULL OR state='Provisioning'))").bind(client.instance_id).bind(env_id).fetch_one(&mut *tx).await.map_err(db_error)?;
            if other {
                tx.commit().await.map_err(db_error)?;
                return Ok(AcquireOutcome::Pending);
            }
        }
        sqlx::query("INSERT INTO manager.execution_leases(id,client_id,instance_id,environment_id,environment_generation,epoch,parent_lease,exclusive,original_expires_at,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,clock_timestamp()+make_interval(secs=>$9),clock_timestamp()+make_interval(secs=>$9))").bind(lease).bind(client.id).bind(client.instance_id).bind(env_id).bind(env.get::<i64,_>("generation")).bind(scope.epoch).bind(request.parent_lease).bind(bounds.exclusive).bind(f64::from(bounds.ttl_seconds)).execute(&mut *tx).await.map_err(db_error)?;
        sqlx::query(
            "UPDATE manager.execution_environments SET last_used_at=clock_timestamp() WHERE id=$1",
        )
        .bind(env_id)
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
        let result = reply(&mut tx, &env, lease, i64::from(bounds.ttl_seconds)).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(AcquireOutcome::Granted(result))
    }
    async fn heartbeat_execution_lease(
        &self,
        scope: &InstallationScope,
        client: &ExecutionClient,
        request: &LeaseRequest,
    ) -> Result<HeartbeatReply, Error> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        let owner = lock_client(&mut tx, scope, client).await?;
        expire_owner(&mut tx, client.instance_id).await?;
        let row=sqlx::query("SELECT e.*,l.state AS lease_state,l.epoch AS lease_epoch FROM manager.execution_leases l JOIN manager.execution_environments e ON e.id=l.environment_id WHERE l.id=$1 AND l.client_id=$2 AND l.instance_id=$3 FOR UPDATE OF l,e").bind(request.lease).bind(client.id).bind(client.instance_id).fetch_optional(&mut *tx).await.map_err(db_error)?.ok_or(Error::NotFound)?;
        let approved = policy(&mut tx, scope, client.instance_id).await?;
        if row.get::<String, _>("lease_state") != "Active"
            || row.get::<String, _>("state") != "Ready"
            || row.get::<i64, _>("lease_epoch") != scope.epoch
            || owner.get::<i64, _>("desired_generation")
                != row.get::<i64, _>("assignment_generation")
            || owner.get::<Option<i64>, _>("drain_generation").is_some()
                && !owner.get::<bool, _>("drain_complete")
            || !approved
                .is_some_and(|(p, e)| e && p.generation == row.get::<i64, _>("policy_generation"))
        {
            tx.commit().await.map_err(db_error)?;
            return Err(Error::PreconditionFailed);
        }
        if !current_open(
            &mut tx,
            scope,
            client.instance_id,
            owner.get("desired_generation"),
            owner
                .get::<Option<String>, _>("namespace_uid")
                .as_deref()
                .ok_or(Error::PreconditionFailed)?,
        )
        .await?
        {
            tx.commit().await.map_err(db_error)?;
            return Err(Error::PreconditionFailed);
        }
        let ttl = limits(&row)?.ttl_seconds;
        let updated=sqlx::query("UPDATE manager.execution_leases SET expires_at=GREATEST(expires_at,clock_timestamp()+make_interval(secs=>$2)),fence=fence+1 WHERE id=$1 AND state='Active' AND expires_at>clock_timestamp()").bind(request.lease).bind(f64::from(ttl)).execute(&mut *tx).await.map_err(db_error)?;
        if updated.rows_affected() != 1 {
            tx.commit().await.map_err(db_error)?;
            return Err(Error::PreconditionFailed);
        }
        tx.commit().await.map_err(db_error)?;
        Ok(HeartbeatReply {
            ttl: i64::from(ttl),
        })
    }
    async fn release_execution_lease(
        &self,
        scope: &InstallationScope,
        client: &ExecutionClient,
        request: &LeaseRequest,
    ) -> Result<ReleaseReply, Error> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        lock_client(&mut tx, scope, client).await?;
        expire_owner(&mut tx, client.instance_id).await?;
        let found:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM manager.execution_leases WHERE id=$1 AND client_id=$2 AND instance_id=$3)").bind(request.lease).bind(client.id).bind(client.instance_id).fetch_one(&mut *tx).await.map_err(db_error)?;
        if !found {
            return Err(Error::NotFound);
        }
        sqlx::query("UPDATE manager.execution_leases SET state='Released',terminal_at=clock_timestamp(),fence=fence+1 WHERE id=$1 AND state='Active'").bind(request.lease).execute(&mut *tx).await.map_err(db_error)?;
        tx.commit().await.map_err(db_error)?;
        Ok(ReleaseReply { released: true })
    }
    async fn close_execution_session(
        &self,
        scope: &InstallationScope,
        client: &ExecutionClient,
        request: &CloseSessionRequest,
    ) -> Result<CloseReply, Error> {
        request.validate()?;
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        lock_client(&mut tx, scope, client).await?;
        let Some((p, _)) = policy(&mut tx, scope, client.instance_id).await? else {
            return Err(Error::Forbidden);
        };
        let runtime = request
            .runtime_id
            .as_deref()
            .unwrap_or(&p.default_runtime_id);
        if !p
            .runtimes
            .iter()
            .any(|r| r.definition.runtime.id == runtime)
        {
            return Err(Error::Forbidden);
        }
        sqlx::query("UPDATE manager.execution_environments SET state='Draining',drain_deadline=COALESCE(drain_deadline,clock_timestamp()+interval '5 minutes'),lease_owner=NULL,lease_until=NULL WHERE instance_id=$1 AND session=$2 AND mode='per_session' AND runtime_id=$3 AND invocation=$4 AND state NOT IN('Draining','Retired')").bind(client.instance_id).bind(&request.session).bind(runtime).bind(serde_json::to_value(&request.runtime).map_err(|_|Error::Invalid)?).execute(&mut *tx).await.map_err(db_error)?;
        tx.commit().await.map_err(db_error)?;
        Ok(CloseReply { closed: true })
    }
    async fn list_execution_environments(
        &self,
        scope: &InstallationScope,
        client: &ExecutionClient,
    ) -> Result<Vec<EnvironmentSummary>, Error> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        lock_client(&mut tx, scope, client).await?;
        let rows=sqlx::query("SELECT e.*,(SELECT count(*) FROM manager.execution_leases l WHERE l.environment_id=e.id AND l.state='Active' AND l.expires_at>clock_timestamp()) AS leases FROM manager.execution_environments e WHERE e.instance_id=$1 ORDER BY e.created_at,e.id LIMIT 64").bind(client.instance_id).fetch_all(&mut *tx).await.map_err(db_error)?;
        let result = rows
            .into_iter()
            .map(|r| {
                Ok(EnvironmentSummary {
                    id: r.get("id"),
                    session: r.get("session"),
                    mode: serde_json::from_value(json!(r.get::<String, _>("mode")))
                        .map_err(|_| Error::RecoveryRequired)?,
                    generation: r.get::<i64, _>("generation").to_string(),
                    stale: matches!(r.get::<String, _>("state").as_str(), "Draining" | "Retired"),
                    provisioned: r.get::<Option<String>, _>("pod_uid").is_some(),
                    leases: r.get("leases"),
                    runtime_id: r.get("runtime_id"),
                })
            })
            .collect::<Result<Vec<_>, Error>>()?;
        tx.commit().await.map_err(db_error)?;
        Ok(result)
    }
    async fn expire_execution_leases(
        &self,
        scope: &InstallationScope,
        limit: u32,
    ) -> Result<u64, Error> {
        if !(1..=128).contains(&limit) {
            return Err(Error::Invalid);
        }
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        gate(&mut tx, scope).await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,184712))")
            .bind(scope.id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        let rows=sqlx::query("WITH picked AS(SELECT l.id FROM manager.execution_leases l JOIN manager.execution_environments e ON e.id=l.environment_id WHERE e.installation_id=$1 AND l.state='Active' AND l.expires_at<=clock_timestamp() ORDER BY l.expires_at,l.id LIMIT $2 FOR UPDATE OF l SKIP LOCKED) UPDATE manager.execution_leases SET state='Expired',terminal_at=clock_timestamp(),fence=fence+1 WHERE id IN(SELECT id FROM picked) RETURNING environment_id").bind(scope.id).bind(i64::from(limit)).fetch_all(&mut *tx).await.map_err(db_error)?;
        let ids: Vec<Uuid> = rows.iter().map(|r| r.get("environment_id")).collect();
        sqlx::query("UPDATE manager.execution_environments SET state='Draining',drain_deadline=COALESCE(drain_deadline,clock_timestamp()+interval '5 minutes'),lease_owner=NULL,lease_until=NULL WHERE id=ANY($1) AND state NOT IN('Draining','Retired') AND NOT EXISTS(SELECT 1 FROM manager.execution_leases live WHERE live.environment_id=manager.execution_environments.id AND live.state='Active' AND live.expires_at>clock_timestamp())").bind(ids).execute(&mut *tx).await.map_err(db_error)?;
        tx.commit().await.map_err(db_error)?;
        Ok(rows.len() as u64)
    }
}

async fn lock_environment_claim(
    tx: &mut Tx<'_>,
    scope: &InstallationScope,
    claim: &EnvironmentClaim,
) -> Result<sqlx::postgres::PgRow, Error> {
    gate(tx, scope).await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,184712))")
        .bind(scope.id.to_string())
        .execute(&mut **tx)
        .await
        .map_err(db_error)?;
    let row=sqlx::query("SELECT e.* FROM manager.execution_environments e JOIN manager.instances i ON i.id=e.instance_id JOIN manager.assignments a ON a.id=i.assignment_id JOIN manager.execution_policy_status s ON s.instance_id=i.id WHERE e.id=$1 AND e.instance_id=$2 AND e.installation_id=$3 AND e.generation=$4 AND e.epoch=$5 AND e.fence=$6 AND e.lease_owner=$7 AND e.lease_until>clock_timestamp() AND e.state='Provisioning' AND a.access='enabled' AND i.desired_generation=e.assignment_generation AND (i.drain_generation IS NULL OR i.drain_completed_at IS NOT NULL) AND i.namespace_uid=$8 AND s.desired_generation=e.policy_generation AND s.applied_generation=e.policy_generation AND s.epoch=$5 FOR UPDATE OF i,a,e")
 .bind(claim.environment_id).bind(claim.instance_id).bind(scope.id).bind(claim.generation).bind(scope.epoch).bind(claim.fence).bind(claim.worker).bind(&claim.policy.namespace_uid).fetch_optional(&mut **tx).await.map_err(db_error)?.ok_or(Error::StaleFence)?;
    if claim.installation_id != scope.id
        || claim.epoch != scope.epoch
        || claim.policy.instance_id != claim.instance_id
        || claim.policy.installation_id != scope.id
        || row.get::<i64, _>("policy_generation") != claim.policy.generation
        || row.get::<String, _>("policy_digest") != claim.policy.digest()?
        || row.get::<String, _>("runtime_id") != claim.runtime_id
        || row.get::<String, _>("pod_name") != claim.pod_name
        || row.get::<i64, _>("assignment_generation") != claim.assignment_generation
    {
        return Err(Error::StaleFence);
    }
    let Some((p, true)) = policy(tx, scope, claim.instance_id).await? else {
        return Err(Error::StaleFence);
    };
    if p != claim.policy {
        return Err(Error::StaleFence);
    }
    if !current_open(
        tx,
        scope,
        claim.instance_id,
        claim.assignment_generation,
        &claim.policy.namespace_uid,
    )
    .await?
    {
        return Err(Error::StaleFence);
    }
    Ok(row)
}
#[async_trait]
impl ExecutionEnvironmentRepository for PgStore {
    async fn claim_execution_environment(
        &self,
        scope: &InstallationScope,
        worker: Uuid,
        lease_seconds: u32,
    ) -> Result<Option<EnvironmentClaim>, Error> {
        if worker.is_nil() || !(5..=120).contains(&lease_seconds) {
            return Err(Error::Invalid);
        }
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        gate(&mut tx, scope).await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,184712))")
            .bind(scope.id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        let candidate:Option<Uuid>=sqlx::query_scalar("SELECT e.id FROM manager.execution_environments e JOIN manager.instances i ON i.id=e.instance_id JOIN manager.assignments a ON a.id=i.assignment_id JOIN manager.execution_policy_status s ON s.instance_id=i.id WHERE e.installation_id=$1 AND e.epoch=$2 AND e.state IN('Pending','Provisioning') AND (e.lease_until IS NULL OR e.lease_until<=clock_timestamp()) AND a.access='enabled' AND i.desired_generation=e.assignment_generation AND (i.drain_generation IS NULL OR i.drain_completed_at IS NOT NULL) AND s.applied_generation=e.policy_generation AND s.desired_generation=e.policy_generation AND s.epoch=$2 AND manager.execution_namespace_open(e.instance_id,$2,e.assignment_generation,i.namespace_uid) AND (NOT COALESCE((e.limits->>'serializeProfile')::boolean,false) OR NOT EXISTS(SELECT 1 FROM manager.execution_environments other WHERE other.instance_id=e.instance_id AND other.id<>e.id AND other.state<>'Retired' AND (other.pod_uid IS NOT NULL OR other.state='Provisioning'))) AND EXISTS(SELECT 1 FROM manager.execution_effective_policy_receipts r WHERE r.instance_id=e.instance_id AND r.generation=e.policy_generation AND r.epoch=e.epoch AND r.content_digest=e.policy_digest) ORDER BY e.created_at,e.id LIMIT 1 FOR UPDATE OF i,a,e SKIP LOCKED").bind(scope.id).bind(scope.epoch).fetch_optional(&mut *tx).await.map_err(db_error)?;
        let Some(id) = candidate else {
            tx.commit().await.map_err(db_error)?;
            return Ok(None);
        };
        let row=sqlx::query("UPDATE manager.execution_environments SET state='Provisioning',fence=fence+1,lease_owner=$2,lease_until=clock_timestamp()+make_interval(secs=>$3) WHERE id=$1 RETURNING *").bind(id).bind(worker).bind(f64::from(lease_seconds)).fetch_one(&mut *tx).await.map_err(db_error)?;
        let Some((approved, true)) = policy(&mut tx, scope, row.get("instance_id")).await? else {
            return Err(Error::StaleFence);
        };
        let claim = EnvironmentClaim {
            installation_id: scope.id,
            instance_id: row.get("instance_id"),
            environment_id: id,
            generation: row.get("generation"),
            epoch: scope.epoch,
            worker,
            fence: row.get("fence"),
            assignment_generation: row.get("assignment_generation"),
            policy: approved,
            runtime_id: row.get("runtime_id"),
            pod_name: row.get("pod_name"),
        };
        tx.commit().await.map_err(db_error)?;
        Ok(Some(claim))
    }
    async fn prepare_execution_worker(
        &self,
        scope: &InstallationScope,
        claim: &EnvironmentClaim,
        spec_digest: &str,
    ) -> Result<WorkerIntent, Error> {
        hermes_domain::validate_sha256(spec_digest)?;
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        lock_environment_claim(&mut tx, scope, claim).await?;
        sqlx::query("INSERT INTO manager.execution_worker_intents(environment_id,generation,epoch,fence,spec_digest) VALUES($1,$2,$3,$4,$5) ON CONFLICT(environment_id,generation) DO NOTHING").bind(claim.environment_id).bind(claim.generation).bind(scope.epoch).bind(claim.fence).bind(spec_digest).execute(&mut *tx).await.map_err(db_error)?;
        let stored:String=sqlx::query_scalar("SELECT spec_digest FROM manager.execution_worker_intents WHERE environment_id=$1 AND generation=$2 AND epoch=$3").bind(claim.environment_id).bind(claim.generation).bind(scope.epoch).fetch_one(&mut *tx).await.map_err(db_error)?;
        if stored != spec_digest {
            return Err(Error::OwnershipConflict);
        }
        tx.commit().await.map_err(db_error)?;
        Ok(WorkerIntent {
            claim: claim.clone(),
            spec_digest: stored,
        })
    }
    async fn bind_execution_worker(
        &self,
        scope: &InstallationScope,
        intent: &WorkerIntent,
        binding: &WorkerBinding,
    ) -> Result<WorkerStartIntent, Error> {
        if !hermes_domain::resources::valid_uid(&binding.pod_uid)
            || !hermes_domain::resources::valid_uid(&binding.resource_version)
        {
            return Err(Error::Invalid);
        }
        hermes_domain::validate_sha256(&binding.observed_spec_digest)?;
        hermes_domain::validate_sha256(&intent.spec_digest)?;
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        lock_environment_claim(&mut tx, scope, &intent.claim).await?;
        let valid:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM manager.execution_worker_intents WHERE environment_id=$1 AND generation=$2 AND spec_digest=$3 AND epoch=$4)").bind(intent.claim.environment_id).bind(intent.claim.generation).bind(&intent.spec_digest).bind(scope.epoch).fetch_one(&mut *tx).await.map_err(db_error)?;
        if !valid {
            return Err(Error::OwnershipConflict);
        }
        let existing:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM manager.execution_worker_start_intents WHERE environment_id=$1 AND generation=$2)").bind(intent.claim.environment_id).bind(intent.claim.generation).fetch_one(&mut *tx).await.map_err(db_error)?;
        if !existing && !binding.gated {
            return Err(Error::OwnershipConflict);
        }
        sqlx::query("INSERT INTO manager.execution_worker_start_intents(environment_id,generation,pod_uid,resource_version,spec_digest,observed_spec_digest) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(environment_id,generation) DO NOTHING").bind(intent.claim.environment_id).bind(intent.claim.generation).bind(&binding.pod_uid).bind(&binding.resource_version).bind(&intent.spec_digest).bind(&binding.observed_spec_digest).execute(&mut *tx).await.map_err(db_error)?;
        let row=sqlx::query("SELECT * FROM manager.execution_worker_start_intents WHERE environment_id=$1 AND generation=$2").bind(intent.claim.environment_id).bind(intent.claim.generation).fetch_one(&mut *tx).await.map_err(db_error)?;
        if row.get::<String, _>("pod_uid") != binding.pod_uid
            || row.get::<String, _>("spec_digest") != intent.spec_digest
            || row.get::<String, _>("observed_spec_digest") != binding.observed_spec_digest
        {
            return Err(Error::OwnershipConflict);
        }
        sqlx::query("INSERT INTO manager.execution_worker_start_inputs(environment_id,generation,resource_version,pod_uid,spec_digest,observed_spec_digest,epoch,fence) VALUES($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT DO NOTHING")
            .bind(intent.claim.environment_id).bind(intent.claim.generation).bind(&binding.resource_version).bind(&binding.pod_uid).bind(&intent.spec_digest).bind(&binding.observed_spec_digest).bind(scope.epoch).bind(intent.claim.fence).execute(&mut *tx).await.map_err(db_error)?;
        let input = sqlx::query("SELECT pod_uid,spec_digest,observed_spec_digest,epoch FROM manager.execution_worker_start_inputs WHERE environment_id=$1 AND generation=$2 AND resource_version=$3")
            .bind(intent.claim.environment_id).bind(intent.claim.generation).bind(&binding.resource_version).fetch_one(&mut *tx).await.map_err(db_error)?;
        if input.get::<String, _>("pod_uid") != binding.pod_uid
            || input.get::<String, _>("spec_digest") != intent.spec_digest
            || input.get::<String, _>("observed_spec_digest") != binding.observed_spec_digest
            || input.get::<i64, _>("epoch") != scope.epoch
        {
            return Err(Error::OwnershipConflict);
        }
        let result = WorkerStartIntent {
            intent: intent.clone(),
            pod_uid: row.get("pod_uid"),
            resource_version: binding.resource_version.clone(),
            observed_spec_digest: row.get("observed_spec_digest"),
        };
        tx.commit().await.map_err(db_error)?;
        Ok(result)
    }
    async fn record_execution_worker_ready(
        &self,
        scope: &InstallationScope,
        intent: &WorkerIntent,
        ready: &WorkerReady,
    ) -> Result<(), Error> {
        if !hermes_domain::resources::valid_uid(&ready.pod_uid)
            || !hermes_domain::resources::valid_uid(&ready.resource_version)
            || ready.host.parse::<std::net::IpAddr>().is_err()
        {
            return Err(Error::Invalid);
        }
        hermes_domain::validate_sha256(&ready.observed_spec_digest)?;
        if ready.spec_digest != intent.spec_digest {
            return Err(Error::OwnershipConflict);
        }
        if let Some(key) = &ready.host_key {
            let public = ssh_key::PublicKey::from_openssh(key).map_err(|_| Error::Invalid)?;
            if key.len() > 1024 || public.algorithm() != ssh_key::Algorithm::Ed25519 {
                return Err(Error::Invalid);
            }
        }
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        lock_environment_claim(&mut tx, scope, &intent.claim).await?;
        let valid:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM manager.execution_worker_start_intents WHERE environment_id=$1 AND generation=$2 AND pod_uid=$3 AND spec_digest=$4 AND observed_spec_digest=$5)").bind(intent.claim.environment_id).bind(intent.claim.generation).bind(&ready.pod_uid).bind(&ready.spec_digest).bind(&ready.observed_spec_digest).fetch_one(&mut *tx).await.map_err(db_error)?;
        if !valid {
            return Err(Error::OwnershipConflict);
        }
        sqlx::query("INSERT INTO manager.execution_worker_ready_receipts(environment_id,generation,pod_uid,resource_version,spec_digest,observed_spec_digest,ssh_host,host_key)VALUES($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT(environment_id,generation) DO NOTHING").bind(intent.claim.environment_id).bind(intent.claim.generation).bind(&ready.pod_uid).bind(&ready.resource_version).bind(&ready.spec_digest).bind(&ready.observed_spec_digest).bind(&ready.host).bind(&ready.host_key).execute(&mut *tx).await.map_err(db_error)?;
        let same:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM manager.execution_worker_ready_receipts WHERE environment_id=$1 AND generation=$2 AND pod_uid=$3 AND spec_digest=$4 AND observed_spec_digest=$5 AND ssh_host=$6 AND host_key IS NOT DISTINCT FROM $7)").bind(intent.claim.environment_id).bind(intent.claim.generation).bind(&ready.pod_uid).bind(&ready.spec_digest).bind(&ready.observed_spec_digest).bind(&ready.host).bind(&ready.host_key).fetch_one(&mut *tx).await.map_err(db_error)?;
        if !same {
            return Err(Error::OwnershipConflict);
        }
        sqlx::query("UPDATE manager.execution_environments SET state='Ready',pod_uid=$2,resource_version=$3,spec_digest=$4,ssh_host=$5,lease_owner=NULL,lease_until=NULL WHERE id=$1").bind(intent.claim.environment_id).bind(&ready.pod_uid).bind(&ready.resource_version).bind(&ready.spec_digest).bind(&ready.host).execute(&mut *tx).await.map_err(db_error)?;
        tx.commit().await.map_err(db_error)?;
        Ok(())
    }
}

impl PgStore {
    pub async fn execution_bootstrap_material(
        &self,
        scope: &InstallationScope,
        claim: &EnvironmentClaim,
    ) -> Result<hermes_application::materials::MaterialRecord, Error> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        lock_environment_claim(&mut tx, scope, claim).await?;
        let row = sqlx::query("SELECT * FROM manager.bootstrap_materials WHERE installation_id=$1 AND instance_id=$2 AND purpose='native-bootstrap-v1' AND epoch<=$3 AND creation_generation<=$4")
            .bind(scope.id).bind(claim.instance_id).bind(scope.epoch).bind(claim.assignment_generation).fetch_one(&mut *tx).await.map_err(db_error)?;
        let result = super::super::materials::record(&row)?;
        tx.commit().await.map_err(db_error)?;
        Ok(result)
    }
    pub async fn defer_execution_environment(
        &self,
        scope: &InstallationScope,
        claim: &EnvironmentClaim,
    ) -> Result<(), Error> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        lock_environment_claim(&mut tx, scope, claim).await?;
        sqlx::query("UPDATE manager.execution_environments SET lease_until=clock_timestamp()+interval '3 seconds' WHERE id=$1")
            .bind(claim.environment_id).execute(&mut *tx).await.map_err(db_error)?;
        tx.commit().await.map_err(db_error)
    }
}
