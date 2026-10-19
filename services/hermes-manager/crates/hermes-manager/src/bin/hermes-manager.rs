//! One manager image; only the future controller role receives Kubernetes rights.
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use hermes_adapters::{
    auth::{AccessTokenVerifier, TokenPolicy, http_client},
    http::{admin_router, resolver_router},
    postgres::PgStore,
    server::{bind, serve_until, tls_config},
};
use hermes_application::{AdminPolicy, Error, InstallationScope, routing::RoutePolicy};
use jsonwebtoken::Algorithm;
use serde::Deserialize;
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Config {
    installation: Installation,
    database_url_file: PathBuf,
    oidc: Oidc,
    admin: Admin,
    resolver: Resolver,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Tls {
    certificate_file: PathBuf,
    private_key_file: PathBuf,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Admin {
    listen: String,
    tls: Tls,
    audience: String,
    allowed_clients: BTreeSet<String>,
    management_client: String,
    administrator_role: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Resolver {
    listen: String,
    tls: Tls,
    client_ca_file: PathBuf,
    service_identity: String,
    audience: String,
    authorized_client: String,
}

async fn read(path: &Path, maximum: usize) -> Result<Vec<u8>, Error> {
    // No secret-bearing path/value/underlying error is printed on failure.
    let metadata = tokio::fs::metadata(path)
        .await
        .map_err(|_| Error::Unavailable)?;
    if metadata.len() > maximum as u64 {
        return Err(Error::Invalid);
    }
    let value = tokio::fs::read(path)
        .await
        .map_err(|_| Error::Unavailable)?;
    if value.is_empty() || value.len() > maximum {
        return Err(Error::Invalid);
    }
    Ok(value)
}

async fn run() -> Result<(), Error> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 3
        || args[1] != "--config"
        || !matches!(args[0].as_str(), "api" | "migrate" | "check")
    {
        return Err(Error::Invalid);
    }
    let config: Config = serde_json::from_slice(&read(Path::new(&args[2]), 64 * 1024).await?)
        .map_err(|_| Error::Invalid)?;
    let scope = InstallationScope {
        id: config.installation.id,
        epoch: config.installation.epoch,
        issuer: config.installation.issuer,
    };
    hermes_domain::validate_issuer(&scope.issuer)?;
    if scope.id.is_nil() || scope.epoch < 1 {
        return Err(Error::Invalid);
    }
    let database_url = String::from_utf8(read(&config.database_url_file, 16 * 1024).await?)
        .map_err(|_| Error::Invalid)?;
    let store = PgStore::connect(database_url.trim()).await?;
    if args[0] == "migrate" {
        return store.migrate(&scope).await;
    }
    if args[0] == "check" {
        return store.check_existing(&scope).await;
    }
    store.check_registry(&scope).await?;
    if config.admin.listen == config.resolver.listen
        || config.admin.audience == config.resolver.audience
        || config.admin.management_client == config.resolver.authorized_client
        || config
            .admin
            .allowed_clients
            .contains(&config.resolver.authorized_client)
        || config.admin.management_client.is_empty()
        || config.admin.administrator_role.is_empty()
    {
        return Err(Error::Invalid);
    }
    let ca = if let Some(path) = config.oidc.ca_file {
        Some(read(&path, 64 * 1024).await?)
    } else {
        None
    };
    let client = http_client(ca.as_deref())?;
    let admin_tokens = AccessTokenVerifier::new(
        TokenPolicy {
            issuer: scope.issuer.clone(),
            jwks_url: config.oidc.jwks_url.clone(),
            audience: config.admin.audience.clone(),
            authorized_clients: config.admin.allowed_clients,
            algorithms: vec![Algorithm::RS256],
            maximum_age_seconds: None,
        },
        client.clone(),
    )?;
    let resolver_tokens = AccessTokenVerifier::new(
        TokenPolicy {
            issuer: scope.issuer.clone(),
            jwks_url: config.oidc.jwks_url,
            audience: config.resolver.audience.clone(),
            authorized_clients: BTreeSet::from([config.resolver.authorized_client.clone()]),
            algorithms: vec![Algorithm::RS256],
            maximum_age_seconds: Some(120),
        },
        client,
    )?;
    let admin = admin_router(
        store.clone(),
        scope.clone(),
        AdminPolicy {
            issuer: scope.issuer.clone(),
            audience: config.admin.audience,
            management_client: config.admin.management_client,
            administrator_role: config.admin.administrator_role,
        },
        admin_tokens,
    )?;
    let resolver = resolver_router(
        store,
        scope.clone(),
        RoutePolicy {
            issuer: scope.issuer,
            audience: config.resolver.audience,
            authorized_client: config.resolver.authorized_client,
            router_service_identity: config.resolver.service_identity.clone(),
        },
        resolver_tokens,
    )?;
    let admin_tls = tls_config(
        &read(&config.admin.tls.certificate_file, 64 * 1024).await?,
        &read(&config.admin.tls.private_key_file, 64 * 1024).await?,
        None,
    )?;
    let resolver_tls = tls_config(
        &read(&config.resolver.tls.certificate_file, 64 * 1024).await?,
        &read(&config.resolver.tls.private_key_file, 64 * 1024).await?,
        Some(&read(&config.resolver.client_ca_file, 64 * 1024).await?),
    )?;
    let (admin_listener, _) = bind(&config.admin.listen).await?;
    let (resolver_listener, _) = bind(&config.resolver.listen).await?;
    let (stop, stop_rx) = tokio::sync::watch::channel(false);
    let mut resolver_stop = stop_rx.clone();
    let mut admin_stop = stop_rx;
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        let _ = stop.send(true);
    });
    tokio::try_join!(
        serve_until(
            admin_listener,
            admin_tls,
            BTreeSet::new(),
            admin,
            async move {
                let _ = admin_stop.changed().await;
            }
        ),
        serve_until(
            resolver_listener,
            resolver_tls,
            BTreeSet::from([config.resolver.service_identity]),
            resolver,
            async move {
                let _ = resolver_stop.changed().await;
            }
        )
    )?;
    Ok(())
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        // Application errors contain only bounded codes, never source errors.
        eprintln!("hermes-manager: {error}");
        std::process::exit(1);
    }
}
