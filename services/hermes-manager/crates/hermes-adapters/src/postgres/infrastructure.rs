//! Poll infrastructure-owned desired mappings. Notifications are never required
//! for correctness; snapshot, generation fencing and outbox commit together.
use super::*;
use hermes_application::commissioning::TemplateDefinition;

impl PgStore {
    pub async fn complete_native_drain(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
    ) -> Result<bool, Error> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        lock_claim(&mut tx, scope, claim).await?;
        let valid:bool=sqlx::query_scalar("SELECT a.infrastructure_managed AND a.access='enabled' AND NOT EXISTS(SELECT 1 FROM manager.execution_environments e WHERE e.instance_id=i.id AND e.state<>'Retired') AND NOT EXISTS(SELECT 1 FROM manager.native_children c WHERE c.instance_id=i.id AND NOT EXISTS(SELECT 1 FROM manager.native_child_actions n WHERE n.pod_uid=c.pod_uid AND n.outcome='absent')) AND (NOT EXISTS(SELECT 1 FROM manager.owned_resources r WHERE r.instance_id=i.id AND r.kind='Deployment' AND r.state<>'Deleted') OR EXISTS(SELECT 1 FROM manager.native_stop_inputs n JOIN manager.operation_steps s USING(operation_id,step_key) WHERE n.operation_id=$2 AND s.verified_at IS NOT NULL)) FROM manager.assignments a JOIN manager.instances i ON i.assignment_id=a.id WHERE i.id=$1")
            .bind(claim.instance_id).bind(claim.id).fetch_one(&mut *tx).await.map_err(db_error)?;
        if !valid {
            return Ok(false);
        }
        sqlx::query("UPDATE manager.instances SET drain_completed_at=coalesce(drain_completed_at,clock_timestamp()) WHERE id=$1 AND drain_started_at IS NOT NULL")
            .bind(claim.instance_id).execute(&mut *tx).await.map_err(db_error)?;
        tx.commit().await.map_err(db_error)?;
        Ok(true)
    }
    pub async fn complete_native_removal(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
    ) -> Result<bool, Error> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        lock_claim(&mut tx, scope, claim).await?;
        let valid: bool = sqlx::query_scalar("SELECT a.infrastructure_managed AND a.access='revoked' AND NOT EXISTS(SELECT 1 FROM manager.owned_resources r WHERE r.instance_id=i.id AND r.kind='Deployment' AND r.state<>'Deleted') AND NOT EXISTS(SELECT 1 FROM manager.native_children c WHERE c.instance_id=i.id AND NOT EXISTS(SELECT 1 FROM manager.native_child_actions n WHERE n.pod_uid=c.pod_uid AND n.outcome='absent')) AND NOT EXISTS(SELECT 1 FROM manager.execution_environments e WHERE e.instance_id=i.id AND e.state<>'Retired') FROM manager.assignments a JOIN manager.instances i ON i.assignment_id=a.id WHERE i.id=$1")
            .bind(claim.instance_id).fetch_one(&mut *tx).await.map_err(db_error)?;
        if !valid {
            return Ok(false);
        }
        sqlx::query("UPDATE manager.instances SET state='Retained',observed_generation=desired_generation,drain_completed_at=coalesce(drain_completed_at,clock_timestamp()),reconcile_after=clock_timestamp()+interval '30 seconds' WHERE id=$1")
            .bind(claim.instance_id).execute(&mut *tx).await.map_err(db_error)?;
        sqlx::query("UPDATE manager.operations SET state='Succeeded',lease_owner=NULL,lease_until=NULL WHERE id=$1")
            .bind(claim.id).execute(&mut *tx).await.map_err(db_error)?;
        tx.commit().await.map_err(db_error)?;
        Ok(true)
    }
    /// Bounded batches under a database lock also serialize multiple replicas.
    /// Infrastructure rows contain no personal seeds or executable Pod specs.
    pub async fn reconcile_infrastructure(&self, scope: &InstallationScope) -> Result<u16, Error> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        gate(&mut tx, scope).await?;
        // A crash on the final permitted attempt can exhaust a claim before
        // schedule() records its condition. Recover only transient infra work;
        // preserve all ownership/admission/manual-recovery blocks.
        sqlx::query("UPDATE manager.instances i SET state='Blocked',conditions=jsonb_set(i.conditions,'{commissioning}',jsonb_build_object('reason','dependency_unavailable','generation',i.desired_generation)),reconcile_after=clock_timestamp()+interval '30 seconds' FROM manager.assignments a WHERE a.id=i.assignment_id AND a.installation_id=$1 AND a.infrastructure_managed AND i.state='Provisioning' AND EXISTS(SELECT 1 FROM manager.operations o WHERE o.instance_id=i.id AND o.generation=i.desired_generation AND o.state='Blocked' AND (o.last_condition IS NULL OR o.last_condition->>'reason'='dependency_unavailable')) AND NOT EXISTS(SELECT 1 FROM manager.operations o WHERE o.instance_id=i.id AND o.state IN ('Pending','Running','Waiting','RetryAt'))")
            .bind(scope.id).execute(&mut *tx).await.map_err(db_error)?;
        sqlx::query("LOCK TABLE manager.desired_users IN SHARE ROW EXCLUSIVE MODE")
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        let rows = sqlx::query("SELECT d.subject,d.template_id,t.definition,a.id AS assignment_id,a.infrastructure_managed FROM manager.desired_users d JOIN manager.infrastructure_templates t ON t.installation_id=d.installation_id AND t.id=d.template_id LEFT JOIN manager.identities k ON k.issuer=$2 AND k.subject=d.subject LEFT JOIN manager.assignments a ON a.installation_id=d.installation_id AND a.identity_id=k.id WHERE d.installation_id=$1 AND t.definition IS DISTINCT FROM t.rejected_definition AND (a.id IS NULL OR (a.infrastructure_managed AND (a.access<>'enabled' OR a.infrastructure_definition IS DISTINCT FROM jsonb_build_object('templateId',d.template_id,'definition',t.definition)))) ORDER BY d.subject LIMIT 64")
            .bind(scope.id).bind(&scope.issuer).fetch_all(&mut *tx).await.map_err(db_error)?;
        let mut count = 0;
        for row in rows {
            let subject: String = row.get("subject");
            Identity {
                issuer: scope.issuer.clone(),
                subject: subject.clone(),
            }
            .validate(&scope.issuer)?;
            let template_id: String = row.get("template_id");
            let definition: Value = row.get("definition");
            let parsed = serde_json::from_value::<TemplateDefinition>(definition.clone())
                .map_err(|_| Error::TemplateUnavailable)
                .and_then(|typed| typed.digest().map(|digest| (typed, digest)));
            let (typed, digest) = match parsed {
                Ok(parsed) => parsed,
                Err(_) => {
                    sqlx::query("UPDATE manager.infrastructure_templates SET rejected_definition=$3,condition='invalid_definition' WHERE installation_id=$1 AND id=$2 AND definition=$3")
                        .bind(scope.id).bind(&template_id).bind(&definition).execute(&mut *tx).await.map_err(db_error)?;
                    continue;
                }
            };
            sqlx::query("UPDATE manager.infrastructure_templates SET rejected_definition=NULL,condition=NULL WHERE installation_id=$1 AND id=$2 AND definition=$3")
                .bind(scope.id).bind(&template_id).bind(&definition).execute(&mut *tx).await.map_err(db_error)?;
            // The catalog is immutable. A changed infra definition becomes a
            // new version, never an in-place rewrite of a running generation.
            sqlx::query("LOCK TABLE manager.instance_templates IN SHARE ROW EXCLUSIVE MODE")
                .execute(&mut *tx)
                .await
                .map_err(db_error)?;
            let mut version: Option<i64> = sqlx::query_scalar("SELECT version FROM manager.instance_templates WHERE id=$1 AND content_digest=$2 AND approved ORDER BY version DESC LIMIT 1")
                .bind(&template_id).bind(&digest).fetch_optional(&mut *tx).await.map_err(db_error)?;
            if version.is_none() {
                let next: i64 = sqlx::query_scalar(
                    "SELECT coalesce(max(version),0)+1 FROM manager.instance_templates WHERE id=$1",
                )
                .bind(&template_id)
                .fetch_one(&mut *tx)
                .await
                .map_err(db_error)?;
                sqlx::query("INSERT INTO manager.instance_templates(id,version,schema_version,image,content_digest,approved) VALUES($1,$2,1,$3,$4,true)")
                    .bind(&template_id).bind(next).bind(&typed.image).bind(&digest).execute(&mut *tx).await.map_err(db_error)?;
                sqlx::query("INSERT INTO manager.template_definitions(template_id,template_version,definition) VALUES($1,$2,$3)")
                    .bind(&template_id).bind(next).bind(serde_json::to_value(&typed).map_err(|_|Error::Invalid)?).execute(&mut *tx).await.map_err(db_error)?;
                version = Some(next);
            }
            let snapshot = json!({"templateId":template_id,"definition":definition});
            let assignment: Option<Uuid> = row.get("assignment_id");
            let (assignment_id, instance_id) = if let Some(assignment_id) = assignment {
                let instance_id: Uuid = sqlx::query_scalar("SELECT i.id FROM manager.assignments a JOIN manager.instances i ON i.assignment_id=a.id WHERE a.id=$1 FOR UPDATE OF a,i")
                    .bind(assignment_id).fetch_one(&mut *tx).await.map_err(db_error)?;
                if uncertain_creation(&mut tx, instance_id).await? {
                    // Recover the original frozen CREATE before changing its
                    // generation. Never reinterpret an unbound old object as
                    // a new template, or abandon an uncertain name reservation.
                    continue;
                }
                advance_instance(&mut tx, instance_id).await?;
                sqlx::query("UPDATE manager.assignments SET access='enabled',template_id=$2,template_version=$3,version=version+1,updated_at=clock_timestamp(),infrastructure_definition=$4 WHERE id=$1")
                    .bind(assignment_id).bind(&template_id).bind(version).bind(snapshot).execute(&mut *tx).await.map_err(db_error)?;
                (assignment_id, instance_id)
            } else {
                sqlx::query("INSERT INTO manager.identities(id,issuer,subject) VALUES($1,$2,$3) ON CONFLICT(issuer,subject) DO NOTHING")
                    .bind(Uuid::new_v4()).bind(&scope.issuer).bind(&subject).execute(&mut *tx).await.map_err(db_error)?;
                let identity: Uuid = sqlx::query_scalar(
                    "SELECT id FROM manager.identities WHERE issuer=$1 AND subject=$2",
                )
                .bind(&scope.issuer)
                .bind(&subject)
                .fetch_one(&mut *tx)
                .await
                .map_err(db_error)?;
                let assignment_id = Uuid::new_v4();
                let instance_id = Uuid::new_v4();
                sqlx::query("INSERT INTO manager.assignments(id,installation_id,identity_id,access,template_id,template_version,version,infrastructure_managed,infrastructure_definition) VALUES($1,$2,$3,'enabled',$4,$5,1,true,$6)")
                    .bind(assignment_id).bind(scope.id).bind(identity).bind(&template_id).bind(version).bind(snapshot).execute(&mut *tx).await.map_err(db_error)?;
                sqlx::query("INSERT INTO manager.instances(id,assignment_id,namespace,desired_generation) VALUES($1,$2,$3,1)")
                    .bind(instance_id).bind(assignment_id).bind(instance_namespace(scope.id,instance_id)).execute(&mut *tx).await.map_err(db_error)?;
                (assignment_id, instance_id)
            };
            enqueue(&mut tx, scope, assignment_id, instance_id, "commission").await?;
            count += 1;
        }
        // A removed desired entry queues retirement once uncertain creation is
        // settled. Retain ownership, data, credentials and deletion work.
        let removed = sqlx::query("SELECT a.id,i.id AS instance_id FROM manager.assignments a JOIN manager.identities k ON k.id=a.identity_id JOIN manager.instances i ON i.assignment_id=a.id WHERE a.installation_id=$1 AND a.infrastructure_managed AND a.access<>'revoked' AND NOT EXISTS(SELECT 1 FROM manager.desired_users d WHERE d.installation_id=a.installation_id AND d.subject=k.subject) ORDER BY a.id FOR UPDATE OF a,i SKIP LOCKED LIMIT 64")
            .bind(scope.id).fetch_all(&mut *tx).await.map_err(db_error)?;
        for row in removed {
            let assignment_id: Uuid = row.get("id");
            let instance_id: Uuid = row.get("instance_id");
            if uncertain_creation(&mut tx, instance_id).await? {
                continue;
            }
            advance_instance(&mut tx, instance_id).await?;
            sqlx::query("UPDATE manager.assignments SET access='revoked',version=version+1,updated_at=clock_timestamp() WHERE id=$1")
                .bind(assignment_id).execute(&mut *tx).await.map_err(db_error)?;
            sqlx::query("WITH tick AS MATERIALIZED(SELECT clock_timestamp() AS now) UPDATE manager.instances SET drain_generation=desired_generation,drain_epoch=$2,drain_started_at=tick.now,drain_deadline_at=tick.now+interval '5 minutes',drain_completed_at=NULL FROM tick WHERE id=$1 AND (drain_started_at IS NULL OR drain_completed_at IS NOT NULL)")
                .bind(instance_id).bind(scope.epoch).execute(&mut *tx).await.map_err(db_error)?;
            enqueue(&mut tx, scope, assignment_id, instance_id, "revoke").await?;
            count += 1;
        }
        tx.commit().await.map_err(db_error)?;
        Ok(count)
    }

    pub async fn infrastructure_managed(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
    ) -> Result<bool, Error> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        lock_claim(&mut tx, scope, claim).await?;
        let managed = sqlx::query_scalar("SELECT a.infrastructure_managed FROM manager.assignments a JOIN manager.instances i ON i.assignment_id=a.id WHERE i.id=$1")
            .bind(claim.instance_id).fetch_one(&mut *tx).await.map_err(db_error)?;
        tx.commit().await.map_err(db_error)?;
        Ok(managed)
    }

    /// Called only after native storage, Pod/init receipt, owner and HTTP gate
    /// observations. No routing record or execution-policy receipt is published.
    pub async fn complete_native_commissioning(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
    ) -> Result<(), Error> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        lock_claim(&mut tx, scope, claim).await?;
        let valid: bool = sqlx::query_scalar("SELECT a.infrastructure_managed AND a.access='enabled' AND NOT EXISTS(SELECT 1 FROM manager.execution_environments e WHERE e.instance_id=i.id AND e.state<>'Retired') FROM manager.assignments a JOIN manager.instances i ON i.assignment_id=a.id WHERE i.id=$1")
            .bind(claim.instance_id).fetch_one(&mut *tx).await.map_err(db_error)?;
        if !valid {
            return Err(Error::Forbidden);
        }
        sqlx::query("UPDATE manager.instances SET state='Ready',observed_generation=desired_generation,reconcile_after=clock_timestamp()+interval '30 seconds',conditions=jsonb_set(conditions,'{commissioning}',jsonb_build_object('reason','native_ready','generation',desired_generation)) WHERE id=$1")
            .bind(claim.instance_id).execute(&mut *tx).await.map_err(db_error)?;
        sqlx::query("UPDATE manager.operations SET state='Succeeded',lease_owner=NULL,lease_until=NULL WHERE id=$1")
            .bind(claim.id).execute(&mut *tx).await.map_err(db_error)?;
        tx.commit().await.map_err(db_error)
    }
}

async fn uncertain_creation(
    tx: &mut Transaction<'_, Postgres>,
    instance: Uuid,
) -> Result<bool, Error> {
    sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM manager.owned_resources WHERE instance_id=$1 AND state='Reserved')")
        .bind(instance).fetch_one(&mut **tx).await.map_err(db_error)
}

async fn enqueue(
    tx: &mut Transaction<'_, Postgres>,
    scope: &InstallationScope,
    assignment: Uuid,
    instance: Uuid,
    kind: &str,
) -> Result<(), Error> {
    let row = sqlx::query("SELECT a.access,a.template_id,a.template_version,a.version,i.desired_generation FROM manager.assignments a JOIN manager.instances i ON i.assignment_id=a.id WHERE a.id=$1")
        .bind(assignment).fetch_one(&mut **tx).await.map_err(db_error)?;
    let input = json!({"access":row.get::<String,_>("access"),"templateId":row.get::<String,_>("template_id"),"templateVersion":row.get::<i64,_>("template_version")});
    let operation = Uuid::new_v4();
    sqlx::query("INSERT INTO manager.operations(id,installation_id,instance_id,kind,generation,epoch,input) VALUES($1,$2,$3,$4,$5,$6,$7)")
        .bind(operation).bind(scope.id).bind(instance).bind(kind).bind(row.get::<i64,_>("desired_generation")).bind(scope.epoch).bind(&input).execute(&mut **tx).await.map_err(db_error)?;
    sqlx::query("INSERT INTO manager.audit_events(id,installation_id,actor_issuer,actor_subject,action,assignment_id,operation_id,request_id,version,change) VALUES($1,$2,'urn:agent-platform:infrastructure',$3,'infrastructure_sync',$4,$5,$6,$7,$8)")
        .bind(Uuid::new_v4()).bind(scope.id).bind(scope.id.to_string()).bind(assignment).bind(operation).bind(Uuid::new_v4()).bind(row.get::<i64,_>("version")).bind(input).execute(&mut **tx).await.map_err(db_error)?;
    Ok(())
}
