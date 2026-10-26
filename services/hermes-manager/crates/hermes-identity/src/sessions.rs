//! PostgreSQL session store: browser proofs, single-use login flows and fenced
//! refresh claims. Each method is one short transaction; provider calls happen
//! between them, never inside.

use std::str::FromStr;
use std::time::Duration;

use hermes_core::domain::identity::Identity;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgRow};
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

use crate::AuthError;
use crate::crypto::{Purpose, Sealed};

/// Absolute session and browser-proof lifetime.
const SESSION_HOURS: &str = "8 hours";

#[derive(Clone)]
pub struct PgSessions {
    pool: PgPool,
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
    /// Provider state was last verified more than 60 seconds ago.
    pub refresh_due: bool,
}

pub struct RefreshClaim {
    pub session: StoredSession,
    pub owner: Uuid,
    pub fence: i64,
}

fn unavailable(_: sqlx::Error) -> AuthError {
    AuthError::Unavailable
}

fn sealed(row: &PgRow) -> Sealed {
    Sealed {
        key_id: row.get("key_id"),
        nonce: row.get("nonce"),
        ciphertext: row.get("ciphertext"),
    }
}

impl PgSessions {
    /// Connects a pool whose search path is the purpose's own schema, so the
    /// same queries serve both stores and each can use its own database role.
    pub async fn connect(database_url: &str, purpose: Purpose) -> Result<Self, AuthError> {
        let options =
            PgConnectOptions::from_str(database_url).map_err(|_| AuthError::Unavailable)?;
        Self::connect_with(options, purpose).await
    }

    pub async fn connect_with(
        options: PgConnectOptions,
        purpose: Purpose,
    ) -> Result<Self, AuthError> {
        let pool = PgPoolOptions::new()
            .max_connections(16)
            .acquire_timeout(Duration::from_secs(2))
            .after_connect(move |connection, _| {
                Box::pin(async move {
                    let settings = match purpose {
                        Purpose::Administration => {
                            "SET search_path = admin_sessions, pg_catalog, pg_temp; \
                             SET statement_timeout = '3s'; SET lock_timeout = '1s'; \
                             SET idle_in_transaction_session_timeout = '3s'; SET synchronous_commit = on;"
                        }
                        Purpose::Gateway => {
                            "SET search_path = gateway_sessions, pg_catalog, pg_temp; \
                             SET statement_timeout = '3s'; SET lock_timeout = '1s'; \
                             SET idle_in_transaction_session_timeout = '3s'; SET synchronous_commit = on;"
                        }
                    };
                    sqlx::raw_sql(settings).execute(&mut *connection).await?;
                    let durable: bool = sqlx::query_scalar(
                        "SELECT current_setting('fsync') = 'on' AND current_setting('full_page_writes') = 'on' \
                         AND current_setting('synchronous_commit') = 'on' AND NOT EXISTS (SELECT 1 FROM pg_class c \
                         JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = current_schema() \
                         AND c.relkind = 'r' AND c.relpersistence <> 'p')",
                    )
                    .fetch_one(connection)
                    .await?;
                    if !durable {
                        return Err(sqlx::Error::Protocol("sessions require durable PostgreSQL".into()));
                    }
                    Ok(())
                })
            })
            .connect_with(options)
            .await
            .map_err(unavailable)?;
        Ok(Self { pool, purpose })
    }

    pub fn purpose(&self) -> Purpose {
        self.purpose
    }

    /// Removes a bounded batch of expired or revoked records, never live ones.
    pub async fn prune(&self, limit: u32) -> Result<u64, AuthError> {
        if !(1..=1000).contains(&limit) {
            return Err(AuthError::Configuration);
        }
        let mut tx = self.pool.begin().await.map_err(unavailable)?;
        let mut removed = 0;
        for query in [
            "DELETE FROM flows WHERE ctid IN (SELECT ctid FROM flows WHERE expires_at <= clock_timestamp() \
             ORDER BY expires_at LIMIT $1)",
            "DELETE FROM sessions WHERE ctid IN (SELECT ctid FROM sessions WHERE expires_at <= clock_timestamp() \
             OR state = 'revoked' ORDER BY expires_at LIMIT $1)",
            "DELETE FROM browsers WHERE ctid IN (SELECT b.ctid FROM browsers b WHERE expires_at <= clock_timestamp() \
             AND NOT EXISTS (SELECT 1 FROM flows f WHERE f.browser_hash = b.id_hash) \
             AND NOT EXISTS (SELECT 1 FROM sessions s WHERE s.browser_hash = b.id_hash) \
             ORDER BY expires_at LIMIT $1)",
        ] {
            removed += sqlx::query(query)
                .bind(i64::from(limit))
                .execute(&mut *tx)
                .await
                .map_err(unavailable)?
                .rows_affected();
        }
        tx.commit().await.map_err(unavailable)?;
        Ok(removed)
    }

    /// Starts a login: advances the browser generation (invalidating its older
    /// sessions and flows) and stores the sealed flow secrets.
    pub async fn begin(
        &self,
        browser_hash: &[u8],
        state_hash: &[u8],
        value: &Sealed,
    ) -> Result<i64, AuthError> {
        if browser_hash.len() != 32 || state_hash.len() != 32 {
            return Err(AuthError::Configuration);
        }
        let mut tx = self.pool.begin().await.map_err(unavailable)?;
        let generation: i64 = sqlx::query_scalar(
            "INSERT INTO browsers (id_hash, generation, expires_at) \
             VALUES ($1, 1, clock_timestamp() + $2::interval) \
             ON CONFLICT (id_hash) DO UPDATE SET generation = browsers.generation + 1, \
             expires_at = clock_timestamp() + $2::interval RETURNING generation",
        )
        .bind(browser_hash)
        .bind(SESSION_HOURS)
        .fetch_one(&mut *tx)
        .await
        .map_err(unavailable)?;
        revoke_browser_sessions(&mut tx, browser_hash).await?;
        sqlx::query(
            "INSERT INTO flows (state_hash, browser_hash, browser_generation, key_id, nonce, ciphertext, expires_at) \
             VALUES ($1, $2, $3, $4, $5, $6, clock_timestamp() + interval '10 minutes')",
        )
        .bind(state_hash)
        .bind(browser_hash)
        .bind(generation)
        .bind(&value.key_id)
        .bind(&value.nonce)
        .bind(&value.ciphertext)
        .execute(&mut *tx)
        .await
        .map_err(unavailable)?;
        tx.commit().await.map_err(unavailable)?;
        Ok(generation)
    }

    async fn browser(
        tx: &mut Transaction<'_, Postgres>,
        browser_hash: &[u8],
    ) -> Result<i64, AuthError> {
        sqlx::query_scalar(
            "SELECT generation FROM browsers WHERE id_hash = $1 AND expires_at > clock_timestamp() FOR UPDATE",
        )
        .bind(browser_hash)
        .fetch_optional(&mut **tx)
        .await
        .map_err(unavailable)?
        .ok_or(AuthError::Evidence)
    }

    /// Consumes a flow exactly once, for the browser generation that began it.
    pub async fn consume(
        &self,
        browser_hash: &[u8],
        state_hash: &[u8],
    ) -> Result<LoginFlow, AuthError> {
        let mut tx = self.pool.begin().await.map_err(unavailable)?;
        let generation = Self::browser(&mut tx, browser_hash).await?;
        let row = sqlx::query(
            "UPDATE flows SET consumed_at = clock_timestamp() WHERE state_hash = $1 AND browser_hash = $2 \
             AND browser_generation = $3 AND consumed_at IS NULL AND expires_at > clock_timestamp() \
             RETURNING key_id, nonce, ciphertext",
        )
        .bind(state_hash)
        .bind(browser_hash)
        .bind(generation)
        .fetch_optional(&mut *tx)
        .await
        .map_err(unavailable)?
        .ok_or(AuthError::Evidence)?;
        let flow = LoginFlow {
            generation,
            sealed: sealed(&row),
        };
        tx.commit().await.map_err(unavailable)?;
        Ok(flow)
    }

    /// Creates the session for a consumed flow, once, while the browser is
    /// still at the flow's generation.
    pub async fn create(
        &self,
        browser_hash: &[u8],
        generation: i64,
        state_hash: &[u8],
        session_hash: &[u8],
        identity: &Identity,
        value: &Sealed,
    ) -> Result<(), AuthError> {
        let mut tx = self.pool.begin().await.map_err(unavailable)?;
        if Self::browser(&mut tx, browser_hash).await? != generation {
            return Err(AuthError::Evidence);
        }
        let reserved = sqlx::query(
            "UPDATE flows SET session_hash = $4 WHERE state_hash = $1 AND browser_hash = $2 \
             AND browser_generation = $3 AND consumed_at IS NOT NULL AND session_hash IS NULL \
             AND expires_at > clock_timestamp()",
        )
        .bind(state_hash)
        .bind(browser_hash)
        .bind(generation)
        .bind(session_hash)
        .execute(&mut *tx)
        .await
        .map_err(unavailable)?
        .rows_affected();
        if reserved != 1 {
            return Err(AuthError::Evidence);
        }
        sqlx::query(
            "INSERT INTO sessions (id_hash, browser_hash, browser_generation, issuer, subject, key_id, nonce, \
             ciphertext, expires_at) SELECT $1, $2, $3, $4, $5, $6, $7, $8, expires_at FROM browsers \
             WHERE id_hash = $2 AND generation = $3",
        )
        .bind(session_hash)
        .bind(browser_hash)
        .bind(generation)
        .bind(&identity.issuer)
        .bind(&identity.subject)
        .bind(&value.key_id)
        .bind(&value.nonce)
        .bind(&value.ciphertext)
        .execute(&mut *tx)
        .await
        .map_err(unavailable)?;
        tx.commit().await.map_err(unavailable)
    }

    async fn locked_session(
        tx: &mut Transaction<'_, Postgres>,
        session_hash: &[u8],
        expected_browser: Option<&[u8]>,
    ) -> Result<PgRow, AuthError> {
        let browser_hash: Vec<u8> =
            sqlx::query_scalar("SELECT browser_hash FROM sessions WHERE id_hash = $1")
                .bind(session_hash)
                .fetch_optional(&mut **tx)
                .await
                .map_err(unavailable)?
                .ok_or(AuthError::Evidence)?;
        if expected_browser.is_some_and(|expected| expected != browser_hash) {
            return Err(AuthError::Evidence);
        }
        let generation = Self::browser(tx, &browser_hash).await?;
        sqlx::query(
            "SELECT id_hash, browser_hash, browser_generation, issuer, subject, key_id, nonce, ciphertext, state, \
             fence, refresh_until > clock_timestamp() AS refresh_live, \
             verified_at <= clock_timestamp() - interval '60 seconds' AS refresh_due \
             FROM sessions WHERE id_hash = $1 AND browser_generation = $2 \
             AND expires_at > clock_timestamp() AND state <> 'revoked' FOR UPDATE",
        )
        .bind(session_hash)
        .bind(generation)
        .fetch_optional(&mut **tx)
        .await
        .map_err(unavailable)?
        .ok_or(AuthError::Evidence)
    }

    fn stored(row: &PgRow) -> StoredSession {
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

    /// Loads a live session. A session whose refresh claim lapsed was
    /// interrupted mid-refresh: it is revoked rather than replayed.
    pub async fn load(
        &self,
        session_hash: &[u8],
        browser_hash: Option<&[u8]>,
    ) -> Result<StoredSession, AuthError> {
        let mut tx = self.pool.begin().await.map_err(unavailable)?;
        let row = Self::locked_session(&mut tx, session_hash, browser_hash).await?;
        if row.get::<String, _>("state") == "refreshing" {
            if row.get::<Option<bool>, _>("refresh_live") != Some(true) {
                revoke(&mut tx, session_hash).await?;
                tx.commit().await.map_err(unavailable)?;
                return Err(AuthError::Evidence);
            }
            return Err(AuthError::Unavailable);
        }
        let stored = Self::stored(&row);
        tx.commit().await.map_err(unavailable)?;
        Ok(stored)
    }

    /// Loads a session for logout, including one that is mid-refresh.
    pub async fn load_for_logout(
        &self,
        session_hash: &[u8],
        browser_hash: &[u8],
    ) -> Result<StoredSession, AuthError> {
        let mut tx = self.pool.begin().await.map_err(unavailable)?;
        let row = Self::locked_session(&mut tx, session_hash, Some(browser_hash)).await?;
        let stored = Self::stored(&row);
        tx.commit().await.map_err(unavailable)?;
        Ok(stored)
    }

    /// Claims the right to refresh, with a 10-second deadline and a new fence.
    pub async fn claim_refresh(
        &self,
        session_hash: &[u8],
        browser_hash: Option<&[u8]>,
    ) -> Result<RefreshClaim, AuthError> {
        let mut tx = self.pool.begin().await.map_err(unavailable)?;
        let row = Self::locked_session(&mut tx, session_hash, browser_hash).await?;
        if row.get::<String, _>("state") != "active" {
            return Err(AuthError::Unavailable);
        }
        let owner = Uuid::new_v4();
        let fence: i64 = sqlx::query_scalar(
            "UPDATE sessions SET state = 'refreshing', refresh_owner = $2, \
             refresh_until = clock_timestamp() + interval '10 seconds', fence = fence + 1 \
             WHERE id_hash = $1 RETURNING fence",
        )
        .bind(session_hash)
        .bind(owner)
        .fetch_one(&mut *tx)
        .await
        .map_err(unavailable)?;
        let claim = RefreshClaim {
            session: Self::stored(&row),
            owner,
            fence,
        };
        tx.commit().await.map_err(unavailable)?;
        Ok(claim)
    }

    /// Commits a refresh only for the current, unexpired claim.
    pub async fn finish_refresh(
        &self,
        claim: &RefreshClaim,
        identity: &Identity,
        value: &Sealed,
    ) -> Result<(), AuthError> {
        if *identity != claim.session.identity {
            return Err(AuthError::Evidence);
        }
        let mut tx = self.pool.begin().await.map_err(unavailable)?;
        if Self::browser(&mut tx, &claim.session.browser_hash).await? != claim.session.generation {
            return Err(AuthError::Evidence);
        }
        let changed = sqlx::query(
            "UPDATE sessions SET state = 'active', key_id = $4, nonce = $5, ciphertext = $6, \
             verified_at = clock_timestamp(), refresh_owner = NULL, refresh_until = NULL \
             WHERE id_hash = $1 AND state = 'refreshing' AND refresh_owner = $2 AND fence = $3 \
             AND refresh_until > clock_timestamp() AND expires_at > clock_timestamp()",
        )
        .bind(&claim.session.id_hash)
        .bind(claim.owner)
        .bind(claim.fence)
        .bind(&value.key_id)
        .bind(&value.nonce)
        .bind(&value.ciphertext)
        .execute(&mut *tx)
        .await
        .map_err(unavailable)?
        .rows_affected();
        if changed != 1 {
            return Err(AuthError::Evidence);
        }
        tx.commit().await.map_err(unavailable)
    }

    pub async fn revoke(&self, session_hash: &[u8]) -> Result<(), AuthError> {
        let mut tx = self.pool.begin().await.map_err(unavailable)?;
        revoke(&mut tx, session_hash).await?;
        tx.commit().await.map_err(unavailable)
    }

    /// Ends every session and pending flow of a browser.
    pub async fn logout(&self, browser_hash: &[u8]) -> Result<(), AuthError> {
        let mut tx = self.pool.begin().await.map_err(unavailable)?;
        sqlx::query("UPDATE browsers SET generation = generation + 1 WHERE id_hash = $1")
            .bind(browser_hash)
            .execute(&mut *tx)
            .await
            .map_err(unavailable)?;
        revoke_browser_sessions(&mut tx, browser_hash).await?;
        tx.commit().await.map_err(unavailable)
    }
}

async fn revoke(tx: &mut Transaction<'_, Postgres>, session_hash: &[u8]) -> Result<(), AuthError> {
    sqlx::query(
        "UPDATE sessions SET state = 'revoked', refresh_owner = NULL, refresh_until = NULL, fence = fence + 1 \
         WHERE id_hash = $1",
    )
    .bind(session_hash)
    .execute(&mut **tx)
    .await
    .map_err(unavailable)?;
    Ok(())
}

async fn revoke_browser_sessions(
    tx: &mut Transaction<'_, Postgres>,
    browser_hash: &[u8],
) -> Result<(), AuthError> {
    sqlx::query(
        "UPDATE sessions SET state = 'revoked', refresh_owner = NULL, refresh_until = NULL, fence = fence + 1 \
         WHERE browser_hash = $1 AND state <> 'revoked'",
    )
    .bind(browser_hash)
    .execute(&mut **tx)
    .await
    .map_err(unavailable)?;
    sqlx::query(
        "UPDATE flows SET consumed_at = coalesce(consumed_at, clock_timestamp()) \
         WHERE browser_hash = $1 AND consumed_at IS NULL",
    )
    .bind(browser_hash)
    .execute(&mut **tx)
    .await
    .map_err(unavailable)?;
    Ok(())
}
