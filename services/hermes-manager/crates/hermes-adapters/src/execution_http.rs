//! Dedicated execution transport. Opaque native client credentials authorize
//! one database-scoped instance, never administration or route lookup.
use std::{sync::Arc, time::Duration};

use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, Request, State, rejection::JsonRejection},
    http::{HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use hermes_application::{
    Error, InstallationScope,
    terminal::{
        clients::{ExecutionClient, ExecutionClientRepository, ExecutionTokenHash},
        leases::{
            AcquireOutcome, AcquireRequest, CloseSessionRequest, ExecutionLeaseRepository,
            LeaseRequest,
        },
    },
};
use serde_json::json;

use crate::{
    http::{api_error, one_header},
    postgres::PgStore,
};

struct ExecutionState {
    store: PgStore,
    scope: InstallationScope,
}

pub fn execution_router(store: PgStore, scope: InstallationScope) -> Result<Router, Error> {
    hermes_domain::validate_issuer(&scope.issuer)?;
    if scope.id.is_nil() || scope.epoch < 1 {
        return Err(Error::Invalid);
    }
    let state = Arc::new(ExecutionState { store, scope });
    let protected = Router::new()
        .route("/execution/v1/leases/acquire", post(acquire))
        .route("/execution/v1/leases/heartbeat", post(heartbeat))
        .route("/execution/v1/leases/release", post(release))
        .route("/execution/v1/sessions/close", post(close))
        .route("/execution/v1/environments", get(environments))
        .route_layer(middleware::from_fn_with_state(state.clone(), authenticate));
    Ok(Router::new()
        .merge(protected)
        .route("/healthz", get(|| async { Json(json!({"status":"ok"})) }))
        .fallback(|| async { (StatusCode::NOT_FOUND, Json(json!({"error":"not_found"}))) })
        .with_state(state)
        .layer(DefaultBodyLimit::max(16 * 1024))
        .layer(middleware::from_fn(bounded)))
}

async fn bounded(request: Request, next: Next) -> Response {
    crate::http::bounded(request, next, Duration::from_secs(15), false).await
}

async fn authenticate(
    State(state): State<Arc<ExecutionState>>,
    mut request: Request,
    next: Next,
) -> Response {
    let token_hash = one_header(request.headers(), "authorization")
        .ok()
        .and_then(|value| value.strip_prefix("Bearer "))
        .and_then(|value| ExecutionTokenHash::from_token(value).ok());
    let Some(token_hash) = token_hash else {
        return api_error(Error::InvalidIdentityEvidence);
    };
    let client = match state
        .store
        .authenticate_execution_client(&state.scope, token_hash)
        .await
    {
        Ok(client) => client,
        Err(Error::Forbidden) => return api_error(Error::InvalidIdentityEvidence),
        Err(error) => return api_error(error),
    };
    if request.uri().query().is_some() {
        return api_error(Error::Invalid);
    }
    request.extensions_mut().insert(client);
    next.run(request).await
}

fn execution_error(error: Error) -> Response {
    match error {
        Error::PreconditionFailed => (
            StatusCode::CONFLICT,
            Json(json!({"error":"lease_not_live"})),
        )
            .into_response(),
        other => api_error(other),
    }
}

async fn acquire(
    State(state): State<Arc<ExecutionState>>,
    Extension(client): Extension<ExecutionClient>,
    body: Result<Json<AcquireRequest>, JsonRejection>,
) -> Response {
    let Ok(Json(request)) = body else {
        return api_error(Error::Invalid);
    };
    match state
        .store
        .acquire_execution_lease(&state.scope, &client, &request)
        .await
    {
        Ok(AcquireOutcome::Granted(reply)) => Json(reply).into_response(),
        Ok(AcquireOutcome::Pending) => {
            let mut response = (
                StatusCode::CONFLICT,
                Json(json!({"error":"environment_pending"})),
            )
                .into_response();
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
            response
        }
        Err(error) => execution_error(error),
    }
}
async fn heartbeat(
    State(state): State<Arc<ExecutionState>>,
    Extension(client): Extension<ExecutionClient>,
    body: Result<Json<LeaseRequest>, JsonRejection>,
) -> Response {
    let Ok(Json(request)) = body else {
        return api_error(Error::Invalid);
    };
    match state
        .store
        .heartbeat_execution_lease(&state.scope, &client, &request)
        .await
    {
        Ok(reply) => Json(reply).into_response(),
        Err(error) => execution_error(error),
    }
}
async fn release(
    State(state): State<Arc<ExecutionState>>,
    Extension(client): Extension<ExecutionClient>,
    body: Result<Json<LeaseRequest>, JsonRejection>,
) -> Response {
    let Ok(Json(request)) = body else {
        return api_error(Error::Invalid);
    };
    match state
        .store
        .release_execution_lease(&state.scope, &client, &request)
        .await
    {
        Ok(reply) => Json(reply).into_response(),
        Err(error) => execution_error(error),
    }
}
async fn close(
    State(state): State<Arc<ExecutionState>>,
    Extension(client): Extension<ExecutionClient>,
    body: Result<Json<CloseSessionRequest>, JsonRejection>,
) -> Response {
    let Ok(Json(request)) = body else {
        return api_error(Error::Invalid);
    };
    match state
        .store
        .close_execution_session(&state.scope, &client, &request)
        .await
    {
        Ok(reply) => Json(reply).into_response(),
        Err(error) => execution_error(error),
    }
}
async fn environments(
    State(state): State<Arc<ExecutionState>>,
    Extension(client): Extension<ExecutionClient>,
) -> Response {
    match state
        .store
        .list_execution_environments(&state.scope, &client)
        .await
    {
        Ok(items) => Json(items).into_response(),
        Err(error) => execution_error(error),
    }
}
