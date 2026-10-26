//! The `api` role: the administration listener, the execution listener and
//! the route resolver. Holds no Kubernetes credentials and no key that opens
//! secrets.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use hermes_core::app::envelope::CommandEnvelope;
use hermes_core::app::error::AppError;
use hermes_core::app::execution::Execution;
use hermes_core::app::routing::Routing;
use hermes_http::admin::assets::DashboardAssets;
use hermes_http::admin::{AdminState, router};
use hermes_http::server::{bind, serve_until, tls_config};
use hermes_identity::tokens::AccessTokenVerifier;
use tokio::sync::watch;

use crate::config::{Config, read};
use crate::faults;

pub async fn run(config: Config) -> Result<(), AppError> {
    let administration = config
        .administration
        .as_ref()
        .ok_or_else(AppError::invalid)?;
    if administration.dashboard_directory.is_some() && administration.browser.is_none() {
        return Err(AppError::invalid());
    }
    let settings = config.settings()?;
    let store = config.store().await?;
    store.check(&config.installation).await?;
    let http = config.http_client().await?;
    let sealer = Arc::new(administration.sealer().await?);
    let directory = administration
        .directory(&config.installation, http.clone())
        .await?;
    let tokens = AccessTokenVerifier::new(
        administration.token_policy(&config.installation),
        http.clone(),
    )?;
    let browser = administration
        .browser(&config.installation, tokens.clone(), http, store.clone())
        .await?;
    let dashboard = match &administration.dashboard_directory {
        Some(directory) => Some(DashboardAssets::load(directory).await?),
        None => None,
    };
    let state = AdminState {
        envelope: CommandEnvelope::new(store.clone(), settings.clone(), faults::hook()),
        settings,
        store: store.clone(),
        tokens,
        browser: browser.clone(),
        sealer,
        directory,
    };
    let app = router(state, dashboard)?;
    let tls = tls_config(
        &read(&administration.tls.certificate_file, 64 * 1024).await?,
        &read(&administration.tls.private_key_file, 64 * 1024).await?,
        None,
    )?;
    let (listener, _) = bind(&administration.listen).await?;

    let (stop, stopped) = watch::channel(false);
    if let Some(browser) = browser {
        let mut stopped = stopped.clone();
        tokio::spawn(async move {
            let mut timer = tokio::time::interval(Duration::from_secs(60));
            loop {
                tokio::select! {
                    _ = stopped.changed() => return,
                    _ = timer.tick() => {
                        let _ = tokio::time::timeout(Duration::from_secs(5), browser.prune()).await;
                    }
                }
            }
        });
    }
    tokio::spawn(async move {
        super::shutdown_signal().await;
        let _ = stop.send(true);
    });
    let execution = match &config.execution {
        Some(section) => {
            let tls = tls_config(
                &read(&section.tls.certificate_file, 64 * 1024).await?,
                &read(&section.tls.private_key_file, 64 * 1024).await?,
                None,
            )?;
            let (listener, _) = bind(&section.listen).await?;
            let app = hermes_http::execution::router(Execution::new(store.clone()));
            let mut stopped = stopped.clone();
            Some(tokio::spawn(async move {
                serve_until(listener, tls, BTreeSet::new(), app, async move {
                    let _ = stopped.changed().await;
                })
                .await
            }))
        }
        None => None,
    };
    let resolver = match &config.resolver {
        Some(section) => {
            if !section.gateway_identity.starts_with("spiffe://")
                && !section.gateway_identity.starts_with("https://")
            {
                return Err(AppError::invalid());
            }
            let tls = tls_config(
                &read(&section.tls.certificate_file, 64 * 1024).await?,
                &read(&section.tls.private_key_file, 64 * 1024).await?,
                Some(&read(&section.client_ca_file, 64 * 1024).await?),
            )?;
            let (listener, _) = bind(&section.listen).await?;
            let app = hermes_http::resolver::router(
                Routing::new(store.clone(), config.installation.clone()),
                section.gateway_identity.clone(),
            );
            let peers = BTreeSet::from([section.gateway_identity.clone()]);
            let mut stopped = stopped.clone();
            Some(tokio::spawn(async move {
                serve_until(listener, tls, peers, app, async move {
                    let _ = stopped.changed().await;
                })
                .await
            }))
        }
        None => None,
    };
    let mut stopped = stopped;
    let administration = serve_until(listener, tls, BTreeSet::new(), app, async move {
        let _ = stopped.changed().await;
    })
    .await;
    for listener in [execution, resolver].into_iter().flatten() {
        listener.await.map_err(|_| AppError::unavailable())??;
    }
    administration
}
