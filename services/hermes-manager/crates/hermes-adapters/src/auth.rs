//! Verification of Keycloak ACCESS tokens, never identity from proxy headers.
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use hermes_application::{Error, VerifiedPrincipal, routing::VerifiedRouteEvidence};
use hermes_domain::{Identity, validate_issuer};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header, jwk::JwkSet};
use serde::Deserialize;
use tokio::sync::{Mutex, RwLock};
use url::Url;

const MAX_TOKEN: usize = 16 * 1024;
const MAX_JWKS: usize = 256 * 1024;
const KEY_TTL: Duration = Duration::from_secs(300);
const REFRESH_COOLDOWN: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub struct TokenPolicy {
    pub issuer: String,
    pub jwks_url: String,
    pub audience: String,
    pub authorized_clients: BTreeSet<String>,
    pub algorithms: Vec<Algorithm>,
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
    fn set(self) -> BTreeSet<String> {
        match self {
            Self::One(value) => BTreeSet::from([value]),
            Self::Many(values) => values.into_iter().collect(),
        }
    }
}

#[derive(Deserialize, Clone, Default)]
struct ClientAccess {
    roles: BTreeSet<String>,
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
    #[serde(default)]
    resource_access: BTreeMap<String, ClientAccess>,
}

/// Secrets deliberately have no Debug/Serialize representation.
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
    pub fn principal(&self) -> VerifiedPrincipal {
        VerifiedPrincipal {
            identity: self.identity(),
            audiences: self.claims.aud.clone().set(),
            client_roles: self
                .claims
                .resource_access
                .iter()
                .map(|(id, access)| (id.clone(), access.roles.clone()))
                .collect(),
        }
    }
    pub fn route_evidence(&self) -> Result<VerifiedRouteEvidence, Error> {
        Ok(VerifiedRouteEvidence {
            identity: self.identity(),
            audiences: self.claims.aud.clone().set(),
            authorized_client: self.claims.azp.clone(),
            issued_at: i64::try_from(self.claims.iat)
                .map_err(|_| Error::InvalidIdentityEvidence)?,
            expires_at: i64::try_from(self.claims.exp)
                .map_err(|_| Error::InvalidIdentityEvidence)?,
        })
    }
}

pub fn unix_time() -> Result<u64, Error> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .map_err(|_| Error::Unavailable)
}

impl AccessTokenVerifier {
    pub fn new(policy: TokenPolicy, client: reqwest::Client) -> Result<Self, Error> {
        validate_issuer(&policy.issuer)?;
        let issuer = Url::parse(&policy.issuer).map_err(|_| Error::Invalid)?;
        let jwks = Url::parse(&policy.jwks_url).map_err(|_| Error::Invalid)?;
        // The deployment pins the JWKS endpoint to the issuer's HTTPS origin.
        // Discovery/client-supplied URLs cannot redirect key fetches elsewhere.
        if jwks.origin() != issuer.origin()
            || !jwks.username().is_empty()
            || jwks.password().is_some()
            || jwks.query().is_some()
            || jwks.fragment().is_some()
            || policy.audience.is_empty()
            || policy.authorized_clients.is_empty()
            || policy.authorized_clients.iter().any(String::is_empty)
            || policy.algorithms.is_empty()
            || policy
                .algorithms
                .iter()
                .any(|a| !matches!(a, Algorithm::RS256 | Algorithm::ES256))
            || policy
                .maximum_age_seconds
                .is_some_and(|age| age == 0 || age > 300)
        {
            return Err(Error::Invalid);
        }
        Ok(Self {
            policy,
            client,
            cache: Arc::new(RwLock::new(None)),
            refresh: Arc::new(Mutex::new(())),
        })
    }

    pub async fn verify(&self, token: &str) -> Result<VerifiedAccess, Error> {
        if token.is_empty() || token.len() > MAX_TOKEN {
            return Err(Error::InvalidIdentityEvidence);
        }
        // Only kid/alg are read here, solely to choose a pinned public key. No
        // unverified claim ever becomes an identity, actor or destination.
        let header = decode_header(token).map_err(|_| Error::InvalidIdentityEvidence)?;
        if !self.policy.algorithms.contains(&header.alg)
            || !matches!(header.typ.as_deref(), Some("JWT" | "at+jwt"))
            || header.jku.is_some()
            || header.jwk.is_some()
            || header.x5u.is_some()
            || header.crit.is_some()
            || header.enc.is_some()
            || header.zip.is_some()
        {
            return Err(Error::InvalidIdentityEvidence);
        }
        let kid = header
            .kid
            .as_deref()
            .filter(|kid| !kid.is_empty() && kid.len() <= 128)
            .ok_or(Error::InvalidIdentityEvidence)?;
        let key = tokio::time::timeout(Duration::from_millis(1400), self.key(kid, header.alg))
            .await
            .map_err(|_| Error::Unavailable)??;
        let mut validation = Validation::new(header.alg);
        validation.leeway = 0;
        validation.validate_nbf = true;
        validation.set_issuer(&[&self.policy.issuer]);
        validation.set_audience(&[&self.policy.audience]);
        validation.set_required_spec_claims(&["iss", "sub", "aud", "iat", "exp"]);
        let claims = decode::<AccessClaims>(token, &key, &validation)
            .map_err(|_| Error::InvalidIdentityEvidence)?
            .claims;
        let now = unix_time()?;
        let identity = Identity {
            issuer: claims.iss.clone(),
            subject: claims.sub.clone(),
        };
        if identity.validate(&self.policy.issuer).is_err()
            || claims.typ != "Bearer"
            || claims.sid.is_empty()
            || claims.sid.len() > 255
            || claims
                .gty
                .as_deref()
                .is_some_and(|grant| matches!(grant, "client_credentials" | "client-credentials"))
            || !self.policy.authorized_clients.contains(&claims.azp)
            || claims.iat > now
            || claims.exp <= now
            || claims.exp <= claims.iat
            || self
                .policy
                .maximum_age_seconds
                .is_some_and(|age| now.saturating_sub(claims.iat) > age)
            || claims.aud.clone().set().len() > 32
            || claims.resource_access.len() > 64
            || claims.resource_access.values().any(|a| a.roles.len() > 128)
        {
            return Err(Error::InvalidIdentityEvidence);
        }
        Ok(VerifiedAccess { claims })
    }

    async fn cached_key(
        &self,
        kid: &str,
        algorithm: Algorithm,
    ) -> Result<Option<DecodingKey>, Error> {
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
        if matches.len() > 1 {
            return Err(Error::InvalidIdentityEvidence);
        }
        let Some(jwk) = matches.first() else {
            return Ok(None);
        };
        if jwk
            .common
            .key_operations
            .as_ref()
            .is_some_and(|operations| {
                !operations.contains(&jsonwebtoken::jwk::KeyOperations::Verify)
            })
            || jwk
                .common
                .public_key_use
                .as_ref()
                .is_some_and(|usage| usage != &jsonwebtoken::jwk::PublicKeyUse::Signature)
            || jwk
                .common
                .key_algorithm
                .as_ref()
                .is_some_and(|alg| alg.to_string() != format!("{algorithm:?}"))
        {
            return Err(Error::InvalidIdentityEvidence);
        }
        DecodingKey::from_jwk(jwk)
            .map(Some)
            .map_err(|_| Error::InvalidIdentityEvidence)
    }

    async fn key(&self, kid: &str, algorithm: Algorithm) -> Result<DecodingKey, Error> {
        if let Some(key) = self.cached_key(kid, algorithm).await? {
            return Ok(key);
        }
        let _guard = self.refresh.lock().await;
        if let Some(key) = self.cached_key(kid, algorithm).await? {
            return Ok(key);
        }
        if self
            .cache
            .read()
            .await
            .as_ref()
            .is_some_and(|s| s.fetched.elapsed() < REFRESH_COOLDOWN)
        {
            return Err(Error::InvalidIdentityEvidence);
        }
        let response = self
            .client
            .get(&self.policy.jwks_url)
            .timeout(Duration::from_secs(1))
            .send()
            .await
            .map_err(|_| Error::Unavailable)?;
        if !response.status().is_success()
            || response
                .content_length()
                .is_some_and(|n| n > MAX_JWKS as u64)
        {
            return Err(Error::Unavailable);
        }
        let mut response = response;
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| Error::Unavailable)? {
            if bytes.len() + chunk.len() > MAX_JWKS {
                return Err(Error::Unavailable);
            }
            bytes.extend_from_slice(&chunk);
        }
        let keys: JwkSet = serde_json::from_slice(&bytes).map_err(|_| Error::Unavailable)?;
        if keys.keys.is_empty() || keys.keys.len() > 32 {
            return Err(Error::Unavailable);
        }
        *self.cache.write().await = Some(CachedKeys {
            keys,
            fetched: Instant::now(),
        });
        self.cached_key(kid, algorithm)
            .await?
            .ok_or(Error::InvalidIdentityEvidence)
    }
}

pub fn http_client(ca_pem: Option<&[u8]>) -> Result<reqwest::Client, Error> {
    let mut builder = reqwest::Client::builder()
        .https_only(true)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(1))
        .timeout(Duration::from_secs(2));
    if let Some(pem) = ca_pem {
        builder = builder
            .add_root_certificate(reqwest::Certificate::from_pem(pem).map_err(|_| Error::Invalid)?);
    }
    builder.build().map_err(|_| Error::Unavailable)
}
