//! The route resolver (`router.openapi.yaml`) on its own mutual-TLS listener.
//! Only the configured gateway certificate identity may resolve. Identities
//! and bodies are never logged or reflected.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Extension, Json, Router};
use hermes_core::app::error::AppError;
use hermes_core::app::routing::{RouteStore, Routing};
use hermes_core::domain::ErrorKind;
use hermes_wire::routing::{DecisionBody, ResolveRequest};

use crate::body::Body;
use crate::error::{ApiError, error_response};
use crate::limits::{bounded, no_query};
use crate::server::PeerIdentity;

/// The gateway's own deadline is 2 s; answer `unavailable` before it.
const RESOLVE_DEADLINE: Duration = Duration::from_millis(1500);
const DEADLINE: Duration = Duration::from_secs(2);

struct Resolver<S> {
    routing: Routing<S>,
    gateway: String,
}

pub fn router<S: RouteStore + 'static>(routing: Routing<S>, gateway: String) -> Router {
    let state = Arc::new(Resolver { routing, gateway });
    Router::new()
        .route("/internal/v1/routes/resolve", post(resolve::<S>))
        .with_state(state)
        .route_layer(middleware::from_fn(no_query))
        .fallback(|| async { error_response(AppError::new(ErrorKind::NotFound, "not_found")) })
        .layer(middleware::from_fn(|request, next| {
            bounded(request, next, DEADLINE)
        }))
}

fn reply(body: &DecisionBody) -> Response {
    let status = match body {
        DecisionBody::Ready { .. } => StatusCode::OK,
        DecisionBody::Pending { .. } => StatusCode::ACCEPTED,
        DecisionBody::Denied { .. } => StatusCode::FORBIDDEN,
        DecisionBody::Unavailable { .. } => StatusCode::SERVICE_UNAVAILABLE,
    };
    (status, Json(body)).into_response()
}

async fn resolve<S: RouteStore>(
    State(resolver): State<Arc<Resolver<S>>>,
    peer: Option<Extension<PeerIdentity>>,
    body: Result<Body<ResolveRequest>, ApiError>,
) -> Response {
    let gateway = peer.is_some_and(|Extension(PeerIdentity(peer))| {
        peer.as_deref() == Some(resolver.gateway.as_str())
    });
    if !gateway {
        return reply(&DecisionBody::denied());
    }
    let request = match body {
        Ok(Body(request)) => request,
        Err(ApiError(error)) => return error_response(error),
    };
    match tokio::time::timeout(
        RESOLVE_DEADLINE,
        resolver.routing.resolve(&request.identity),
    )
    .await
    {
        Ok(Ok(decision)) => reply(&DecisionBody::from(&decision)),
        Ok(Err(error)) => error_response(error),
        Err(_) => reply(&DecisionBody::unavailable()),
    }
}
