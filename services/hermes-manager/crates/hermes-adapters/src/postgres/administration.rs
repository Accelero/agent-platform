use async_trait::async_trait;
use hermes_application::{
    AssignmentView, Error, InstallationScope,
    administration::{AdministrationReader, AuditView, IdentityView, OperationView, TemplateView},
};
use hermes_domain::Identity;
use sqlx::Row;
use uuid::Uuid;

use super::{PgStore, assignment_view, db_error, gate};

fn validate_limit(limit: u16) -> Result<i64, Error> {
    if (1..=100).contains(&limit) {
        Ok(i64::from(limit))
    } else {
        Err(Error::Invalid)
    }
}

#[async_trait]
impl AdministrationReader for PgStore {
    async fn identities(
        &self,
        scope: &InstallationScope,
        after: Option<Uuid>,
        limit: u16,
    ) -> Result<Vec<IdentityView>, Error> {
        let limit = validate_limit(limit)?;
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        gate(&mut tx, scope).await?;
        let rows = sqlx::query("SELECT k.id,k.issuer,k.subject FROM manager.identities k JOIN manager.assignments a ON a.identity_id=k.id WHERE a.installation_id=$1 AND ($2::uuid IS NULL OR k.id>$2) ORDER BY k.id LIMIT $3")
            .bind(scope.id).bind(after).bind(limit).fetch_all(&mut *tx).await.map_err(db_error)?;
        let result = rows
            .iter()
            .map(|row| IdentityView {
                id: row.get("id"),
                identity: Identity {
                    issuer: row.get("issuer"),
                    subject: row.get("subject"),
                },
                directory_verified: false,
            })
            .collect();
        tx.commit().await.map_err(db_error)?;
        Ok(result)
    }

    async fn templates(&self, scope: &InstallationScope) -> Result<Vec<TemplateView>, Error> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        gate(&mut tx, scope).await?;
        let rows = sqlx::query("SELECT id,version,image,native_port,native_scheme FROM manager.instance_templates WHERE approved AND schema_version=1 ORDER BY id,version LIMIT 1000")
            .fetch_all(&mut *tx).await.map_err(db_error)?;
        let result = rows
            .iter()
            .map(|row| {
                Ok(TemplateView {
                    id: row.get("id"),
                    version: row.get("version"),
                    image: row.get("image"),
                    native_port: u16::try_from(row.get::<i32, _>("native_port"))
                        .map_err(|_| Error::RecoveryRequired)?,
                    native_scheme: row.get("native_scheme"),
                })
            })
            .collect::<Result<_, Error>>()?;
        tx.commit().await.map_err(db_error)?;
        Ok(result)
    }

    async fn instance(&self, scope: &InstallationScope, id: Uuid) -> Result<AssignmentView, Error> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        gate(&mut tx, scope).await?;
        let row = sqlx::query("SELECT a.id,k.issuer,k.subject,a.access,a.template_id,a.template_version,a.version,i.id AS instance_id,i.namespace,i.desired_generation,i.observed_generation,i.state FROM manager.assignments a JOIN manager.identities k ON k.id=a.identity_id JOIN manager.instances i ON i.assignment_id=a.id WHERE a.installation_id=$1 AND i.id=$2")
            .bind(scope.id).bind(id).fetch_optional(&mut *tx).await.map_err(db_error)?.ok_or(Error::NotFound)?;
        let result = assignment_view(row)?;
        tx.commit().await.map_err(db_error)?;
        Ok(result)
    }

    async fn operation(&self, scope: &InstallationScope, id: Uuid) -> Result<OperationView, Error> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        gate(&mut tx, scope).await?;
        let row = sqlx::query("SELECT id,instance_id,kind,generation,state,attempt,extract(epoch from next_attempt_at)::bigint AS next_attempt_at FROM manager.operations WHERE installation_id=$1 AND id=$2")
            .bind(scope.id).bind(id).fetch_optional(&mut *tx).await.map_err(db_error)?.ok_or(Error::NotFound)?;
        let result = OperationView {
            id: row.get("id"),
            instance_id: row.get("instance_id"),
            kind: row.get("kind"),
            generation: row.get("generation"),
            state: row.get("state"),
            attempt: row.get("attempt"),
            next_attempt_at: row.get("next_attempt_at"),
        };
        tx.commit().await.map_err(db_error)?;
        Ok(result)
    }

    async fn audit(
        &self,
        scope: &InstallationScope,
        after: Option<Uuid>,
        limit: u16,
    ) -> Result<Vec<AuditView>, Error> {
        let limit = validate_limit(limit)?;
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        gate(&mut tx, scope).await?;
        let rows = sqlx::query("SELECT id,actor_issuer,actor_subject,action,assignment_id,operation_id,request_id,version,extract(epoch from occurred_at)::bigint AS occurred_at FROM manager.audit_events WHERE installation_id=$1 AND ($2::uuid IS NULL OR id>$2) ORDER BY id LIMIT $3")
            .bind(scope.id).bind(after).bind(limit).fetch_all(&mut *tx).await.map_err(db_error)?;
        let result = rows
            .iter()
            .map(|row| AuditView {
                id: row.get("id"),
                actor: Identity {
                    issuer: row.get("actor_issuer"),
                    subject: row.get("actor_subject"),
                },
                action: row.get("action"),
                assignment_id: row.get("assignment_id"),
                operation_id: row.get("operation_id"),
                request_id: row.get("request_id"),
                version: row.get("version"),
                occurred_at: row.get("occurred_at"),
            })
            .collect();
        tx.commit().await.map_err(db_error)?;
        Ok(result)
    }
}
