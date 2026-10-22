//! Explicit pin changes share existing generation fencing, journal and drain rules.
use super::authoring::{command_begin, command_finish};
use super::*;
use hermes_application::{VerifiedPrincipal, commissioning::TemplateDefinition};
use serde::{Deserialize, Serialize};
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TemplateSelection {
    pub template_id: String,
    pub template_version: i64,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BatchMember {
    pub assignment_id: Uuid,
    pub expected_version: i64,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TemplateBatch {
    pub selection: TemplateSelection,
    pub members: Vec<BatchMember>,
}
async fn target(
    tx: &mut Transaction<'_, Postgres>,
    selection: &TemplateSelection,
) -> Result<TemplateDefinition, Error> {
    hermes_domain::validate_name(&selection.template_id)?;
    if selection.template_version < 1 {
        return Err(Error::Invalid);
    }
    let row=sqlx::query("SELECT d.definition,c.content_digest FROM manager.instance_templates c JOIN manager.template_definitions d ON d.template_id=c.id AND d.template_version=c.version WHERE c.id=$1 AND c.version=$2 AND c.approved AND c.schema_version=1").bind(&selection.template_id).bind(selection.template_version).fetch_optional(&mut **tx).await.map_err(db_error)?.ok_or(Error::TemplateUnavailable)?;
    let value: Value = row.get("definition");
    if !crate::template_validation::validate(&value).is_empty() {
        return Err(Error::TemplateUnavailable);
    }
    let definition: TemplateDefinition =
        serde_json::from_value(value).map_err(|_| Error::TemplateUnavailable)?;
    if definition.digest()? != row.get::<String, _>("content_digest") {
        return Err(Error::OwnershipConflict);
    }
    Ok(definition)
}
fn migration(current: &TemplateDefinition, proposed: &TemplateDefinition) -> bool {
    current.storage_class != proposed.storage_class
        || proposed.home_gib < current.home_gib
        || proposed.workspace_gib < current.workspace_gib
}
async fn preview(
    tx: &mut Transaction<'_, Postgres>,
    scope: &InstallationScope,
    id: Uuid,
    selection: &TemplateSelection,
) -> Result<Value, Error> {
    let proposed = target(tx, selection).await?;
    let row=sqlx::query("SELECT a.version,a.access,a.infrastructure_managed,a.template_id,a.template_version,d.definition,i.namespace_uid FROM manager.assignments a JOIN manager.instances i ON i.assignment_id=a.id LEFT JOIN manager.template_definitions d ON d.template_id=a.template_id AND d.template_version=a.template_version WHERE a.installation_id=$1 AND a.id=$2").bind(scope.id).bind(id).fetch_optional(&mut **tx).await.map_err(db_error)?.ok_or(Error::NotFound)?;
    let current: Option<TemplateDefinition> = row
        .get::<Option<Value>, _>("definition")
        .map(serde_json::from_value)
        .transpose()
        .map_err(|_| Error::TemplateUnavailable)?;
    let mut blocked = Vec::new();
    if row.get::<bool, _>("infrastructure_managed") {
        blocked.push("infrastructure_owned");
    }
    if current
        .as_ref()
        .is_some_and(|current| migration(current, &proposed))
        || (current.is_none() && row.get::<Option<String>, _>("namespace_uid").is_some())
    {
        blocked.push("storage_migration_required");
    }
    let current = if let Some(current) = current {
        let value = serde_json::to_value(current).map_err(|_| Error::Invalid)?;
        if !crate::template_validation::validate(&value).is_empty() {
            return Err(Error::TemplateUnavailable);
        }
        Some(value)
    } else {
        None
    };
    Ok(
        json!({"assignmentId":id,"expectedVersion":row.get::<i64,_>("version"),"currentTemplate":{"id":row.get::<String,_>("template_id"),"version":row.get::<i64,_>("template_version")},"proposedTemplate":{"id":selection.template_id,"version":selection.template_version},"currentDefinition":current,"proposedDefinition":proposed,"restart":row.get::<String,_>("access")=="enabled","eligible":blocked.is_empty(),"blockedReasons":blocked}),
    )
}
impl PgStore {
    pub async fn dashboard_template_preview(
        &self,
        scope: &InstallationScope,
        id: Uuid,
        selection: &TemplateSelection,
    ) -> Result<Value, Error> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        gate(&mut tx, scope).await?;
        let value = preview(&mut tx, scope, id, selection).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(value)
    }
    pub async fn dashboard_apply_template(
        &self,
        scope: &InstallationScope,
        actor: &VerifiedPrincipal,
        key: &str,
        member: &BatchMember,
        selection: &TemplateSelection,
    ) -> Result<Value, Error> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        if let Some(result) = command_begin(
            &mut tx,
            scope,
            &actor.identity,
            key,
            &json!({"command":"assignment.set_template","member":member,"selection":selection}),
        )
        .await?
        {
            tx.commit().await.map_err(db_error)?;
            return Ok(result);
        }
        let row=sqlx::query("SELECT a.version,i.id FROM manager.assignments a JOIN manager.instances i ON i.assignment_id=a.id WHERE a.installation_id=$1 AND a.id=$2 FOR UPDATE OF a,i").bind(scope.id).bind(member.assignment_id).fetch_optional(&mut *tx).await.map_err(db_error)?.ok_or(Error::NotFound)?;
        if row.get::<i64, _>("version") != member.expected_version {
            return Err(Error::PreconditionFailed);
        }
        let inspected = preview(&mut tx, scope, member.assignment_id, selection).await?;
        if inspected["blockedReasons"]
            .as_array()
            .is_some_and(|reasons| reasons.iter().any(|v| v == "infrastructure_owned"))
        {
            return Err(Error::OwnershipConflict);
        }
        if inspected["eligible"] != true {
            return Err(Error::MigrationRequired);
        }
        let instance: Uuid = row.get("id");
        let uncertain:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM manager.owned_resources WHERE instance_id=$1 AND state='Reserved')").bind(instance).fetch_one(&mut *tx).await.map_err(db_error)?;
        if uncertain {
            return Err(Error::OwnershipConflict);
        }
        advance_instance(&mut tx, instance).await?;
        let version:i64=sqlx::query_scalar("UPDATE manager.assignments SET template_id=$2,template_version=$3,version=version+1,updated_at=clock_timestamp(),dashboard_managed=true,dashboard_definition=$4 WHERE id=$1 RETURNING version").bind(member.assignment_id).bind(&selection.template_id).bind(selection.template_version).bind(&inspected["proposedDefinition"]).fetch_one(&mut *tx).await.map_err(db_error)?;
        let current=sqlx::query("SELECT a.access,i.desired_generation FROM manager.assignments a JOIN manager.instances i ON i.assignment_id=a.id WHERE a.id=$1").bind(member.assignment_id).fetch_one(&mut *tx).await.map_err(db_error)?;
        let access: String = current.get("access");
        let kind = match access.as_str() {
            "enabled" => "commission",
            "suspended" => "suspend",
            _ => "revoke",
        };
        let operation = Uuid::new_v4();
        let input = json!({"access":access,"templateId":selection.template_id,"templateVersion":selection.template_version});
        sqlx::query("INSERT INTO manager.operations(id,installation_id,instance_id,kind,generation,epoch,input) VALUES($1,$2,$3,$4,$5,$6,$7)").bind(operation).bind(scope.id).bind(instance).bind(kind).bind(current.get::<i64,_>("desired_generation")).bind(scope.epoch).bind(&input).execute(&mut *tx).await.map_err(db_error)?;
        sqlx::query("WITH tick AS MATERIALIZED(SELECT clock_timestamp() AS now) UPDATE manager.instances SET drain_generation=desired_generation,drain_epoch=$2,drain_started_at=tick.now,drain_deadline_at=tick.now+interval '5 minutes',drain_completed_at=NULL FROM tick WHERE id=$1 AND (drain_started_at IS NULL OR drain_completed_at IS NOT NULL)").bind(instance).bind(scope.epoch).execute(&mut *tx).await.map_err(db_error)?;
        sqlx::query("INSERT INTO manager.audit_events(id,installation_id,actor_issuer,actor_subject,action,assignment_id,operation_id,request_id,version,change) VALUES($1,$2,$3,$4,'assignment.set_template',$5,$6,$7,$8,$9)").bind(Uuid::new_v4()).bind(scope.id).bind(&actor.identity.issuer).bind(&actor.identity.subject).bind(member.assignment_id).bind(operation).bind(Uuid::new_v4()).bind(version).bind(input).execute(&mut *tx).await.map_err(db_error)?;
        let result = json!({"assignmentId":member.assignment_id,"instanceId":instance,"operationId":operation,"version":version});
        command_finish(&mut tx, scope, &actor.identity, key, &result).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(result)
    }
    pub async fn dashboard_batch(
        &self,
        scope: &InstallationScope,
        id: Uuid,
    ) -> Result<Value, Error> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        gate(&mut tx, scope).await?;
        let batch=sqlx::query("SELECT template_id,template_version,actor_issuer,actor_subject FROM manager.dashboard_template_batches WHERE installation_id=$1 AND id=$2").bind(scope.id).bind(id).fetch_optional(&mut *tx).await.map_err(db_error)?.ok_or(Error::NotFound)?;
        let rows=sqlx::query("SELECT assignment_id,expected_version,state,operation_id,error_code FROM manager.dashboard_template_batch_items WHERE batch_id=$1 ORDER BY assignment_id LIMIT 50").bind(id).fetch_all(&mut *tx).await.map_err(db_error)?;
        let items=rows.into_iter().map(|row|json!({"assignmentId":row.get::<Uuid,_>("assignment_id"),"expectedVersion":row.get::<i64,_>("expected_version"),"state":row.get::<String,_>("state"),"operationId":row.get::<Option<Uuid>,_>("operation_id"),"errorCode":row.get::<Option<String>,_>("error_code")})).collect::<Vec<_>>();
        let state = if items.iter().any(|v| v["state"] == "Pending") {
            "Pending"
        } else if items.iter().any(|v| v["state"] == "Failed") {
            "Failed"
        } else {
            "Succeeded"
        };
        tx.commit().await.map_err(db_error)?;
        Ok(
            json!({"id":id,"state":state,"items":items,"selection":{"templateId":batch.get::<String,_>("template_id"),"templateVersion":batch.get::<i64,_>("template_version")},"actor":{"issuer":batch.get::<String,_>("actor_issuer"),"subject":batch.get::<String,_>("actor_subject")}}),
        )
    }
    pub async fn dashboard_start_batch(
        &self,
        scope: &InstallationScope,
        actor: &VerifiedPrincipal,
        key: &str,
        input: &TemplateBatch,
    ) -> Result<Uuid, Error> {
        if input.members.is_empty()
            || input.members.len() > 50
            || input
                .members
                .iter()
                .any(|member| member.expected_version < 1)
            || input
                .members
                .iter()
                .map(|v| v.assignment_id)
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != input.members.len()
        {
            return Err(Error::Invalid);
        }
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        if let Some(result) = command_begin(
            &mut tx,
            scope,
            &actor.identity,
            key,
            &json!({"command":"batch.apply","input":input}),
        )
        .await?
        {
            tx.commit().await.map_err(db_error)?;
            return serde_json::from_value(result["id"].clone())
                .map_err(|_| Error::RecoveryRequired);
        }
        target(&mut tx, &input.selection).await?;
        let id = super::authoring::authoring_audit(
            &mut tx,
            scope,
            &actor.identity,
            "batch.apply",
            "template_batch",
            &input.selection.template_id,
            1,
        )
        .await?;
        sqlx::query("UPDATE manager.dashboard_operations SET state='Pending' WHERE id=$1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        sqlx::query("INSERT INTO manager.dashboard_template_batches(id,installation_id,template_id,template_version,actor_issuer,actor_subject) VALUES($1,$2,$3,$4,$5,$6)").bind(id).bind(scope.id).bind(&input.selection.template_id).bind(input.selection.template_version).bind(&actor.identity.issuer).bind(&actor.identity.subject).execute(&mut *tx).await.map_err(db_error)?;
        for member in &input.members {
            let exists:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM manager.assignments WHERE installation_id=$1 AND id=$2)").bind(scope.id).bind(member.assignment_id).fetch_one(&mut *tx).await.map_err(db_error)?;
            if !exists {
                return Err(Error::NotFound);
            }
            sqlx::query("INSERT INTO manager.dashboard_template_batch_items(batch_id,assignment_id,expected_version) VALUES($1,$2,$3)").bind(id).bind(member.assignment_id).bind(member.expected_version).execute(&mut *tx).await.map_err(db_error)?;
        }
        command_finish(&mut tx, scope, &actor.identity, key, &json!({"id":id})).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(id)
    }
    pub async fn dashboard_dispatch_batch(
        &self,
        scope: &InstallationScope,
        actor: &VerifiedPrincipal,
        id: Uuid,
        input: &TemplateBatch,
    ) -> Result<(), Error> {
        for member in &input.members {
            let pending:bool=sqlx::query_scalar("SELECT state='Pending' FROM manager.dashboard_template_batch_items WHERE batch_id=$1 AND assignment_id=$2").bind(id).bind(member.assignment_id).fetch_one(&self.pool).await.map_err(db_error)?;
            if !pending {
                continue;
            }
            let result = self
                .dashboard_apply_template(
                    scope,
                    actor,
                    &format!("{id}:{}", member.assignment_id),
                    member,
                    &input.selection,
                )
                .await;
            let (operation, code) = match result {
                Ok(value) => (
                    Some(
                        serde_json::from_value::<Uuid>(value["operationId"].clone())
                            .map_err(|_| Error::RecoveryRequired)?,
                    ),
                    None,
                ),
                Err(error) => (
                    None,
                    Some(match error {
                        Error::PreconditionFailed => "stale_version",
                        Error::OwnershipConflict => "ownership_conflict",
                        Error::MigrationRequired => "storage_migration_required",
                        Error::Forbidden => "access_denied",
                        _ => "dependency_unavailable",
                    }),
                ),
            };
            if code == Some("dependency_unavailable") {
                continue;
            }
            sqlx::query("UPDATE manager.dashboard_template_batch_items SET state=$3,operation_id=$4,error_code=$5 WHERE batch_id=$1 AND assignment_id=$2 AND state='Pending'").bind(id).bind(member.assignment_id).bind(if operation.is_some(){"Accepted"}else{"Failed"}).bind(operation).bind(code).execute(&self.pool).await.map_err(db_error)?;
        }
        let result = self.dashboard_batch(scope, id).await?;
        sqlx::query(
            "UPDATE manager.dashboard_operations SET state=$2 WHERE id=$1 AND installation_id=$3",
        )
        .bind(id)
        .bind(result["state"].as_str().ok_or(Error::RecoveryRequired)?)
        .bind(scope.id)
        .execute(&self.pool)
        .await
        .map_err(db_error)?;
        Ok(())
    }
}
impl PgStore {
    pub async fn dashboard_resume_batch(
        &self,
        scope: &InstallationScope,
        actor: &VerifiedPrincipal,
        id: Uuid,
    ) -> Result<TemplateBatch, Error> {
        self.authorize_administrator(scope, &actor.identity).await?;
        let value = self.dashboard_batch(scope, id).await?;
        if value["actor"] != serde_json::to_value(&actor.identity).map_err(|_| Error::Invalid)? {
            return Err(Error::Forbidden);
        }
        let members=value["items"].as_array().ok_or(Error::RecoveryRequired)?.iter().map(|item|serde_json::from_value(json!({"assignmentId":item["assignmentId"],"expectedVersion":item["expectedVersion"]})).map_err(|_|Error::RecoveryRequired)).collect::<Result<Vec<_>,_>>()?;
        Ok(TemplateBatch {
            selection: serde_json::from_value(value["selection"].clone())
                .map_err(|_| Error::RecoveryRequired)?,
            members,
        })
    }
}
