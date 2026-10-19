//! Router OIDC/ForwardAuth process; session credentials only, never assignments.
use hermes_adapters::{
    auth::{AccessTokenVerifier, TokenPolicy, http_client},
    auth_config::{SessionConfig, read},
    router_auth::{RouterAuthConfig, routers},
    server::{bind, serve_until, tls_config},
    session_crypto::Purpose,
};
use hermes_application::{Error, InstallationScope};
use jsonwebtoken::Algorithm;
use serde::Deserialize;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Config {
    installation: Installation,
    oidc: Oidc,
    session: SessionConfig,
    public: Listener,
    internal: Internal,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Installation {
    id: Uuid,
    epoch: i64,
    issuer: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Oidc {
    jwks_url: String,
    ca_file: Option<PathBuf>,
    audience: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Listener {
    listen: String,
    certificate_file: PathBuf,
    private_key_file: PathBuf,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Internal {
    listen: String,
    certificate_file: PathBuf,
    private_key_file: PathBuf,
    client_ca_file: PathBuf,
    forward_auth_identity: String,
    session_plugin_identity: String,
}

async fn run() -> Result<(), Error> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 2 || args[0] != "--config" {
        return Err(Error::Invalid);
    }
    let config: Config = serde_json::from_slice(&read(Path::new(&args[1]), 64 * 1024).await?)
        .map_err(|_| Error::Invalid)?;
    if config.public.listen == config.internal.listen {
        return Err(Error::Invalid);
    }
    let scope = InstallationScope {
        id: config.installation.id,
        epoch: config.installation.epoch,
        issuer: config.installation.issuer,
    };
    let ca = match config.oidc.ca_file {
        Some(path) => Some(read(&path, 64 * 1024).await?),
        None => None,
    };
    let http = http_client(ca.as_deref().map(|v| v.as_slice()))?;
    let tokens = AccessTokenVerifier::new(
        TokenPolicy {
            issuer: scope.issuer.clone(),
            jwks_url: config.oidc.jwks_url,
            audience: config.oidc.audience,
            authorized_clients: BTreeSet::from([config.session.client_id.clone()]),
            algorithms: vec![Algorithm::RS256],
            maximum_age_seconds: Some(120),
        },
        http.clone(),
    )?;
    let auth = config
        .session
        .build(scope, Purpose::Router, tokens, http, None)
        .await?;
    let identities = BTreeSet::from([
        config.internal.forward_auth_identity.clone(),
        config.internal.session_plugin_identity.clone(),
    ]);
    let cleanup = auth.clone();
    let (public, internal) = routers(
        auth,
        RouterAuthConfig {
            forward_auth_identity: config.internal.forward_auth_identity,
            session_plugin_identity: config.internal.session_plugin_identity,
        },
    )?;
    let public_tls = tls_config(
        &read(&config.public.certificate_file, 64 * 1024).await?,
        &read(&config.public.private_key_file, 64 * 1024).await?,
        None,
    )?;
    let internal_tls = tls_config(
        &read(&config.internal.certificate_file, 64 * 1024).await?,
        &read(&config.internal.private_key_file, 64 * 1024).await?,
        Some(&read(&config.internal.client_ca_file, 64 * 1024).await?),
    )?;
    let (public_listener, _) = bind(&config.public.listen).await?;
    let (internal_listener, _) = bind(&config.internal.listen).await?;
    let (stop, rx) = tokio::sync::watch::channel(false);
    tokio::spawn(cleanup.cleanup_until(rx.clone()));
    let mut public_rx = rx.clone();
    let mut internal_rx = rx;
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        let _ = stop.send(true);
    });
    tokio::try_join!(
        serve_until(
            public_listener,
            public_tls,
            BTreeSet::new(),
            public,
            async move {
                let _ = public_rx.changed().await;
            }
        ),
        serve_until(
            internal_listener,
            internal_tls,
            identities,
            internal,
            async move {
                let _ = internal_rx.changed().await;
            }
        )
    )?;
    Ok(())
}
#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("hermes-router-auth: {e}");
        std::process::exit(1);
    }
}
