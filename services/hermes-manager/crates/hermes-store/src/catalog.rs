//! Catalog versions of templates and terminal backends.

use async_trait::async_trait;
use hermes_core::app::catalog::Definition;
use hermes_core::app::error::StoreError;
use hermes_core::app::ports::{CatalogEntry, CatalogTx};
use hermes_core::domain::catalog::{CatalogKind, VersionState, VersionStatus};
use sqlx::Row;
use sqlx::types::Json;

use crate::{PgTx, store_error};

#[async_trait]
impl CatalogTx for PgTx {
    async fn catalog_version<D: Definition>(
        &mut self,
        id: &str,
        version: i64,
    ) -> Result<Option<CatalogEntry<D>>, StoreError> {
        let row = sqlx::query(
            "SELECT state, revision, definition FROM manager.catalog_versions \
             WHERE kind = $1 AND id = $2 AND version = $3 FOR UPDATE",
        )
        .bind(D::KIND.as_str())
        .bind(id)
        .bind(version)
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(store_error)?;
        let Some(row) = row else { return Ok(None) };
        let state = VersionState::parse(row.get("state")).ok_or(StoreError::Unavailable)?;
        let Json(definition) = row
            .try_get::<Json<D>, _>("definition")
            .map_err(|_| StoreError::Unavailable)?;
        Ok(Some(CatalogEntry {
            status: VersionStatus {
                state,
                revision: row.get("revision"),
            },
            definition,
        }))
    }

    async fn last_catalog_version(
        &mut self,
        kind: CatalogKind,
        id: &str,
    ) -> Result<i64, StoreError> {
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('manager.catalog:' || $1 || ':' || $2, 0))")
            .bind(kind.as_str())
            .bind(id)
            .execute(&mut *self.tx)
            .await
            .map_err(store_error)?;
        sqlx::query_scalar("SELECT coalesce(max(version), 0) FROM manager.catalog_versions WHERE kind = $1 AND id = $2")
            .bind(kind.as_str())
            .bind(id)
            .fetch_one(&mut *self.tx)
            .await
            .map_err(store_error)
    }

    async fn put_catalog_version<D: Definition>(
        &mut self,
        id: &str,
        version: i64,
        entry: &CatalogEntry<D>,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO manager.catalog_versions (kind, id, version, state, revision, definition) \
             VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT (kind, id, version) DO UPDATE SET \
             state = excluded.state, revision = excluded.revision, definition = excluded.definition, \
             updated_at = clock_timestamp()",
        )
        .bind(D::KIND.as_str())
        .bind(id)
        .bind(version)
        .bind(entry.status.state.as_str())
        .bind(entry.status.revision)
        .bind(Json(&entry.definition))
        .execute(&mut *self.tx)
        .await
        .map_err(store_error)?;
        Ok(())
    }
}
