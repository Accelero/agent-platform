//! SQL-only periodic enqueue; no Kubernetes I/O or synthetic readiness.
use async_trait::async_trait;
use hermes_application::{
    Error, InstallationScope,
    resync::{PeriodicResync, RESYNC_INTERVAL_SECONDS, ResyncInput},
};
use hermes_domain::{Access, resources::valid_uid};
use sqlx::Row;
use uuid::Uuid;

use super::{PgStore, db_error, gate};

#[async_trait]
impl PeriodicResync for PgStore {
    async fn enqueue_due(&self, scope: &InstallationScope, limit: u16) -> Result<u16, Error> {
        if !(1..=64).contains(&limit) {
            return Err(Error::Invalid);
        }
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        gate(&mut tx, scope).await?;
        // Keep the same owner-before-operation lock order as administrator
        // mutations. An in-flight command or manual-review failure owns recovery;
        // periodic observation cannot supersede it or silently retry it.
        let rows = sqlx::query("SELECT a.access,a.template_id,a.template_version,a.version,i.id,i.namespace_uid,i.desired_generation FROM manager.assignments a JOIN manager.instances i ON i.assignment_id=a.id WHERE a.installation_id=$1 AND i.reconcile_after<=clock_timestamp() AND ((a.infrastructure_managed AND i.state='Blocked' AND i.conditions->'commissioning'->>'reason' IN ('dependency_unavailable','deadline_exceeded')) OR (i.observed_generation=i.desired_generation AND ((i.state='Ready' AND a.access='enabled' AND i.namespace_uid IS NOT NULL) OR (i.state='Suspended' AND a.access='suspended') OR (i.state='Retained' AND a.access='revoked')))) AND NOT EXISTS(SELECT 1 FROM manager.operations o WHERE o.instance_id=i.id AND (o.state IN ('Pending','Running','Waiting','RetryAt') OR (o.generation=i.desired_generation AND o.state IN ('Blocked','Failed') AND NOT (a.infrastructure_managed AND i.conditions->'commissioning'->>'reason' IN ('dependency_unavailable','deadline_exceeded'))))) ORDER BY i.reconcile_after,i.id FOR UPDATE OF a,i SKIP LOCKED LIMIT $2")
            .bind(scope.id).bind(i64::from(limit)).fetch_all(&mut *tx).await.map_err(db_error)?;
        for row in &rows {
            let instance_id: Uuid = row.get("id");
            let namespace_uid: Option<String> = row.get("namespace_uid");
            if namespace_uid.as_deref().is_some_and(|uid| !valid_uid(uid)) {
                return Err(Error::OwnershipConflict);
            }
            let access = match row.get::<String, _>("access").as_str() {
                "enabled" => Access::Enabled,
                "suspended" => Access::Suspended,
                "revoked" => Access::Revoked,
                _ => return Err(Error::RecoveryRequired),
            };
            let input = ResyncInput {
                access,
                template_id: row.get("template_id"),
                template_version: row.get("template_version"),
                assignment_version: row.get("version"),
                generation: row.get("desired_generation"),
                epoch: scope.epoch,
                namespace_uid,
            };
            sqlx::query("INSERT INTO manager.operations(id,installation_id,instance_id,kind,generation,epoch,input) VALUES($1,$2,$3,'resync',$4,$5,$6)")
                .bind(Uuid::new_v4()).bind(scope.id).bind(instance_id).bind(input.generation).bind(scope.epoch)
                .bind(serde_json::to_value(input).map_err(|_|Error::Invalid)?)
                .execute(&mut *tx).await.map_err(db_error)?;
            sqlx::query("UPDATE manager.instances SET reconcile_after=clock_timestamp()+make_interval(secs=>$2) WHERE id=$1")
                .bind(instance_id).bind(i32::from(RESYNC_INTERVAL_SECONDS)).execute(&mut *tx).await.map_err(db_error)?;
        }
        let enqueued = u16::try_from(rows.len()).map_err(|_| Error::Invalid)?;
        tx.commit().await.map_err(db_error)?;
        Ok(enqueued)
    }
}
