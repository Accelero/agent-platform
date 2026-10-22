//! Installation-scoped PostgreSQL authorization; roles in OIDC tokens grant nothing.
use super::*;
use hermes_application::VerifiedPrincipal;
use serde::{Deserialize, Serialize};
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdministratorInput {
    pub identity: Identity,
    pub active: bool,
    pub can_manage_administrators: bool,
}
pub(super) async fn authorize_grant(
    tx: &mut Transaction<'_, Postgres>,
    scope: &InstallationScope,
    actor: &Identity,
    manage: bool,
) -> Result<(), Error> {
    actor.validate(&scope.issuer)?;
    let granted:Option<bool>=sqlx::query_scalar("SELECT NOT $4 OR can_manage_administrators FROM manager_admin_auth.administrator_grants WHERE installation_id=$1 AND issuer=$2 AND subject=$3 AND active FOR SHARE").bind(scope.id).bind(&actor.issuer).bind(&actor.subject).bind(manage).fetch_optional(&mut **tx).await.map_err(db_error)?;
    if granted != Some(true) {
        return Err(Error::Forbidden);
    }
    Ok(())
}
impl PgStore {
    pub async fn authorize_administrator(
        &self,
        scope: &InstallationScope,
        actor: &Identity,
    ) -> Result<bool, Error> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        // Authentication remains available during recovery/maintenance; commands
        // retain their independent active-installation gate.
        installation_mode(&mut tx, scope).await?;
        authorize_grant(&mut tx, scope, actor, false).await?;
        let manage:bool=sqlx::query_scalar("SELECT can_manage_administrators FROM manager_admin_auth.administrator_grants WHERE installation_id=$1 AND issuer=$2 AND subject=$3 AND active").bind(scope.id).bind(&actor.issuer).bind(&actor.subject).fetch_one(&mut *tx).await.map_err(db_error)?;
        tx.commit().await.map_err(db_error)?;
        Ok(manage)
    }
    pub async fn note_verified_identity(
        &self,
        scope: &InstallationScope,
        identity: &Identity,
    ) -> Result<(), Error> {
        identity.validate(&scope.issuer)?;
        sqlx::query("INSERT INTO manager_admin_auth.verified_identities(id,installation_id,issuer,subject) VALUES($1,$2,$3,$4) ON CONFLICT(installation_id,issuer,subject) DO UPDATE SET verified_at=clock_timestamp()").bind(Uuid::new_v4()).bind(scope.id).bind(&identity.issuer).bind(&identity.subject).execute(&self.pool).await.map_err(db_error)?;
        Ok(())
    }
    pub async fn administrator_candidates(
        &self,
        scope: &InstallationScope,
        actor: &Identity,
        query: &super::dashboard::DashboardQuery,
    ) -> Result<Value, Error> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        gate(&mut tx, scope).await?;
        authorize_grant(&mut tx, scope, actor, true).await?;
        tx.commit().await.map_err(db_error)?;
        self.dashboard_list(
            scope,
            super::dashboard::DashboardList::AdministratorCandidates,
            query,
        )
        .await
    }
    pub async fn administrator_grant(
        &self,
        scope: &InstallationScope,
        id: Uuid,
    ) -> Result<Value, Error> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        gate(&mut tx, scope).await?;
        let row=sqlx::query("SELECT id,issuer,subject,active,can_manage_administrators,version FROM manager_admin_auth.administrator_grants WHERE installation_id=$1 AND id=$2").bind(scope.id).bind(id).fetch_optional(&mut *tx).await.map_err(db_error)?.ok_or(Error::NotFound)?;
        let value = grant_view(row);
        tx.commit().await.map_err(db_error)?;
        Ok(value)
    }
    pub async fn update_administrator(
        &self,
        scope: &InstallationScope,
        actor: &VerifiedPrincipal,
        key: &str,
        change: AdministratorChange,
    ) -> Result<Value, Error> {
        let AdministratorChange {
            id,
            expected,
            input,
        } = change;
        input.identity.validate(&scope.issuer)?;
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        // Serialize grant changes before locking individual grants to protect last-admin invariants.
        sqlx::query(
            "LOCK TABLE manager_admin_auth.administrator_grants IN SHARE ROW EXCLUSIVE MODE",
        )
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
        if let Some(result) = super::authoring::command_begin(
            &mut tx,
            scope,
            &actor.identity,
            key,
            &json!({"command":"administrator.update","id":id,"expected":expected,"input":input}),
        )
        .await?
        {
            tx.commit().await.map_err(db_error)?;
            return Ok(result);
        }
        authorize_grant(&mut tx, scope, &actor.identity, true).await?;
        let verified:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM manager_admin_auth.verified_identities WHERE installation_id=$1 AND issuer=$2 AND subject=$3)").bind(scope.id).bind(&input.identity.issuer).bind(&input.identity.subject).fetch_one(&mut *tx).await.map_err(db_error)?;
        if !verified {
            return Err(Error::UnverifiedIdentity);
        }
        let id = if let Some(id) = id {
            let row=sqlx::query("SELECT issuer,subject,version,active,can_manage_administrators FROM manager_admin_auth.administrator_grants WHERE installation_id=$1 AND id=$2 FOR UPDATE").bind(scope.id).bind(id).fetch_optional(&mut *tx).await.map_err(db_error)?.ok_or(Error::NotFound)?;
            if Some(row.get::<i64, _>("version")) != expected {
                return Err(Error::PreconditionFailed);
            }
            if row.get::<String, _>("issuer") != input.identity.issuer
                || row.get::<String, _>("subject") != input.identity.subject
            {
                return Err(Error::OwnershipConflict);
            }
            if row.get::<bool, _>("active")
                && row.get::<bool, _>("can_manage_administrators")
                && (!input.active || !input.can_manage_administrators)
            {
                let count:i64=sqlx::query_scalar("SELECT count(*) FROM manager_admin_auth.administrator_grants WHERE installation_id=$1 AND active AND can_manage_administrators").bind(scope.id).fetch_one(&mut *tx).await.map_err(db_error)?;
                if count <= 1 {
                    return Err(Error::LastAdministrator);
                }
            }
            sqlx::query("UPDATE manager_admin_auth.administrator_grants SET active=$3,can_manage_administrators=$4,version=version+1 WHERE installation_id=$1 AND id=$2").bind(scope.id).bind(id).bind(input.active).bind(input.can_manage_administrators).execute(&mut *tx).await.map_err(db_error)?;
            id
        } else {
            let exists:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM manager_admin_auth.administrator_grants WHERE installation_id=$1 AND issuer=$2 AND subject=$3)").bind(scope.id).bind(&input.identity.issuer).bind(&input.identity.subject).fetch_one(&mut *tx).await.map_err(db_error)?;
            if exists {
                return Err(Error::AdministratorExists);
            }
            let id = Uuid::new_v4();
            sqlx::query("INSERT INTO manager_admin_auth.administrator_grants(id,installation_id,issuer,subject,active,can_manage_administrators) VALUES($1,$2,$3,$4,$5,$6)").bind(id).bind(scope.id).bind(&input.identity.issuer).bind(&input.identity.subject).bind(input.active).bind(input.can_manage_administrators).execute(&mut *tx).await.map_err(db_error)?;
            id
        };
        if !input.active {
            sqlx::query("UPDATE manager_admin_auth.sessions SET state='revoked',refresh_owner=NULL,refresh_until=NULL,fence=fence+1 WHERE issuer=$1 AND subject=$2 AND installation_id=$3 AND state IN ('active','refreshing')").bind(&input.identity.issuer).bind(&input.identity.subject).bind(scope.id).execute(&mut *tx).await.map_err(db_error)?;
        }
        let version: i64 = sqlx::query_scalar(
            "SELECT version FROM manager_admin_auth.administrator_grants WHERE id=$1",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .map_err(db_error)?;
        let operation = super::authoring::authoring_audit(
            &mut tx,
            scope,
            &actor.identity,
            if input.active {
                "administrator.grant"
            } else {
                "administrator.revoke"
            },
            "administrator",
            &id.to_string(),
            version,
        )
        .await?;
        let result = json!({"id":id,"version":version,"operationId":operation});
        super::authoring::command_finish(&mut tx, scope, &actor.identity, key, &result).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(result)
    }
    /// Trusted installation operator action; identity comes from a private file, never CLI arguments.
    pub async fn bootstrap_administrator(
        &self,
        scope: &InstallationScope,
        identity: &Identity,
        recover: bool,
    ) -> Result<(), Error> {
        identity.validate(&scope.issuer)?;
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        installation_mode(&mut tx, scope).await?;
        sqlx::query(
            "LOCK TABLE manager_admin_auth.administrator_grants IN SHARE ROW EXCLUSIVE MODE",
        )
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM manager_admin_auth.administrator_grants WHERE installation_id=$1",
        )
        .bind(scope.id)
        .fetch_one(&mut *tx)
        .await
        .map_err(db_error)?;
        if count > 0 && !recover {
            return Err(Error::AlreadyExists);
        }
        sqlx::query("INSERT INTO manager_admin_auth.verified_identities(id,installation_id,issuer,subject) VALUES($1,$2,$3,$4) ON CONFLICT DO NOTHING").bind(Uuid::new_v4()).bind(scope.id).bind(&identity.issuer).bind(&identity.subject).execute(&mut *tx).await.map_err(db_error)?;
        let (id,version):(Uuid,i64)=sqlx::query_as("INSERT INTO manager_admin_auth.administrator_grants(id,installation_id,issuer,subject,active,can_manage_administrators) VALUES($1,$2,$3,$4,true,true) ON CONFLICT(installation_id,issuer,subject) DO UPDATE SET active=true,can_manage_administrators=true,version=administrator_grants.version+1 RETURNING id,version").bind(Uuid::new_v4()).bind(scope.id).bind(&identity.issuer).bind(&identity.subject).fetch_one(&mut *tx).await.map_err(db_error)?;
        super::authoring::authoring_audit(
            &mut tx,
            scope,
            &Identity {
                issuer: "urn:agent-platform:operator".into(),
                subject: "administrator-recovery".into(),
            },
            if recover {
                "administrator.recover"
            } else {
                "administrator.bootstrap"
            },
            "administrator",
            &id.to_string(),
            version,
        )
        .await?;
        tx.commit().await.map_err(db_error)?;
        Ok(())
    }
}
pub struct AdministratorChange {
    pub id: Option<Uuid>,
    pub expected: Option<i64>,
    pub input: AdministratorInput,
}
pub(super) fn grant_view(row: sqlx::postgres::PgRow) -> Value {
    json!({"id":row.get::<Uuid,_>("id"),"identity":{"issuer":row.get::<String,_>("issuer"),"subject":row.get::<String,_>("subject")},"active":row.get::<bool,_>("active"),"canManageAdministrators":row.get::<bool,_>("can_manage_administrators"),"version":row.get::<i64,_>("version"),"source":"postgresql"})
}
