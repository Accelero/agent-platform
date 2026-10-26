//! Identities and administrator grants.

use async_trait::async_trait;
use hermes_core::app::error::StoreError;
use hermes_core::app::ports::{AccessReader, AccessTx, IdentityRecord};
use hermes_core::domain::access::Grant;
use hermes_core::domain::identity::Identity;
use sqlx::Row;
use uuid::Uuid;

use crate::{PgStore, PgTx, store_error};

#[async_trait]
impl AccessTx for PgTx {
    async fn identity(
        &mut self,
        identity: &Identity,
    ) -> Result<Option<IdentityRecord>, StoreError> {
        let row = sqlx::query(
            "SELECT id, verified_at IS NOT NULL AS verified FROM manager.identities \
             WHERE issuer = $1 AND subject = $2",
        )
        .bind(&identity.issuer)
        .bind(&identity.subject)
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(store_error)?;
        Ok(row.map(|row| IdentityRecord {
            id: row.get("id"),
            verified: row.get("verified"),
        }))
    }

    async fn ensure_identity(&mut self, identity: &Identity) -> Result<IdentityRecord, StoreError> {
        sqlx::query(
            "INSERT INTO manager.identities (id, issuer, subject) VALUES ($1, $2, $3) \
             ON CONFLICT (issuer, subject) DO NOTHING",
        )
        .bind(Uuid::new_v4())
        .bind(&identity.issuer)
        .bind(&identity.subject)
        .execute(&mut *self.tx)
        .await
        .map_err(store_error)?;
        self.identity(identity)
            .await?
            .ok_or(StoreError::Unavailable)
    }

    async fn grant(&mut self, identity: Uuid) -> Result<Option<Grant>, StoreError> {
        let row = sqlx::query(
            "SELECT active, can_manage_administrators, version FROM manager.administrator_grants \
             WHERE identity_id = $1 FOR SHARE",
        )
        .bind(identity)
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(store_error)?;
        Ok(row.map(|row| Grant {
            active: row.get("active"),
            can_manage_administrators: row.get("can_manage_administrators"),
            version: row.get("version"),
        }))
    }

    async fn grant_count(&mut self) -> Result<usize, StoreError> {
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM manager.administrator_grants")
            .fetch_one(&mut *self.tx)
            .await
            .map_err(store_error)?;
        usize::try_from(count).map_err(|_| StoreError::Unavailable)
    }

    async fn lock_active_managers(&mut self) -> Result<usize, StoreError> {
        let rows = sqlx::query(
            "SELECT identity_id FROM manager.administrator_grants \
             WHERE active AND can_manage_administrators ORDER BY identity_id FOR UPDATE",
        )
        .fetch_all(&mut *self.tx)
        .await
        .map_err(store_error)?;
        Ok(rows.len())
    }

    async fn put_grant(&mut self, identity: Uuid, grant: Grant) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO manager.administrator_grants \
             (identity_id, active, can_manage_administrators, version) VALUES ($1, $2, $3, $4) \
             ON CONFLICT (identity_id) DO UPDATE SET active = excluded.active, \
             can_manage_administrators = excluded.can_manage_administrators, \
             version = excluded.version, updated_at = clock_timestamp()",
        )
        .bind(identity)
        .bind(grant.active)
        .bind(grant.can_manage_administrators)
        .bind(grant.version)
        .execute(&mut *self.tx)
        .await
        .map_err(store_error)?;
        Ok(())
    }
}

#[async_trait]
impl AccessReader for PgStore {
    async fn grant_of(&self, identity: &Identity) -> Result<Option<Grant>, StoreError> {
        let row = sqlx::query(
            "SELECT g.active, g.can_manage_administrators, g.version FROM manager.administrator_grants g \
             JOIN manager.identities i ON i.id = g.identity_id WHERE i.issuer = $1 AND i.subject = $2",
        )
        .bind(&identity.issuer)
        .bind(&identity.subject)
        .fetch_optional(&self.pool)
        .await
        .map_err(store_error)?;
        Ok(row.map(|row| Grant {
            active: row.get("active"),
            can_manage_administrators: row.get("can_manage_administrators"),
            version: row.get("version"),
        }))
    }

    async fn record_login(&self, identity: &Identity) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO manager.identities (id, issuer, subject, verified_at) \
             VALUES ($1, $2, $3, clock_timestamp()) \
             ON CONFLICT (issuer, subject) DO UPDATE SET verified_at = clock_timestamp() \
             WHERE manager.identities.verified_at IS NULL \
             OR manager.identities.verified_at < clock_timestamp() - interval '5 minutes'",
        )
        .bind(Uuid::new_v4())
        .bind(&identity.issuer)
        .bind(&identity.subject)
        .execute(&self.pool)
        .await
        .map_err(store_error)?;
        Ok(())
    }
}
