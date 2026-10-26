//! Browser sign-in shared by the administration API and the gateway.
//!
//! The browser holds two opaque cookies: a browser proof and a session handle.
//! Provider tokens, PKCE verifiers, nonces and the CSRF token stay sealed in
//! PostgreSQL. Provider state is re-verified at most every 60 seconds by a
//! fenced refresh; an interrupted refresh revokes the session.

use std::sync::Arc;

use async_trait::async_trait;
use hermes_core::domain::identity::Identity;
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use url::Url;
use zeroize::{ZeroizeOnDrop, Zeroizing};

use crate::crypto::{Purpose, Sealer, hash, random_handle, valid_handle};
use crate::oidc::{LoginSecrets, OidcProvider};
use crate::sessions::{PgSessions, StoredSession};
use crate::tokens::{AccessTokenVerifier, VerifiedAccess};
use crate::{AuthError, unix_time};

/// Evidence older than this is never accepted (seconds since token issue).
const MAXIMUM_EVIDENCE_AGE: u64 = 120;

/// Decides whether a verified identity may hold a session of this purpose.
/// Called at login and after every provider refresh.
#[async_trait]
pub trait Admission: Send + Sync {
    async fn admit(&self, identity: &Identity) -> Result<(), AuthError>;
}

/// Admits every verified identity. The gateway uses it: routing, not the
/// session, decides what an identity may reach.
pub struct AdmitAll;

#[async_trait]
impl Admission for AdmitAll {
    async fn admit(&self, _: &Identity) -> Result<(), AuthError> {
        Ok(())
    }
}

#[derive(Clone)]
pub struct BrowserConfig {
    /// This service's public origin, for example `https://hermes-admin.example.org`.
    pub public_origin: String,
    /// The other public origin, which must use a different host so that
    /// host-only cookies never cross between them.
    pub other_origin: String,
}

#[derive(Serialize, Deserialize, ZeroizeOnDrop)]
struct SessionSecrets {
    access_token: String,
    refresh_token: String,
    nonce: String,
    csrf: String,
    provider_session: String,
    browser_hash: Vec<u8>,
}

#[derive(Clone)]
pub struct BrowserAuth {
    store: PgSessions,
    provider: OidcProvider,
    tokens: AccessTokenVerifier,
    sealer: Sealer,
    config: BrowserConfig,
    issuer: String,
    admission: Arc<dyn Admission>,
}

pub struct LoginRedirect {
    pub location: String,
    pub browser: String,
}

pub struct LoginComplete {
    pub location: String,
    pub browser: String,
    pub session: String,
}

/// A currently valid browser session.
pub struct BrowserIdentity {
    pub identity: Identity,
    pub csrf_token: String,
    /// Hex of the session hash; identifies the session without the cookie value.
    pub session_handle: String,
}

fn origin(value: &str) -> Result<Url, AuthError> {
    let url = Url::parse(value).map_err(|_| AuthError::Configuration)?;
    let valid = url.scheme() == "https"
        && url.host_str().is_some()
        && matches!(url.path(), "" | "/")
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none();
    valid.then_some(url).ok_or(AuthError::Configuration)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn decode_handle(value: &str) -> Result<Vec<u8>, AuthError> {
    if !valid_handle(value) {
        return Err(AuthError::Evidence);
    }
    (0..64)
        .step_by(2)
        .map(|i| u8::from_str_radix(&value[i..i + 2], 16).map_err(|_| AuthError::Evidence))
        .collect()
}

impl BrowserAuth {
    pub fn new(
        store: PgSessions,
        provider: OidcProvider,
        tokens: AccessTokenVerifier,
        sealer: Sealer,
        config: BrowserConfig,
        admission: Arc<dyn Admission>,
    ) -> Result<Self, AuthError> {
        let public = origin(&config.public_origin)?;
        let other = origin(&config.other_origin)?;
        let callback = public
            .join(Self::paths(store.purpose()).1)
            .map_err(|_| AuthError::Configuration)?;
        let issuer = provider.config().issuer.clone();
        if public.host_str() == other.host_str()
            || provider.config().callback_url != callback.as_str()
            || tokens.policy().issuer != issuer
        {
            return Err(AuthError::Configuration);
        }
        Ok(Self {
            store,
            provider,
            tokens,
            sealer,
            config,
            issuer,
            admission,
        })
    }

    /// Login, callback and logout paths of a purpose.
    pub fn paths(purpose: Purpose) -> (&'static str, &'static str, &'static str) {
        match purpose {
            Purpose::Administration => ("/auth/login", "/auth/oidc/callback", "/auth/logout"),
            Purpose::Gateway => (
                "/_platform/login",
                "/_platform/oidc/callback",
                "/_platform/logout",
            ),
        }
    }

    pub fn purpose(&self) -> Purpose {
        self.store.purpose()
    }

    pub fn issuer(&self) -> &str {
        &self.issuer
    }

    pub fn public_origin(&self) -> &str {
        &self.config.public_origin
    }

    /// Session and browser-proof cookie names of a purpose.
    pub fn cookies(purpose: Purpose) -> (&'static str, &'static str) {
        match purpose {
            Purpose::Administration => ("__Host-hermes-admin", "__Host-hermes-admin-browser"),
            Purpose::Gateway => ("__Host-hermes-platform", "__Host-hermes-platform-browser"),
        }
    }

    pub fn browser_cookie(&self) -> &'static str {
        Self::cookies(self.purpose()).1
    }

    pub fn session_cookie(&self) -> &'static str {
        Self::cookies(self.purpose()).0
    }

    pub async fn prune(&self) -> Result<u64, AuthError> {
        self.store.prune(500).await
    }

    /// A local path to return to after login. Authentication, API and
    /// platform paths are refused so a login cannot be replayed into them.
    pub fn safe_return_path(&self, path: &str) -> Result<String, AuthError> {
        let valid = path.len() <= 2048
            && path.starts_with('/')
            && !path.starts_with("//")
            && !path.contains('\\')
            && !path.chars().any(char::is_control)
            && !path.starts_with("/auth")
            && !path.starts_with("/_platform")
            && !path.starts_with("/api/");
        valid
            .then(|| path.to_owned())
            .ok_or(AuthError::Configuration)
    }

    pub fn check_origin(&self, provided: &str) -> Result<(), AuthError> {
        let public = origin(&self.config.public_origin)?;
        if provided != public.origin().ascii_serialization() {
            return Err(AuthError::Forbidden);
        }
        Ok(())
    }

    pub fn check_csrf(expected: &str, provided: &str) -> Result<(), AuthError> {
        if expected.is_empty() || !bool::from(expected.as_bytes().ct_eq(provided.as_bytes())) {
            return Err(AuthError::Forbidden);
        }
        Ok(())
    }

    fn payload(&self, session: &StoredSession) -> Result<SessionSecrets, AuthError> {
        let bytes =
            self.sealer
                .open(self.purpose(), "session", &session.id_hash, &session.sealed)?;
        let payload: SessionSecrets =
            serde_json::from_slice(&bytes).map_err(|_| AuthError::Evidence)?;
        if payload.browser_hash != session.browser_hash {
            return Err(AuthError::Evidence);
        }
        Ok(payload)
    }

    async fn authorize(&self, verified: &VerifiedAccess) -> Result<(), AuthError> {
        if unix_time()?.saturating_sub(verified.issued_at()) > MAXIMUM_EVIDENCE_AGE
            || verified.authorized_client() != self.provider.config().client_id
            || verified.identity().issuer != self.issuer
        {
            return Err(AuthError::Evidence);
        }
        self.admission.admit(&verified.identity()).await
    }

    fn seal_session(
        &self,
        session_hash: &[u8],
        secrets: &SessionSecrets,
    ) -> Result<crate::crypto::Sealed, AuthError> {
        let plaintext =
            Zeroizing::new(serde_json::to_vec(secrets).map_err(|_| AuthError::Configuration)?);
        self.sealer
            .seal(self.purpose(), "session", session_hash, &plaintext)
    }

    /// Starts a login. Reuses a valid browser proof or issues a new one.
    pub async fn begin(
        &self,
        browser: Option<&str>,
        return_path: &str,
    ) -> Result<LoginRedirect, AuthError> {
        let browser = match browser.filter(|value| valid_handle(value)) {
            Some(value) => value.to_owned(),
            None => random_handle()?,
        };
        let browser_hash = hash(&browser);
        let start = self
            .provider
            .begin(browser_hash.clone(), self.safe_return_path(return_path)?)
            .await?;
        let plaintext = Zeroizing::new(
            serde_json::to_vec(&start.secrets).map_err(|_| AuthError::Configuration)?,
        );
        let state_hash = hash(&start.state);
        let sealed = self
            .sealer
            .seal(self.purpose(), "flow", &state_hash, &plaintext)?;
        self.store
            .begin(&browser_hash, &state_hash, &sealed)
            .await?;
        Ok(LoginRedirect {
            location: start.authorization_url,
            browser,
        })
    }

    /// Completes a login from the provider callback.
    pub async fn complete(
        &self,
        browser: &str,
        state: &str,
        code: &str,
    ) -> Result<LoginComplete, AuthError> {
        if !valid_handle(browser)
            || state.is_empty()
            || state.len() > 128
            || state.bytes().any(|b| !b.is_ascii_graphic())
        {
            return Err(AuthError::Evidence);
        }
        let browser_hash = hash(browser);
        let state_hash = hash(state);
        let flow = self.store.consume(&browser_hash, &state_hash).await?;
        let secrets: LoginSecrets = serde_json::from_slice(&self.sealer.open(
            self.purpose(),
            "flow",
            &state_hash,
            &flow.sealed,
        )?)
        .map_err(|_| AuthError::Evidence)?;
        if secrets.browser_hash != browser_hash {
            return Err(AuthError::Evidence);
        }
        let tokens = self.provider.exchange(code, &secrets).await?;
        let verified = self.tokens.verify(&tokens.access_token).await?;
        if verified.identity().subject != tokens.subject {
            return Err(AuthError::Evidence);
        }
        self.authorize(&verified).await?;
        let session = random_handle()?;
        let session_hash = hash(&session);
        let payload = SessionSecrets {
            access_token: tokens.access_token.clone(),
            refresh_token: tokens.refresh_token.clone(),
            nonce: secrets.nonce.clone(),
            csrf: random_handle()?,
            provider_session: verified.provider_session().into(),
            browser_hash: browser_hash.clone(),
        };
        let sealed = self.seal_session(&session_hash, &payload)?;
        let created = self
            .store
            .create(
                &browser_hash,
                flow.generation,
                &state_hash,
                &session_hash,
                &verified.identity(),
                &sealed,
            )
            .await;
        if let Err(error) = created {
            self.provider.revoke(&payload.refresh_token).await;
            return Err(error);
        }
        Ok(LoginComplete {
            location: secrets.return_path.clone(),
            browser: browser.to_owned(),
            session,
        })
    }

    /// Authenticates a browser request from its two cookies.
    pub async fn authenticate(
        &self,
        session: &str,
        browser: &str,
    ) -> Result<BrowserIdentity, AuthError> {
        if !valid_handle(session) || !valid_handle(browser) {
            return Err(AuthError::Evidence);
        }
        self.authenticate_hash(&hash(session), Some(&hash(browser)))
            .await
    }

    /// Re-authenticates an established connection by its session handle.
    pub async fn authenticate_handle(&self, handle: &str) -> Result<BrowserIdentity, AuthError> {
        self.authenticate_hash(&decode_handle(handle)?, None).await
    }

    async fn authenticate_hash(
        &self,
        session_hash: &[u8],
        browser_hash: Option<&[u8]>,
    ) -> Result<BrowserIdentity, AuthError> {
        let stored = self.store.load(session_hash, browser_hash).await?;
        let payload = self.payload(&stored)?;
        if !stored.refresh_due {
            match self.tokens.verify(&payload.access_token).await {
                Ok(verified) if verified.expires_at() > unix_time()? + 5 => {
                    self.authorize(&verified).await?;
                    if verified.identity() != stored.identity
                        || verified.provider_session() != payload.provider_session
                    {
                        return Err(AuthError::Evidence);
                    }
                    return Ok(BrowserIdentity {
                        identity: stored.identity,
                        csrf_token: payload.csrf.clone(),
                        session_handle: hex(session_hash),
                    });
                }
                Err(AuthError::Unavailable) => return Err(AuthError::Unavailable),
                _ => {}
            }
        }
        let claim = self.store.claim_refresh(session_hash, browser_hash).await?;
        let previous = self.payload(&claim.session)?;
        let refreshed = async {
            let tokens = self
                .provider
                .refresh(&previous.refresh_token, &previous.nonce)
                .await?;
            let verified = self.tokens.verify(&tokens.access_token).await?;
            self.authorize(&verified).await?;
            if verified.identity() != claim.session.identity
                || verified.identity().subject != tokens.subject
                || verified.provider_session() != previous.provider_session
                || unix_time()?.saturating_sub(verified.issued_at()) > 60
            {
                return Err(AuthError::Evidence);
            }
            let updated = SessionSecrets {
                access_token: tokens.access_token.clone(),
                refresh_token: tokens.refresh_token.clone(),
                nonce: previous.nonce.clone(),
                csrf: previous.csrf.clone(),
                provider_session: previous.provider_session.clone(),
                browser_hash: previous.browser_hash.clone(),
            };
            let sealed = self.seal_session(session_hash, &updated)?;
            self.store
                .finish_refresh(&claim, &verified.identity(), &sealed)
                .await?;
            Ok(BrowserIdentity {
                identity: verified.identity(),
                csrf_token: updated.csrf.clone(),
                session_handle: hex(session_hash),
            })
        }
        .await;
        if refreshed.is_err() {
            let _ = self.store.revoke(session_hash).await;
        }
        refreshed
    }

    /// Ends the browser's sessions after checking the session's CSRF token,
    /// then revokes the provider refresh token on a best-effort basis.
    pub async fn logout(&self, session: &str, browser: &str, csrf: &str) -> Result<(), AuthError> {
        if !valid_handle(session) || !valid_handle(browser) {
            return Err(AuthError::Evidence);
        }
        let stored = self
            .store
            .load_for_logout(&hash(session), &hash(browser))
            .await?;
        let payload = self.payload(&stored)?;
        Self::check_csrf(&payload.csrf, csrf)?;
        self.store.logout(&hash(browser)).await?;
        self.provider.revoke(&payload.refresh_token).await;
        Ok(())
    }
}
