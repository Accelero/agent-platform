//! Redacted inspection views; journal inputs, credentials and arbitrary results stay private.
use super::*;
use hermes_application::administration::AdministrationReader;
impl PgStore {
    pub async fn dashboard_runtime(
        &self,
        scope: &InstallationScope,
        id: &str,
    ) -> Result<Value, Error> {
        hermes_domain::validate_name(id)?;
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        gate(&mut tx, scope).await?;
        let row=sqlx::query("SELECT definition,content_digest FROM manager.terminal_runtime_versions WHERE id=$1 ORDER BY version DESC LIMIT 1").bind(id).fetch_optional(&mut *tx).await.map_err(db_error)?.ok_or(Error::NotFound)?;
        let value: Value = row.get("definition");
        let definition: hermes_application::execution::RuntimeDefinition =
            serde_json::from_value(value.clone()).map_err(|_| Error::RecoveryRequired)?;
        if definition.digest()? != row.get::<String, _>("content_digest") {
            return Err(Error::OwnershipConflict);
        }
        tx.commit().await.map_err(db_error)?;
        Ok(value)
    }
    pub async fn dashboard_identity(
        &self,
        scope: &InstallationScope,
        id: Uuid,
    ) -> Result<Value, Error> {
        let result = self
            .dashboard_list(
                scope,
                super::dashboard::DashboardList::Identities,
                &super::dashboard::DashboardQuery {
                    limit: 1,
                    identity_id: Some(id),
                    ..Default::default()
                },
            )
            .await?;
        result["items"]
            .as_array()
            .and_then(|v| v.first())
            .cloned()
            .ok_or(Error::NotFound)
    }
    pub async fn dashboard_operation_details(
        &self,
        scope: &InstallationScope,
        id: Uuid,
    ) -> Result<Value, Error> {
        let operation = self.operation(scope, id).await?;
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        gate(&mut tx, scope).await?;
        let assignment:Option<Uuid>=sqlx::query_scalar("SELECT i.assignment_id FROM manager.operations o JOIN manager.instances i ON i.id=o.instance_id WHERE o.installation_id=$1 AND o.id=$2").bind(scope.id).bind(id).fetch_optional(&mut *tx).await.map_err(db_error)?;
        let rows=sqlx::query("SELECT step_key,extract(epoch FROM recorded_at)::bigint AS recorded_at,extract(epoch FROM verified_at)::bigint AS verified_at FROM manager.operation_steps WHERE operation_id=$1 ORDER BY step_key LIMIT 100").bind(id).fetch_all(&mut *tx).await.map_err(db_error)?;
        let steps=rows.into_iter().map(|r|json!({"key":r.get::<String,_>("step_key"),"recordedAt":r.get::<i64,_>("recorded_at"),"verifiedAt":r.get::<Option<i64>,_>("verified_at")})).collect::<Vec<_>>();
        tx.commit().await.map_err(db_error)?;
        Ok(json!({"operation":operation,"assignmentId":assignment,"steps":steps}))
    }
    pub async fn dashboard_audit_detail(
        &self,
        scope: &InstallationScope,
        id: Uuid,
    ) -> Result<Value, Error> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        gate(&mut tx, scope).await?;
        let row=sqlx::query("SELECT e.*,coalesce(e.operation_id,e.entity_operation_id) AS related_operation,CASE WHEN e.execution_event THEN 'Recorded' ELSE coalesce(o.state,d.state,'Succeeded') END AS outcome,extract(epoch FROM e.occurred_at)::bigint AS occurred FROM manager.dashboard_activity e LEFT JOIN manager.operations o ON o.id=e.operation_id LEFT JOIN manager.dashboard_operations d ON d.id=e.entity_operation_id WHERE e.installation_id=$1 AND e.id=$2").bind(scope.id).bind(id).fetch_optional(&mut *tx).await.map_err(db_error)?.ok_or(Error::NotFound)?;
        let raw: Value = row.get("change");
        let mut change = serde_json::Map::new();
        for key in [
            "access",
            "templateId",
            "templateVersion",
            "runtimeId",
            "version",
            "storageBindingId",
            "instanceId",
            "assignmentId",
            "generation",
            "digest",
            "pullSecretName",
        ] {
            if let Some(value) = raw.get(key) {
                change.insert(key.into(), value.clone());
            }
        }
        let event = json!({"id":id,"actor":{"issuer":row.get::<String,_>("actor_issuer"),"subject":row.get::<String,_>("actor_subject")},"action":row.get::<String,_>("action"),"assignmentId":row.get::<Option<Uuid>,_>("assignment_id"),"entityType":row.get::<Option<String>,_>("entity_type"),"entityId":row.get::<Option<String>,_>("entity_id"),"operationId":row.get::<Option<Uuid>,_>("related_operation"),"requestId":row.get::<Uuid,_>("request_id"),"version":row.get::<i64,_>("version"),"occurredAt":row.get::<i64,_>("occurred"),"outcome":row.get::<String,_>("outcome")});
        tx.commit().await.map_err(db_error)?;
        Ok(json!({"event":event,"change":change,"redacted":true}))
    }
    pub async fn dashboard_template_version(
        &self,
        scope: &InstallationScope,
        id: &str,
        version: i64,
    ) -> Result<Value, Error> {
        hermes_domain::validate_name(id)?;
        if version < 1 {
            return Err(Error::Invalid);
        }
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        gate(&mut tx, scope).await?;
        let row=sqlx::query("SELECT d.definition,t.content_digest FROM manager.instance_templates t JOIN manager.template_definitions d ON d.template_id=t.id AND d.template_version=t.version WHERE t.id=$1 AND t.version=$2 AND t.approved").bind(id).bind(version).fetch_optional(&mut *tx).await.map_err(db_error)?.ok_or(Error::NotFound)?;
        let mut definition: Value = row.get("definition");
        let mut redacted = Vec::new();
        let typed: hermes_application::commissioning::TemplateDefinition =
            serde_json::from_value(definition.clone()).map_err(|_| Error::TemplateUnavailable)?;
        typed.validate()?;
        if typed.digest()? != row.get::<String, _>("content_digest") {
            return Err(Error::OwnershipConflict);
        }
        if let Some(config) = definition.get_mut("adminConfig") {
            super::dashboard::redact_configuration(config, "/adminConfig", &mut redacted);
        }
        tx.commit().await.map_err(db_error)?;
        Ok(
            json!({"id":id,"version":version,"digest":row.get::<String,_>("content_digest"),"definition":definition,"redactedPaths":redacted}),
        )
    }
}

impl PgStore {
    pub async fn dashboard_template_observation(
        &self,
        scope: &InstallationScope,
        id: Uuid,
    ) -> Result<Value, Error> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        gate(&mut tx, scope).await?;
        let row=sqlx::query("SELECT a.template_id,a.template_version,i.desired_generation,i.observed_generation,i.state,i.conditions,o.input AS observed_input FROM manager.assignments a JOIN manager.instances i ON i.assignment_id=a.id LEFT JOIN manager.operations o ON o.instance_id=i.id AND o.generation=i.observed_generation AND o.epoch=$3 WHERE a.installation_id=$1 AND a.id=$2").bind(scope.id).bind(id).bind(scope.epoch).fetch_optional(&mut *tx).await.map_err(db_error)?.ok_or(Error::NotFound)?;
        let observed = row
            .get::<Option<Value>, _>("observed_input")
            .and_then(|value| {
                let id = value["templateId"].as_str()?;
                let version = value["templateVersion"].as_i64()?;
                if hermes_domain::validate_name(id).is_err() || version < 1 {
                    return None;
                }
                Some(json!({"id":id,"version":version}))
            });
        let conditions: Value = row.get("conditions");
        let mut safe = serde_json::Map::new();
        for key in ["commissioning", "execution", "lifecycle"] {
            if let Some(reason) = conditions[key]["reason"].as_str() {
                safe.insert(
                    key.into(),
                    json!(match reason {
                        "native_ready"
                        | "dependency_unavailable"
                        | "ownership_conflict"
                        | "initialization_uncertain"
                        | "policy_unavailable" => reason,
                        _ => "review_required",
                    }),
                );
            }
        }
        let value = json!({"conditions":safe,"desiredTemplate":{"id":row.get::<String,_>("template_id"),"version":row.get::<i64,_>("template_version")},"observedTemplate":observed,"desiredGeneration":row.get::<i64,_>("desired_generation"),"observedGeneration":row.get::<i64,_>("observed_generation"),"state":row.get::<String,_>("state")});
        tx.commit().await.map_err(db_error)?;
        Ok(value)
    }
}
