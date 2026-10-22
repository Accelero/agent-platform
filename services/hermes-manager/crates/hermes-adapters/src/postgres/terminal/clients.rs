use super::super::{PgStore, db_error, gate, lock_claim};
use async_trait::async_trait;
use hermes_application::{
    Error, InstallationScope, OperationClaim,
    materials::MaterialContext,
    terminal::clients::{ExecutionClient, ExecutionClientRepository, ExecutionTokenHash},
};
use sqlx::Row;
use uuid::Uuid;

fn client(row: &sqlx::postgres::PgRow) -> ExecutionClient {
    ExecutionClient {
        id: row.get("id"),
        installation_id: row.get("installation_id"),
        instance_id: row.get("instance_id"),
        epoch: row.get("authorization_epoch"),
    }
}
#[async_trait]
impl ExecutionClientRepository for PgStore {
    async fn register_under_current_claim(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
        material: &MaterialContext,
        token_hash: ExecutionTokenHash,
    ) -> Result<ExecutionClient, Error> {
        material.validate()?;
        if material.purpose != "native-bootstrap-v1"
            || material.installation_id != scope.id
            || material.instance_id != claim.instance_id
            || material.epoch > scope.epoch
            || material.generation > claim.generation
        {
            return Err(Error::Invalid);
        }
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        lock_claim(&mut tx, scope, claim).await?;
        let valid:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM manager.assignments a JOIN manager.instances i ON i.assignment_id=a.id JOIN manager.operations o ON o.id=$3 JOIN manager.bootstrap_materials m ON m.id=$4 WHERE a.installation_id=$1 AND i.id=$2 AND a.access='enabled' AND o.kind IN ('commission','resync') AND o.kind=$5 AND o.input->>'access'='enabled' AND m.installation_id=$1 AND m.instance_id=$2 AND m.epoch=$6 AND m.creation_generation=$7 AND m.creation_operation=$8 AND m.purpose='native-bootstrap-v1')")
            .bind(scope.id).bind(claim.instance_id).bind(claim.id).bind(material.id).bind(&claim.kind).bind(material.epoch).bind(material.generation).bind(material.operation_id).fetch_one(&mut *tx).await.map_err(db_error)?;
        if !valid {
            return Err(Error::Forbidden);
        }
        sqlx::query("INSERT INTO manager.execution_clients(id,installation_id,instance_id,material_id,token_hash,authorization_epoch) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(instance_id) DO NOTHING")
            .bind(Uuid::new_v4()).bind(scope.id).bind(claim.instance_id).bind(material.id).bind(token_hash.bytes().as_slice()).bind(scope.epoch).execute(&mut *tx).await.map_err(db_error)?;
        let row =
            sqlx::query("SELECT *, revoked_at IS NOT NULL AS revoked FROM manager.execution_clients WHERE instance_id=$1 FOR UPDATE")
                .bind(claim.instance_id)
                .fetch_one(&mut *tx)
                .await
                .map_err(db_error)?;
        if row.get::<Uuid, _>("installation_id") != scope.id
            || row.get::<Uuid, _>("material_id") != material.id
            || row.get::<Vec<u8>, _>("token_hash").as_slice() != token_hash.bytes()
        {
            return Err(Error::OwnershipConflict);
        }
        if row.get::<bool, _>("revoked") {
            return Err(Error::Forbidden);
        }
        sqlx::query(
            "UPDATE manager.execution_clients SET authorization_epoch=$2 WHERE instance_id=$1",
        )
        .bind(claim.instance_id)
        .bind(scope.epoch)
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
        let mut result = client(&row);
        result.epoch = scope.epoch;
        tx.commit().await.map_err(db_error)?;
        Ok(result)
    }
    async fn authenticate_execution_client(
        &self,
        scope: &InstallationScope,
        token_hash: ExecutionTokenHash,
    ) -> Result<ExecutionClient, Error> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        gate(&mut tx, scope).await?;
        let row=sqlx::query("SELECT c.* FROM manager.execution_clients c JOIN manager.instances i ON i.id=c.instance_id JOIN manager.assignments a ON a.id=i.assignment_id WHERE c.installation_id=$1 AND a.installation_id=$1 AND c.token_hash=$2 AND c.authorization_epoch=$3 AND c.revoked_at IS NULL AND a.access='enabled'")
            .bind(scope.id).bind(token_hash.bytes().as_slice()).bind(scope.epoch).fetch_optional(&mut *tx).await.map_err(db_error)?.ok_or(Error::Forbidden)?;
        let result = client(&row);
        tx.commit().await.map_err(db_error)?;
        Ok(result)
    }
}
