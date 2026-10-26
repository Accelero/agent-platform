//! `hermes-gateway`: authenticates people and forwards them to their own
//! Hermes instance. Composition only; it holds no registry store and no
//! Kubernetes client.

pub mod config;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::middleware::{from_fn, from_fn_with_state};
use hermes_core::app::error::AppError;
use hermes_http::files::{keyring, read, read_text};
use hermes_http::limits::bounded;
use hermes_http::server::{bind, serve_bounded, shutdown_signal, tls_config};
use hermes_identity::browser::{AdmitAll, BrowserAuth, BrowserConfig};
use hermes_identity::crypto::{Purpose, Sealer};
use hermes_identity::oidc::{OidcConfig, OidcProvider};
use hermes_identity::sessions::PgSessions;
use hermes_identity::tokens::{AccessTokenVerifier, Algorithm, TokenPolicy};
use hermes_proxy::gateway::{Dial, Gateway, cluster_dial, expire_on_logout, platform, router};
use hermes_proxy::resolver::ResolverClient;
use hermes_proxy::supervise::{Checks, Timing};

use crate::config::Config;

/// Platform routes finish within this deadline.
const PLATFORM_DEADLINE: Duration = Duration::from_secs(15);
/// Browsers hold several connections each, plus their WebSockets.
const CONNECTIONS: usize = 4096;

/// The platform sessions of `config`, with the gateway purpose: every
/// verified identity may sign in; routing decides what it reaches.
pub async fn sessions(config: &Config) -> Result<BrowserAuth, AppError> {
    let ca = match &config.oidc_ca_file {
        Some(path) => Some(read(path, 64 * 1024).await?),
        None => None,
    };
    let http = hermes_identity::http_client(ca.as_deref().map(|ca| &ca[..]))?;
    let oidc = &config.oidc;
    let tokens = AccessTokenVerifier::new(
        TokenPolicy {
            issuer: config.installation.issuer.clone(),
            jwks_url: oidc.jwks_url.clone(),
            audience: oidc.audience.clone(),
            authorized_clients: BTreeSet::from([oidc.client_id.clone()]),
            algorithms: vec![Algorithm::RS256],
            maximum_age_seconds: None,
        },
        http.clone(),
    )?;
    let url = read_text(&config.database_url_file, 16 * 1024).await?;
    let store = PgSessions::connect(&url, Purpose::Gateway).await?;
    let (active, keys) = keyring(&config.keyring_file).await?;
    let sealer = Sealer::new(config.installation.id, active, keys)?;
    let client_secret = match &oidc.client_secret_file {
        Some(path) => Some(read_text(path, 4096).await?.to_string()),
        None => None,
    };
    let callback = url::Url::parse(&config.public_origin)
        .and_then(|origin| origin.join(BrowserAuth::paths(Purpose::Gateway).1))
        .map_err(|_| AppError::invalid())?;
    let provider = OidcProvider::new(
        OidcConfig {
            issuer: config.installation.issuer.clone(),
            client_id: oidc.client_id.clone(),
            callback_url: callback.to_string(),
            scopes: oidc.scopes.clone(),
            client_secret,
            revocation_url: oidc.revocation_url.clone(),
        },
        http,
    )?;
    let browser = BrowserConfig {
        public_origin: config.public_origin.clone(),
        other_origin: config.admin_origin.clone(),
    };
    Ok(BrowserAuth::new(
        store,
        provider,
        tokens,
        sealer,
        browser,
        Arc::new(AdmitAll),
    )?)
}

/// The gateway's whole router over `auth`, reaching instances through `dial`.
pub async fn app(config: &Config, auth: BrowserAuth, dial: Dial) -> Result<Router, AppError> {
    let section = &config.resolver;
    let mut identity = read(&section.private_key_file, 64 * 1024).await?.to_vec();
    identity.extend_from_slice(&read(&section.certificate_file, 64 * 1024).await?);
    let resolver = ResolverClient::new(
        &section.url,
        &read(&section.ca_file, 64 * 1024).await?,
        &identity,
        config.installation.clone(),
    )?;
    let checks = Checks {
        sessions: Arc::new(auth.clone()),
        resolver: Arc::new(resolver),
        timing: Timing::default(),
    };
    let gateway = Arc::new(Gateway::new(
        checks,
        dial,
        &config.public_origin,
        config.max_upload_mib * 1024 * 1024,
    )?);
    let own = hermes_http::browser::router(auth)
        .merge(platform(gateway.clone()))
        .layer(from_fn_with_state(gateway.clone(), expire_on_logout))
        .layer(from_fn(|request, next| {
            bounded(request, next, PLATFORM_DEADLINE)
        }));
    Ok(router(gateway, own))
}

pub async fn run(config: Config) -> Result<(), AppError> {
    let auth = sessions(&config).await?;
    let app = app(
        &config,
        auth.clone(),
        cluster_dial(config.cluster_domain.clone()),
    )
    .await?;
    let tls = tls_config(
        &read(&config.tls.certificate_file, 64 * 1024).await?,
        &read(&config.tls.private_key_file, 64 * 1024).await?,
        None,
    )?;
    let (listener, _) = bind(&config.listen).await?;
    let pruning = tokio::spawn(async move {
        let mut timer = tokio::time::interval(Duration::from_secs(60));
        loop {
            timer.tick().await;
            let _ = tokio::time::timeout(Duration::from_secs(5), auth.prune()).await;
        }
    });
    let served = serve_bounded(
        listener,
        tls,
        BTreeSet::new(),
        app,
        shutdown_signal(),
        CONNECTIONS,
    )
    .await;
    pruning.abort();
    served
}
