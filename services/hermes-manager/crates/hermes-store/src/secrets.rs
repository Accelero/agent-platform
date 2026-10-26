//! Sealed admin secret versions and their references.

use async_trait::async_trait;
use hermes_core::app::error::StoreError;
use hermes_core::app::ports::{SecretRecord, SecretTx};
use hermes_core::domain::secrets::SecretKind;
use sqlx::Row;
use uuid::Uuid;

use crate::{PgTx, store_error};

#[async_trait]
impl SecretTx for PgTx {
    async fn secret(&mut self, name: &str) -> Result<Option<SecretRecord>, StoreError> {
        let row = sqlx::query(
            "SELECT kind, version FROM manager.admin_secrets WHERE name = $1 ORDER BY version DESC LIMIT 1 FOR UPDATE",
        )
        .bind(name)
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(store_error)?;
        row.map(|row| {
            Ok(SecretRecord {
                kind: SecretKind::parse(row.get("kind")).ok_or(StoreError::Unavailable)?,
                version: row.get("version"),
            })
        })
        .transpose()
    }

    async fn put_secret_version(
        &mut self,
        name: &str,
        record: SecretRecord,
        sealed: &[u8],
    ) -> Result<(), StoreError> {
        sqlx::query("INSERT INTO manager.admin_secrets (name, version, kind, sealed) VALUES ($1, $2, $3, $4)")
            .bind(name)
            .bind(record.version)
            .bind(record.kind.as_str())
            .bind(sealed)
            .execute(&mut *self.tx)
            .await
            .map_err(store_error)?;
        Ok(())
    }

    async fn delete_secret(&mut self, name: &str) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM manager.admin_secrets WHERE name = $1")
            .bind(name)
            .execute(&mut *self.tx)
            .await
            .map_err(store_error)?;
        Ok(())
    }

    async fn secret_referenced(&mut self, name: &str) -> Result<bool, StoreError> {
        sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM manager.catalog_versions c WHERE \
                (c.kind = 'template' AND c.definition -> 'secrets' @> jsonb_build_array(jsonb_build_object('name', $1::text))) \
                OR (c.kind = 'terminal_backend' AND c.definition ->> 'registrySecret' = $1)) \
             OR EXISTS (SELECT 1 FROM manager.assignments a \
                WHERE a.secrets @> jsonb_build_array(jsonb_build_object('name', $1::text)))",
        )
        .bind(name)
        .fetch_one(&mut *self.tx)
        .await
        .map_err(store_error)
    }

    async fn instances_using_secret(&mut self, name: &str) -> Result<Vec<Uuid>, StoreError> {
        sqlx::query_scalar(
            "SELECT i.id FROM manager.instances i JOIN manager.assignments a ON a.id = i.assignment_id WHERE \
                a.secrets @> jsonb_build_array(jsonb_build_object('name', $1::text)) \
                OR EXISTS (SELECT 1 FROM manager.catalog_versions c WHERE c.kind = 'template' \
                    AND c.id = a.template_id AND c.version = a.template_version \
                    AND c.definition -> 'secrets' @> jsonb_build_array(jsonb_build_object('name', $1::text))) \
                OR EXISTS (SELECT 1 FROM manager.assignment_backends b JOIN manager.catalog_versions c \
                    ON c.kind = 'terminal_backend' AND c.id = b.backend_id AND c.version = b.backend_version \
                    WHERE b.assignment_id = a.id AND c.definition ->> 'registrySecret' = $1) \
             ORDER BY i.id",
        )
        .bind(name)
        .fetch_all(&mut *self.tx)
        .await
        .map_err(store_error)
    }
}
