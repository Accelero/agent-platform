//! PostgreSQL implementation of the application's store ports.
//!
//! Every port call runs inside one short transaction ([`PgTx`]). Rules live in
//! `hermes-core`; SQL here loads and saves rows, takes the row locks the ports
//! promise and maps database failures to [`StoreError`] without exposing SQL
//! text, URLs or row contents.

mod access;
mod assignments;
mod catalog;
mod controller;
mod execution;
mod kernel;
mod queries;
mod routing;
mod secrets;
#[cfg(test)]
mod testing;

use std::str::FromStr;
use std::time::Duration;

use async_trait::async_trait;
use hermes_core::app::error::{AppError, StoreError};
use hermes_core::app::ports::Store;
use hermes_core::domain::installation::Installation;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{PgPool, Postgres, Row, Transaction};

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// Session settings for every pooled connection. Statements and lock waits are
/// bounded; an idle transaction is cut off rather than left holding locks.
const SESSION: &str = "SET statement_timeout = '10s'; SET lock_timeout = '3s'; \
    SET idle_in_transaction_session_timeout = '10s'; SET synchronous_commit = on;";

const DURABLE: &str = "SELECT current_setting('fsync') = 'on' \
    AND current_setting('full_page_writes') = 'on' \
    AND current_setting('synchronous_commit') = 'on'";

#[derive(Clone)]
pub struct PgStore {
    pool: PgPool,
}

/// One store transaction. Dropping it without [`commit`](hermes_core::app::ports::KernelTx::commit) rolls back.
pub struct PgTx {
    tx: Transaction<'static, Postgres>,
}

impl PgStore {
    /// Connects with bounded timeouts and refuses servers without durable
    /// commit settings. The URL is never logged or included in errors.
    pub async fn connect(database_url: &str) -> Result<Self, StoreError> {
        let options =
            PgConnectOptions::from_str(database_url).map_err(|_| StoreError::Unavailable)?;
        Self::connect_with(options).await
    }

    pub async fn connect_with(options: PgConnectOptions) -> Result<Self, StoreError> {
        let pool = PgPoolOptions::new()
            .max_connections(16)
            .acquire_timeout(Duration::from_secs(5))
            .after_connect(|connection, _| {
                Box::pin(async move {
                    sqlx::raw_sql(SESSION).execute(&mut *connection).await?;
                    let durable: bool = sqlx::query_scalar(DURABLE).fetch_one(connection).await?;
                    if !durable {
                        return Err(sqlx::Error::Protocol(
                            "manager requires durable PostgreSQL settings".into(),
                        ));
                    }
                    Ok(())
                })
            })
            .connect_with(options)
            .await
            .map_err(store_error)?;
        Ok(Self { pool })
    }

    #[cfg(test)]
    pub(crate) fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Explicit operator step: applies pending migrations.
    pub async fn migrate(&self) -> Result<(), StoreError> {
        MIGRATOR
            .run(&self.pool)
            .await
            .map_err(|_| StoreError::Unavailable)
    }

    /// Fresh installation only: records the installation in an empty registry.
    pub async fn initialize(
        &self,
        installation: &Installation,
        cluster_uid: &str,
    ) -> Result<(), AppError> {
        installation.validate()?;
        let mut tx = self.pool.begin().await.map_err(store_error)?;
        sqlx::query("LOCK TABLE manager.installation IN EXCLUSIVE MODE")
            .execute(&mut *tx)
            .await
            .map_err(store_error)?;
        let populated: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM manager.installation) \
             OR EXISTS (SELECT 1 FROM manager.identities)",
        )
        .fetch_one(&mut *tx)
        .await
        .map_err(store_error)?;
        if populated {
            return Err(AppError::registry_mismatch());
        }
        sqlx::query(
            "INSERT INTO manager.installation (id, issuer, cluster_uid) VALUES ($1, $2, $3)",
        )
        .bind(installation.id)
        .bind(&installation.issuer)
        .bind(cluster_uid)
        .execute(&mut *tx)
        .await
        .map_err(store_error)?;
        tx.commit().await.map_err(store_error)?;
        Ok(())
    }

    /// Startup check: the schema is exactly this build's migrations, every
    /// manager table is logged, and the registry belongs to `installation`.
    pub async fn check(&self, installation: &Installation) -> Result<(), AppError> {
        let applied =
            sqlx::query("SELECT version, checksum, success FROM _sqlx_migrations ORDER BY version")
                .fetch_all(&self.pool)
                .await
                .map_err(|_| AppError::registry_mismatch())?;
        let expected: Vec<_> = MIGRATOR.iter().collect();
        let current = applied.len() == expected.len()
            && applied.iter().zip(&expected).all(|(row, migration)| {
                row.get::<i64, _>("version") == migration.version
                    && row.get::<bool, _>("success")
                    && row.get::<Vec<u8>, _>("checksum") == migration.checksum.as_ref()
            });
        if !current {
            return Err(AppError::registry_mismatch());
        }
        let unlogged: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname IN ('manager', 'admin_sessions', 'gateway_sessions') \
             AND c.relkind = 'r' AND c.relpersistence <> 'p')",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(store_error)?;
        if unlogged {
            return Err(AppError::registry_mismatch());
        }
        let row = sqlx::query("SELECT id, issuer FROM manager.installation")
            .fetch_optional(&self.pool)
            .await
            .map_err(store_error)?
            .ok_or_else(AppError::registry_mismatch)?;
        let registered = Installation {
            id: row.get("id"),
            issuer: row.get("issuer"),
        };
        if &registered != installation {
            return Err(AppError::registry_mismatch());
        }
        Ok(())
    }

    /// The cluster this registry was installed on (the `kube-system` UID).
    pub async fn cluster_uid(&self) -> Result<String, StoreError> {
        sqlx::query_scalar("SELECT cluster_uid FROM manager.installation")
            .fetch_one(&self.pool)
            .await
            .map_err(store_error)
    }

    /// Operator step after restoring onto a new cluster: records its identity
    /// and clears every namespace binding, so the controller recreates the
    /// namespaces against the restored data; ends every lease, retires every
    /// terminal environment and clears the orphan report. The caller has
    /// verified that the new cluster holds no namespace of this installation.
    pub async fn rebind_cluster(&self, cluster_uid: &str) -> Result<u64, StoreError> {
        let mut tx = self.pool.begin().await.map_err(store_error)?;
        sqlx::query("SELECT set_config('manager.rebind', 'on', true)")
            .execute(&mut *tx)
            .await
            .map_err(store_error)?;
        sqlx::query("UPDATE manager.installation SET cluster_uid = $1")
            .bind(cluster_uid)
            .execute(&mut *tx)
            .await
            .map_err(store_error)?;
        let cleared = sqlx::query(
            "UPDATE manager.instances SET namespace_uid = NULL, observed_generation = 0, \
             phase = 'provisioning', blocked_reason = NULL, conditions = '[]' WHERE namespace_uid IS NOT NULL",
        )
        .execute(&mut *tx)
        .await
        .map_err(store_error)?
        .rows_affected();
        // Workers and leases of the old cluster are gone with it: end every
        // lease and retire every environment, so acquisitions start afresh.
        sqlx::query(
            "UPDATE manager.leases SET state = 'expired', ended_at = clock_timestamp() \
             WHERE state = 'active'",
        )
        .execute(&mut *tx)
        .await
        .map_err(store_error)?;
        sqlx::query(
            "UPDATE manager.environments SET state = 'draining', \
             drain_deadline = COALESCE(drain_deadline, clock_timestamp()) \
             WHERE state IN ('pending', 'starting', 'ready')",
        )
        .execute(&mut *tx)
        .await
        .map_err(store_error)?;
        sqlx::query("UPDATE manager.environments SET state = 'retired' WHERE state = 'draining'")
            .execute(&mut *tx)
            .await
            .map_err(store_error)?;
        // The old cluster's orphan report says nothing about the new one.
        sqlx::query("DELETE FROM manager.orphans")
            .execute(&mut *tx)
            .await
            .map_err(store_error)?;
        sqlx::query("DELETE FROM manager.orphan_scan")
            .execute(&mut *tx)
            .await
            .map_err(store_error)?;
        tx.commit().await.map_err(store_error)?;
        Ok(cleared)
    }

    /// Instance IDs from `NOTIFY manager_reconcile`, on a dedicated connection.
    pub async fn notifications(&self) -> Result<Notifications, StoreError> {
        let mut listener = sqlx::postgres::PgListener::connect_with(&self.pool)
            .await
            .map_err(store_error)?;
        listener
            .listen("manager_reconcile")
            .await
            .map_err(store_error)?;
        Ok(Notifications { listener })
    }
}

/// A stream of instances to reconcile. A lost connection is an error; the
/// caller resyncs everything after reconnecting.
pub struct Notifications {
    listener: sqlx::postgres::PgListener,
}

impl Notifications {
    pub async fn next(&mut self) -> Result<Option<uuid::Uuid>, StoreError> {
        let notification = self.listener.recv().await.map_err(store_error)?;
        Ok(uuid::Uuid::parse_str(notification.payload()).ok())
    }
}

#[async_trait]
impl Store for PgStore {
    type Tx = PgTx;

    async fn begin(&self) -> Result<PgTx, StoreError> {
        let tx = self.pool.begin().await.map_err(store_error)?;
        Ok(PgTx { tx })
    }
}

/// Retryable concurrency failures become [`StoreError::Conflict`]; everything
/// else, including guard-trigger refusals, is [`StoreError::Unavailable`].
pub(crate) fn store_error(error: sqlx::Error) -> StoreError {
    let code = error
        .as_database_error()
        .and_then(|e| e.code())
        .map(|code| code.into_owned());
    match code.as_deref() {
        // serialization_failure, deadlock_detected, unique_violation
        Some("40001" | "40P01" | "23505") => StoreError::Conflict,
        _ => StoreError::Unavailable,
    }
}
