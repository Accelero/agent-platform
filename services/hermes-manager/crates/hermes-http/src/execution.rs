//! The execution API (`execution.openapi.yaml`) on its own listener. The
//! bearer credential alone selects the instance; no request carries a scope.
//! Query strings, unknown fields and duplicate keys are refused, and nothing
//! submitted is ever reflected.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, header};
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use hermes_core::app::error::AppError;
use hermes_core::app::execution::{
    AcquireRequest, CloseSessionRequest, Execution, ExecutionStore, LeaseRequest,
};
use hermes_core::domain::ErrorKind;
use hermes_core::domain::execution::ExecutionRejected;

use crate::body::Body;
use crate::cookies::one_header;
use crate::error::{ApiError, error_response};
use crate::limits::{bounded, no_query};

const DEADLINE: Duration = Duration::from_secs(15);

type Shared<S> = Arc<Execution<S>>;

fn unauthenticated() -> AppError {
    ExecutionRejected::Unauthenticated.into()
}

/// The single `Authorization: Bearer <credential>` value.
fn credential(headers: &HeaderMap) -> Result<String, AppError> {
    one_header(headers, "authorization")
        .ok()
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::to_owned)
        .ok_or_else(unauthenticated)
}

/// Pending acquisitions carry `Retry-After: 1`.
fn failure(error: AppError) -> Response {
    let pending = error.kind == ErrorKind::Pending;
    let mut response = error_response(error);
    if pending {
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
    }
    response
}

fn reply<T: serde::Serialize>(result: Result<T, AppError>) -> Response {
    match result {
        Ok(body) => Json(body).into_response(),
        Err(error) => failure(error),
    }
}

pub fn router<S: ExecutionStore + 'static>(execution: Execution<S>) -> Router {
    let api = Router::new()
        .route("/execution/v1/leases/acquire", post(acquire::<S>))
        .route("/execution/v1/leases/heartbeat", post(heartbeat::<S>))
        .route("/execution/v1/leases/release", post(release::<S>))
        .route("/execution/v1/sessions/close", post(close::<S>))
        .route("/execution/v1/environments", get(environments::<S>))
        .with_state(Arc::new(execution))
        .route_layer(middleware::from_fn(no_query));
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .merge(api)
        .fallback(|| async { error_response(AppError::new(ErrorKind::NotFound, "not_found")) })
        .layer(middleware::from_fn(|request, next| {
            bounded(request, next, DEADLINE)
        }))
}

async fn acquire<S: ExecutionStore>(
    State(execution): State<Shared<S>>,
    headers: HeaderMap,
    body: Result<Body<AcquireRequest>, ApiError>,
) -> Response {
    let credential = match credential(&headers) {
        Ok(credential) => credential,
        Err(error) => return failure(error),
    };
    match body {
        Ok(Body(request)) => reply(execution.acquire(&credential, &request).await),
        Err(ApiError(error)) => failure(error),
    }
}

async fn heartbeat<S: ExecutionStore>(
    State(execution): State<Shared<S>>,
    headers: HeaderMap,
    body: Result<Body<LeaseRequest>, ApiError>,
) -> Response {
    let credential = match credential(&headers) {
        Ok(credential) => credential,
        Err(error) => return failure(error),
    };
    match body {
        Ok(Body(request)) => reply(execution.heartbeat(&credential, &request).await),
        Err(ApiError(error)) => failure(error),
    }
}

async fn release<S: ExecutionStore>(
    State(execution): State<Shared<S>>,
    headers: HeaderMap,
    body: Result<Body<LeaseRequest>, ApiError>,
) -> Response {
    let credential = match credential(&headers) {
        Ok(credential) => credential,
        Err(error) => return failure(error),
    };
    match body {
        Ok(Body(request)) => reply(execution.release(&credential, &request).await),
        Err(ApiError(error)) => failure(error),
    }
}

async fn close<S: ExecutionStore>(
    State(execution): State<Shared<S>>,
    headers: HeaderMap,
    body: Result<Body<CloseSessionRequest>, ApiError>,
) -> Response {
    let credential = match credential(&headers) {
        Ok(credential) => credential,
        Err(error) => return failure(error),
    };
    match body {
        Ok(Body(request)) => reply(execution.close(&credential, &request).await),
        Err(ApiError(error)) => failure(error),
    }
}

async fn environments<S: ExecutionStore>(
    State(execution): State<Shared<S>>,
    headers: HeaderMap,
) -> Response {
    match credential(&headers) {
        Ok(credential) => reply(execution.environments(&credential).await),
        Err(error) => failure(error),
    }
}
