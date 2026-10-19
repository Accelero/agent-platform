//! Shared browser/session use cases for administrator and router authentication.
use hermes_application::{AdminPolicy, Error};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use url::Url;
use zeroize::{ZeroizeOnDrop, Zeroizing};

use crate::{
    auth::{AccessTokenVerifier, VerifiedAccess, unix_time},
    oidc::{LoginSecrets, OidcProvider},
    session_crypto::{Purpose, Sealer, hash, random_handle, valid_handle},
    sessions::{PgSessions, StoredSession},
};

#[derive(Clone)]
pub struct BrowserConfig {
    pub public_origin: String,
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
    admin: Option<AdminPolicy>,
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
pub struct BrowserIdentity {
    pub access: VerifiedAccess,
    pub csrf_token: String,
    pub session_handle: String,
    pub(crate) access_token: Zeroizing<String>,
}

fn hex_hash(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn decode_handle(value: &str) -> Result<Vec<u8>, Error> {
    if !valid_handle(value) {
        return Err(Error::InvalidIdentityEvidence);
    }
    (0..64)
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&value[i..i + 2], 16).map_err(|_| Error::InvalidIdentityEvidence)
        })
        .collect()
}

fn origin(value: &str) -> Result<Url, Error> {
    let url = Url::parse(value).map_err(|_| Error::Invalid)?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !matches!(url.path(), "" | "/")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(Error::Invalid);
    }
    Ok(url)
}

impl BrowserAuth {
    pub fn new(
        store: PgSessions,
        provider: OidcProvider,
        tokens: AccessTokenVerifier,
        sealer: Sealer,
        config: BrowserConfig,
        admin: Option<AdminPolicy>,
    ) -> Result<Self, Error> {
        let public = origin(&config.public_origin)?;
        let other = origin(&config.other_origin)?;
        let purpose = store.purpose();
        let path = match purpose {
            Purpose::Administration => "/auth/oidc/callback",
            Purpose::Router => "/_platform/oidc/callback",
        };
        if public.host_str() == other.host_str()
            || provider.config().issuer != store.scope().issuer
            || provider.config().callback_url
                != public.join(path).map_err(|_| Error::Invalid)?.as_str()
            || (purpose == Purpose::Administration) != admin.is_some()
        {
            return Err(Error::Invalid);
        }
        Ok(Self {
            store,
            provider,
            tokens,
            sealer,
            config,
            admin,
        })
    }

    pub fn purpose(&self) -> Purpose {
        self.store.purpose()
    }
    pub async fn prune(&self) -> Result<u64, Error> {
        self.store.prune(500).await
    }
    pub async fn cleanup_until(self, mut stop: tokio::sync::watch::Receiver<bool>) {
        let mut timer = tokio::time::interval(std::time::Duration::from_secs(60));
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _=stop.changed()=>return,
                _=timer.tick()=>{let _=tokio::time::timeout(std::time::Duration::from_secs(5),self.prune()).await;}
            }
        }
    }
    pub fn public_origin(&self) -> &str {
        &self.config.public_origin
    }
    pub fn issuer(&self) -> &str {
        &self.store.scope().issuer
    }
    pub fn browser_cookie(&self) -> &'static str {
        match self.purpose() {
            Purpose::Administration => "__Host-hermes-admin-browser",
            Purpose::Router => "__Host-hermes-platform-browser",
        }
    }
    pub fn session_cookie(&self) -> &'static str {
        match self.purpose() {
            Purpose::Administration => "__Host-hermes-admin",
            Purpose::Router => "__Host-hermes-platform",
        }
    }
    pub fn login_path(&self) -> &'static str {
        match self.purpose() {
            Purpose::Administration => "/auth/login",
            Purpose::Router => "/_platform/login",
        }
    }

    pub fn safe_return_path(&self, path: &str) -> Result<String, Error> {
        if path.len() > 2048
            || !path.starts_with('/')
            || path.starts_with("//")
            || path.contains('\\')
            || path.chars().any(|c| c.is_control())
            || path.starts_with("/auth")
            || path.starts_with("/_platform")
            || path.starts_with("/api/")
        {
            return Err(Error::Invalid);
        }
        Ok(path.into())
    }

    pub fn check_origin(&self, provided: &str) -> Result<(), Error> {
        let public = origin(&self.config.public_origin)?;
        if provided != public.origin().ascii_serialization() {
            return Err(Error::Forbidden);
        }
        Ok(())
    }

    fn payload(&self, session: &StoredSession) -> Result<SessionSecrets, Error> {
        let bytes = self.sealer.open(
            self.store.scope(),
            self.purpose(),
            "session",
            &session.id_hash,
            &session.sealed,
        )?;
        let payload: SessionSecrets =
            serde_json::from_slice(&bytes).map_err(|_| Error::InvalidIdentityEvidence)?;
        if payload.browser_hash != session.browser_hash {
            return Err(Error::InvalidIdentityEvidence);
        }
        Ok(payload)
    }

    fn authorize(&self, verified: &VerifiedAccess) -> Result<(), Error> {
        if unix_time()?.saturating_sub(verified.issued_at()) > 120
            || verified.authorized_client() != self.provider.config().client_id
            || verified.identity().issuer != self.store.scope().issuer
        {
            return Err(Error::InvalidIdentityEvidence);
        }
        if let Some(policy) = &self.admin {
            policy.authorize(&verified.principal())?;
        }
        Ok(())
    }

    pub async fn begin(
        &self,
        browser: Option<&str>,
        return_path: &str,
    ) -> Result<LoginRedirect, Error> {
        let browser = match browser.filter(|v| valid_handle(v)) {
            Some(v) => v.to_string(),
            None => random_handle()?,
        };
        let browser_hash = hash(&browser);
        let start = self
            .provider
            .begin(browser_hash.clone(), self.safe_return_path(return_path)?)
            .await?;
        let plaintext =
            Zeroizing::new(serde_json::to_vec(&start.secrets).map_err(|_| Error::Invalid)?);
        let state_hash = hash(&start.state);
        let sealed = self.sealer.seal(
            self.store.scope(),
            self.purpose(),
            "flow",
            &state_hash,
            &plaintext,
        )?;
        self.store
            .begin(&browser_hash, &state_hash, &sealed)
            .await?;
        Ok(LoginRedirect {
            location: start.authorization_url,
            browser,
        })
    }

    pub async fn complete(
        &self,
        browser: &str,
        state: &str,
        code: &str,
    ) -> Result<LoginComplete, Error> {
        if !valid_handle(browser)
            || state.is_empty()
            || state.len() > 128
            || state.bytes().any(|b| !b.is_ascii_graphic())
        {
            return Err(Error::InvalidIdentityEvidence);
        }
        let browser_hash = hash(browser);
        let state_hash = hash(state);
        let flow = self.store.consume(&browser_hash, &state_hash).await?;
        let secrets: LoginSecrets = serde_json::from_slice(&self.sealer.open(
            self.store.scope(),
            self.purpose(),
            "flow",
            &state_hash,
            &flow.sealed,
        )?)
        .map_err(|_| Error::InvalidIdentityEvidence)?;
        if secrets.browser_hash != browser_hash {
            return Err(Error::InvalidIdentityEvidence);
        }
        let tokens = self.provider.exchange(code, &secrets).await?;
        let verified = self.tokens.verify(&tokens.access_token).await?;
        self.authorize(&verified)?;
        if verified.identity().subject != tokens.subject {
            return Err(Error::InvalidIdentityEvidence);
        }
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
        let plaintext = Zeroizing::new(serde_json::to_vec(&payload).map_err(|_| Error::Invalid)?);
        let sealed = self.sealer.seal(
            self.store.scope(),
            self.purpose(),
            "session",
            &session_hash,
            &plaintext,
        )?;
        if let Err(error) = self
            .store
            .create(
                &browser_hash,
                flow.generation,
                &state_hash,
                &session_hash,
                &verified.identity(),
                &sealed,
            )
            .await
        {
            self.provider.revoke(&payload.refresh_token).await;
            return Err(error);
        }
        Ok(LoginComplete {
            location: secrets.return_path.clone(),
            browser: browser.into(),
            session,
        })
    }

    pub async fn authenticate(
        &self,
        session: &str,
        browser: &str,
    ) -> Result<BrowserIdentity, Error> {
        if !valid_handle(session) || !valid_handle(browser) {
            return Err(Error::InvalidIdentityEvidence);
        }
        self.authenticate_hash(&hash(session), Some(&hash(browser)))
            .await
    }

    pub async fn authenticate_handle(&self, handle: &str) -> Result<BrowserIdentity, Error> {
        self.authenticate_hash(&decode_handle(handle)?, None).await
    }

    async fn authenticate_hash(
        &self,
        session_hash: &[u8],
        browser_hash: Option<&[u8]>,
    ) -> Result<BrowserIdentity, Error> {
        let stored = self.store.load(session_hash, browser_hash).await?;
        let payload = self.payload(&stored)?;
        if !stored.refresh_due {
            match self.tokens.verify(&payload.access_token).await {
                Ok(verified) if verified.expires_at() > unix_time()? + 5 => {
                    self.authorize(&verified)?;
                    if verified.identity() != stored.identity
                        || verified.provider_session() != payload.provider_session
                    {
                        return Err(Error::InvalidIdentityEvidence);
                    }
                    return Ok(BrowserIdentity {
                        access: verified,
                        csrf_token: payload.csrf.clone(),
                        session_handle: hex_hash(session_hash),
                        access_token: Zeroizing::new(payload.access_token.clone()),
                    });
                }
                Err(Error::Unavailable) => return Err(Error::Unavailable),
                _ => {}
            }
        }
        let claim = self.store.claim_refresh(session_hash, browser_hash).await?;
        let previous = self.payload(&claim.session)?;
        let result = async {
            let tokens = self
                .provider
                .refresh(&previous.refresh_token, &previous.nonce)
                .await?;
            let verified = self.tokens.verify(&tokens.access_token).await?;
            self.authorize(&verified)?;
            if verified.identity() != claim.session.identity
                || verified.identity().subject != tokens.subject
                || verified.provider_session() != previous.provider_session
                || unix_time()?.saturating_sub(verified.issued_at()) > 60
            {
                return Err(Error::InvalidIdentityEvidence);
            }
            let updated = SessionSecrets {
                access_token: tokens.access_token.clone(),
                refresh_token: tokens.refresh_token.clone(),
                nonce: previous.nonce.clone(),
                csrf: previous.csrf.clone(),
                provider_session: previous.provider_session.clone(),
                browser_hash: previous.browser_hash.clone(),
            };
            let plaintext =
                Zeroizing::new(serde_json::to_vec(&updated).map_err(|_| Error::Invalid)?);
            let sealed = self.sealer.seal(
                self.store.scope(),
                self.purpose(),
                "session",
                session_hash,
                &plaintext,
            )?;
            self.store
                .finish_refresh(&claim, &verified.identity(), &sealed)
                .await?;
            Ok(BrowserIdentity {
                access: verified,
                csrf_token: updated.csrf.clone(),
                session_handle: hex_hash(session_hash),
                access_token: Zeroizing::new(updated.access_token.clone()),
            })
        }
        .await;
        if result.is_err() {
            let _ = self.store.revoke(session_hash).await;
        }
        result
    }

    pub fn check_csrf(expected: &str, provided: &str) -> Result<(), Error> {
        if expected.is_empty() || !bool::from(expected.as_bytes().ct_eq(provided.as_bytes())) {
            return Err(Error::Forbidden);
        }
        Ok(())
    }

    pub async fn logout(&self, session: &str, browser: &str, csrf: &str) -> Result<(), Error> {
        if !valid_handle(session) || !valid_handle(browser) {
            return Err(Error::InvalidIdentityEvidence);
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
