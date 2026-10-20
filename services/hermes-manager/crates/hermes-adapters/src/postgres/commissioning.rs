use super::*;
use hermes_application::commissioning::{
    CommissionPlan, CommissioningRepository, ConditionReason, Schedule, TemplateDefinition,
};

#[async_trait]
impl CommissioningRepository for PgStore {
    async fn commission_plan(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
    ) -> Result<CommissionPlan, Error> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        lock_claim(&mut tx, scope, claim).await?;
        let row=sqlx::query("SELECT a.id,k.issuer,k.subject,a.access,a.template_id,a.template_version,a.version,i.id AS instance_id,i.namespace,i.namespace_uid,i.desired_generation,i.observed_generation,i.state,t.image,t.content_digest,t.approved,t.schema_version,t.native_port,t.native_scheme,d.definition,o.input FROM manager.assignments a JOIN manager.identities k ON k.id=a.identity_id JOIN manager.instances i ON i.assignment_id=a.id JOIN manager.instance_templates t ON t.id=a.template_id AND t.version=a.template_version LEFT JOIN manager.template_definitions d ON d.template_id=t.id AND d.template_version=t.version JOIN manager.operations o ON o.id=$3 WHERE a.installation_id=$1 AND i.id=$2")
            .bind(scope.id).bind(claim.instance_id).bind(claim.id).fetch_one(&mut *tx).await.map_err(db_error)?;
        if row.get::<String, _>("access") != "enabled" {
            return Err(Error::Forbidden);
        }
        if !row.get::<bool, _>("approved")
            || row.get::<i64, _>("schema_version") != 1
            || row.get::<i32, _>("native_port") != 8080
            || row.get::<String, _>("native_scheme") != "http"
        {
            return Err(Error::TemplateUnavailable);
        }
        let template: TemplateDefinition = serde_json::from_value(
            row.get::<Option<Value>, _>("definition")
                .ok_or(Error::TemplateUnavailable)?,
        )
        .map_err(|_| Error::TemplateUnavailable)?;
        let template_digest = template.digest().map_err(|_| Error::TemplateUnavailable)?;
        if template.image != row.get::<String, _>("image")
            || template_digest != row.get::<String, _>("content_digest")
        {
            return Err(Error::TemplateUnavailable);
        }
        let input: Value = row.get("input");
        if input["access"] != "enabled"
            || input["templateId"] != row.get::<String, _>("template_id")
            || input["templateVersion"] != row.get::<i64, _>("template_version")
        {
            return Err(Error::OwnershipConflict);
        }
        let namespace_uid = row.get("namespace_uid");
        let assignment = assignment_view(row)?;
        assignment.identity.validate(&scope.issuer)?;
        if assignment.namespace != instance_namespace(scope.id, claim.instance_id) {
            return Err(Error::OwnershipConflict);
        }
        tx.commit().await.map_err(db_error)?;
        Ok(CommissionPlan {
            assignment,
            namespace_uid,
            template,
            template_digest,
        })
    }

    async fn schedule(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
        schedule: Schedule,
    ) -> Result<(), Error> {
        let (state, reason, seconds, waiting) = match schedule {
            Schedule::Waiting { reason, seconds } => ("Waiting", reason, seconds, true),
            Schedule::Retry { reason, seconds } => ("RetryAt", reason, seconds, false),
            Schedule::Blocked { reason } => ("Blocked", reason, 0, false),
        };
        if state != "Blocked" && !(1..=300).contains(&seconds) {
            return Err(Error::Invalid);
        }
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        lock_claim(&mut tx, scope, claim).await?;
        let row=sqlx::query("SELECT a.access='enabled' AS enabled,o.kind IN ('commission','retry') AND o.created_at + interval '15 minutes'<=clock_timestamp() AS expired,o.attempt>=o.max_attempts AS exhausted FROM manager.operations o JOIN manager.instances i ON i.id=o.instance_id JOIN manager.assignments a ON a.id=i.assignment_id WHERE o.id=$1")
            .bind(claim.id).fetch_one(&mut *tx).await.map_err(db_error)?;
        let enabled = row.get::<bool, _>("enabled");
        let expired = row.get::<bool, _>("expired");
        let blocked =
            state == "Blocked" || expired || (!waiting && row.get::<bool, _>("exhausted"));
        let reason = if expired {
            ConditionReason::DeadlineExceeded
        } else {
            reason
        };
        let condition =
            json!({"reason":reason,"generation":claim.generation,"operationId":claim.id});
        sqlx::query("UPDATE manager.operations SET state=$2,lease_owner=NULL,lease_until=NULL,attempt=attempt-CASE WHEN $3 THEN 1 ELSE 0 END,next_attempt_at=clock_timestamp()+make_interval(secs=>$4),last_condition=$5 WHERE id=$1")
            .bind(claim.id).bind(if blocked {"Blocked"}else{state}).bind(waiting).bind(f64::from(seconds)).bind(&condition).execute(&mut *tx).await.map_err(db_error)?;
        sqlx::query("UPDATE manager.instances SET state=$2,conditions=jsonb_set(conditions,'{commissioning}',$3),reconcile_after=CASE WHEN $2='Blocked' THEN clock_timestamp()+interval '30 seconds' ELSE reconcile_after END WHERE id=$1")
            .bind(claim.instance_id).bind(if blocked {"Blocked"}else if enabled {"Provisioning"}else{"Suspending"}).bind(condition).execute(&mut *tx).await.map_err(db_error)?;
        tx.commit().await.map_err(db_error)
    }
}
