use std::{str::FromStr, time::Duration};

use async_trait::async_trait;
use hermes_application::routing::{
    DeniedReason, PendingReason, RegisteredDestination, RouteDecision, RouteRegistry,
    UnavailableReason,
};
use hermes_application::{
    Accepted, Action, AssignmentCommand, AssignmentReader, AssignmentStore, AssignmentView, Error,
    InstallationScope, JournalStep, Mutation, OperationClaim, OperationQueue, StepIntent,
    StepResult,
};
use hermes_domain::{
    Access, Identity, TemplateVersion, instance_namespace, validate_name, validate_sha256,
};
use serde_json::{Value, json};
use sqlx::{
    PgPool, Postgres, Row, Transaction,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use uuid::Uuid;

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!();

mod administration;
mod commissioning;
mod execution;
mod lifecycle;
mod materials;
mod namespace_fence;
mod native_children;
mod native_stop;
mod resources;
mod resync;

#[derive(Clone)]
pub struct PgStore {
    pool: PgPool,
}

/// Evidence from the trusted installation inventory adapter, never an HTTP
/// request. The Kubernetes inventory implementation must prove the scan covers
/// the configured cluster/storage scope before constructing this value.
pub struct FreshInventory {
    pub cluster_identity: String,
    pub digest: String,
    pub registered_instances: u64,
    pub unexplained_resources: u64,
}

impl PgStore {
    pub async fn connect(database_url: &str) -> Result<Self, Error> {
        // Do not format options or SQLx errors: they can contain credentials.
        let options = PgConnectOptions::from_str(database_url).map_err(|_| Error::Unavailable)?;
        Self::connect_with(options).await
    }

    pub async fn connect_with(options: PgConnectOptions) -> Result<Self, Error> {
        let pool = PgPoolOptions::new().max_connections(16)
            .acquire_timeout(Duration::from_secs(5))
            .after_connect(|connection, _| Box::pin(async move {
                sqlx::raw_sql("SET statement_timeout = '10s'; SET lock_timeout = '3s'; SET idle_in_transaction_session_timeout = '10s'; SET synchronous_commit = on;")
                    .execute(&mut *connection).await?;
                let durable: bool = sqlx::query_scalar("SELECT current_setting('fsync') = 'on' AND current_setting('full_page_writes') = 'on' AND current_setting('synchronous_commit') = 'on'")
                    .fetch_one(connection).await?;
                if !durable { return Err(sqlx::Error::Protocol("manager requires durable PostgreSQL settings".into())); }
                Ok(())
            }))
            .connect_with(options).await.map_err(db_error)?;
        Ok(Self { pool })
    }

    /// Explicit operator action, separate from opening an existing installation.
    pub async fn migrate(&self, scope: &InstallationScope) -> Result<(), Error> {
        hermes_domain::validate_issuer(&scope.issuer)?;
        if scope.id.is_nil() || scope.epoch < 1 {
            return Err(Error::Invalid);
        }
        let existing: bool =
            sqlx::query_scalar("SELECT to_regclass('manager.installations') IS NOT NULL")
                .fetch_one(&self.pool)
                .await
                .map_err(db_error)?;
        if existing {
            let row = sqlx::query(
                "SELECT id,issuer,epoch,mode FROM manager.installations WHERE singleton",
            )
            .fetch_optional(&self.pool)
            .await
            .map_err(db_error)?
            .ok_or(Error::RecoveryRequired)?;
            if row.get::<Uuid, _>("id") != scope.id
                || row.get::<String, _>("issuer") != scope.issuer
                || row.get::<i64, _>("epoch") != scope.epoch
            {
                return Err(Error::RecoveryRequired);
            }
            let applied: i64 = sqlx::query_scalar(
                "SELECT coalesce(max(version),0) FROM _sqlx_migrations WHERE success",
            )
            .fetch_one(&self.pool)
            .await
            .map_err(|_| Error::RecoveryRequired)?;
            if MIGRATOR.iter().any(|migration| migration.version > applied)
                && row.get::<String, _>("mode") != "maintenance"
            {
                return Err(Error::Maintenance);
            }
        }
        MIGRATOR
            .run(&self.pool)
            .await
            .map_err(|_| Error::RecoveryRequired)
    }

    pub async fn initialize_fresh(
        &self,
        scope: &InstallationScope,
        evidence: FreshInventory,
    ) -> Result<(), Error> {
        hermes_domain::validate_issuer(&scope.issuer)?;
        validate_sha256(&evidence.digest)?;
        if scope.id.is_nil()
            || scope.epoch != 1
            || evidence.cluster_identity.is_empty()
            || evidence.cluster_identity.len() > 255
            || evidence.registered_instances != 0
            || evidence.unexplained_resources != 0
        {
            return Err(Error::RecoveryRequired);
        }
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        // Serialize bootstrap and refuse a populated registry. There is no
        // connect-error -> fresh-install fallback and no automatic adoption.
        sqlx::query("LOCK TABLE manager.installations IN EXCLUSIVE MODE")
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        let populated: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM manager.installations) OR EXISTS(SELECT 1 FROM manager.instances) OR EXISTS(SELECT 1 FROM manager.identities)")
            .fetch_one(&mut *tx).await.map_err(db_error)?;
        if populated {
            return Err(Error::RecoveryRequired);
        }
        sqlx::query("INSERT INTO manager.installations (id, issuer, schema_version, template_schema_version, epoch, mode, cluster_identity, fresh_inventory_digest) VALUES ($1,$2,1,1,1,'active',$3,$4)")
            .bind(scope.id).bind(&scope.issuer).bind(evidence.cluster_identity).bind(evidence.digest)
            .execute(&mut *tx).await.map_err(db_error)?;
        tx.commit().await.map_err(db_error)
    }

    pub async fn check_existing(&self, scope: &InstallationScope) -> Result<(), Error> {
        self.check_startup(scope, true).await
    }

    /// API/authentication and lifecycle coordination may stay reachable during
    /// maintenance/restore. Every assignment/route/execution write still uses
    /// the active-operation gate in its own SQL transaction.
    pub async fn check_registry(&self, scope: &InstallationScope) -> Result<(), Error> {
        self.check_startup(scope, false).await
    }

    /// Explicit installation binding, including while the queue is idle. This
    /// cannot be reconstructed from a configured context or namespace name.
    pub async fn cluster_identity(&self, scope: &InstallationScope) -> Result<String, Error> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        installation_mode(&mut tx, scope).await?;
        let identity =
            sqlx::query_scalar("SELECT cluster_identity FROM manager.installations WHERE id=$1")
                .bind(scope.id)
                .fetch_one(&mut *tx)
                .await
                .map_err(db_error)?;
        tx.commit().await.map_err(db_error)?;
        Ok(identity)
    }

    async fn check_startup(
        &self,
        scope: &InstallationScope,
        require_active: bool,
    ) -> Result<(), Error> {
        // Compare migration history as well as the installation version. Reject
        // a future/modified schema without trying to migrate it on startup.
        let applied =
            sqlx::query("SELECT version, checksum, success FROM _sqlx_migrations ORDER BY version")
                .fetch_all(&self.pool)
                .await
                .map_err(|_| Error::RecoveryRequired)?;
        let expected: Vec<_> = MIGRATOR.iter().collect();
        if applied.len() != expected.len()
            || applied.iter().zip(expected).any(|(row, migration)| {
                row.get::<i64, _>("version") != migration.version
                    || !row.get::<bool, _>("success")
                    || row.get::<Vec<u8>, _>("checksum") != migration.checksum.as_ref()
            })
        {
            return Err(Error::RecoveryRequired);
        }
        let unlogged: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname='manager' AND c.relkind='r' AND c.relpersistence <> 'p')")
            .fetch_one(&self.pool).await.map_err(db_error)?;
        if unlogged {
            return Err(Error::RecoveryRequired);
        }
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        if require_active {
            gate(&mut tx, scope).await?;
        } else {
            installation_mode(&mut tx, scope).await?;
        }
        tx.commit().await.map_err(db_error)
    }
}

fn db_error(error: sqlx::Error) -> Error {
    match error.as_database_error().and_then(|e| e.code()).as_deref() {
        Some("23505") => Error::AlreadyExists,
        _ => Error::Unavailable,
    }
}

/// Held only over SQL, so a concurrent maintenance transition cannot pass an
/// acknowledged mutation or journal completion under an old epoch.
async fn gate(tx: &mut Transaction<'_, Postgres>, scope: &InstallationScope) -> Result<(), Error> {
    match installation_mode(tx, scope).await?.as_str() {
        "active" => Ok(()),
        "maintenance" => Err(Error::Maintenance),
        _ => Err(Error::RecoveryRequired),
    }
}

async fn installation_mode(
    tx: &mut Transaction<'_, Postgres>,
    scope: &InstallationScope,
) -> Result<String, Error> {
    let row = sqlx::query("SELECT id, issuer, epoch, mode, schema_version, template_schema_version FROM manager.installations WHERE singleton FOR SHARE")
        .fetch_optional(&mut **tx).await.map_err(db_error)?.ok_or(Error::RecoveryRequired)?;
    if row.get::<Uuid, _>("id") != scope.id
        || row.get::<String, _>("issuer") != scope.issuer
        || row.get::<i64, _>("epoch") != scope.epoch
        || row.get::<i64, _>("schema_version") != 1
        || row.get::<i64, _>("template_schema_version") != 1
    {
        return Err(Error::RecoveryRequired);
    }
    Ok(row.get("mode"))
}

#[async_trait]
impl AssignmentStore for PgStore {
    async fn mutate(&self, mutation: Mutation) -> Result<Accepted, Error> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        gate(&mut tx, &mutation.installation).await?;
        let scope = mutation.command.scope();
        sqlx::query("INSERT INTO manager.mutation_requests (installation_id,actor_issuer,actor_subject,scope,idempotency_key,body_digest) VALUES ($1,$2,$3,$4,$5,$6) ON CONFLICT DO NOTHING")
            .bind(mutation.installation.id).bind(&mutation.actor.issuer).bind(&mutation.actor.subject)
            .bind(scope).bind(&mutation.idempotency_key).bind(&mutation.body_digest)
            .execute(&mut *tx).await.map_err(db_error)?;
        let request = sqlx::query("SELECT body_digest,result FROM manager.mutation_requests WHERE installation_id=$1 AND actor_issuer=$2 AND actor_subject=$3 AND scope=$4 AND idempotency_key=$5 FOR UPDATE")
            .bind(mutation.installation.id).bind(&mutation.actor.issuer).bind(&mutation.actor.subject)
            .bind(scope).bind(&mutation.idempotency_key).fetch_one(&mut *tx).await.map_err(db_error)?;
        if request.get::<Vec<u8>, _>("body_digest") != mutation.body_digest {
            return Err(Error::IdempotencyConflict);
        }
        if let Some(receipt) = request.get::<Option<Value>, _>("result") {
            let accepted = serde_json::from_value(receipt).map_err(|_| Error::RecoveryRequired)?;
            tx.commit().await.map_err(db_error)?;
            return Ok(accepted);
        }

        // Resolve an omitted template version only AFTER replaying an existing
        // idempotency receipt. A later catalog version must not change the result
        // of the same acknowledged request.
        let command = if let AssignmentCommand::CreateDefaultTemplate {
            identity,
            access,
            template_id,
        } = &mutation.command
        {
            let version: i64 = sqlx::query_scalar("SELECT version FROM manager.instance_templates WHERE id=$1 AND approved AND schema_version=1 ORDER BY version DESC LIMIT 1")
                .bind(template_id).fetch_optional(&mut *tx).await.map_err(db_error)?.ok_or(Error::TemplateUnavailable)?;
            AssignmentCommand::Create {
                identity: identity.clone(),
                access: *access,
                template: TemplateVersion {
                    id: template_id.clone(),
                    version,
                },
            }
        } else {
            mutation.command.clone()
        };
        let (accepted, was_enabled) = match &command {
            AssignmentCommand::CreateDefaultTemplate { .. } => return Err(Error::Invalid),
            AssignmentCommand::Create {
                identity,
                access,
                template,
            } => {
                let approved: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM manager.instance_templates WHERE id=$1 AND version=$2 AND approved AND schema_version=1)")
                    .bind(&template.id).bind(template.version).fetch_one(&mut *tx).await.map_err(db_error)?;
                if !approved {
                    return Err(Error::TemplateUnavailable);
                }
                sqlx::query("INSERT INTO manager.identities (id,issuer,subject) VALUES ($1,$2,$3) ON CONFLICT (issuer,subject) DO NOTHING")
                    .bind(Uuid::new_v4()).bind(&identity.issuer).bind(&identity.subject).execute(&mut *tx).await.map_err(db_error)?;
                let identity_id: Uuid = sqlx::query_scalar(
                    "SELECT id FROM manager.identities WHERE issuer=$1 AND subject=$2",
                )
                .bind(&identity.issuer)
                .bind(&identity.subject)
                .fetch_one(&mut *tx)
                .await
                .map_err(db_error)?;
                let accepted = Accepted {
                    assignment_id: Uuid::new_v4(),
                    instance_id: Uuid::new_v4(),
                    operation_id: Uuid::new_v4(),
                    version: 1,
                };
                sqlx::query("INSERT INTO manager.assignments (id,installation_id,identity_id,access,template_id,template_version,version) VALUES ($1,$2,$3,$4,$5,$6,1)")
                    .bind(accepted.assignment_id).bind(mutation.installation.id).bind(identity_id).bind(access.as_str())
                    .bind(&template.id).bind(template.version).execute(&mut *tx).await.map_err(db_error)?;
                sqlx::query("INSERT INTO manager.instances (id,assignment_id,namespace,desired_generation) VALUES ($1,$2,$3,1)")
                    .bind(accepted.instance_id).bind(accepted.assignment_id).bind(instance_namespace(mutation.installation.id, accepted.instance_id))
                    .execute(&mut *tx).await.map_err(db_error)?;
                (accepted, false)
            }
            AssignmentCommand::SetAccess {
                assignment_id,
                expected_version,
                access,
            } => {
                let row = sqlx::query("SELECT a.version,a.access,i.id FROM manager.assignments a JOIN manager.instances i ON i.assignment_id=a.id WHERE a.id=$1 AND a.installation_id=$2 FOR UPDATE OF a,i")
                    .bind(assignment_id).bind(mutation.installation.id).fetch_optional(&mut *tx).await.map_err(db_error)?.ok_or(Error::NotFound)?;
                if row.get::<i64, _>("version") != *expected_version {
                    return Err(Error::PreconditionFailed);
                }
                let version: i64 = sqlx::query_scalar("UPDATE manager.assignments SET access=$2, version=version+1, updated_at=clock_timestamp() WHERE id=$1 RETURNING version")
                    .bind(assignment_id).bind(access.as_str()).fetch_one(&mut *tx).await.map_err(db_error)?;
                let instance_id = row.get::<Uuid, _>("id");
                advance_instance(&mut tx, instance_id).await?;
                (
                    Accepted {
                        assignment_id: *assignment_id,
                        instance_id,
                        operation_id: Uuid::new_v4(),
                        version,
                    },
                    row.get::<String, _>("access") == "enabled",
                )
            }
            AssignmentCommand::Retry {
                instance_id,
                expected_version,
            } => {
                let row = sqlx::query("SELECT a.id,a.version FROM manager.assignments a JOIN manager.instances i ON i.assignment_id=a.id WHERE i.id=$1 AND a.installation_id=$2 FOR UPDATE OF a,i")
                    .bind(instance_id).bind(mutation.installation.id).fetch_optional(&mut *tx).await.map_err(db_error)?.ok_or(Error::NotFound)?;
                if row.get::<i64, _>("version") != *expected_version {
                    return Err(Error::PreconditionFailed);
                }
                let assignment_id = row.get::<Uuid, _>("id");
                let version = sqlx::query_scalar("UPDATE manager.assignments SET version=version+1,updated_at=clock_timestamp() WHERE id=$1 RETURNING version")
                    .bind(assignment_id).fetch_one(&mut *tx).await.map_err(db_error)?;
                advance_instance(&mut tx, *instance_id).await?;
                (
                    Accepted {
                        assignment_id,
                        instance_id: *instance_id,
                        operation_id: Uuid::new_v4(),
                        version,
                    },
                    false,
                )
            }
        };
        let row = sqlx::query("SELECT a.access,a.template_id,a.template_version,i.desired_generation FROM manager.assignments a JOIN manager.instances i ON i.assignment_id=a.id WHERE a.id=$1")
            .bind(accepted.assignment_id).fetch_one(&mut *tx).await.map_err(db_error)?;
        let access = row.get::<String, _>("access");
        let kind = match access.as_str() {
            "enabled" => "commission",
            "suspended" => "suspend",
            _ => "revoke",
        };
        if access != "enabled" {
            // Use one database tick for exact duration/provenance. Retrying or
            // toggling access while a drain is active preserves its deadline.
            // A new enabled -> disabled cycle may start only after the prior
            // barrier was actually completed by verified lifecycle work.
            sqlx::query("WITH tick AS MATERIALIZED (SELECT clock_timestamp() AS now) UPDATE manager.instances i SET drain_generation=CASE WHEN i.drain_started_at IS NULL OR ($2 AND i.drain_completed_at IS NOT NULL) THEN i.desired_generation ELSE i.drain_generation END,drain_epoch=CASE WHEN i.drain_started_at IS NULL OR ($2 AND i.drain_completed_at IS NOT NULL) THEN $3 ELSE i.drain_epoch END,drain_started_at=CASE WHEN i.drain_started_at IS NULL OR ($2 AND i.drain_completed_at IS NOT NULL) THEN tick.now ELSE i.drain_started_at END,drain_deadline_at=CASE WHEN i.drain_started_at IS NULL OR ($2 AND i.drain_completed_at IS NOT NULL) THEN tick.now+interval '5 minutes' ELSE i.drain_deadline_at END,drain_completed_at=CASE WHEN i.drain_started_at IS NULL OR ($2 AND i.drain_completed_at IS NOT NULL) THEN NULL ELSE i.drain_completed_at END FROM tick WHERE i.id=$1")
                .bind(accepted.instance_id).bind(was_enabled).bind(mutation.installation.epoch)
                .execute(&mut *tx).await.map_err(db_error)?;
        }
        let input = json!({"access": access, "templateId": row.get::<String, _>("template_id"), "templateVersion": row.get::<i64, _>("template_version")});
        sqlx::query("INSERT INTO manager.operations (id,installation_id,instance_id,kind,generation,epoch,input) VALUES ($1,$2,$3,$4,$5,$6,$7)")
            .bind(accepted.operation_id).bind(mutation.installation.id).bind(accepted.instance_id).bind(kind)
            .bind(row.get::<i64, _>("desired_generation")).bind(mutation.installation.epoch).bind(&input)
            .execute(&mut *tx).await.map_err(db_error)?;
        sqlx::query("INSERT INTO manager.audit_events (id,installation_id,actor_issuer,actor_subject,action,assignment_id,operation_id,request_id,version,change) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)")
            .bind(Uuid::new_v4()).bind(mutation.installation.id).bind(&mutation.actor.issuer).bind(&mutation.actor.subject).bind(scope)
            .bind(accepted.assignment_id).bind(accepted.operation_id).bind(mutation.request_id).bind(accepted.version).bind(input)
            .execute(&mut *tx).await.map_err(db_error)?;
        sqlx::query("UPDATE manager.mutation_requests SET result=$6 WHERE installation_id=$1 AND actor_issuer=$2 AND actor_subject=$3 AND scope=$4 AND idempotency_key=$5")
            .bind(mutation.installation.id).bind(&mutation.actor.issuer).bind(&mutation.actor.subject).bind(scope).bind(&mutation.idempotency_key)
            .bind(serde_json::to_value(&accepted).map_err(|_| Error::Invalid)?).execute(&mut *tx).await.map_err(db_error)?;
        tx.commit().await.map_err(db_error)?;
        Ok(accepted)
    }
}

async fn advance_instance(tx: &mut Transaction<'_, Postgres>, instance: Uuid) -> Result<(), Error> {
    sqlx::query("UPDATE manager.instances SET desired_generation=desired_generation+1 WHERE id=$1")
        .bind(instance)
        .execute(&mut **tx)
        .await
        .map_err(db_error)?;
    // Existing external actions remain journalled; no old worker can publish
    // their results. The new generation must observe partial owned resources.
    sqlx::query("UPDATE manager.operations SET state='Superseded',fence=fence+1,lease_owner=NULL,lease_until=NULL WHERE instance_id=$1 AND state NOT IN ('Succeeded','Superseded')")
        .bind(instance).execute(&mut **tx).await.map_err(db_error)?;
    Ok(())
}

fn assignment_view(row: sqlx::postgres::PgRow) -> Result<AssignmentView, Error> {
    let access = match row.get::<String, _>("access").as_str() {
        "enabled" => Access::Enabled,
        "suspended" => Access::Suspended,
        "revoked" => Access::Revoked,
        _ => return Err(Error::RecoveryRequired),
    };
    Ok(AssignmentView {
        id: row.get("id"),
        identity: Identity {
            issuer: row.get("issuer"),
            subject: row.get("subject"),
        },
        access,
        template: TemplateVersion {
            id: row.get("template_id"),
            version: row.get("template_version"),
        },
        version: row.get("version"),
        instance_id: row.get("instance_id"),
        namespace: row.get("namespace"),
        desired_generation: row.get("desired_generation"),
        observed_generation: row.get("observed_generation"),
        state: row.get("state"),
    })
}

#[async_trait]
impl AssignmentReader for PgStore {
    async fn get_assignment(
        &self,
        scope: &InstallationScope,
        id: Uuid,
    ) -> Result<AssignmentView, Error> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        gate(&mut tx, scope).await?;
        let row = sqlx::query("SELECT a.id,k.issuer,k.subject,a.access,a.template_id,a.template_version,a.version,i.id AS instance_id,i.namespace,i.desired_generation,i.observed_generation,i.state FROM manager.assignments a JOIN manager.identities k ON k.id=a.identity_id JOIN manager.instances i ON i.assignment_id=a.id WHERE a.installation_id=$1 AND a.id=$2")
        .bind(scope.id)
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_error)?
        .ok_or(Error::NotFound)?;
        let view = assignment_view(row)?;
        tx.commit().await.map_err(db_error)?;
        Ok(view)
    }

    async fn list_assignments(
        &self,
        scope: &InstallationScope,
        after: Option<Uuid>,
        limit: u16,
    ) -> Result<Vec<AssignmentView>, Error> {
        if !(1..=100).contains(&limit) {
            return Err(Error::Invalid);
        }
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        gate(&mut tx, scope).await?;
        let rows = sqlx::query("SELECT a.id,k.issuer,k.subject,a.access,a.template_id,a.template_version,a.version,i.id AS instance_id,i.namespace,i.desired_generation,i.observed_generation,i.state FROM manager.assignments a JOIN manager.identities k ON k.id=a.identity_id JOIN manager.instances i ON i.assignment_id=a.id WHERE a.installation_id=$1 AND ($2::uuid IS NULL OR a.id>$2) ORDER BY a.id LIMIT $3")
            .bind(scope.id).bind(after).bind(i64::from(limit)).fetch_all(&mut *tx).await.map_err(db_error)?;
        let views = rows
            .into_iter()
            .map(assignment_view)
            .collect::<Result<_, _>>()?;
        tx.commit().await.map_err(db_error)?;
        Ok(views)
    }
}

#[async_trait]
impl RouteRegistry for PgStore {
    async fn resolve(
        &self,
        scope: &InstallationScope,
        identity: &Identity,
    ) -> Result<RouteDecision, Error> {
        identity.validate(&scope.issuer)?;
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        gate(&mut tx, scope).await?;
        let row = sqlx::query("SELECT a.access,a.version,i.id,i.namespace,i.namespace_uid,i.state,i.desired_generation,i.observed_generation,i.route_generation,r.namespace_uid AS route_namespace_uid,r.service,r.port,r.scheme,r.generation AS registered_generation,t.native_port,t.native_scheme FROM manager.identities k JOIN manager.assignments a ON a.identity_id=k.id JOIN manager.instances i ON i.assignment_id=a.id JOIN manager.instance_templates t ON t.id=a.template_id AND t.version=a.template_version LEFT JOIN manager.instance_routes r ON r.instance_id=i.id WHERE a.installation_id=$1 AND k.issuer=$2 AND k.subject=$3")
            .bind(scope.id).bind(&identity.issuer).bind(&identity.subject).fetch_optional(&mut *tx).await.map_err(db_error)?;
        let result = if let Some(row) = row {
            if row.get::<String, _>("access") != "enabled" {
                RouteDecision::Denied {
                    reason: DeniedReason::AccessDenied,
                }
            } else if row.get::<String, _>("state") != "Ready"
                || row.get::<i64, _>("desired_generation")
                    != row.get::<i64, _>("observed_generation")
            {
                RouteDecision::Pending {
                    reason: PendingReason::InstanceNotReady,
                }
            } else if row.get::<Option<String>, _>("namespace_uid").is_none()
                || row.get::<Option<String>, _>("namespace_uid")
                    != row.get::<Option<String>, _>("route_namespace_uid")
                || row.get::<Option<i64>, _>("registered_generation")
                    != Some(row.get::<i64, _>("route_generation"))
                || row.get::<i64, _>("route_generation") < 1
                || row.get::<Option<i32>, _>("port") != Some(row.get::<i32, _>("native_port"))
                || row.get::<Option<String>, _>("scheme")
                    != Some(row.get::<String, _>("native_scheme"))
            {
                RouteDecision::Unavailable {
                    reason: UnavailableReason::RegistryUnavailable,
                }
            } else {
                RouteDecision::Ready {
                    instance_id: row.get("id"),
                    route_generation: row.get("route_generation"),
                    authorization_version: row.get("version"),
                    destination: RegisteredDestination {
                        namespace: row.get("namespace"),
                        service: row.get("service"),
                        port: u16::try_from(row.get::<i32, _>("port"))
                            .map_err(|_| Error::RecoveryRequired)?,
                        scheme: row.get("scheme"),
                    },
                }
            }
        } else {
            RouteDecision::Denied {
                reason: DeniedReason::AccessDenied,
            }
        };
        tx.commit().await.map_err(db_error)?;
        Ok(result)
    }
}

fn lease_duration(seconds: u16) -> Result<i32, Error> {
    if !(1..=300).contains(&seconds) {
        return Err(Error::Invalid);
    }
    Ok(i32::from(seconds))
}

/// Lock assignment and instance before its operation, matching mutation lock
/// order. Exclusive instance locks also avoid a SHARE-to-UPDATE upgrade deadlock
/// when concurrent callbacks try to bind a namespace under the same claim.
async fn lock_claim(
    tx: &mut Transaction<'_, Postgres>,
    scope: &InstallationScope,
    claim: &OperationClaim,
) -> Result<(), Error> {
    gate(tx, scope).await?;
    if claim.epoch != scope.epoch {
        return Err(Error::StaleFence);
    }
    let generation: Option<i64> = sqlx::query_scalar("SELECT i.desired_generation FROM manager.instances i JOIN manager.assignments a ON a.id=i.assignment_id WHERE i.id=$1 AND a.installation_id=$2 FOR UPDATE OF a,i")
        .bind(claim.instance_id).bind(scope.id).fetch_optional(&mut **tx).await.map_err(db_error)?;
    if generation != Some(claim.generation) {
        return Err(Error::StaleFence);
    }
    let valid: Option<bool> = sqlx::query_scalar("SELECT lease_until > clock_timestamp() FROM manager.operations WHERE id=$1 AND installation_id=$2 AND instance_id=$3 AND generation=$4 AND epoch=$5 AND lease_owner=$6 AND fence=$7 AND state='Running' FOR UPDATE")
        .bind(claim.id).bind(scope.id).bind(claim.instance_id).bind(claim.generation).bind(scope.epoch).bind(claim.worker).bind(claim.fence)
        .fetch_optional(&mut **tx).await.map_err(db_error)?;
    if valid != Some(true) {
        return Err(Error::StaleFence);
    }
    Ok(())
}

#[async_trait]
impl OperationQueue for PgStore {
    async fn claim(
        &self,
        scope: &InstallationScope,
        worker: Uuid,
        lease_seconds: u16,
    ) -> Result<Option<OperationClaim>, Error> {
        let seconds = lease_duration(lease_seconds)?;
        if worker.is_nil() {
            return Err(Error::Invalid);
        }
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        gate(&mut tx, scope).await?;
        sqlx::query("UPDATE manager.operations SET state='Blocked',lease_owner=NULL,lease_until=NULL,fence=fence+1 WHERE installation_id=$1 AND state IN ('Pending','Running','Waiting','RetryAt') AND attempt>=max_attempts AND (lease_until IS NULL OR lease_until<=clock_timestamp())")
            .bind(scope.id).execute(&mut *tx).await.map_err(db_error)?;
        let row = sqlx::query("WITH candidate AS (SELECT o.id FROM manager.operations o JOIN manager.instances i ON i.id=o.instance_id WHERE o.installation_id=$1 AND o.epoch=$2 AND o.generation=i.desired_generation AND o.state IN ('Pending','Running','Waiting','RetryAt') AND (o.lease_until IS NULL OR o.lease_until<=clock_timestamp()) AND o.next_attempt_at<=clock_timestamp() AND o.attempt<o.max_attempts ORDER BY o.next_attempt_at,o.created_at FOR UPDATE OF o SKIP LOCKED LIMIT 1) UPDATE manager.operations o SET state='Running',lease_owner=$3,lease_until=clock_timestamp()+make_interval(secs=>$4),fence=fence+1,attempt=attempt+1 FROM candidate WHERE o.id=candidate.id RETURNING o.id,o.instance_id,o.generation,o.epoch,o.fence,o.kind")
            .bind(scope.id).bind(scope.epoch).bind(worker).bind(seconds).fetch_optional(&mut *tx).await.map_err(db_error)?;
        let claim = row.map(|r| OperationClaim {
            id: r.get("id"),
            instance_id: r.get("instance_id"),
            generation: r.get("generation"),
            epoch: r.get("epoch"),
            worker,
            fence: r.get("fence"),
            kind: r.get("kind"),
        });
        tx.commit().await.map_err(db_error)?;
        Ok(claim)
    }

    async fn renew(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
        lease_seconds: u16,
    ) -> Result<(), Error> {
        let seconds = lease_duration(lease_seconds)?;
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        lock_claim(&mut tx, scope, claim).await?;
        sqlx::query("UPDATE manager.operations SET lease_until=clock_timestamp()+make_interval(secs=>$2) WHERE id=$1")
            .bind(claim.id).bind(seconds).execute(&mut *tx).await.map_err(db_error)?;
        tx.commit().await.map_err(db_error)
    }

    async fn prepare_step(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
        key: &str,
        intent: StepIntent,
    ) -> Result<JournalStep, Error> {
        validate_name(key)?;
        intent.validate()?;
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        lock_claim(&mut tx, scope, claim).await?;
        sqlx::query("INSERT INTO manager.operation_steps (operation_id,step_key,intent,fence) VALUES ($1,$2,$3,$4) ON CONFLICT DO NOTHING")
            .bind(claim.id).bind(key).bind(serde_json::to_value(&intent).map_err(|_| Error::Invalid)?).bind(claim.fence)
            .execute(&mut *tx).await.map_err(db_error)?;
        let row = sqlx::query("SELECT intent,result FROM manager.operation_steps WHERE operation_id=$1 AND step_key=$2 FOR UPDATE")
            .bind(claim.id).bind(key).fetch_one(&mut *tx).await.map_err(db_error)?;
        let original: StepIntent =
            serde_json::from_value(row.get("intent")).map_err(|_| Error::RecoveryRequired)?;
        if original != intent {
            return Err(Error::OwnershipConflict);
        }
        let result =
            serde_json::from_value(row.get("result")).map_err(|_| Error::RecoveryRequired)?;
        sqlx::query(
            "UPDATE manager.operation_steps SET fence=$3 WHERE operation_id=$1 AND step_key=$2",
        )
        .bind(claim.id)
        .bind(key)
        .bind(claim.fence)
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
        tx.commit().await.map_err(db_error)?;
        Ok(JournalStep {
            intent: original,
            result,
        })
    }

    async fn record_result(
        &self,
        scope: &InstallationScope,
        claim: &OperationClaim,
        key: &str,
        result: StepResult,
    ) -> Result<(), Error> {
        if matches!(
            result,
            StepResult::Reconciled { .. } | StepResult::ObservedMissing
        ) {
            return Err(Error::OwnershipConflict);
        }
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        lock_claim(&mut tx, scope, claim).await?;
        let row = sqlx::query("SELECT intent,result,fence FROM manager.operation_steps WHERE operation_id=$1 AND step_key=$2 FOR UPDATE")
            .bind(claim.id).bind(key).fetch_optional(&mut *tx).await.map_err(db_error)?.ok_or(Error::NotFound)?;
        if row.get::<i64, _>("fence") != claim.fence {
            return Err(Error::StaleFence);
        }
        if matches!(
            result,
            StepResult::Verified { .. } | StepResult::Absent { .. }
        ) {
            let owned:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM manager.resource_steps WHERE operation_id=$1 AND step_key=$2)")
                .bind(claim.id).bind(key).fetch_one(&mut *tx).await.map_err(db_error)?;
            if owned {
                return Err(Error::OwnershipConflict);
            }
        }
        let intent: StepIntent =
            serde_json::from_value(row.get("intent")).map_err(|_| Error::RecoveryRequired)?;
        if matches!(
            intent.action,
            Action::Observe | Action::NativeStop | Action::FenceClose | Action::FenceOpen
        ) {
            return Err(Error::OwnershipConflict);
        }
        let previous: StepResult =
            serde_json::from_value(row.get("result")).map_err(|_| Error::RecoveryRequired)?;
        if previous != StepResult::Unknown && previous != result {
            return Err(Error::OwnershipConflict);
        }
        let verified = match &result {
            StepResult::Verified {
                uid,
                resource_version,
            } => {
                if intent.action == Action::Delete
                    || uid.is_empty()
                    || uid.len() > 255
                    || resource_version.is_empty()
                    || resource_version.len() > 255
                    || intent
                        .expected_uid
                        .as_ref()
                        .is_some_and(|expected| expected != uid)
                {
                    return Err(Error::OwnershipConflict);
                }
                true
            }
            StepResult::Absent { deleted_uid } => {
                if intent.action != Action::Delete
                    || intent.expected_uid.as_ref() != Some(deleted_uid)
                {
                    return Err(Error::OwnershipConflict);
                }
                true
            }
            _ => false,
        };
        sqlx::query("UPDATE manager.operation_steps SET result=$3,verified_at=CASE WHEN $4 THEN clock_timestamp() ELSE NULL END WHERE operation_id=$1 AND step_key=$2")
            .bind(claim.id).bind(key).bind(serde_json::to_value(&result).map_err(|_| Error::Invalid)?).bind(verified)
            .execute(&mut *tx).await.map_err(db_error)?;
        if matches!(result, StepResult::Blocked { .. }) {
            sqlx::query("UPDATE manager.operations SET state='Blocked',lease_owner=NULL,lease_until=NULL WHERE id=$1")
                .bind(claim.id).execute(&mut *tx).await.map_err(db_error)?;
        }
        tx.commit().await.map_err(db_error)
    }

    async fn finish(&self, scope: &InstallationScope, claim: &OperationClaim) -> Result<(), Error> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        lock_claim(&mut tx, scope, claim).await?;
        let complete: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM manager.operation_steps WHERE operation_id=$1) AND NOT EXISTS(SELECT 1 FROM manager.operation_steps WHERE operation_id=$1 AND verified_at IS NULL) AND NOT EXISTS(SELECT 1 FROM manager.native_child_actions WHERE operation_id=$1 AND completed_at IS NULL)")
            .bind(claim.id).fetch_one(&mut *tx).await.map_err(db_error)?;
        if !complete {
            return Err(Error::OwnershipConflict);
        }
        // Completing a journal never implicitly publishes instance readiness.
        // That requires separate verified native/storage/policy observations.
        sqlx::query("UPDATE manager.operations SET state='Succeeded',lease_owner=NULL,lease_until=NULL WHERE id=$1")
            .bind(claim.id).execute(&mut *tx).await.map_err(db_error)?;
        tx.commit().await.map_err(db_error)
    }
}
