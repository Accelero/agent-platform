//! PostgreSQL-backed browser proof, single-use login and fenced refresh state.
//! Every external provider call occurs outside these short transactions.
use std::time::Duration;

use hermes_application::{Error, InstallationScope};
use hermes_domain::Identity;
use sqlx::{
    PgPool, Postgres, Row, Transaction,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use uuid::Uuid;

use crate::session_crypto::{Purpose, Sealed};

#[derive(Clone)]
pub struct PgSessions {
    pool: PgPool,
    scope: InstallationScope,
    purpose: Purpose,
}

pub struct LoginFlow {
    pub generation: i64,
    pub sealed: Sealed,
}
pub struct StoredSession {
    pub id_hash: Vec<u8>,
    pub browser_hash: Vec<u8>,
    pub generation: i64,
    pub identity: Identity,
    pub sealed: Sealed,
    pub refresh_due: bool,
}
pub struct RefreshClaim {
    pub session: StoredSession,
    pub owner: Uuid,
    pub fence: i64,
}

fn error(_: sqlx::Error) -> Error {
    Error::Unavailable
}
fn sealed(row: &sqlx::postgres::PgRow) -> Sealed {
    Sealed {
        key_id: row.get("key_id"),
        nonce: row.get("nonce"),
        ciphertext: row.get("ciphertext"),
    }
}

impl PgSessions {
    pub async fn authorize_administrator(&self, identity: &Identity) -> Result<(), Error> {
        identity.validate(&self.scope.issuer)?;
        sqlx::query("INSERT INTO manager_admin_auth.verified_identities(id,installation_id,issuer,subject) VALUES($1,$2,$3,$4) ON CONFLICT(installation_id,issuer,subject) DO UPDATE SET verified_at=clock_timestamp()").bind(Uuid::new_v4()).bind(self.scope.id).bind(&identity.issuer).bind(&identity.subject).execute(&self.pool).await.map_err(error)?;
        let granted: bool =
            sqlx::query_scalar("SELECT manager_admin_auth.has_administrator_grant($1,$2,$3)")
                .bind(self.scope.id)
                .bind(&identity.issuer)
                .bind(&identity.subject)
                .fetch_one(&self.pool)
                .await
                .map_err(error)?;
        if !granted {
            return Err(Error::Forbidden);
        }
        Ok(())
    }

    pub async fn connect(
        options: PgConnectOptions,
        scope: InstallationScope,
        purpose: Purpose,
    ) -> Result<Self, Error> {
        hermes_domain::validate_issuer(&scope.issuer)?;
        if scope.id.is_nil() || scope.epoch < 1 {
            return Err(Error::Invalid);
        }
        let pool=PgPoolOptions::new().max_connections(16).acquire_timeout(Duration::from_secs(2))
            .after_connect(move|connection,_|Box::pin(async move{
                let settings=match purpose {
                    Purpose::Administration=>"SET search_path=manager_admin_auth,pg_catalog,pg_temp;SET statement_timeout='3s';SET lock_timeout='1s';SET idle_in_transaction_session_timeout='3s';SET synchronous_commit=on;",
                    Purpose::Router=>"SET search_path=manager_router_auth,pg_catalog,pg_temp;SET statement_timeout='3s';SET lock_timeout='1s';SET idle_in_transaction_session_timeout='3s';SET synchronous_commit=on;",
                };
                sqlx::raw_sql(settings).execute(&mut *connection).await?;
                let durable:bool=sqlx::query_scalar("SELECT current_setting('fsync')='on' AND current_setting('full_page_writes')='on' AND current_setting('synchronous_commit')='on' AND NOT EXISTS(SELECT 1 FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname=current_schema() AND c.relkind='r' AND c.relpersistence<>'p')").fetch_one(connection).await?;
                if !durable{return Err(sqlx::Error::Protocol("authentication requires durable PostgreSQL".into()));}Ok(())
            })).connect_with(options).await.map_err(error)?;
        let store = Self {
            pool,
            scope,
            purpose,
        };
        store.check().await?;
        Ok(store)
    }

    pub fn scope(&self) -> &InstallationScope {
        &self.scope
    }
    pub fn purpose(&self) -> Purpose {
        self.purpose
    }

    async fn gate(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        allow_inactive: bool,
    ) -> Result<(), Error> {
        let mode: String = sqlx::query_scalar("SELECT check_scope($1,$2,$3)")
            .bind(self.scope.id)
            .bind(self.scope.epoch)
            .bind(&self.scope.issuer)
            .fetch_one(&mut **tx)
            .await
            .map_err(|e| {
                if e.as_database_error()
                    .is_some_and(|e| e.code().as_deref() == Some("P0001"))
                {
                    Error::RecoveryRequired
                } else {
                    Error::Unavailable
                }
            })?;
        if !allow_inactive && self.purpose == Purpose::Router && mode != "active" {
            return Err(Error::Maintenance);
        }
        Ok(())
    }

    pub async fn check(&self) -> Result<(), Error> {
        let mut tx = self.pool.begin().await.map_err(error)?;
        self.gate(&mut tx, true).await?;
        tx.commit().await.map_err(error)
    }

    /// Remove bounded batches of expired/terminal credentials, never live work.
    /// Old epochs are removed too once the configured process matches scope.
    pub async fn prune(&self, limit: u32) -> Result<u64, Error> {
        if !(1..=1000).contains(&limit) {
            return Err(Error::Invalid);
        }
        let mut tx = self.pool.begin().await.map_err(error)?;
        self.gate(&mut tx, true).await?;
        let mut removed = 0;
        for query in [
            "DELETE FROM flows WHERE ctid IN (SELECT ctid FROM flows WHERE expires_at<=clock_timestamp() ORDER BY expires_at LIMIT $1)",
            "DELETE FROM sessions WHERE ctid IN (SELECT ctid FROM sessions WHERE expires_at<=clock_timestamp() OR state='revoked' ORDER BY expires_at LIMIT $1)",
            "DELETE FROM browsers WHERE ctid IN (SELECT b.ctid FROM browsers b WHERE expires_at<=clock_timestamp() AND NOT EXISTS(SELECT 1 FROM flows f WHERE f.installation_id=b.installation_id AND f.epoch=b.epoch AND f.browser_hash=b.id_hash) AND NOT EXISTS(SELECT 1 FROM sessions s WHERE s.installation_id=b.installation_id AND s.epoch=b.epoch AND s.browser_hash=b.id_hash) ORDER BY expires_at LIMIT $1)",
        ] {
            removed += sqlx::query(query)
                .bind(i64::from(limit))
                .execute(&mut *tx)
                .await
                .map_err(error)?
                .rows_affected();
        }
        tx.commit().await.map_err(error)?;
        Ok(removed)
    }

    pub async fn begin(
        &self,
        browser_hash: &[u8],
        state_hash: &[u8],
        value: &Sealed,
    ) -> Result<i64, Error> {
        if browser_hash.len() != 32 || state_hash.len() != 32 {
            return Err(Error::Invalid);
        }
        let mut tx = self.pool.begin().await.map_err(error)?;
        self.gate(&mut tx, false).await?;
        let generation:i64=sqlx::query_scalar("INSERT INTO browsers(installation_id,epoch,id_hash,generation,expires_at) VALUES($1,$2,$3,1,clock_timestamp()+interval '8 hours') ON CONFLICT(installation_id,epoch,id_hash) DO UPDATE SET generation=browsers.generation+1,expires_at=clock_timestamp()+interval '8 hours' RETURNING generation")
            .bind(self.scope.id).bind(self.scope.epoch).bind(browser_hash).fetch_one(&mut *tx).await.map_err(error)?;
        sqlx::query("UPDATE sessions SET state='revoked',refresh_owner=NULL,refresh_until=NULL,fence=fence+1 WHERE installation_id=$1 AND epoch=$2 AND browser_hash=$3 AND state<>'revoked'")
            .bind(self.scope.id).bind(self.scope.epoch).bind(browser_hash).execute(&mut *tx).await.map_err(error)?;
        sqlx::query("UPDATE flows SET consumed_at=coalesce(consumed_at,clock_timestamp()) WHERE installation_id=$1 AND epoch=$2 AND browser_hash=$3 AND consumed_at IS NULL")
            .bind(self.scope.id).bind(self.scope.epoch).bind(browser_hash).execute(&mut *tx).await.map_err(error)?;
        sqlx::query("INSERT INTO flows(installation_id,epoch,state_hash,browser_hash,browser_generation,key_id,nonce,ciphertext,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,clock_timestamp()+interval '10 minutes')")
            .bind(self.scope.id).bind(self.scope.epoch).bind(state_hash).bind(browser_hash).bind(generation).bind(&value.key_id).bind(&value.nonce).bind(&value.ciphertext)
            .execute(&mut *tx).await.map_err(error)?;
        tx.commit().await.map_err(error)?;
        Ok(generation)
    }

    async fn browser(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        browser_hash: &[u8],
    ) -> Result<i64, Error> {
        sqlx::query_scalar("SELECT generation FROM browsers WHERE installation_id=$1 AND epoch=$2 AND id_hash=$3 AND expires_at>clock_timestamp() FOR UPDATE")
            .bind(self.scope.id).bind(self.scope.epoch).bind(browser_hash).fetch_optional(&mut **tx).await.map_err(error)?.ok_or(Error::InvalidIdentityEvidence)
    }

    pub async fn consume(
        &self,
        browser_hash: &[u8],
        state_hash: &[u8],
    ) -> Result<LoginFlow, Error> {
        let mut tx = self.pool.begin().await.map_err(error)?;
        self.gate(&mut tx, false).await?;
        let generation = self.browser(&mut tx, browser_hash).await?;
        let row=sqlx::query("UPDATE flows SET consumed_at=clock_timestamp() WHERE installation_id=$1 AND epoch=$2 AND state_hash=$3 AND browser_hash=$4 AND browser_generation=$5 AND consumed_at IS NULL AND expires_at>clock_timestamp() RETURNING key_id,nonce,ciphertext")
            .bind(self.scope.id).bind(self.scope.epoch).bind(state_hash).bind(browser_hash).bind(generation).fetch_optional(&mut *tx).await.map_err(error)?.ok_or(Error::InvalidIdentityEvidence)?;
        let flow = LoginFlow {
            generation,
            sealed: sealed(&row),
        };
        tx.commit().await.map_err(error)?;
        Ok(flow)
    }

    pub async fn create(
        &self,
        browser_hash: &[u8],
        generation: i64,
        state_hash: &[u8],
        session_hash: &[u8],
        identity: &Identity,
        value: &Sealed,
    ) -> Result<(), Error> {
        identity.validate(&self.scope.issuer)?;
        let mut tx = self.pool.begin().await.map_err(error)?;
        self.gate(&mut tx, false).await?;
        if self.browser(&mut tx, browser_hash).await? != generation {
            return Err(Error::InvalidIdentityEvidence);
        }
        let reserved=sqlx::query("UPDATE flows SET session_hash=$6 WHERE installation_id=$1 AND epoch=$2 AND state_hash=$3 AND browser_hash=$4 AND browser_generation=$5 AND consumed_at IS NOT NULL AND session_hash IS NULL AND expires_at>clock_timestamp()")
            .bind(self.scope.id).bind(self.scope.epoch).bind(state_hash).bind(browser_hash).bind(generation).bind(session_hash).execute(&mut *tx).await.map_err(error)?.rows_affected();
        if reserved != 1 {
            return Err(Error::InvalidIdentityEvidence);
        }
        sqlx::query("INSERT INTO sessions(installation_id,epoch,id_hash,browser_hash,browser_generation,issuer,subject,key_id,nonce,ciphertext,expires_at) SELECT $1,$2,$3,$4,$5,$6,$7,$8,$9,$10,expires_at FROM browsers WHERE installation_id=$1 AND epoch=$2 AND id_hash=$4 AND generation=$5")
            .bind(self.scope.id).bind(self.scope.epoch).bind(session_hash).bind(browser_hash).bind(generation).bind(&identity.issuer).bind(&identity.subject).bind(&value.key_id).bind(&value.nonce).bind(&value.ciphertext).execute(&mut *tx).await.map_err(error)?;
        tx.commit().await.map_err(error)
    }

    async fn locked_session(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        session_hash: &[u8],
        expected_browser: Option<&[u8]>,
    ) -> Result<sqlx::postgres::PgRow, Error> {
        let browser_hash:Vec<u8>=sqlx::query_scalar("SELECT browser_hash FROM sessions WHERE installation_id=$1 AND epoch=$2 AND id_hash=$3")
            .bind(self.scope.id).bind(self.scope.epoch).bind(session_hash).fetch_optional(&mut **tx).await.map_err(error)?.ok_or(Error::InvalidIdentityEvidence)?;
        if expected_browser.is_some_and(|expected| expected != browser_hash) {
            return Err(Error::InvalidIdentityEvidence);
        }
        let generation = self.browser(tx, &browser_hash).await?;
        sqlx::query("SELECT id_hash,browser_hash,browser_generation,issuer,subject,key_id,nonce,ciphertext,state,fence,refresh_owner,refresh_until>clock_timestamp() AS refresh_live,verified_at<=clock_timestamp()-interval '60 seconds' AS refresh_due FROM sessions WHERE installation_id=$1 AND epoch=$2 AND id_hash=$3 AND browser_generation=$4 AND expires_at>clock_timestamp() AND state<>'revoked' FOR UPDATE")
            .bind(self.scope.id).bind(self.scope.epoch).bind(session_hash).bind(generation).fetch_optional(&mut **tx).await.map_err(error)?.ok_or(Error::InvalidIdentityEvidence)
    }

    fn stored(row: &sqlx::postgres::PgRow) -> StoredSession {
        StoredSession {
            id_hash: row.get("id_hash"),
            browser_hash: row.get("browser_hash"),
            generation: row.get("browser_generation"),
            identity: Identity {
                issuer: row.get("issuer"),
                subject: row.get("subject"),
            },
            sealed: sealed(row),
            refresh_due: row.get("refresh_due"),
        }
    }

    pub async fn load(
        &self,
        session_hash: &[u8],
        browser_hash: Option<&[u8]>,
    ) -> Result<StoredSession, Error> {
        let mut tx = self.pool.begin().await.map_err(error)?;
        self.gate(&mut tx, false).await?;
        let row = self
            .locked_session(&mut tx, session_hash, browser_hash)
            .await?;
        if row.get::<String, _>("state") == "refreshing" {
            if row.get::<Option<bool>, _>("refresh_live") != Some(true) {
                self.revoke_in(&mut tx, session_hash).await?;
                tx.commit().await.map_err(error)?;
                return Err(Error::InvalidIdentityEvidence);
            }
            return Err(Error::Unavailable);
        }
        let stored = Self::stored(&row);
        tx.commit().await.map_err(error)?;
        Ok(stored)
    }

    pub async fn load_for_logout(
        &self,
        session_hash: &[u8],
        browser_hash: &[u8],
    ) -> Result<StoredSession, Error> {
        let mut tx = self.pool.begin().await.map_err(error)?;
        self.gate(&mut tx, true).await?;
        let row = self
            .locked_session(&mut tx, session_hash, Some(browser_hash))
            .await?;
        let stored = Self::stored(&row);
        tx.commit().await.map_err(error)?;
        Ok(stored)
    }

    pub async fn claim_refresh(
        &self,
        session_hash: &[u8],
        browser_hash: Option<&[u8]>,
    ) -> Result<RefreshClaim, Error> {
        let mut tx = self.pool.begin().await.map_err(error)?;
        self.gate(&mut tx, false).await?;
        let row = self
            .locked_session(&mut tx, session_hash, browser_hash)
            .await?;
        if row.get::<String, _>("state") != "active" {
            return Err(Error::Unavailable);
        }
        let owner = Uuid::new_v4();
        let fence:i64=sqlx::query_scalar("UPDATE sessions SET state='refreshing',refresh_owner=$4,refresh_until=clock_timestamp()+interval '10 seconds',fence=fence+1 WHERE installation_id=$1 AND epoch=$2 AND id_hash=$3 RETURNING fence")
            .bind(self.scope.id).bind(self.scope.epoch).bind(session_hash).bind(owner).fetch_one(&mut *tx).await.map_err(error)?;
        let claim = RefreshClaim {
            session: Self::stored(&row),
            owner,
            fence,
        };
        tx.commit().await.map_err(error)?;
        Ok(claim)
    }

    pub async fn finish_refresh(
        &self,
        claim: &RefreshClaim,
        identity: &Identity,
        value: &Sealed,
    ) -> Result<(), Error> {
        if *identity != claim.session.identity {
            return Err(Error::InvalidIdentityEvidence);
        }
        let mut tx = self.pool.begin().await.map_err(error)?;
        self.gate(&mut tx, false).await?;
        if self.browser(&mut tx, &claim.session.browser_hash).await? != claim.session.generation {
            return Err(Error::InvalidIdentityEvidence);
        }
        let changed=sqlx::query("UPDATE sessions SET state='active',key_id=$6,nonce=$7,ciphertext=$8,verified_at=clock_timestamp(),refresh_owner=NULL,refresh_until=NULL WHERE installation_id=$1 AND epoch=$2 AND id_hash=$3 AND state='refreshing' AND refresh_owner=$4 AND fence=$5 AND refresh_until>clock_timestamp() AND expires_at>clock_timestamp()")
            .bind(self.scope.id).bind(self.scope.epoch).bind(&claim.session.id_hash).bind(claim.owner).bind(claim.fence).bind(&value.key_id).bind(&value.nonce).bind(&value.ciphertext).execute(&mut *tx).await.map_err(error)?.rows_affected();
        if changed != 1 {
            return Err(Error::InvalidIdentityEvidence);
        }
        tx.commit().await.map_err(error)
    }

    async fn revoke_in(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        session_hash: &[u8],
    ) -> Result<(), Error> {
        sqlx::query("UPDATE sessions SET state='revoked',refresh_owner=NULL,refresh_until=NULL,fence=fence+1 WHERE installation_id=$1 AND epoch=$2 AND id_hash=$3")
            .bind(self.scope.id).bind(self.scope.epoch).bind(session_hash).execute(&mut **tx).await.map_err(error)?;
        Ok(())
    }

    pub async fn revoke(&self, session_hash: &[u8]) -> Result<(), Error> {
        let mut tx = self.pool.begin().await.map_err(error)?;
        self.gate(&mut tx, true).await?;
        self.revoke_in(&mut tx, session_hash).await?;
        tx.commit().await.map_err(error)
    }

    pub async fn logout(&self, browser_hash: &[u8]) -> Result<(), Error> {
        let mut tx = self.pool.begin().await.map_err(error)?;
        self.gate(&mut tx, true).await?;
        sqlx::query("UPDATE browsers SET generation=generation+1 WHERE installation_id=$1 AND epoch=$2 AND id_hash=$3")
            .bind(self.scope.id).bind(self.scope.epoch).bind(browser_hash).execute(&mut *tx).await.map_err(error)?;
        sqlx::query("UPDATE sessions SET state='revoked',refresh_owner=NULL,refresh_until=NULL,fence=fence+1 WHERE installation_id=$1 AND epoch=$2 AND browser_hash=$3 AND state<>'revoked'")
            .bind(self.scope.id).bind(self.scope.epoch).bind(browser_hash).execute(&mut *tx).await.map_err(error)?;
        sqlx::query("UPDATE flows SET consumed_at=coalesce(consumed_at,clock_timestamp()) WHERE installation_id=$1 AND epoch=$2 AND browser_hash=$3 AND consumed_at IS NULL")
            .bind(self.scope.id).bind(self.scope.epoch).bind(browser_hash).execute(&mut *tx).await.map_err(error)?;
        tx.commit().await.map_err(error)
    }
}
