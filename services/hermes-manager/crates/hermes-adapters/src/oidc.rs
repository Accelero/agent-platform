//! OIDC authorization code + S256 PKCE through the maintained protocol library.
//! Access tokens are verified independently for the manager API audience.
use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};

use hermes_application::Error;
use hermes_domain::validate_issuer;
use openidconnect::{
    AccessTokenHash, AsyncHttpClient, AuthorizationCode, ClientId, ClientSecret, CsrfToken,
    EndpointMaybeSet, EndpointNotSet, EndpointSet, HttpRequest, HttpResponse, IssuerUrl, Nonce,
    OAuth2TokenResponse, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, RefreshToken, Scope,
    TokenResponse,
    core::{
        CoreAuthenticationFlow, CoreClient, CoreJwsSigningAlgorithm, CoreProviderMetadata,
        CoreSubjectIdentifierType, CoreTokenResponse,
    },
};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use tokio::sync::{Mutex, RwLock};
use url::Url;
use zeroize::ZeroizeOnDrop;

type ProtocolClient = CoreClient<
    EndpointSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointMaybeSet,
    EndpointMaybeSet,
>;

#[derive(Clone, ZeroizeOnDrop)]
pub struct OidcConfig {
    pub issuer: String,
    pub client_id: String,
    pub callback_url: String,
    pub scopes: Vec<String>,
    pub client_secret: Option<String>,
    pub revocation_url: Option<String>,
}

#[derive(Clone)]
pub struct OidcProvider {
    config: Arc<OidcConfig>,
    http: ProviderHttp,
    cache: Arc<RwLock<Option<CachedClient>>>,
    refresh: Arc<Mutex<()>>,
}
struct CachedClient {
    fetched: Instant,
    client: Arc<ProtocolClient>,
}

#[derive(Serialize, Deserialize, ZeroizeOnDrop)]
pub struct LoginSecrets {
    pub verifier: String,
    pub nonce: String,
    pub browser_hash: Vec<u8>,
    pub return_path: String,
}
pub struct LoginStart {
    pub authorization_url: String,
    pub state: String,
    pub secrets: LoginSecrets,
}
#[derive(ZeroizeOnDrop)]
pub struct ProviderTokens {
    pub access_token: String,
    pub refresh_token: String,
    pub subject: String,
}

#[derive(Clone)]
struct ProviderHttp {
    client: reqwest::Client,
    origin: url::Origin,
}
#[derive(Debug)]
struct ProviderHttpError;
impl std::fmt::Display for ProviderHttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("identity provider unavailable")
    }
}
impl std::error::Error for ProviderHttpError {}

impl<'c> AsyncHttpClient<'c> for ProviderHttp {
    type Error = ProviderHttpError;
    type Future = Pin<Box<dyn Future<Output = Result<HttpResponse, Self::Error>> + Send + 'c>>;
    fn call(&'c self, request: HttpRequest) -> Self::Future {
        Box::pin(async move {
            let (parts, body) = request.into_parts();
            let url = Url::parse(&parts.uri.to_string()).map_err(|_| ProviderHttpError)?;
            if url.origin() != self.origin
                || !url.username().is_empty()
                || url.password().is_some()
                || url.fragment().is_some()
                || body.len() > 16 * 1024
            {
                return Err(ProviderHttpError);
            }
            let mut response = self
                .client
                .request(parts.method, url)
                .headers(parts.headers)
                .body(body)
                .send()
                .await
                .map_err(|_| ProviderHttpError)?;
            if response
                .content_length()
                .is_some_and(|size| size > 256 * 1024)
            {
                return Err(ProviderHttpError);
            }
            let mut result = axum::http::Response::builder().status(response.status());
            *result.headers_mut().ok_or(ProviderHttpError)? = response.headers().clone();
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|_| ProviderHttpError)? {
                if bytes.len() + chunk.len() > 256 * 1024 {
                    return Err(ProviderHttpError);
                }
                bytes.extend_from_slice(&chunk);
            }
            result.body(bytes).map_err(|_| ProviderHttpError)
        })
    }
}

impl OidcProvider {
    pub fn new(config: OidcConfig, client: reqwest::Client) -> Result<Self, Error> {
        validate_issuer(&config.issuer)?;
        let issuer = Url::parse(&config.issuer).map_err(|_| Error::Invalid)?;
        let callback = Url::parse(&config.callback_url).map_err(|_| Error::Invalid)?;
        if config.client_id.is_empty()
            || config.client_id.len() > 255
            || callback.scheme() != "https"
            || callback.host_str().is_none()
            || !callback.username().is_empty()
            || callback.password().is_some()
            || callback.query().is_some()
            || callback.fragment().is_some()
            || config.scopes.len() > 16
            || config
                .scopes
                .iter()
                .any(|s| s.is_empty() || s.len() > 128 || s.bytes().any(|b| !b.is_ascii_graphic()))
            || !config.scopes.iter().any(|s| s == "openid")
        {
            return Err(Error::Invalid);
        }
        if let Some(url) = &config.revocation_url {
            let url = Url::parse(url).map_err(|_| Error::Invalid)?;
            if url.origin() != issuer.origin()
                || url.query().is_some()
                || url.fragment().is_some()
                || !url.username().is_empty()
                || url.password().is_some()
            {
                return Err(Error::Invalid);
            }
        }
        Ok(Self {
            http: ProviderHttp {
                client,
                origin: issuer.origin(),
            },
            config: Arc::new(config),
            cache: Arc::new(RwLock::new(None)),
            refresh: Arc::new(Mutex::new(())),
        })
    }

    pub fn config(&self) -> &OidcConfig {
        &self.config
    }

    async fn client(&self, force: bool) -> Result<Arc<ProtocolClient>, Error> {
        if !force
            && let Some(cached) = self
                .cache
                .read()
                .await
                .as_ref()
                .filter(|cached| cached.fetched.elapsed() < Duration::from_secs(300))
        {
            return Ok(cached.client.clone());
        }
        let _guard = self.refresh.lock().await;
        if !force
            && let Some(cached) = self
                .cache
                .read()
                .await
                .as_ref()
                .filter(|cached| cached.fetched.elapsed() < Duration::from_secs(300))
        {
            return Ok(cached.client.clone());
        }
        let metadata = CoreProviderMetadata::discover_async(
            IssuerUrl::new(self.config.issuer.clone()).map_err(|_| Error::Invalid)?,
            &self.http,
        )
        .await
        .map_err(|_| Error::Unavailable)?;
        let endpoint_ok = |url: &Url| {
            url.origin() == self.http.origin
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
        };
        if metadata.issuer().as_str() != self.config.issuer
            || !metadata
                .subject_types_supported()
                .contains(&CoreSubjectIdentifierType::Public)
            || !endpoint_ok(metadata.authorization_endpoint().url())
            || !endpoint_ok(metadata.jwks_uri().url())
            || metadata
                .token_endpoint()
                .is_none_or(|url| !endpoint_ok(url.url()))
        {
            return Err(Error::InvalidIdentityEvidence);
        }
        let client = CoreClient::from_provider_metadata(
            metadata,
            ClientId::new(self.config.client_id.clone()),
            self.config.client_secret.clone().map(ClientSecret::new),
        )
        .set_redirect_uri(
            RedirectUrl::new(self.config.callback_url.clone()).map_err(|_| Error::Invalid)?,
        );
        let client = Arc::new(client);
        *self.cache.write().await = Some(CachedClient {
            fetched: Instant::now(),
            client: client.clone(),
        });
        Ok(client)
    }

    pub async fn begin(
        &self,
        browser_hash: Vec<u8>,
        return_path: String,
    ) -> Result<LoginStart, Error> {
        let client = self.client(false).await?;
        let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
        let mut request = client
            .authorize_url(
                CoreAuthenticationFlow::AuthorizationCode,
                CsrfToken::new_random,
                Nonce::new_random,
            )
            .set_pkce_challenge(challenge);
        for scope in self.config.scopes.iter().filter(|s| s.as_str() != "openid") {
            request = request.add_scope(Scope::new(scope.clone()));
        }
        let (url, state, nonce) = request.url();
        Ok(LoginStart {
            authorization_url: url.to_string(),
            state: state.secret().clone(),
            secrets: LoginSecrets {
                verifier: verifier.secret().clone(),
                nonce: nonce.secret().clone(),
                browser_hash,
                return_path,
            },
        })
    }

    fn verify_response(
        &self,
        client: &ProtocolClient,
        response: &CoreTokenResponse,
        expected_nonce: &str,
        refresh: bool,
    ) -> Result<String, Error> {
        let id_token = response.id_token().ok_or(Error::InvalidIdentityEvidence)?;
        let verifier = client
            .id_token_verifier()
            .set_allowed_algs([CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256]);
        let claims = id_token
            .claims(&verifier, |nonce: Option<&Nonce>| {
                if let Some(nonce) = nonce {
                    if !bool::from(nonce.secret().as_bytes().ct_eq(expected_nonce.as_bytes())) {
                        return Err("nonce mismatch".into());
                    }
                } else if !refresh {
                    return Err("missing nonce".into());
                }
                Ok(())
            })
            .map_err(|_| Error::InvalidIdentityEvidence)?;
        if let Some(expected) = claims.access_token_hash() {
            let actual = AccessTokenHash::from_token(
                response.access_token(),
                id_token
                    .signing_alg()
                    .map_err(|_| Error::InvalidIdentityEvidence)?,
                id_token
                    .signing_key(&verifier)
                    .map_err(|_| Error::InvalidIdentityEvidence)?,
            )
            .map_err(|_| Error::InvalidIdentityEvidence)?;
            if actual != *expected {
                return Err(Error::InvalidIdentityEvidence);
            }
        }
        Ok(claims.subject().as_str().to_string())
    }

    async fn validated(
        &self,
        response: CoreTokenResponse,
        nonce: &str,
        previous_refresh: Option<&str>,
    ) -> Result<ProviderTokens, Error> {
        if response.access_token().secret().len() > 16 * 1024
            || response
                .refresh_token()
                .is_some_and(|v| v.secret().len() > 16 * 1024)
        {
            return Err(Error::InvalidIdentityEvidence);
        }
        let subject = match self.verify_response(
            self.client(false).await?.as_ref(),
            &response,
            nonce,
            previous_refresh.is_some(),
        ) {
            Ok(subject) => subject,
            Err(_) => self.verify_response(
                self.client(true).await?.as_ref(),
                &response,
                nonce,
                previous_refresh.is_some(),
            )?,
        };
        let refresh_token = response
            .refresh_token()
            .map(|t| t.secret().as_str())
            .or(previous_refresh)
            .filter(|s| !s.is_empty())
            .ok_or(Error::InvalidIdentityEvidence)?;
        Ok(ProviderTokens {
            access_token: response.access_token().secret().clone(),
            refresh_token: refresh_token.into(),
            subject,
        })
    }

    pub async fn exchange(
        &self,
        code: &str,
        secrets: &LoginSecrets,
    ) -> Result<ProviderTokens, Error> {
        if code.is_empty() || code.len() > 4096 {
            return Err(Error::InvalidIdentityEvidence);
        }
        let client = self.client(false).await?;
        let response = client
            .exchange_code(AuthorizationCode::new(code.into()))
            .map_err(|_| Error::Invalid)?
            .set_pkce_verifier(PkceCodeVerifier::new(secrets.verifier.clone()))
            .request_async(&self.http)
            .await
            .map_err(|error| match error {
                openidconnect::RequestTokenError::ServerResponse(_) => {
                    Error::InvalidIdentityEvidence
                }
                _ => Error::Unavailable,
            })?;
        self.validated(response, &secrets.nonce, None).await
    }

    pub async fn refresh(&self, refresh_token: &str, nonce: &str) -> Result<ProviderTokens, Error> {
        let client = self.client(false).await?;
        let refresh = RefreshToken::new(refresh_token.into());
        let mut request = client
            .exchange_refresh_token(&refresh)
            .map_err(|_| Error::Invalid)?;
        for scope in &self.config.scopes {
            request = request.add_scope(Scope::new(scope.clone()));
        }
        let response = request
            .request_async(&self.http)
            .await
            .map_err(|error| match error {
                openidconnect::RequestTokenError::ServerResponse(_) => {
                    Error::InvalidIdentityEvidence
                }
                _ => Error::Unavailable,
            })?;
        self.validated(response, nonce, Some(refresh_token)).await
    }

    pub async fn revoke(&self, refresh_token: &str) {
        let Some(endpoint) = &self.config.revocation_url else {
            return;
        };
        let body = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("token", refresh_token)
            .append_pair("token_type_hint", "refresh_token")
            .append_pair("client_id", &self.config.client_id)
            .finish();
        let mut request = self
            .http
            .client
            .post(endpoint)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(body);
        if let Some(secret) = &self.config.client_secret {
            request = request.basic_auth(&self.config.client_id, Some(secret));
        }
        let _ = request.send().await;
    }
}
