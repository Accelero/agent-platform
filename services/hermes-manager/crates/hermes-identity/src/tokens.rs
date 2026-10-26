//! Verification of Keycloak access tokens. Identity never comes from headers,
//! unverified claims or token roles.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use hermes_core::domain::identity::{Identity, validate_issuer};
pub use jsonwebtoken::Algorithm;
use jsonwebtoken::jwk::{JwkSet, KeyOperations, PublicKeyUse};
use jsonwebtoken::{DecodingKey, Validation, decode, decode_header};
use serde::Deserialize;
use tokio::sync::{Mutex, RwLock};
use url::Url;

use crate::{AuthError, unix_time};

const MAX_TOKEN: usize = 16 * 1024;
const MAX_JWKS: usize = 256 * 1024;
const KEY_TTL: Duration = Duration::from_secs(300);
const REFRESH_COOLDOWN: Duration = Duration::from_secs(5);

/// What a token must satisfy for one audience.
#[derive(Clone)]
pub struct TokenPolicy {
    pub issuer: String,
    /// Must be on the issuer's HTTPS origin.
    pub jwks_url: String,
    pub audience: String,
    /// Allowed `azp` values: the OIDC clients that may obtain this audience.
    pub authorized_clients: BTreeSet<String>,
    pub algorithms: Vec<Algorithm>,
    /// Rejects tokens issued longer ago than this, when set (at most 300 s).
    pub maximum_age_seconds: Option<u64>,
}

struct CachedKeys {
    keys: JwkSet,
    fetched: Instant,
}

#[derive(Clone)]
pub struct AccessTokenVerifier {
    policy: TokenPolicy,
    client: reqwest::Client,
    cache: Arc<RwLock<Option<CachedKeys>>>,
    refresh: Arc<Mutex<()>>,
}

#[derive(Deserialize, Clone)]
#[serde(untagged)]
enum Audiences {
    One(String),
    Many(Vec<String>),
}

impl Audiences {
    fn len(&self) -> usize {
        match self {
            Self::One(_) => 1,
            Self::Many(values) => values.len(),
        }
    }

    fn contains(&self, audience: &str) -> bool {
        match self {
            Self::One(value) => value == audience,
            Self::Many(values) => values.iter().any(|value| value == audience),
        }
    }
}

#[derive(Deserialize, Clone)]
struct AccessClaims {
    iss: String,
    sub: String,
    aud: Audiences,
    azp: String,
    typ: String,
    sid: String,
    #[serde(default)]
    gty: Option<String>,
    iat: u64,
    exp: u64,
}

/// A verified token. Deliberately has no `Debug` or `Serialize`.
pub struct VerifiedAccess {
    claims: AccessClaims,
}

impl VerifiedAccess {
    pub fn identity(&self) -> Identity {
        Identity {
            issuer: self.claims.iss.clone(),
            subject: self.claims.sub.clone(),
        }
    }

    pub fn issued_at(&self) -> u64 {
        self.claims.iat
    }

    pub fn expires_at(&self) -> u64 {
        self.claims.exp
    }

    /// Keycloak's user-session ID (`sid`).
    pub fn provider_session(&self) -> &str {
        &self.claims.sid
    }

    pub fn authorized_client(&self) -> &str {
        &self.claims.azp
    }
}

impl AccessTokenVerifier {
    pub fn new(policy: TokenPolicy, client: reqwest::Client) -> Result<Self, AuthError> {
        validate_issuer(&policy.issuer).map_err(|_| AuthError::Configuration)?;
        let issuer = Url::parse(&policy.issuer).map_err(|_| AuthError::Configuration)?;
        let jwks = Url::parse(&policy.jwks_url).map_err(|_| AuthError::Configuration)?;
        let valid = jwks.origin() == issuer.origin()
            && jwks.username().is_empty()
            && jwks.password().is_none()
            && jwks.query().is_none()
            && jwks.fragment().is_none()
            && !policy.audience.is_empty()
            && !policy.authorized_clients.is_empty()
            && !policy.authorized_clients.iter().any(String::is_empty)
            && !policy.algorithms.is_empty()
            && policy
                .algorithms
                .iter()
                .all(|a| matches!(a, Algorithm::RS256 | Algorithm::ES256))
            && policy
                .maximum_age_seconds
                .is_none_or(|age| (1..=300).contains(&age));
        if !valid {
            return Err(AuthError::Configuration);
        }
        Ok(Self {
            policy,
            client,
            cache: Arc::new(RwLock::new(None)),
            refresh: Arc::new(Mutex::new(())),
        })
    }

    pub fn policy(&self) -> &TokenPolicy {
        &self.policy
    }

    pub async fn verify(&self, token: &str) -> Result<VerifiedAccess, AuthError> {
        if token.is_empty() || token.len() > MAX_TOKEN {
            return Err(AuthError::Evidence);
        }
        // Only `kid` and `alg` are read before verification, to pick a pinned key.
        let header = decode_header(token).map_err(|_| AuthError::Evidence)?;
        if !self.policy.algorithms.contains(&header.alg)
            || !matches!(header.typ.as_deref(), Some("JWT" | "at+jwt"))
            || header.jku.is_some()
            || header.jwk.is_some()
            || header.x5u.is_some()
            || header.crit.is_some()
            || header.enc.is_some()
            || header.zip.is_some()
        {
            return Err(AuthError::Evidence);
        }
        let kid = header
            .kid
            .as_deref()
            .filter(|kid| !kid.is_empty() && kid.len() <= 128)
            .ok_or(AuthError::Evidence)?;
        let key = tokio::time::timeout(Duration::from_millis(1400), self.key(kid, header.alg))
            .await
            .map_err(|_| AuthError::Unavailable)??;
        let mut validation = Validation::new(header.alg);
        validation.leeway = 0;
        validation.validate_nbf = true;
        validation.set_issuer(&[&self.policy.issuer]);
        validation.set_audience(&[&self.policy.audience]);
        validation.set_required_spec_claims(&["iss", "sub", "aud", "iat", "exp"]);
        let claims = decode::<AccessClaims>(token, &key, &validation)
            .map_err(|_| AuthError::Evidence)?
            .claims;
        let now = unix_time()?;
        let identity = Identity {
            issuer: claims.iss.clone(),
            subject: claims.sub.clone(),
        };
        let client_credentials = claims
            .gty
            .as_deref()
            .is_some_and(|grant| matches!(grant, "client_credentials" | "client-credentials"));
        let valid = identity.validate(&self.policy.issuer).is_ok()
            && claims.typ == "Bearer"
            && !claims.sid.is_empty()
            && claims.sid.len() <= 255
            && !client_credentials
            && self.policy.authorized_clients.contains(&claims.azp)
            && claims.iat <= now
            && claims.exp > now
            && claims.exp > claims.iat
            && self
                .policy
                .maximum_age_seconds
                .is_none_or(|age| now.saturating_sub(claims.iat) <= age)
            && claims.aud.len() <= 32
            && claims.aud.contains(&self.policy.audience);
        if !valid {
            return Err(AuthError::Evidence);
        }
        Ok(VerifiedAccess { claims })
    }

    async fn cached_key(
        &self,
        kid: &str,
        algorithm: Algorithm,
    ) -> Result<Option<DecodingKey>, AuthError> {
        let cache = self.cache.read().await;
        let Some(snapshot) = cache.as_ref().filter(|s| s.fetched.elapsed() < KEY_TTL) else {
            return Ok(None);
        };
        let matches: Vec<_> = snapshot
            .keys
            .keys
            .iter()
            .filter(|k| k.common.key_id.as_deref() == Some(kid))
            .collect();
        let [jwk] = matches.as_slice() else {
            return if matches.is_empty() {
                Ok(None)
            } else {
                Err(AuthError::Evidence)
            };
        };
        let unusable = jwk
            .common
            .key_operations
            .as_ref()
            .is_some_and(|operations| !operations.contains(&KeyOperations::Verify))
            || jwk
                .common
                .public_key_use
                .as_ref()
                .is_some_and(|usage| usage != &PublicKeyUse::Signature)
            || jwk
                .common
                .key_algorithm
                .as_ref()
                .is_some_and(|alg| alg.to_string() != format!("{algorithm:?}"));
        if unusable {
            return Err(AuthError::Evidence);
        }
        DecodingKey::from_jwk(jwk)
            .map(Some)
            .map_err(|_| AuthError::Evidence)
    }

    async fn key(&self, kid: &str, algorithm: Algorithm) -> Result<DecodingKey, AuthError> {
        if let Some(key) = self.cached_key(kid, algorithm).await? {
            return Ok(key);
        }
        let _guard = self.refresh.lock().await;
        if let Some(key) = self.cached_key(kid, algorithm).await? {
            return Ok(key);
        }
        // An unknown kid right after a fetch is not a reason to fetch again.
        let recent = self
            .cache
            .read()
            .await
            .as_ref()
            .is_some_and(|s| s.fetched.elapsed() < REFRESH_COOLDOWN);
        if recent {
            return Err(AuthError::Evidence);
        }
        let mut response = self
            .client
            .get(&self.policy.jwks_url)
            .timeout(Duration::from_secs(1))
            .send()
            .await
            .map_err(|_| AuthError::Unavailable)?;
        if !response.status().is_success()
            || response
                .content_length()
                .is_some_and(|n| n > MAX_JWKS as u64)
        {
            return Err(AuthError::Unavailable);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| AuthError::Unavailable)? {
            if bytes.len() + chunk.len() > MAX_JWKS {
                return Err(AuthError::Unavailable);
            }
            bytes.extend_from_slice(&chunk);
        }
        let keys: JwkSet = serde_json::from_slice(&bytes).map_err(|_| AuthError::Unavailable)?;
        if keys.keys.is_empty() || keys.keys.len() > 32 {
            return Err(AuthError::Unavailable);
        }
        *self.cache.write().await = Some(CachedKeys {
            keys,
            fetched: Instant::now(),
        });
        self.cached_key(kid, algorithm)
            .await?
            .ok_or(AuthError::Evidence)
    }
}
