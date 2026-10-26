//! The controller's view of the registry: desired instances, create-once
//! material, namespace binding and epoch-guarded status.

use std::collections::BTreeMap;

use async_trait::async_trait;
use hermes_core::app::controller::WorkerEnvironment;
use hermes_core::app::controller::{
    AllowedBackend, DesiredInstance, InstanceStore, NewMaterial, SealedSecret, StoredMaterial,
};
use hermes_core::app::error::StoreError;
use hermes_core::domain::backends::BackendDefinition;
use hermes_core::domain::execution::{EnvironmentState, WorkerFailure};
use hermes_core::domain::identity::Identity;
use hermes_core::domain::instances::InstanceStatus;
use hermes_core::domain::materials::MaterialPurpose;
use hermes_core::domain::orphans::Orphan;
use hermes_core::domain::secrets::SecretKind;
use hermes_core::domain::settings::merge;
use hermes_core::domain::templates::TemplateDefinition;
use serde_json::json;
use sqlx::Row;
use sqlx::types::Json;
use uuid::Uuid;

use crate::{PgStore, assignments, store_error};

#[async_trait]
impl InstanceStore for PgStore {
    async fn instance_ids(&self) -> Result<Vec<Uuid>, StoreError> {
        sqlx::query_scalar("SELECT id FROM manager.instances ORDER BY id")
            .fetch_all(&self.pool)
            .await
            .map_err(store_error)
    }

    async fn desired_instance(
        &self,
        instance: Uuid,
    ) -> Result<Option<DesiredInstance>, StoreError> {
        // One snapshot for every table the desired state is read from.
        let mut tx = self.pool.begin().await.map_err(store_error)?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
            .execute(&mut *tx)
            .await
            .map_err(store_error)?;
        let Some(row) = sqlx::query(
            "SELECT i.assignment_id, i.namespace, i.namespace_uid, i.desired_generation, \
             i.execution_generation, d.issuer, d.subject, c.definition \
             FROM manager.instances i \
             JOIN manager.assignments a ON a.id = i.assignment_id \
             JOIN manager.identities d ON d.id = a.identity_id \
             JOIN manager.catalog_versions c \
               ON c.kind = 'template' AND c.id = a.template_id AND c.version = a.template_version \
             WHERE i.id = $1",
        )
        .bind(instance)
        .fetch_optional(&mut *tx)
        .await
        .map_err(store_error)?
        else {
            return Ok(None);
        };
        let assignment_id: Uuid = row.get("assignment_id");
        let record = assignments::load(&mut tx, assignment_id, false)
            .await?
            .ok_or(StoreError::Unavailable)?;
        let rows = sqlx::query(
            "SELECT b.backend_id, b.backend_version, c.definition FROM manager.assignment_backends b \
             JOIN manager.catalog_versions c ON c.kind = 'terminal_backend' AND c.id = b.backend_id \
               AND c.version = b.backend_version WHERE b.assignment_id = $1",
        )
        .bind(assignment_id)
        .fetch_all(&mut *tx)
        .await
        .map_err(store_error)?;
        let mut backends = BTreeMap::new();
        for backend in rows {
            let Json(definition) = backend
                .try_get::<Json<BackendDefinition>, _>("definition")
                .map_err(|_| StoreError::Unavailable)?;
            backends.insert(
                backend.get::<String, _>("backend_id"),
                AllowedBackend {
                    version: backend.get("backend_version"),
                    definition,
                },
            );
        }
        tx.commit().await.map_err(store_error)?;
        let Json(definition) = row
            .try_get::<Json<TemplateDefinition>, _>("definition")
            .map_err(|_| StoreError::Unavailable)?;
        let spec = record.spec;
        let mut admin_config = definition.admin_config.clone();
        merge(&mut admin_config, &spec.admin_config);
        let secrets = definition
            .secrets
            .iter()
            .chain(&spec.secrets)
            .cloned()
            .collect();
        Ok(Some(DesiredInstance {
            instance_id: instance,
            assignment_id,
            identity: Identity {
                issuer: row.get("issuer"),
                subject: row.get("subject"),
            },
            namespace: row.get("namespace"),
            namespace_uid: row.get("namespace_uid"),
            access: spec.access,
            desired_generation: row.get("desired_generation"),
            execution_generation: row.get("execution_generation"),
            template: spec.template,
            definition,
            volumes: spec.volumes,
            terminal: spec.terminal,
            admin_config,
            secrets,
            backends,
        }))
    }

    async fn materials(&self, instance: Uuid) -> Result<Vec<StoredMaterial>, StoreError> {
        let rows = sqlx::query(
            "SELECT id, purpose, sealed, public FROM manager.materials WHERE instance_id = $1",
        )
        .bind(instance)
        .fetch_all(&self.pool)
        .await
        .map_err(store_error)?;
        rows.into_iter()
            .map(|row| {
                Ok(StoredMaterial {
                    id: row.get("id"),
                    purpose: MaterialPurpose::parse(row.get("purpose"))
                        .ok_or(StoreError::Unavailable)?,
                    sealed: row.get("sealed"),
                    public: row.get("public"),
                })
            })
            .collect()
    }

    async fn insert_material(
        &self,
        instance: Uuid,
        purpose: MaterialPurpose,
        material: &NewMaterial,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO manager.materials (instance_id, purpose, id, sealed, public) \
             VALUES ($1, $2, $3, $4, $5) ON CONFLICT (instance_id, purpose) DO NOTHING",
        )
        .bind(instance)
        .bind(purpose.as_str())
        .bind(Uuid::new_v4())
        .bind(&material.sealed)
        .bind(&material.public)
        .execute(&self.pool)
        .await
        .map_err(store_error)?;
        Ok(())
    }

    async fn bind_namespace(&self, instance: Uuid, uid: &str) -> Result<(), StoreError> {
        sqlx::query("UPDATE manager.instances SET namespace_uid = $2 WHERE id = $1 AND namespace_uid IS NULL")
            .bind(instance)
            .bind(uid)
            .execute(&self.pool)
            .await
            .map_err(store_error)?;
        Ok(())
    }

    async fn record_status(
        &self,
        epoch: i64,
        instance: Uuid,
        generation: i64,
        status: &InstanceStatus,
    ) -> Result<bool, StoreError> {
        let conditions: Vec<_> = status
            .conditions
            .iter()
            .map(|c| json!({"type": c.kind.as_str(), "status": c.status.as_str(), "reason": c.reason}))
            .collect();
        // The epoch row is share-locked: a new leader's epoch advance waits
        // for this write, and a write after the advance sees the new epoch.
        let written = sqlx::query(
            "UPDATE manager.instances SET phase = $3, blocked_reason = $4, conditions = $5, \
             observed_generation = CASE WHEN $6 THEN GREATEST(observed_generation, LEAST($7, desired_generation)) \
                                        ELSE observed_generation END, \
             status_epoch = $1, observed_at = clock_timestamp() \
             WHERE id = $2 AND status_epoch <= $1 \
               AND $1 = (SELECT epoch FROM manager.controller_epoch FOR SHARE)",
        )
        .bind(epoch)
        .bind(instance)
        .bind(status.phase.as_str())
        .bind(status.blocked.map(|b| b.as_str()))
        .bind(Json(conditions))
        .bind(status.converged)
        .bind(generation)
        .execute(&self.pool)
        .await
        .map_err(store_error)?
        .rows_affected();
        Ok(written == 1)
    }

    async fn latest_secrets(&self, names: &[String]) -> Result<Vec<SealedSecret>, StoreError> {
        let rows = sqlx::query(
            "SELECT DISTINCT ON (name) name, kind, version, sealed FROM manager.admin_secrets \
             WHERE name = ANY($1) ORDER BY name, version DESC",
        )
        .bind(names)
        .fetch_all(&self.pool)
        .await
        .map_err(store_error)?;
        rows.into_iter()
            .map(|row| {
                Ok(SealedSecret {
                    name: row.get("name"),
                    kind: SecretKind::parse(row.get("kind")).ok_or(StoreError::Unavailable)?,
                    version: row.get("version"),
                    sealed: row.get("sealed"),
                })
            })
            .collect()
    }

    async fn worker_environments(
        &self,
        instance: Uuid,
    ) -> Result<Vec<WorkerEnvironment>, StoreError> {
        self.worker_environment_rows(instance).await
    }

    async fn transition_environment(
        &self,
        environment: Uuid,
        from: EnvironmentState,
        to: EnvironmentState,
        detail: Option<&str>,
    ) -> Result<bool, StoreError> {
        self.move_environment(environment, from, to, detail).await
    }

    async fn fail_environment(
        &self,
        environment: Uuid,
        from: EnvironmentState,
        failure: WorkerFailure,
    ) -> Result<bool, StoreError> {
        self.fail(environment, from, failure).await
    }

    async fn instance_namespaces(&self) -> Result<BTreeMap<Uuid, String>, StoreError> {
        let rows = sqlx::query("SELECT id, namespace FROM manager.instances")
            .fetch_all(&self.pool)
            .await
            .map_err(store_error)?;
        Ok(rows
            .iter()
            .map(|row| (row.get("id"), row.get("namespace")))
            .collect())
    }

    async fn record_orphans(&self, epoch: i64, orphans: &[Orphan]) -> Result<bool, StoreError> {
        let mut tx = self.pool.begin().await.map_err(store_error)?;
        // Share-locked like a status write: a newer leader's advance waits.
        let current: i64 =
            sqlx::query_scalar("SELECT epoch FROM manager.controller_epoch FOR SHARE")
                .fetch_one(&mut *tx)
                .await
                .map_err(store_error)?;
        if current != epoch {
            return Ok(false);
        }
        let kinds: Vec<&str> = orphans.iter().map(|o| o.kind.as_str()).collect();
        let names: Vec<&str> = orphans.iter().map(|o| o.name.as_str()).collect();
        let uids: Vec<&str> = orphans.iter().map(|o| o.uid.as_str()).collect();
        sqlx::query(
            "DELETE FROM manager.orphans WHERE (kind, name) NOT IN \
             (SELECT * FROM unnest($1::text[], $2::text[]))",
        )
        .bind(&kinds)
        .bind(&names)
        .execute(&mut *tx)
        .await
        .map_err(store_error)?;
        // A recreated object under the same name is seen for the first time.
        sqlx::query(
            "INSERT INTO manager.orphans (kind, name, uid) \
             SELECT * FROM unnest($1::text[], $2::text[], $3::text[]) \
             ON CONFLICT (kind, name) DO UPDATE SET last_seen_at = clock_timestamp(), \
               first_seen_at = CASE WHEN manager.orphans.uid = EXCLUDED.uid \
                                    THEN manager.orphans.first_seen_at ELSE clock_timestamp() END, \
               uid = EXCLUDED.uid",
        )
        .bind(&kinds)
        .bind(&names)
        .bind(&uids)
        .execute(&mut *tx)
        .await
        .map_err(store_error)?;
        sqlx::query(
            "INSERT INTO manager.orphan_scan (scanned_at) VALUES (clock_timestamp()) \
             ON CONFLICT (singleton) DO UPDATE SET scanned_at = EXCLUDED.scanned_at",
        )
        .execute(&mut *tx)
        .await
        .map_err(store_error)?;
        tx.commit().await.map_err(store_error)?;
        Ok(true)
    }

    async fn advance_epoch(&self) -> Result<i64, StoreError> {
        sqlx::query_scalar("UPDATE manager.controller_epoch SET epoch = epoch + 1 RETURNING epoch")
            .fetch_one(&self.pool)
            .await
            .map_err(store_error)
    }
}
