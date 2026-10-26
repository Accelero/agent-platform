//! The resolver's registry read: one statement over the identity, its
//! assignment and its instance, without locks.

use async_trait::async_trait;
use hermes_core::app::error::StoreError;
use hermes_core::app::routing::RouteStore;
use hermes_core::domain::assignments::Access;
use hermes_core::domain::identity::Identity;
use hermes_core::domain::instances::Phase;
use hermes_core::domain::routing::RouteRow;
use sqlx::Row;

use crate::{PgStore, store_error};

#[async_trait]
impl RouteStore for PgStore {
    async fn route_row(&self, identity: &Identity) -> Result<Option<RouteRow>, StoreError> {
        let Some(row) = sqlx::query(
            "SELECT i.id, i.namespace, i.namespace_uid IS NOT NULL AS bound, a.access, i.phase, \
             i.desired_generation, i.observed_generation, a.version \
             FROM manager.identities d JOIN manager.assignments a ON a.identity_id = d.id \
             JOIN manager.instances i ON i.assignment_id = a.id \
             WHERE d.issuer = $1 AND d.subject = $2",
        )
        .bind(&identity.issuer)
        .bind(&identity.subject)
        .fetch_optional(&self.pool)
        .await
        .map_err(store_error)?
        else {
            return Ok(None);
        };
        Ok(Some(RouteRow {
            instance_id: row.get("id"),
            namespace: row.get("namespace"),
            namespace_bound: row.get("bound"),
            access: Access::parse(row.get("access")).ok_or(StoreError::Unavailable)?,
            phase: Phase::parse(row.get("phase")).ok_or(StoreError::Unavailable)?,
            desired_generation: row.get("desired_generation"),
            observed_generation: row.get("observed_generation"),
            assignment_version: row.get("version"),
        }))
    }
}
