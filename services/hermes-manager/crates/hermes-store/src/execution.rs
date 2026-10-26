//! Execution transactions and the controller's environment rows. Every time
//! comparison uses the database clock.

use std::collections::BTreeMap;

use async_trait::async_trait;
use hermes_core::app::controller::WorkerEnvironment;
use hermes_core::app::error::StoreError;
use hermes_core::app::execution::{
    BackendLimits, Client, EnvironmentRow, ExecutionStore, ExecutionTx, LeaseRow, NewEnvironment,
    StoredRequest,
};
use hermes_core::domain::assignments::{LifetimeMode, TerminalGrant};
use hermes_core::domain::backends::BackendDefinition;
use hermes_core::domain::execution::{
    DRAIN_DEADLINE_SECONDS, EnvironmentState, EnvironmentView, Invocation, WorkerFailure,
};
use sqlx::Row;
use sqlx::postgres::PgRow;
use sqlx::types::Json;
use uuid::Uuid;

use crate::{PgStore, PgTx, store_error};

/// `SELECT` of environment rows with their derived, database-time fields.
macro_rules! select_environments {
    () => {
        "SELECT e.id, e.state, e.mode, e.backend_id, e.backend_version, e.execution_generation, e.key, \
         e.pod_uid, e.ssh_host, e.invocation, e.failure, \
         (e.drain_deadline IS NOT NULL AND e.drain_deadline <= clock_timestamp()) AS drain_expired, \
         floor(extract(epoch FROM clock_timestamp() - e.last_used_at))::bigint AS idle_seconds, \
         (SELECT count(*) FROM manager.leases l WHERE l.environment_id = e.id AND l.state = 'active' \
            AND l.expires_at > clock_timestamp()) AS live_leases \
         FROM manager.environments e "
    };
}

fn mode(row: &PgRow) -> Result<LifetimeMode, StoreError> {
    LifetimeMode::parse(row.get("mode")).ok_or(StoreError::Unavailable)
}

fn environment(row: &PgRow) -> Result<EnvironmentRow, StoreError> {
    Ok(EnvironmentRow {
        id: row.get("id"),
        state: EnvironmentState::parse(row.get("state")).ok_or(StoreError::Unavailable)?,
        mode: mode(row)?,
        backend: (row.get("backend_id"), row.get("backend_version")),
        execution_generation: row.get("execution_generation"),
        key: row.get("key"),
        pod_uid: row.get("pod_uid"),
        ssh_host: row.get("ssh_host"),
        drain_expired: row.get("drain_expired"),
        live_leases: row.get("live_leases"),
        failure: row
            .get::<Option<String>, _>("failure")
            .map(|value| WorkerFailure::parse(&value).ok_or(StoreError::Unavailable))
            .transpose()?,
    })
}

#[async_trait]
impl ExecutionTx for PgTx {
    async fn authenticate(
        &mut self,
        credential_digest: &str,
    ) -> Result<Option<Client>, StoreError> {
        let Some(row) = sqlx::query(
            "SELECT i.id, i.execution_generation, a.id AS assignment_id, a.terminal_mode, a.default_backend \
             FROM manager.materials m JOIN manager.instances i ON i.id = m.instance_id \
             JOIN manager.assignments a ON a.id = i.assignment_id \
             WHERE m.purpose = 'execution-credential' AND m.public = $1 AND a.access = 'enabled' \
             FOR UPDATE OF i",
        )
        .bind(credential_digest)
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(store_error)?
        else {
            return Ok(None);
        };
        let backends = sqlx::query(
            "SELECT b.backend_id, b.backend_version, c.definition FROM manager.assignment_backends b \
             JOIN manager.catalog_versions c ON c.kind = 'terminal_backend' AND c.id = b.backend_id \
               AND c.version = b.backend_version WHERE b.assignment_id = $1",
        )
        .bind(row.get::<Uuid, _>("assignment_id"))
        .fetch_all(&mut *self.tx)
        .await
        .map_err(store_error)?;
        let mut allowed = BTreeMap::new();
        let mut limits = BTreeMap::new();
        for backend in backends {
            let id: String = backend.get("backend_id");
            let Json(definition) = backend
                .try_get::<Json<BackendDefinition>, _>("definition")
                .map_err(|_| StoreError::Unavailable)?;
            allowed.insert(id.clone(), backend.get::<i64, _>("backend_version"));
            limits.insert(
                id,
                BackendLimits {
                    max_lease_seconds: definition.limits.max_lease_seconds,
                    max_environments: definition.limits.max_environments,
                },
            );
        }
        Ok(Some(Client {
            instance_id: row.get("id"),
            execution_generation: row.get("execution_generation"),
            grant: TerminalGrant {
                mode: LifetimeMode::parse(row.get("terminal_mode"))
                    .ok_or(StoreError::Unavailable)?,
                default: row.get("default_backend"),
                allowed,
            },
            limits,
        }))
    }

    async fn expire_due(&mut self, instance: Uuid) -> Result<(), StoreError> {
        sqlx::query(
            "UPDATE manager.leases SET state = 'expired', ended_at = clock_timestamp() \
             WHERE instance_id = $1 AND state = 'active' AND expires_at <= clock_timestamp()",
        )
        .bind(instance)
        .execute(&mut *self.tx)
        .await
        .map_err(store_error)?;
        Ok(())
    }

    async fn request(
        &mut self,
        instance: Uuid,
        request: &str,
    ) -> Result<Option<StoredRequest>, StoreError> {
        let row = sqlx::query(
            "SELECT body_digest, environment_id, lease_id FROM manager.acquire_requests \
             WHERE instance_id = $1 AND request = $2",
        )
        .bind(instance)
        .bind(request)
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(store_error)?;
        Ok(row.map(|row| StoredRequest {
            digest: row.get("body_digest"),
            environment: row.get("environment_id"),
            lease: row.get("lease_id"),
        }))
    }

    async fn put_request(
        &mut self,
        instance: Uuid,
        request: &str,
        digest: &[u8],
        environment: Uuid,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO manager.acquire_requests (instance_id, request, body_digest, environment_id) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(instance)
        .bind(request)
        .bind(digest)
        .bind(environment)
        .execute(&mut *self.tx)
        .await
        .map_err(store_error)?;
        Ok(())
    }

    async fn grant_request(
        &mut self,
        instance: Uuid,
        request: &str,
        lease: Uuid,
    ) -> Result<(), StoreError> {
        sqlx::query("UPDATE manager.acquire_requests SET lease_id = $3 WHERE instance_id = $1 AND request = $2")
            .bind(instance)
            .bind(request)
            .bind(lease)
            .execute(&mut *self.tx)
            .await
            .map_err(store_error)?;
        Ok(())
    }

    async fn environment(
        &mut self,
        instance: Uuid,
        id: Uuid,
    ) -> Result<Option<EnvironmentRow>, StoreError> {
        sqlx::query(concat!(
            select_environments!(),
            "WHERE e.instance_id = $1 AND e.id = $2"
        ))
        .bind(instance)
        .bind(id)
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(store_error)?
        .as_ref()
        .map(environment)
        .transpose()
    }

    async fn live_environment(
        &mut self,
        instance: Uuid,
        backend: &(String, i64),
        execution_generation: i64,
        mode: LifetimeMode,
        key: &str,
    ) -> Result<Option<EnvironmentRow>, StoreError> {
        sqlx::query(concat!(
            select_environments!(),
            "WHERE e.instance_id = $1 AND e.backend_id = $2 AND e.backend_version = $3 \
             AND e.execution_generation = $4 AND e.mode = $5 AND e.key = $6 AND e.state <> 'retired' FOR UPDATE"
        ))
            .bind(instance)
            .bind(&backend.0)
            .bind(backend.1)
            .bind(execution_generation)
            .bind(mode.as_str())
            .bind(key)
            .fetch_optional(&mut *self.tx)
            .await
            .map_err(store_error)?
            .as_ref()
            .map(environment)
            .transpose()
    }

    async fn count_environments(
        &mut self,
        instance: Uuid,
        backend: &str,
    ) -> Result<(u32, u32), StoreError> {
        let row = sqlx::query(
            "SELECT count(*) FILTER (WHERE backend_id = $2) AS same, count(*) AS total \
             FROM manager.environments WHERE instance_id = $1 AND state <> 'retired'",
        )
        .bind(instance)
        .bind(backend)
        .fetch_one(&mut *self.tx)
        .await
        .map_err(store_error)?;
        let count = |name: &str| {
            u32::try_from(row.get::<i64, _>(name)).map_err(|_| StoreError::Unavailable)
        };
        Ok((count("same")?, count("total")?))
    }

    async fn insert_environment(&mut self, environment: &NewEnvironment) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO manager.environments \
             (id, instance_id, backend_id, backend_version, execution_generation, mode, key, invocation, state) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'pending')",
        )
        .bind(environment.id)
        .bind(environment.instance_id)
        .bind(&environment.backend.0)
        .bind(environment.backend.1)
        .bind(environment.execution_generation)
        .bind(environment.mode.as_str())
        .bind(&environment.key)
        .bind(environment.invocation.as_ref().map(Json))
        .execute(&mut *self.tx)
        .await
        .map_err(store_error)?;
        Ok(())
    }

    async fn touch_environment(&mut self, id: Uuid) -> Result<(), StoreError> {
        sqlx::query("UPDATE manager.environments SET last_used_at = clock_timestamp() WHERE id = $1 AND state <> 'retired'")
            .bind(id)
            .execute(&mut *self.tx)
            .await
            .map_err(store_error)?;
        Ok(())
    }

    async fn drain_session(
        &mut self,
        instance: Uuid,
        backend: &str,
        key: &str,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "UPDATE manager.environments SET state = 'draining', \
             drain_deadline = COALESCE(drain_deadline, clock_timestamp() + make_interval(secs => $4)) \
             WHERE instance_id = $1 AND backend_id = $2 AND key = $3 AND mode = 'per_session' \
             AND state IN ('pending', 'starting', 'ready')",
        )
        .bind(instance)
        .bind(backend)
        .bind(key)
        .bind(f64::from(DRAIN_DEADLINE_SECONDS))
        .execute(&mut *self.tx)
        .await
        .map_err(store_error)?;
        Ok(())
    }

    async fn lease(&mut self, instance: Uuid, id: Uuid) -> Result<Option<LeaseRow>, StoreError> {
        let row = sqlx::query(
            "SELECT id, environment_id, state = 'active' AS active, \
             ceil(extract(epoch FROM expires_at - clock_timestamp()))::bigint AS remaining \
             FROM manager.leases WHERE id = $1 AND instance_id = $2",
        )
        .bind(id)
        .bind(instance)
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(store_error)?;
        Ok(row.map(|row| LeaseRow {
            id: row.get("id"),
            environment: row.get("environment_id"),
            active: row.get("active"),
            remaining: row.get("remaining"),
        }))
    }

    async fn insert_lease(
        &mut self,
        id: Uuid,
        instance: Uuid,
        environment: Uuid,
        parent: Option<Uuid>,
        ttl: u32,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO manager.leases (id, instance_id, environment_id, parent, state, expires_at) \
             VALUES ($1, $2, $3, $4, 'active', clock_timestamp() + make_interval(secs => $5))",
        )
        .bind(id)
        .bind(instance)
        .bind(environment)
        .bind(parent)
        .bind(f64::from(ttl))
        .execute(&mut *self.tx)
        .await
        .map_err(store_error)?;
        Ok(())
    }

    async fn extend_lease(
        &mut self,
        id: Uuid,
        ttl: u32,
        max_seconds: u32,
    ) -> Result<Option<i64>, StoreError> {
        sqlx::query_scalar(
            "UPDATE manager.leases SET expires_at = GREATEST(expires_at, \
               LEAST(clock_timestamp() + make_interval(secs => $2), created_at + make_interval(secs => $3))) \
             WHERE id = $1 AND state = 'active' AND expires_at > clock_timestamp() \
             RETURNING ceil(extract(epoch FROM expires_at - clock_timestamp()))::bigint",
        )
        .bind(id)
        .bind(f64::from(ttl))
        .bind(f64::from(max_seconds))
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(store_error)
    }

    async fn release_lease(&mut self, id: Uuid) -> Result<(), StoreError> {
        sqlx::query("UPDATE manager.leases SET state = 'released', ended_at = clock_timestamp() WHERE id = $1 AND state = 'active'")
            .bind(id)
            .execute(&mut *self.tx)
            .await
            .map_err(store_error)?;
        Ok(())
    }

    async fn environments(&mut self, instance: Uuid) -> Result<Vec<EnvironmentRow>, StoreError> {
        sqlx::query(concat!(
            select_environments!(),
            "WHERE e.instance_id = $1 ORDER BY e.created_at, e.id LIMIT 64"
        ))
        .bind(instance)
        .fetch_all(&mut *self.tx)
        .await
        .map_err(store_error)?
        .iter()
        .map(environment)
        .collect()
    }

    async fn worker_host_key(&mut self, instance: Uuid) -> Result<Option<String>, StoreError> {
        let key: Option<Option<String>> = sqlx::query_scalar(
            "SELECT public FROM manager.materials WHERE instance_id = $1 AND purpose = 'worker-host'",
        )
        .bind(instance)
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(store_error)?;
        Ok(key.flatten())
    }

    async fn notify(&mut self, instance: Uuid) -> Result<(), StoreError> {
        sqlx::query("SELECT pg_notify('manager_reconcile', $1)")
            .bind(instance.to_string())
            .execute(&mut *self.tx)
            .await
            .map_err(store_error)?;
        Ok(())
    }

    async fn commit(self) -> Result<(), StoreError> {
        self.tx.commit().await.map_err(store_error)
    }
}

#[async_trait]
impl ExecutionStore for PgStore {
    type Tx = PgTx;

    async fn begin_execution(&self) -> Result<PgTx, StoreError> {
        let tx = self.pool.begin().await.map_err(store_error)?;
        Ok(PgTx { tx })
    }

    async fn expire_leases(&self) -> Result<u64, StoreError> {
        Ok(sqlx::query(
            "UPDATE manager.leases SET state = 'expired', ended_at = clock_timestamp() \
             WHERE state = 'active' AND expires_at <= clock_timestamp()",
        )
        .execute(&self.pool)
        .await
        .map_err(store_error)?
        .rows_affected())
    }
}

impl PgStore {
    pub(crate) async fn worker_environment_rows(
        &self,
        instance: Uuid,
    ) -> Result<Vec<WorkerEnvironment>, StoreError> {
        let rows = sqlx::query(concat!(
            select_environments!(),
            "WHERE e.instance_id = $1 AND e.state <> 'retired' ORDER BY e.created_at, e.id"
        ))
        .bind(instance)
        .fetch_all(&self.pool)
        .await
        .map_err(store_error)?;
        rows.iter()
            .map(|row| {
                let base = environment(row)?;
                let invocation = row
                    .try_get::<Option<Json<Invocation>>, _>("invocation")
                    .map_err(|_| StoreError::Unavailable)?
                    .map(|Json(i)| i);
                Ok(WorkerEnvironment {
                    id: base.id,
                    view: EnvironmentView {
                        state: base.state,
                        mode: base.mode,
                        backend: base.backend,
                        execution_generation: base.execution_generation,
                        pod_uid: base.pod_uid,
                        live_leases: u32::try_from(base.live_leases).unwrap_or(u32::MAX),
                        idle_seconds: u64::try_from(row.get::<i64, _>("idle_seconds")).unwrap_or(0),
                        drain_expired: base.drain_expired,
                    },
                    invocation,
                })
            })
            .collect()
    }

    pub(crate) async fn move_environment(
        &self,
        id: Uuid,
        from: EnvironmentState,
        to: EnvironmentState,
        detail: Option<&str>,
    ) -> Result<bool, StoreError> {
        if !from.allows(to) {
            return Ok(false);
        }
        let moved = sqlx::query(
            "UPDATE manager.environments SET state = $3, \
             pod_uid = CASE WHEN $3 = 'starting' THEN $4 ELSE pod_uid END, \
             ssh_host = CASE WHEN $3 = 'ready' THEN $4 ELSE ssh_host END, \
             drain_deadline = CASE WHEN $3 = 'draining' \
               THEN COALESCE(drain_deadline, clock_timestamp() + make_interval(secs => $5)) ELSE drain_deadline END \
             WHERE id = $1 AND state = $2",
        )
        .bind(id)
        .bind(from.as_str())
        .bind(to.as_str())
        .bind(detail)
        .bind(f64::from(DRAIN_DEADLINE_SECONDS))
        .execute(&self.pool)
        .await
        .map_err(store_error)?
        .rows_affected();
        Ok(moved == 1)
    }

    pub(crate) async fn fail(
        &self,
        id: Uuid,
        from: EnvironmentState,
        failure: WorkerFailure,
    ) -> Result<bool, StoreError> {
        if !matches!(from, EnvironmentState::Pending | EnvironmentState::Starting) {
            return Ok(false);
        }
        let moved = sqlx::query(
            "UPDATE manager.environments SET state = 'draining', failure = $3, \
             drain_deadline = COALESCE(drain_deadline, clock_timestamp() + make_interval(secs => $4)) \
             WHERE id = $1 AND state = $2",
        )
        .bind(id)
        .bind(from.as_str())
        .bind(failure.as_str())
        .bind(f64::from(DRAIN_DEADLINE_SECONDS))
        .execute(&self.pool)
        .await
        .map_err(store_error)?
        .rows_affected();
        Ok(moved == 1)
    }
}
