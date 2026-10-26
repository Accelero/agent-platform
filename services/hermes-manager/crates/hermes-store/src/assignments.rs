//! Assignments, their volumes and backends, and instance generations.

use std::collections::BTreeMap;

use async_trait::async_trait;
use hermes_core::app::error::StoreError;
use hermes_core::app::ports::{AssignmentRecord, AssignmentTx};
use hermes_core::domain::assignments::{Access, AssignmentSpec, LifetimeMode, TerminalGrant};
use hermes_core::domain::catalog::VersionRef;
use hermes_core::domain::storage::VolumeSpec;
use sqlx::types::Json;
use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;

use crate::{PgTx, store_error};

/// The full assignment record from its three tables.
pub(crate) async fn load(
    tx: &mut Transaction<'static, Postgres>,
    id: Uuid,
    lock: bool,
) -> Result<Option<AssignmentRecord>, StoreError> {
    let query = if lock {
        "SELECT a.*, i.id AS instance_id FROM manager.assignments a \
         JOIN manager.instances i ON i.assignment_id = a.id WHERE a.id = $1 FOR UPDATE OF a"
    } else {
        "SELECT a.*, i.id AS instance_id FROM manager.assignments a \
         JOIN manager.instances i ON i.assignment_id = a.id WHERE a.id = $1"
    };
    let Some(row) = sqlx::query(query)
        .bind(id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(store_error)?
    else {
        return Ok(None);
    };
    let volumes = sqlx::query("SELECT name, source, path, size_gib, read_only FROM manager.assignment_volumes WHERE assignment_id = $1")
        .bind(id)
        .fetch_all(&mut **tx)
        .await
        .map_err(store_error)?
        .into_iter()
        .map(|v| {
            let size: Option<i32> = v.get("size_gib");
            (
                v.get::<String, _>("name"),
                VolumeSpec {
                    source: v.get("source"),
                    path: v.get("path"),
                    size_gib: size.map(|s| s as u32),
                    read_only: v.get("read_only"),
                },
            )
        })
        .collect();
    let allowed: BTreeMap<String, i64> =
        sqlx::query("SELECT backend_id, backend_version FROM manager.assignment_backends WHERE assignment_id = $1")
            .bind(id)
            .fetch_all(&mut **tx)
            .await
            .map_err(store_error)?
            .into_iter()
            .map(|b| (b.get("backend_id"), b.get("backend_version")))
            .collect();
    let decode = |name: &str| {
        row.try_get::<Json<serde_json::Value>, _>(name)
            .map(|Json(v)| v)
    };
    let admin_config = decode("admin_config")
        .ok()
        .and_then(|v| v.as_object().cloned())
        .ok_or(StoreError::Unavailable)?;
    let secrets = decode("secrets")
        .ok()
        .and_then(|v| serde_json::from_value(v).ok())
        .ok_or(StoreError::Unavailable)?;
    let spec = AssignmentSpec {
        access: Access::parse(row.get("access")).ok_or(StoreError::Unavailable)?,
        template: VersionRef {
            id: row.get("template_id"),
            version: row.get("template_version"),
        },
        volumes,
        terminal: TerminalGrant {
            mode: LifetimeMode::parse(row.get("terminal_mode")).ok_or(StoreError::Unavailable)?,
            default: row.get("default_backend"),
            allowed,
        },
        admin_config,
        secrets,
    };
    Ok(Some(AssignmentRecord {
        id,
        identity_id: row.get("identity_id"),
        instance_id: row.get("instance_id"),
        spec,
        version: row.get("version"),
    }))
}

async fn write_children(
    tx: &mut Transaction<'static, Postgres>,
    record: &AssignmentRecord,
) -> Result<(), StoreError> {
    sqlx::query("DELETE FROM manager.assignment_volumes WHERE assignment_id = $1")
        .bind(record.id)
        .execute(&mut **tx)
        .await
        .map_err(store_error)?;
    for (name, volume) in &record.spec.volumes {
        sqlx::query(
            "INSERT INTO manager.assignment_volumes (assignment_id, name, source, path, size_gib, read_only) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(record.id)
        .bind(name)
        .bind(&volume.source)
        .bind(&volume.path)
        .bind(volume.size_gib.map(|s| s as i32))
        .bind(volume.read_only)
        .execute(&mut **tx)
        .await
        .map_err(store_error)?;
    }
    sqlx::query("DELETE FROM manager.assignment_backends WHERE assignment_id = $1")
        .bind(record.id)
        .execute(&mut **tx)
        .await
        .map_err(store_error)?;
    for (backend, version) in &record.spec.terminal.allowed {
        sqlx::query("INSERT INTO manager.assignment_backends (assignment_id, backend_id, backend_version) VALUES ($1, $2, $3)")
            .bind(record.id)
            .bind(backend)
            .bind(version)
            .execute(&mut **tx)
            .await
            .map_err(store_error)?;
    }
    Ok(())
}

#[async_trait]
impl AssignmentTx for PgTx {
    async fn assignment(&mut self, id: Uuid) -> Result<Option<AssignmentRecord>, StoreError> {
        load(&mut self.tx, id, true).await
    }

    async fn assignment_of(&mut self, identity: Uuid) -> Result<Option<Uuid>, StoreError> {
        sqlx::query_scalar("SELECT id FROM manager.assignments WHERE identity_id = $1 FOR UPDATE")
            .bind(identity)
            .fetch_optional(&mut *self.tx)
            .await
            .map_err(store_error)
    }

    async fn insert_assignment(
        &mut self,
        record: &AssignmentRecord,
        namespace: &str,
    ) -> Result<(), StoreError> {
        let spec = &record.spec;
        sqlx::query(
            "INSERT INTO manager.assignments (id, identity_id, access, template_id, template_version, terminal_mode, \
             default_backend, admin_config, secrets, version) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(record.id)
        .bind(record.identity_id)
        .bind(spec.access.as_str())
        .bind(&spec.template.id)
        .bind(spec.template.version)
        .bind(spec.terminal.mode.as_str())
        .bind(&spec.terminal.default)
        .bind(Json(&spec.admin_config))
        .bind(Json(&spec.secrets))
        .bind(record.version)
        .execute(&mut *self.tx)
        .await
        .map_err(store_error)?;
        sqlx::query(
            "INSERT INTO manager.instances (id, assignment_id, namespace, desired_generation, execution_generation) \
             VALUES ($1, $2, $3, 1, 1)",
        )
        .bind(record.instance_id)
        .bind(record.id)
        .bind(namespace)
        .execute(&mut *self.tx)
        .await
        .map_err(store_error)?;
        write_children(&mut self.tx, record).await
    }

    async fn update_assignment(&mut self, record: &AssignmentRecord) -> Result<(), StoreError> {
        let spec = &record.spec;
        sqlx::query(
            "UPDATE manager.assignments SET access = $2, template_id = $3, template_version = $4, terminal_mode = $5, \
             default_backend = $6, admin_config = $7, secrets = $8, version = $9, updated_at = clock_timestamp() \
             WHERE id = $1",
        )
        .bind(record.id)
        .bind(spec.access.as_str())
        .bind(&spec.template.id)
        .bind(spec.template.version)
        .bind(spec.terminal.mode.as_str())
        .bind(&spec.terminal.default)
        .bind(Json(&spec.admin_config))
        .bind(Json(&spec.secrets))
        .bind(record.version)
        .execute(&mut *self.tx)
        .await
        .map_err(store_error)?;
        write_children(&mut self.tx, record).await
    }

    async fn advance_instance(
        &mut self,
        instance: Uuid,
        execution: bool,
    ) -> Result<(), StoreError> {
        let updated = sqlx::query(
            "UPDATE manager.instances SET desired_generation = desired_generation + 1, \
             execution_generation = execution_generation + CASE WHEN $2 THEN 1 ELSE 0 END WHERE id = $1",
        )
        .bind(instance)
        .bind(execution)
        .execute(&mut *self.tx)
        .await
        .map_err(store_error)?;
        if updated.rows_affected() != 1 {
            return Err(StoreError::Unavailable);
        }
        Ok(())
    }

    async fn lock_source_volumes(
        &mut self,
        sources: &[String],
        except: Option<Uuid>,
    ) -> Result<Vec<hermes_core::domain::storage::VolumeSpec>, StoreError> {
        let mut sorted = sources.to_vec();
        sorted.sort();
        sorted.dedup();
        for source in &sorted {
            sqlx::query(
                "SELECT pg_advisory_xact_lock(hashtextextended('manager.volumes:' || $1, 0))",
            )
            .bind(source)
            .execute(&mut *self.tx)
            .await
            .map_err(store_error)?;
        }
        let rows = sqlx::query(
            "SELECT source, path, read_only FROM manager.assignment_volumes \
             WHERE source = ANY($1) AND path IS NOT NULL AND assignment_id IS DISTINCT FROM $2",
        )
        .bind(&sorted)
        .bind(except)
        .fetch_all(&mut *self.tx)
        .await
        .map_err(store_error)?;
        Ok(rows
            .into_iter()
            .map(|row| VolumeSpec {
                source: row.get("source"),
                path: row.get("path"),
                size_gib: None,
                read_only: row.get("read_only"),
            })
            .collect())
    }
}
