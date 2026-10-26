//! The administration API (`contracts/hermes-manager/openapi.yaml`), its
//! browser sign-in and the dashboard, on one origin. Every handler calls one
//! use case or query; authorization of mutations happens in the command
//! envelope against the actor's current grant.

pub mod assets;
pub mod auth;
mod catalog;
mod people;
mod session;
mod storage;
mod support;
mod work;

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::middleware;
use axum::routing::{get, post};
use hermes_core::app::directory::IdentityDirectory;
use hermes_core::app::envelope::CommandEnvelope;
use hermes_core::app::error::AppError;
use hermes_core::app::ports::{AccessReader, Store};
use hermes_core::app::queries::AdminQueries;
use hermes_core::app::secrets::SecretSealer;
use hermes_core::app::settings::Settings;
use hermes_core::domain::ErrorKind;
use hermes_core::domain::backends::BackendDefinition;
use hermes_core::domain::installation::Installation;
use hermes_core::domain::templates::TemplateDefinition;
use hermes_identity::browser::BrowserAuth;
use hermes_identity::crypto::Purpose;
use hermes_identity::tokens::AccessTokenVerifier;

use crate::error::error_response;
use crate::limits::bounded;
use assets::DashboardAssets;

/// Overall deadline of one administration request.
const DEADLINE: Duration = Duration::from_secs(15);

/// What administration handlers need from the store.
pub trait AdminStore: Store + AccessReader + AdminQueries + Clone + 'static {}

impl<T: Store + AccessReader + AdminQueries + Clone + 'static> AdminStore for T {}

/// Everything administration handlers use.
pub struct AdminState<S: Store> {
    pub settings: Arc<Settings>,
    pub store: S,
    pub envelope: CommandEnvelope<S>,
    pub tokens: AccessTokenVerifier,
    pub browser: Option<BrowserAuth>,
    pub sealer: Arc<dyn SecretSealer>,
    pub directory: Option<Arc<dyn IdentityDirectory>>,
}

impl<S: Store> AdminState<S> {
    pub fn installation(&self) -> &Installation {
        &self.settings.installation
    }
}

/// The administration router. The dashboard needs browser sign-in, and the
/// browser must be an administration one.
pub fn router<S: AdminStore>(
    state: AdminState<S>,
    dashboard: Option<DashboardAssets>,
) -> Result<Router, AppError> {
    if dashboard.is_some() && state.browser.is_none() {
        return Err(AppError::invalid());
    }
    if state
        .browser
        .as_ref()
        .is_some_and(|b| b.purpose() != Purpose::Administration)
    {
        return Err(AppError::invalid());
    }
    let browser = state.browser.clone();
    let state = Arc::new(state);
    let api = Router::new()
        .route("/api/v1/session", get(session::session))
        .route("/api/v1/identities", get(people::identities::<S>))
        .route(
            "/api/v1/identities/{identityId}",
            get(people::identity::<S>),
        )
        .route("/api/v1/directory-users", get(people::directory::<S>))
        .route(
            "/api/v1/administrators",
            get(people::administrators::<S>).post(people::grant::<S>),
        )
        .route(
            "/api/v1/administrators/{identityId}",
            get(people::administrator::<S>).patch(people::update_grant::<S>),
        )
        .route(
            "/api/v1/administrator-candidates",
            get(people::candidates::<S>),
        )
        .route(
            "/api/v1/assignments",
            get(work::assignments::<S>).post(work::create::<S>),
        )
        .route(
            "/api/v1/assignments/{assignmentId}",
            get(work::assignment::<S>).patch(work::update::<S>),
        )
        .route(
            "/api/v1/assignments/{assignmentId}/preview",
            post(work::preview::<S>),
        )
        .route(
            "/api/v1/assignments/{assignmentId}/retry",
            post(work::retry::<S>),
        )
        .route("/api/v1/assignment-rollouts", post(work::rollout::<S>))
        .route("/api/v1/instances/{instanceId}", get(work::instance::<S>))
        .route(
            "/api/v1/instances/{instanceId}/environments",
            get(work::environments::<S>),
        )
        .route("/api/v1/orphans", get(work::orphans::<S>))
        .route("/api/v1/audit-events", get(work::audit::<S>))
        .route(
            "/api/v1/audit-events/{eventId}",
            get(work::audit_event::<S>),
        )
        .route("/api/v1/storage-sources", get(storage::sources::<S>))
        .route("/api/v1/volumes", get(storage::volumes::<S>))
        .route("/api/v1/secrets", get(storage::secrets::<S>))
        .route(
            "/api/v1/secrets/{secretName}",
            axum::routing::put(storage::write_secret::<S>).delete(storage::delete_secret::<S>),
        )
        .merge(catalog::routes::<S, TemplateDefinition>("templates"))
        .merge(catalog::routes::<S, BackendDefinition>("terminal-backends"))
        .with_state(state.clone())
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::authenticate::<S>,
        ));
    let mut router = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .merge(api)
        .route(
            "/api/{*rest}",
            get(not_found)
                .post(not_found)
                .patch(not_found)
                .put(not_found)
                .delete(not_found),
        );
    if let Some(browser) = browser {
        router = router.merge(crate::browser::router(browser));
    }
    if let Some(dashboard) = dashboard {
        router = router.merge(assets::router(dashboard, state));
    }
    Ok(router
        .fallback(not_found)
        .layer(middleware::from_fn(|request, next| {
            bounded(request, next, DEADLINE)
        })))
}

async fn not_found() -> axum::response::Response {
    error_response(AppError::new(ErrorKind::NotFound, "not_found"))
}
