//! Versioned manager-owned drafts and immutable catalog publication.
use super::*;
use hermes_application::VerifiedPrincipal;
use hermes_application::commissioning::TemplateDefinition;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DraftInput {
    pub template_id: String,
    pub definition: TemplateDefinition,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DraftView {
    pub id: Uuid,
    pub template_id: String,
    pub definition: TemplateDefinition,
    pub version: i64,
    pub published_version: Option<i64>,
}
pub struct DraftChange {
    pub id: Option<Uuid>,
    pub expected: Option<i64>,
    pub input: DraftInput,
    pub publish: bool,
}
pub(super) fn draft_view(row: sqlx::postgres::PgRow) -> Result<DraftView, Error> {
    let definition: Value = row.get("definition");
    if !crate::template_validation::validate(&definition).is_empty() {
        return Err(Error::RecoveryRequired);
    }
    Ok(DraftView {
        id: row.get("id"),
        template_id: row.get("template_id"),
        definition: serde_json::from_value(definition).map_err(|_| Error::RecoveryRequired)?,
        version: row.get("version"),
        published_version: row.get("published_version"),
    })
}
pub(super) async fn command_begin(
    tx: &mut Transaction<'_, Postgres>,
    scope: &InstallationScope,
    actor: &Identity,
    key: &str,
    input: &Value,
) -> Result<Option<Value>, Error> {
    if key.is_empty() || key.len() > 128 || !key.bytes().all(|v| v.is_ascii_graphic()) {
        return Err(Error::Invalid);
    }
    gate(tx, scope).await?;
    super::administrators::authorize_grant(tx, scope, actor, false).await?;
    let digest = Sha256::digest(serde_json::to_vec(input).map_err(|_| Error::Invalid)?).to_vec();
    sqlx::query("INSERT INTO manager.dashboard_commands(installation_id,actor_issuer,actor_subject,idempotency_key,body_digest) VALUES($1,$2,$3,$4,$5) ON CONFLICT DO NOTHING").bind(scope.id).bind(&actor.issuer).bind(&actor.subject).bind(key).bind(&digest).execute(&mut **tx).await.map_err(db_error)?;
    let row=sqlx::query("SELECT body_digest,result FROM manager.dashboard_commands WHERE installation_id=$1 AND actor_issuer=$2 AND actor_subject=$3 AND idempotency_key=$4 FOR UPDATE").bind(scope.id).bind(&actor.issuer).bind(&actor.subject).bind(key).fetch_one(&mut **tx).await.map_err(db_error)?;
    if row.get::<Vec<u8>, _>("body_digest") != digest {
        return Err(Error::IdempotencyConflict);
    }
    Ok(row.get("result"))
}
pub(super) async fn command_finish(
    tx: &mut Transaction<'_, Postgres>,
    scope: &InstallationScope,
    actor: &Identity,
    key: &str,
    result: &Value,
) -> Result<(), Error> {
    sqlx::query("UPDATE manager.dashboard_commands SET result=$5 WHERE installation_id=$1 AND actor_issuer=$2 AND actor_subject=$3 AND idempotency_key=$4").bind(scope.id).bind(&actor.issuer).bind(&actor.subject).bind(key).bind(result).execute(&mut **tx).await.map_err(db_error)?;
    Ok(())
}
pub(super) async fn authoring_audit(
    tx: &mut Transaction<'_, Postgres>,
    scope: &InstallationScope,
    actor: &Identity,
    action: &str,
    entity_type: &str,
    entity: &str,
    version: i64,
) -> Result<Uuid, Error> {
    let operation = Uuid::new_v4();
    sqlx::query("INSERT INTO manager.dashboard_operations(id,installation_id,kind,state) VALUES($1,$2,$3,'Succeeded')").bind(operation).bind(scope.id).bind(action).execute(&mut **tx).await.map_err(db_error)?;
    sqlx::query("INSERT INTO manager.audit_events(id,installation_id,actor_issuer,actor_subject,action,request_id,version,entity_type,entity_id,entity_operation_id,change) VALUES($1,$2,$3,$4,$5,$6,$7,$10,$8,$9,'{}')").bind(Uuid::new_v4()).bind(scope.id).bind(&actor.issuer).bind(&actor.subject).bind(action).bind(Uuid::new_v4()).bind(version).bind(entity).bind(operation).bind(entity_type).execute(&mut **tx).await.map_err(db_error)?;
    Ok(operation)
}
impl PgStore {
    pub async fn dashboard_drafts(
        &self,
        scope: &InstallationScope,
        id: Option<Uuid>,
    ) -> Result<Vec<DraftView>, Error> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        gate(&mut tx, scope).await?;
        let rows=sqlx::query("SELECT id,template_id,definition,version,published_version FROM manager.dashboard_template_drafts WHERE installation_id=$1 AND ($2::uuid IS NULL OR id=$2) ORDER BY id LIMIT 1000").bind(scope.id).bind(id).fetch_all(&mut *tx).await.map_err(db_error)?;
        if id.is_some() && rows.is_empty() {
            return Err(Error::NotFound);
        }
        let items = rows
            .into_iter()
            .map(draft_view)
            .collect::<Result<Vec<_>, _>>()?;
        tx.commit().await.map_err(db_error)?;
        Ok(items)
    }
    pub async fn dashboard_save_draft(
        &self,
        scope: &InstallationScope,
        actor: &VerifiedPrincipal,
        key: &str,
        change: DraftChange,
    ) -> Result<Value, Error> {
        let DraftChange {
            id,
            expected,
            input,
            publish,
        } = change;
        hermes_domain::validate_name(&input.template_id)?;
        input.definition.validate()?;
        if !crate::template_validation::validate(
            &serde_json::to_value(&input.definition).map_err(|_| Error::Invalid)?,
        )
        .is_empty()
        {
            return Err(Error::Invalid);
        }
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        let fingerprint = json!({"command":if publish{"publish"}else{"draft"},"id":id,"expected":expected,"input":input});
        if let Some(result) =
            command_begin(&mut tx, scope, &actor.identity, key, &fingerprint).await?
        {
            tx.commit().await.map_err(db_error)?;
            return Ok(result);
        }
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,928451))")
            .bind(&input.template_id)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        // Infrastructure desired rows never participate in dashboard draft storage.
        let foreign:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM manager.infrastructure_templates WHERE installation_id=$1 AND id=$2) OR (EXISTS(SELECT 1 FROM manager.instance_templates WHERE id=$2) AND NOT EXISTS(SELECT 1 FROM manager.dashboard_template_drafts WHERE installation_id=$1 AND template_id=$2))").bind(scope.id).bind(&input.template_id).fetch_one(&mut *tx).await.map_err(db_error)?;
        if foreign {
            return Err(Error::OwnershipConflict);
        }
        let id = if let Some(id) = id {
            let row=sqlx::query("SELECT version,template_id,published_version FROM manager.dashboard_template_drafts WHERE installation_id=$1 AND id=$2 FOR UPDATE").bind(scope.id).bind(id).fetch_optional(&mut *tx).await.map_err(db_error)?.ok_or(Error::NotFound)?;
            if expected != Some(row.get("version")) {
                return Err(Error::PreconditionFailed);
            }
            if row.get::<Option<i64>, _>("published_version").is_some()
                || row.get::<String, _>("template_id") != input.template_id
            {
                return Err(Error::OwnershipConflict);
            }
            id
        } else {
            if publish || expected.is_some() {
                return Err(Error::Invalid);
            }
            let id = Uuid::new_v4();
            sqlx::query("INSERT INTO manager.dashboard_template_drafts(id,installation_id,template_id,definition) VALUES($1,$2,$3,$4)").bind(id).bind(scope.id).bind(&input.template_id).bind(serde_json::to_value(&input.definition).map_err(|_|Error::Invalid)?).execute(&mut *tx).await.map_err(db_error)?;
            id
        };
        let published = if publish {
            sqlx::query("LOCK TABLE manager.instance_templates IN SHARE ROW EXCLUSIVE MODE")
                .execute(&mut *tx)
                .await
                .map_err(db_error)?;
            let version: i64 = sqlx::query_scalar(
                "SELECT coalesce(max(version),0)+1 FROM manager.instance_templates WHERE id=$1",
            )
            .bind(&input.template_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(db_error)?;
            sqlx::query("INSERT INTO manager.instance_templates(id,version,schema_version,image,content_digest,approved) VALUES($1,$2,1,$3,$4,true)").bind(&input.template_id).bind(version).bind(&input.definition.image).bind(input.definition.digest()?).execute(&mut *tx).await.map_err(db_error)?;
            sqlx::query("INSERT INTO manager.template_definitions(template_id,template_version,definition) VALUES($1,$2,$3)").bind(&input.template_id).bind(version).bind(serde_json::to_value(&input.definition).map_err(|_|Error::Invalid)?).execute(&mut *tx).await.map_err(db_error)?;
            Some(version)
        } else {
            None
        };
        if expected.is_some() {
            sqlx::query("UPDATE manager.dashboard_template_drafts SET definition=$3,version=version+1,published_version=$4 WHERE installation_id=$1 AND id=$2").bind(scope.id).bind(id).bind(serde_json::to_value(&input.definition).map_err(|_|Error::Invalid)?).bind(published).execute(&mut *tx).await.map_err(db_error)?;
        }
        let draft=draft_view(sqlx::query("SELECT id,template_id,definition,version,published_version FROM manager.dashboard_template_drafts WHERE id=$1").bind(id).fetch_one(&mut *tx).await.map_err(db_error)?)?;
        let operation = authoring_audit(
            &mut tx,
            scope,
            &actor.identity,
            if publish {
                "template.publish"
            } else {
                "template.draft.save"
            },
            "template",
            &input.template_id,
            draft.version,
        )
        .await?;
        let result = json!({"draft":draft,"operationId":operation});
        command_finish(&mut tx, scope, &actor.identity, key, &result).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(result)
    }
}
