use super::*;
use hermes_application::materials::{
    Ciphertext, MaterialContext, MaterialRecord, MaterialRepository,
};
use hermes_application::resync::ResyncInput;
use hermes_domain::{Access, resources::valid_uid};

pub(in crate::postgres) fn record(row: &sqlx::postgres::PgRow) -> Result<MaterialRecord, Error> {
    let context = MaterialContext {
        id: row.get("id"),
        installation_id: row.get("installation_id"),
        instance_id: row.get("instance_id"),
        epoch: row.get("epoch"),
        operation_id: row.get("creation_operation"),
        generation: row.get("creation_generation"),
        purpose: row.get("purpose"),
    };
    context.validate()?;
    Ok(MaterialRecord {
        context,
        sealed: Ciphertext {
            key_id: row.get("key_id"),
            nonce: row.get("nonce"),
            ciphertext: row.get("ciphertext"),
        },
    })
}
async fn enabled(tx: &mut Transaction<'_, Postgres>, claim: &OperationClaim) -> Result<(), Error> {
    let access:String=sqlx::query_scalar("SELECT a.access FROM manager.assignments a JOIN manager.instances i ON i.assignment_id=a.id WHERE i.id=$1")
        .bind(claim.instance_id).fetch_one(&mut **tx).await.map_err(db_error)?;
    if access != "enabled" {
        return Err(Error::Forbidden);
    }
    Ok(())
}
#[async_trait]
impl MaterialRepository for PgStore {
    async fn find_retained_material(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
        purpose: &str,
    ) -> Result<Option<MaterialRecord>, Error> {
        validate_name(purpose)?;
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        lock_claim(&mut tx, scope, claim).await?;
        let operation = sqlx::query("SELECT o.kind,o.input,a.access,a.template_id,a.template_version,a.version AS assignment_version,i.namespace,i.namespace_uid FROM manager.operations o JOIN manager.instances i ON i.id=o.instance_id JOIN manager.assignments a ON a.id=i.assignment_id WHERE o.id=$1 AND o.installation_id=$2 AND i.id=$3 AND a.installation_id=$2")
            .bind(claim.id).bind(scope.id).bind(claim.instance_id).fetch_one(&mut *tx).await.map_err(db_error)?;
        let kind = operation.get::<String, _>("kind");
        if claim.kind != kind {
            return Err(Error::Forbidden);
        }
        let input = operation.get::<Value, _>("input");
        let access = match kind.as_str() {
            "suspend" => "suspended",
            "revoke" => "revoked",
            "resync" => {
                let pinned: ResyncInput =
                    serde_json::from_value(input.clone()).map_err(|_| Error::OwnershipConflict)?;
                let access = match pinned.access {
                    Access::Suspended => "suspended",
                    Access::Revoked => "revoked",
                    Access::Enabled => return Err(Error::Forbidden),
                };
                let namespace_uid = operation.get::<Option<String>, _>("namespace_uid");
                if pinned.assignment_version != operation.get::<i64, _>("assignment_version")
                    || pinned.generation != claim.generation
                    || pinned.epoch != scope.epoch
                    || operation.get::<String, _>("namespace")
                        != instance_namespace(scope.id, claim.instance_id)
                    || namespace_uid.as_deref().is_some_and(|uid| !valid_uid(uid))
                    || pinned
                        .namespace_uid
                        .as_deref()
                        .is_some_and(|uid| !valid_uid(uid) || namespace_uid.as_deref() != Some(uid))
                {
                    return Err(Error::OwnershipConflict);
                }
                // A None snapshot can become Some only through the separately
                // authenticated namespace binding. Retained reads do not adopt
                // or write a UID; they preserve the original material context.
                access
            }
            _ => return Err(Error::Forbidden),
        };
        if operation.get::<String, _>("access") != access || input["access"] != access {
            return Err(Error::Forbidden);
        }
        if input["templateId"] != operation.get::<String, _>("template_id")
            || input["templateVersion"] != operation.get::<i64, _>("template_version")
        {
            return Err(Error::OwnershipConflict);
        }
        let row = sqlx::query("SELECT * FROM manager.bootstrap_materials WHERE installation_id=$1 AND instance_id=$2 AND purpose=$3")
            .bind(scope.id).bind(claim.instance_id).bind(purpose).fetch_optional(&mut *tx).await.map_err(db_error)?;
        let retained = row.as_ref().map(record).transpose()?;
        if retained.as_ref().is_some_and(|record| {
            record.context.installation_id != scope.id
                || record.context.instance_id != claim.instance_id
                || record.context.purpose != purpose
                || record.context.epoch > scope.epoch
                || record.context.generation > claim.generation
        }) {
            return Err(Error::RecoveryRequired);
        }
        tx.commit().await.map_err(db_error)?;
        Ok(retained)
    }
    async fn find_material(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
        purpose: &str,
    ) -> Result<Option<MaterialRecord>, Error> {
        validate_name(purpose)?;
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        lock_claim(&mut tx, scope, claim).await?;
        enabled(&mut tx, claim).await?;
        let row=sqlx::query("SELECT * FROM manager.bootstrap_materials WHERE installation_id=$1 AND instance_id=$2 AND purpose=$3")
            .bind(scope.id).bind(claim.instance_id).bind(purpose).fetch_optional(&mut *tx).await.map_err(db_error)?;
        let record = row.as_ref().map(record).transpose()?;
        if record.as_ref().is_some_and(|record| {
            record.context.epoch > scope.epoch || record.context.generation > claim.generation
        }) {
            return Err(Error::RecoveryRequired);
        }
        tx.commit().await.map_err(db_error)?;
        Ok(record)
    }
    async fn reserve_material(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
        candidate: MaterialRecord,
    ) -> Result<MaterialRecord, Error> {
        let c = &candidate.context;
        c.validate()?;
        validate_name(&candidate.sealed.key_id)?;
        if c.installation_id != scope.id
            || c.instance_id != claim.instance_id
            || c.epoch != scope.epoch
            || c.operation_id != claim.id
            || c.generation != claim.generation
            || candidate.sealed.nonce.len() != 24
            || !(16..=65536).contains(&candidate.sealed.ciphertext.len())
        {
            return Err(Error::Invalid);
        }
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        lock_claim(&mut tx, scope, claim).await?;
        enabled(&mut tx, claim).await?;
        sqlx::query("INSERT INTO manager.bootstrap_materials(id,installation_id,instance_id,epoch,purpose,key_id,nonce,ciphertext,creation_operation,creation_generation) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) ON CONFLICT(instance_id,purpose) DO NOTHING")
            .bind(c.id).bind(c.installation_id).bind(c.instance_id).bind(c.epoch).bind(&c.purpose).bind(&candidate.sealed.key_id).bind(&candidate.sealed.nonce).bind(&candidate.sealed.ciphertext).bind(c.operation_id).bind(c.generation).execute(&mut *tx).await.map_err(db_error)?;
        let row=sqlx::query("SELECT * FROM manager.bootstrap_materials WHERE installation_id=$1 AND instance_id=$2 AND purpose=$3")
            .bind(scope.id).bind(claim.instance_id).bind(&c.purpose).fetch_one(&mut *tx).await.map_err(db_error)?;
        let original = record(&row)?;
        if original.context.epoch > scope.epoch || original.context.generation > claim.generation {
            return Err(Error::RecoveryRequired);
        }
        tx.commit().await.map_err(db_error)?;
        Ok(original)
    }
}
