//! Catalog routes, shared by Hermes templates and terminal backends.

use std::marker::PhantomData;
use std::sync::Arc;

use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use hermes_core::app::catalog::{self, CreateDraft, Definition, EditDraft, Publish, Withdraw};
use hermes_core::app::queries::search;
use hermes_core::domain::names::validate_name;
use hermes_wire::admin::{Draft, Validation};
use serde::Deserialize;

use super::auth::Authenticated;
use super::support::{
    Body, NOT_FOUND, idempotency_key, if_match, page, query, request_id, tagged, with_status,
};
use super::{AdminState, AdminStore};
use crate::error::ApiError;

/// `/api/v1/{collection}` routes for one definition type.
pub fn routes<S: AdminStore, D: Definition>(collection: &str) -> Router<Arc<AdminState<S>>> {
    let base = format!("/api/v1/{collection}");
    Router::new()
        .route(&base, get(list::<S, D>))
        .route(&format!("{base}/validate"), post(validate::<S, D>))
        .route(
            &format!("{base}/{{catalogId}}/versions"),
            get(versions::<S, D>).post(create::<S, D>),
        )
        .route(
            &format!("{base}/{{catalogId}}/versions/{{version}}"),
            get(version::<S, D>).patch(edit::<S, D>),
        )
        .route(
            &format!("{base}/{{catalogId}}/versions/{{version}}/publish"),
            post(publish::<S, D>),
        )
        .route(
            &format!("{base}/{{catalogId}}/versions/{{version}}/withdraw"),
            post(withdraw::<S, D>),
        )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchPage {
    q: Option<String>,
    after: Option<String>,
    limit: Option<u16>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Paging {
    after: Option<String>,
    limit: Option<u16>,
}

fn catalog_id(id: &str) -> Result<String, ApiError> {
    validate_name(id).map_err(|_| NOT_FOUND)?;
    Ok(id.to_owned())
}

fn version_number(version: &str) -> Result<i64, ApiError> {
    version
        .parse::<i64>()
        .ok()
        .filter(|v| *v >= 1)
        .ok_or_else(|| NOT_FOUND.into())
}

async fn list<S: AdminStore, D: Definition>(
    State(state): State<Arc<AdminState<S>>>,
    RawQuery(raw): RawQuery,
) -> Result<Response, ApiError> {
    let params: SearchPage = query(raw.as_deref())?;
    let q = search(params.q)?;
    let result = state
        .store
        .catalog(D::KIND, q.as_deref(), &page(params.after, params.limit)?)
        .await?;
    Ok(Json(result).into_response())
}

async fn versions<S: AdminStore, D: Definition>(
    State(state): State<Arc<AdminState<S>>>,
    Path(id): Path<String>,
    RawQuery(raw): RawQuery,
) -> Result<Response, ApiError> {
    let params: Paging = query(raw.as_deref())?;
    let result = state
        .store
        .catalog_versions(
            D::KIND,
            &catalog_id(&id)?,
            &page(params.after, params.limit)?,
        )
        .await?;
    Ok(Json(result).into_response())
}

async fn version<S: AdminStore, D: Definition>(
    State(state): State<Arc<AdminState<S>>>,
    Path((id, version)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let found = state
        .store
        .catalog_version::<D>(&catalog_id(&id)?, version_number(&version)?)
        .await?
        .ok_or(NOT_FOUND)?;
    Ok(tagged(found.revision, &found))
}

async fn validate<S: AdminStore, D: Definition>(
    State(state): State<Arc<AdminState<S>>>,
    Body(draft): Body<Draft<D>>,
) -> Result<Response, ApiError> {
    let problems = catalog::validate(&state.store, &state.settings, &draft.definition).await?;
    Ok(Json(Validation {
        valid: problems.is_empty(),
        problems: hermes_wire::problems(&problems),
    })
    .into_response())
}

async fn create<S: AdminStore, D: Definition>(
    State(state): State<Arc<AdminState<S>>>,
    Extension(authenticated): Extension<Authenticated>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Body(draft): Body<Draft<D>>,
) -> Result<Response, ApiError> {
    let command = CreateDraft {
        id: catalog_id(&id)?,
        definition: draft.definition,
    };
    let receipt = state
        .envelope
        .execute(
            &authenticated.actor,
            request_id(),
            &idempotency_key(&headers)?,
            &command,
        )
        .await?;
    Ok(with_status(StatusCode::CREATED, &receipt))
}

async fn edit<S: AdminStore, D: Definition>(
    State(state): State<Arc<AdminState<S>>>,
    Extension(authenticated): Extension<Authenticated>,
    Path((id, version)): Path<(String, String)>,
    headers: HeaderMap,
    Body(draft): Body<Draft<D>>,
) -> Result<Response, ApiError> {
    let command = EditDraft {
        id: catalog_id(&id)?,
        version: version_number(&version)?,
        expected_revision: if_match(&headers)?,
        definition: draft.definition,
    };
    let receipt = state
        .envelope
        .execute(
            &authenticated.actor,
            request_id(),
            &idempotency_key(&headers)?,
            &command,
        )
        .await?;
    Ok(Json(receipt).into_response())
}

async fn publish<S: AdminStore, D: Definition>(
    State(state): State<Arc<AdminState<S>>>,
    Extension(authenticated): Extension<Authenticated>,
    Path((id, version)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let command = Publish::<D> {
        id: catalog_id(&id)?,
        version: version_number(&version)?,
        expected_revision: if_match(&headers)?,
        kind: PhantomData,
    };
    let receipt = state
        .envelope
        .execute(
            &authenticated.actor,
            request_id(),
            &idempotency_key(&headers)?,
            &command,
        )
        .await?;
    Ok(Json(receipt).into_response())
}

async fn withdraw<S: AdminStore, D: Definition>(
    State(state): State<Arc<AdminState<S>>>,
    Extension(authenticated): Extension<Authenticated>,
    Path((id, version)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let command = Withdraw::<D> {
        id: catalog_id(&id)?,
        version: version_number(&version)?,
        expected_revision: if_match(&headers)?,
        kind: PhantomData,
    };
    let receipt = state
        .envelope
        .execute(
            &authenticated.actor,
            request_id(),
            &idempotency_key(&headers)?,
            &command,
        )
        .await?;
    Ok(Json(receipt).into_response())
}
