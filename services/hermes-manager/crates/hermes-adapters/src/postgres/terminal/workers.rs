//! Bounded, fenced terminal retirement. Kubernetes absence is observed outside SQL.
use super::super::{PgStore, db_error, gate};
use hermes_application::{
    Error, InstallationScope, execution::ResolvedPolicy, terminal::leases::*,
};
use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;

async fn lock(
    tx: &mut Transaction<'_, Postgres>,
    scope: &InstallationScope,
    work: &WorkerRetirement,
) -> Result<(), Error> {
    gate(tx, scope).await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,184712))")
        .bind(scope.id.to_string())
        .execute(&mut **tx)
        .await
        .map_err(db_error)?;
    let c = &work.claim;
    let valid: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM manager.execution_environments WHERE id=$1 AND instance_id=$2 AND installation_id=$3 AND epoch=$4 AND generation=$5 AND fence=$6 AND lease_owner=$7 AND lease_until>clock_timestamp() AND state='Draining' AND (drain_deadline<=clock_timestamp() OR NOT EXISTS(SELECT 1 FROM manager.execution_leases l WHERE l.environment_id=$1 AND l.state='Active' AND l.expires_at>clock_timestamp())))")
        .bind(c.environment_id).bind(c.instance_id).bind(scope.id).bind(scope.epoch).bind(c.generation).bind(c.fence).bind(c.worker).fetch_one(&mut **tx).await.map_err(db_error)?;
    if !valid || c.installation_id != scope.id || c.epoch != scope.epoch {
        return Err(Error::StaleFence);
    }
    Ok(())
}
impl PgStore {
    pub async fn claim_ready_worker_observation(
        &self,
        scope: &InstallationScope,
        worker: Uuid,
    ) -> Result<Option<WorkerRetirement>, Error> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        gate(&mut tx, scope).await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,184712))")
            .bind(scope.id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        let row = sqlx::query("SELECT e.*,v.policy,w.spec_digest AS intent_digest FROM manager.execution_environments e JOIN manager.execution_policy_versions v ON v.instance_id=e.instance_id AND v.generation=e.policy_generation JOIN manager.execution_worker_intents w ON w.environment_id=e.id AND w.generation=e.generation WHERE e.installation_id=$1 AND e.epoch=$2 AND e.state='Ready' AND (e.lease_until IS NULL OR e.lease_until<=clock_timestamp()) ORDER BY e.lease_until NULLS FIRST,e.id LIMIT 1 FOR UPDATE OF e SKIP LOCKED")
            .bind(scope.id).bind(scope.epoch).fetch_optional(&mut *tx).await.map_err(db_error)?;
        let Some(row) = row else {
            tx.commit().await.map_err(db_error)?;
            return Ok(None);
        };
        let id: Uuid = row.get("id");
        let fence: i64 = sqlx::query_scalar("UPDATE manager.execution_environments SET fence=fence+1,lease_owner=$2,lease_until=clock_timestamp()+interval '30 seconds' WHERE id=$1 RETURNING fence")
            .bind(id).bind(worker).fetch_one(&mut *tx).await.map_err(db_error)?;
        let policy: ResolvedPolicy =
            serde_json::from_value(row.get("policy")).map_err(|_| Error::RecoveryRequired)?;
        if policy.installation_id != scope.id
            || policy.instance_id != row.get::<Uuid, _>("instance_id")
            || policy.digest()? != row.get::<String, _>("policy_digest")
        {
            return Err(Error::RecoveryRequired);
        }
        let claim = EnvironmentClaim {
            installation_id: scope.id,
            instance_id: row.get("instance_id"),
            environment_id: id,
            generation: row.get("generation"),
            epoch: scope.epoch,
            worker,
            fence,
            assignment_generation: row.get("assignment_generation"),
            policy,
            runtime_id: row.get("runtime_id"),
            pod_name: row.get("pod_name"),
        };
        let work = WorkerRetirement {
            intent: Some(WorkerIntent {
                claim: claim.clone(),
                spec_digest: row.get("intent_digest"),
            }),
            claim,
            pod_uid: row.get("pod_uid"),
        };
        tx.commit().await.map_err(db_error)?;
        Ok(Some(work))
    }
    pub async fn mark_execution_worker_lost(
        &self,
        scope: &InstallationScope,
        work: &WorkerRetirement,
    ) -> Result<(), Error> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        gate(&mut tx, scope).await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,184712))")
            .bind(scope.id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        let c = &work.claim;
        let changed = sqlx::query("UPDATE manager.execution_environments SET state='Draining',drain_deadline=COALESCE(drain_deadline,clock_timestamp()),lease_owner=NULL,lease_until=NULL,fence=fence+1 WHERE installation_id=$1 AND epoch=$2 AND id=$3 AND instance_id=$4 AND generation=$5 AND state='Ready' AND fence=$6 AND lease_owner=$7 AND lease_until>clock_timestamp() AND pod_uid=$8")
            .bind(scope.id).bind(scope.epoch).bind(c.environment_id).bind(c.instance_id).bind(c.generation).bind(c.fence).bind(c.worker).bind(&work.pod_uid).execute(&mut *tx).await.map_err(db_error)?;
        if changed.rows_affected() != 1 {
            return Err(Error::StaleFence);
        }
        tx.commit().await.map_err(db_error)
    }
    pub async fn defer_worker_retirement(
        &self,
        scope: &InstallationScope,
        work: &WorkerRetirement,
    ) -> Result<(), Error> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        lock(&mut tx, scope, work).await?;
        sqlx::query("UPDATE manager.execution_environments SET lease_until=clock_timestamp()+interval '3 seconds' WHERE id=$1")
            .bind(work.claim.environment_id).execute(&mut *tx).await.map_err(db_error)?;
        tx.commit().await.map_err(db_error)
    }
    pub async fn claim_worker_retirement(
        &self,
        scope: &InstallationScope,
        worker: Uuid,
    ) -> Result<Option<WorkerRetirement>, Error> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        gate(&mut tx, scope).await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,184712))")
            .bind(scope.id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        // Changing desired native generation invalidates worker starts too.
        // Idle retirement waits for all child leases and excludes always-running.
        sqlx::query("WITH picked AS (SELECT e.id FROM manager.execution_environments e JOIN manager.instances i ON i.id=e.instance_id JOIN manager.assignments a ON a.id=i.assignment_id WHERE e.installation_id=$1 AND e.epoch=$2 AND e.state IN ('Pending','Provisioning','Ready') AND (a.access<>'enabled' OR i.desired_generation<>e.assignment_generation OR (e.mode<>'per_profile_always_running' AND e.last_used_at+make_interval(secs=>(e.limits->>'idleSeconds')::double precision)<=clock_timestamp() AND NOT EXISTS(SELECT 1 FROM manager.execution_leases l WHERE l.environment_id=e.id AND l.state='Active' AND l.expires_at>clock_timestamp()))) ORDER BY e.created_at,e.id LIMIT 64 FOR UPDATE OF e SKIP LOCKED) UPDATE manager.execution_environments SET state='Draining',drain_deadline=COALESCE(drain_deadline,clock_timestamp()+interval '5 minutes'),lease_owner=NULL,lease_until=NULL WHERE id IN(SELECT id FROM picked)")
            .bind(scope.id).bind(scope.epoch).execute(&mut *tx).await.map_err(db_error)?;
        let row = sqlx::query("SELECT e.*,v.policy,w.spec_digest AS intent_digest,coalesce(e.pod_uid,b.pod_uid,(SELECT x.pod_uid FROM manager.execution_worker_stop_inputs x WHERE x.environment_id=e.id AND x.generation=e.generation ORDER BY x.resource_version LIMIT 1)) AS bound_uid FROM manager.execution_environments e JOIN manager.execution_policy_versions v ON v.instance_id=e.instance_id AND v.generation=e.policy_generation LEFT JOIN manager.execution_worker_intents w ON w.environment_id=e.id AND w.generation=e.generation LEFT JOIN manager.execution_worker_start_intents b ON b.environment_id=e.id AND b.generation=e.generation WHERE e.installation_id=$1 AND e.epoch=$2 AND e.state='Draining' AND (e.lease_until IS NULL OR e.lease_until<=clock_timestamp()) AND (e.drain_deadline<=clock_timestamp() OR NOT EXISTS(SELECT 1 FROM manager.execution_leases l WHERE l.environment_id=e.id AND l.state='Active' AND l.expires_at>clock_timestamp())) ORDER BY e.created_at,e.id LIMIT 1 FOR UPDATE OF e SKIP LOCKED")
            .bind(scope.id).bind(scope.epoch).fetch_optional(&mut *tx).await.map_err(db_error)?;
        let Some(row) = row else {
            tx.commit().await.map_err(db_error)?;
            return Ok(None);
        };
        let id: Uuid = row.get("id");
        let fence: i64 = sqlx::query_scalar("UPDATE manager.execution_environments SET fence=fence+1,lease_owner=$2,lease_until=clock_timestamp()+interval '120 seconds' WHERE id=$1 RETURNING fence")
            .bind(id).bind(worker).fetch_one(&mut *tx).await.map_err(db_error)?;
        let policy: ResolvedPolicy =
            serde_json::from_value(row.get("policy")).map_err(|_| Error::RecoveryRequired)?;
        if policy.instance_id != row.get::<Uuid, _>("instance_id")
            || policy.installation_id != scope.id
            || policy.digest()? != row.get::<String, _>("policy_digest")
        {
            return Err(Error::RecoveryRequired);
        }
        let claim = EnvironmentClaim {
            installation_id: scope.id,
            instance_id: row.get("instance_id"),
            environment_id: id,
            generation: row.get("generation"),
            epoch: scope.epoch,
            worker,
            fence,
            assignment_generation: row.get("assignment_generation"),
            policy,
            runtime_id: row.get("runtime_id"),
            pod_name: row.get("pod_name"),
        };
        let intent = row
            .get::<Option<String>, _>("intent_digest")
            .map(|spec_digest| WorkerIntent {
                claim: claim.clone(),
                spec_digest,
            });
        let work = WorkerRetirement {
            claim,
            intent,
            pod_uid: row.get("bound_uid"),
        };
        tx.commit().await.map_err(db_error)?;
        Ok(Some(work))
    }
    pub async fn prepare_worker_stop(
        &self,
        scope: &InstallationScope,
        work: &WorkerRetirement,
        binding: &WorkerBinding,
    ) -> Result<(), Error> {
        if !hermes_domain::resources::valid_uid(&binding.pod_uid)
            || !hermes_domain::resources::valid_uid(&binding.resource_version)
        {
            return Err(Error::Invalid);
        }
        hermes_domain::validate_sha256(&binding.observed_spec_digest)?;
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        lock(&mut tx, scope, work).await?;
        if work.intent.is_none()
            || work
                .pod_uid
                .as_ref()
                .is_some_and(|uid| uid != &binding.pod_uid)
        {
            return Err(Error::OwnershipConflict);
        }
        let same: bool = sqlx::query_scalar("SELECT NOT EXISTS(SELECT 1 FROM manager.execution_worker_start_intents WHERE environment_id=$1 AND generation=$2 AND (pod_uid<>$3 OR observed_spec_digest<>$4))")
            .bind(work.claim.environment_id).bind(work.claim.generation).bind(&binding.pod_uid).bind(&binding.observed_spec_digest).fetch_one(&mut *tx).await.map_err(db_error)?;
        if !same {
            return Err(Error::OwnershipConflict);
        }
        sqlx::query("INSERT INTO manager.execution_worker_stop_inputs(environment_id,generation,pod_uid,resource_version,spec_digest,epoch) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT DO NOTHING")
            .bind(work.claim.environment_id).bind(work.claim.generation).bind(&binding.pod_uid).bind(&binding.resource_version).bind(&binding.observed_spec_digest).bind(scope.epoch).execute(&mut *tx).await.map_err(db_error)?;
        tx.commit().await.map_err(db_error)
    }
    pub async fn complete_worker_retirement(
        &self,
        scope: &InstallationScope,
        work: &WorkerRetirement,
        uid: Option<&str>,
    ) -> Result<(), Error> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        lock(&mut tx, scope, work).await?;
        if work.intent.is_some() && uid.is_none() {
            return Err(Error::RecoveryRequired);
        }
        if uid.is_none() {
            let dispatched: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM manager.execution_worker_intents WHERE environment_id=$1 AND generation=$2)")
                .bind(work.claim.environment_id).bind(work.claim.generation).fetch_one(&mut *tx).await.map_err(db_error)?;
            if dispatched {
                return Err(Error::RecoveryRequired);
            }
        }
        if work
            .pod_uid
            .as_deref()
            .is_some_and(|known| Some(known) != uid)
        {
            return Err(Error::OwnershipConflict);
        }
        if let Some(uid) = uid {
            let bound: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM manager.execution_worker_start_intents WHERE environment_id=$1 AND generation=$2 AND pod_uid=$3) OR EXISTS(SELECT 1 FROM manager.execution_worker_stop_inputs WHERE environment_id=$1 AND generation=$2 AND pod_uid=$3)")
                .bind(work.claim.environment_id).bind(work.claim.generation).bind(uid).fetch_one(&mut *tx).await.map_err(db_error)?;
            if !bound {
                return Err(Error::OwnershipConflict);
            }
        }
        sqlx::query("INSERT INTO manager.execution_worker_absence(environment_id,generation,pod_uid) VALUES($1,$2,$3) ON CONFLICT DO NOTHING")
            .bind(work.claim.environment_id).bind(work.claim.generation).bind(uid).execute(&mut *tx).await.map_err(db_error)?;
        sqlx::query("UPDATE manager.execution_leases SET state='Expired',terminal_at=clock_timestamp(),fence=fence+1 WHERE environment_id=$1 AND state='Active'")
            .bind(work.claim.environment_id).execute(&mut *tx).await.map_err(db_error)?;
        sqlx::query("UPDATE manager.execution_environments SET state='Retired',lease_owner=NULL,lease_until=NULL WHERE id=$1")
            .bind(work.claim.environment_id).execute(&mut *tx).await.map_err(db_error)?;
        tx.commit().await.map_err(db_error)
    }
}
