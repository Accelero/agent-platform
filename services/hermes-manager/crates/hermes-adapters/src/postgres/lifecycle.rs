//! Read-only, fenced retained-state planning. No resource or material creation.
use super::*;
use hermes_application::lifecycle::{
    DrainBarrier, LifecyclePlan, LifecycleRepository, RenderingReference, RetainedResource,
};
use hermes_domain::resources::ResourceKind;

async fn retained_resource(
    tx: &mut Transaction<'_, Postgres>,
    scope: &InstallationScope,
    claim: &OperationClaim,
    kind: ResourceKind,
    namespace: &str,
) -> Result<Option<RetainedResource>, Error> {
    let (parent, name, label) = match kind {
        ResourceKind::Namespace => ("", namespace, "namespace"),
        ResourceKind::Deployment => (namespace, "hermes", "instance"),
        _ => return Err(Error::Invalid),
    };
    let row = sqlx::query("SELECT * FROM manager.owned_resources WHERE installation_id=$1 AND instance_id=$2 AND kind=$3 AND namespace=$4 AND name=$5 AND state<>'Deleted'")
        .bind(scope.id).bind(claim.instance_id).bind(kind.as_str()).bind(parent).bind(name)
        .fetch_optional(&mut **tx).await.map_err(db_error)?;
    let Some(row) = row else { return Ok(None) };
    let owned = super::resources::resource(&row)?;
    if owned.creation.epoch > scope.epoch
        || owned.creation.generation > claim.generation
        || (owned.uid.is_none() && owned.creation.epoch != scope.epoch)
    {
        return Err(Error::RecoveryRequired);
    }
    let purpose = format!("render-{label}-g{}", owned.creation.generation);
    let snapshot = sqlx::query("SELECT id,epoch,creation_generation,creation_operation FROM manager.bootstrap_materials WHERE installation_id=$1 AND instance_id=$2 AND purpose=$3")
        .bind(scope.id).bind(claim.instance_id).bind(&purpose).fetch_optional(&mut **tx).await.map_err(db_error)?;
    let original_rendering = match snapshot {
        Some(row) => {
            if row.get::<i64, _>("epoch") != owned.creation.epoch
                || row.get::<i64, _>("creation_generation") != owned.creation.generation
                || row.get::<Uuid, _>("creation_operation") != owned.creation.operation_id
            {
                return Err(Error::RecoveryRequired);
            }
            Some(RenderingReference {
                id: row.get("id"),
                purpose,
                generation: owned.creation.generation,
            })
        }
        None if owned.uid.is_none() => return Err(Error::RecoveryRequired),
        None => None,
    };
    Ok(Some(RetainedResource {
        owned,
        original_rendering,
    }))
}

#[async_trait]
impl LifecycleRepository for PgStore {
    async fn lifecycle_plan(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
    ) -> Result<LifecyclePlan, Error> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        lock_claim(&mut tx, scope, claim).await?;
        let row = sqlx::query("SELECT a.id,a.infrastructure_managed,k.issuer,k.subject,a.access,a.template_id,a.template_version,a.version,i.id AS instance_id,i.namespace,i.namespace_uid,i.desired_generation,i.observed_generation,i.state,o.kind,o.input,i.drain_generation,i.drain_epoch,(extract(epoch FROM i.drain_started_at)*1000)::bigint AS drain_started,(extract(epoch FROM i.drain_deadline_at)*1000)::bigint AS drain_deadline,(extract(epoch FROM i.drain_completed_at)*1000)::bigint AS drain_completed,i.drain_deadline_at<=clock_timestamp() AS drain_expired FROM manager.assignments a JOIN manager.identities k ON k.id=a.identity_id JOIN manager.instances i ON i.assignment_id=a.id JOIN manager.operations o ON o.id=$3 WHERE a.installation_id=$1 AND i.id=$2")
            .bind(scope.id).bind(claim.instance_id).bind(claim.id).fetch_one(&mut *tx).await.map_err(db_error)?;
        let access: String = row.get("access");
        let kind: String = row.get("kind");
        let input: Value = row.get("input");
        let valid_kind = match access.as_str() {
            "enabled" => matches!(kind.as_str(), "commission" | "resync"),
            "suspended" => matches!(kind.as_str(), "suspend" | "resync"),
            "revoked" => matches!(kind.as_str(), "revoke" | "resync"),
            _ => false,
        };
        if kind != claim.kind
            || !valid_kind
            || input["access"] != access
            || input["templateId"] != row.get::<String, _>("template_id")
            || input["templateVersion"] != row.get::<i64, _>("template_version")
        {
            return Err(Error::OwnershipConflict);
        }
        if kind == "resync" {
            let pinned: hermes_application::resync::ResyncInput =
                serde_json::from_value(input).map_err(|_| Error::OwnershipConflict)?;
            if pinned.epoch != scope.epoch
                || pinned.generation != claim.generation
                || pinned.assignment_version != row.get::<i64, _>("version")
                || pinned.namespace_uid.as_ref().is_some_and(|uid| {
                    Some(uid) != row.get::<Option<String>, _>("namespace_uid").as_ref()
                })
            {
                return Err(Error::StaleFence);
            }
        }
        let namespace_uid: Option<String> = row.get("namespace_uid");
        let drain = row
            .get::<Option<i64>, _>("drain_generation")
            .map(|generation| DrainBarrier {
                generation,
                epoch: row.get("drain_epoch"),
                started_at_millis: row.get("drain_started"),
                deadline_at_millis: row.get("drain_deadline"),
                completed_at_millis: row.get("drain_completed"),
                deadline_expired: row.get("drain_expired"),
            });
        let assignment = assignment_view(row)?;
        assignment.identity.validate(&scope.issuer)?;
        if assignment.namespace != instance_namespace(scope.id, claim.instance_id)
            || (assignment.access != Access::Enabled && drain.is_none())
            || drain
                .as_ref()
                .is_some_and(|d| d.epoch > scope.epoch || d.generation > claim.generation)
        {
            return Err(Error::OwnershipConflict);
        }
        let namespace = retained_resource(
            &mut tx,
            scope,
            claim,
            ResourceKind::Namespace,
            &assignment.namespace,
        )
        .await?;
        let deployment = retained_resource(
            &mut tx,
            scope,
            claim,
            ResourceKind::Deployment,
            &assignment.namespace,
        )
        .await?;
        if namespace.as_ref().and_then(|n| n.owned.uid.as_ref()) != namespace_uid.as_ref()
            || deployment
                .as_ref()
                .is_some_and(|d| d.owned.creation.spec.namespace_uid != namespace_uid)
        {
            return Err(Error::OwnershipConflict);
        }
        tx.commit().await.map_err(db_error)?;
        Ok(LifecyclePlan {
            assignment,
            namespace_uid,
            namespace,
            deployment,
            drain,
        })
    }
}
