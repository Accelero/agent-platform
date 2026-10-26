//! Installation, idempotency receipts, audit and notifications.

use async_trait::async_trait;
use hermes_core::app::actor::Actor;
use hermes_core::app::error::StoreError;
use hermes_core::app::ports::{AuditEvent, KernelTx, Reservation};
use hermes_core::domain::installation::Installation;
use sqlx::Row;
use sqlx::types::Json;
use uuid::Uuid;

use crate::{PgTx, store_error};

/// Receipt key columns: the operator has no identity.
fn actor_columns(actor: &Actor) -> (&'static str, &str, &str) {
    match actor {
        Actor::Operator => ("operator", "", ""),
        Actor::Administrator(identity) => ("administrator", &identity.issuer, &identity.subject),
    }
}

#[async_trait]
impl KernelTx for PgTx {
    async fn installation(&mut self) -> Result<Option<Installation>, StoreError> {
        let row = sqlx::query("SELECT id, issuer FROM manager.installation FOR SHARE")
            .fetch_optional(&mut *self.tx)
            .await
            .map_err(store_error)?;
        Ok(row.map(|row| Installation {
            id: row.get("id"),
            issuer: row.get("issuer"),
        }))
    }

    async fn reserve_receipt(
        &mut self,
        actor: &Actor,
        action: &'static str,
        key: &str,
        digest: [u8; 32],
    ) -> Result<Reservation, StoreError> {
        let (kind, issuer, subject) = actor_columns(actor);
        // A concurrent reservation of the same key blocks here until the other
        // transaction ends; then this one sees its committed receipt or nothing.
        sqlx::query(
            "INSERT INTO manager.command_receipts \
             (actor_kind, actor_issuer, actor_subject, action, idempotency_key, body_digest) \
             VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT DO NOTHING",
        )
        .bind(kind)
        .bind(issuer)
        .bind(subject)
        .bind(action)
        .bind(key)
        .bind(&digest[..])
        .execute(&mut *self.tx)
        .await
        .map_err(store_error)?;
        let row = sqlx::query(
            "SELECT body_digest, receipt FROM manager.command_receipts \
             WHERE actor_kind = $1 AND actor_issuer = $2 AND actor_subject = $3 \
             AND action = $4 AND idempotency_key = $5 FOR UPDATE",
        )
        .bind(kind)
        .bind(issuer)
        .bind(subject)
        .bind(action)
        .bind(key)
        .fetch_one(&mut *self.tx)
        .await
        .map_err(store_error)?;
        if row.get::<Vec<u8>, _>("body_digest") != digest {
            return Ok(Reservation::Conflict);
        }
        match row.get::<Option<Json<serde_json::Value>>, _>("receipt") {
            Some(Json(receipt)) => serde_json::to_vec(&receipt)
                .map(Reservation::Replay)
                .map_err(|_| StoreError::Unavailable),
            None => Ok(Reservation::Fresh),
        }
    }

    async fn store_receipt(
        &mut self,
        actor: &Actor,
        action: &'static str,
        key: &str,
        receipt: Vec<u8>,
    ) -> Result<(), StoreError> {
        let receipt: serde_json::Value =
            serde_json::from_slice(&receipt).map_err(|_| StoreError::Unavailable)?;
        let (kind, issuer, subject) = actor_columns(actor);
        let stored = sqlx::query(
            "UPDATE manager.command_receipts SET receipt = $6 \
             WHERE actor_kind = $1 AND actor_issuer = $2 AND actor_subject = $3 \
             AND action = $4 AND idempotency_key = $5 AND receipt IS NULL",
        )
        .bind(kind)
        .bind(issuer)
        .bind(subject)
        .bind(action)
        .bind(key)
        .bind(Json(receipt))
        .execute(&mut *self.tx)
        .await
        .map_err(store_error)?;
        if stored.rows_affected() != 1 {
            return Err(StoreError::Unavailable);
        }
        Ok(())
    }

    async fn append_audit(&mut self, event: AuditEvent) -> Result<(), StoreError> {
        let change: serde_json::Value =
            serde_json::from_str(&event.change).map_err(|_| StoreError::Unavailable)?;
        let identity = event.actor.identity();
        sqlx::query(
            "INSERT INTO manager.audit_events \
             (id, actor_kind, actor_issuer, actor_subject, action, target_kind, target_id, request_id, change) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(event.id)
        .bind(event.actor.kind())
        .bind(identity.map(|i| i.issuer.as_str()))
        .bind(identity.map(|i| i.subject.as_str()))
        .bind(event.action)
        .bind(event.target.kind)
        .bind(&event.target.id)
        .bind(event.request_id)
        .bind(Json(change))
        .execute(&mut *self.tx)
        .await
        .map_err(store_error)?;
        Ok(())
    }

    async fn notify_instance(&mut self, instance: Uuid) -> Result<(), StoreError> {
        // Delivered by PostgreSQL only when the transaction commits.
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
