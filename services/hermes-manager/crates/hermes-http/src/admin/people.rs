//! Identities, directory lookup and administrator grants.

use std::sync::Arc;

use axum::Extension;
use axum::Json;
use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use hermes_core::app::access::{GrantAdministrator, UpdateAdministrator};
use hermes_core::app::queries::search;
use hermes_wire::admin::{DirectoryPage, GrantChange, GrantRequest};
use serde::Deserialize;

use super::auth::Authenticated;
use super::support::{
    Body, NOT_FOUND, idempotency_key, if_match, page, path_uuid, query, request_id, tagged,
    with_status,
};
use super::{AdminState, AdminStore};
use crate::error::ApiError;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchPage {
    q: Option<String>,
    after: Option<String>,
    limit: Option<u16>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Paging {
    after: Option<String>,
    limit: Option<u16>,
}

pub async fn identities<S: AdminStore>(
    State(state): State<Arc<AdminState<S>>>,
    RawQuery(raw): RawQuery,
) -> Result<Response, ApiError> {
    let params: SearchPage = query(raw.as_deref())?;
    let q = search(params.q)?;
    let result = state
        .store
        .identities(q.as_deref(), &page(params.after, params.limit)?)
        .await?;
    Ok(Json(result).into_response())
}

pub async fn identity<S: AdminStore>(
    State(state): State<Arc<AdminState<S>>>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let identity = state
        .store
        .identity(path_uuid(&id)?)
        .await?
        .ok_or(NOT_FOUND)?;
    Ok(Json(identity).into_response())
}

pub async fn candidates<S: AdminStore>(
    State(state): State<Arc<AdminState<S>>>,
    RawQuery(raw): RawQuery,
) -> Result<Response, ApiError> {
    let params: SearchPage = query(raw.as_deref())?;
    let q = search(params.q)?;
    let result = state
        .store
        .administrator_candidates(q.as_deref(), &page(params.after, params.limit)?)
        .await?;
    Ok(Json(result).into_response())
}

pub async fn directory<S: AdminStore>(
    State(state): State<Arc<AdminState<S>>>,
    RawQuery(raw): RawQuery,
) -> Result<Response, ApiError> {
    let params: SearchPage = query(raw.as_deref())?;
    let request = page(params.after, params.limit)?;
    let Some(directory) = &state.directory else {
        let empty: DirectoryPage<()> = DirectoryPage {
            enabled: false,
            items: Vec::new(),
            next_cursor: None,
        };
        return Ok(Json(empty).into_response());
    };
    let q = search(params.q)?.unwrap_or_default();
    let results = directory
        .search(&q, request.after.as_deref(), request.limit)
        .await?;
    Ok(Json(DirectoryPage {
        enabled: true,
        items: results.items,
        next_cursor: results.next_cursor,
    })
    .into_response())
}

pub async fn administrators<S: AdminStore>(
    State(state): State<Arc<AdminState<S>>>,
    RawQuery(raw): RawQuery,
) -> Result<Response, ApiError> {
    let params: Paging = query(raw.as_deref())?;
    let result = state
        .store
        .administrators(&page(params.after, params.limit)?)
        .await?;
    Ok(Json(result).into_response())
}

pub async fn administrator<S: AdminStore>(
    State(state): State<Arc<AdminState<S>>>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let grant = state
        .store
        .administrator(path_uuid(&id)?)
        .await?
        .ok_or(NOT_FOUND)?;
    Ok(tagged(grant.version, &grant))
}

pub async fn grant<S: AdminStore>(
    State(state): State<Arc<AdminState<S>>>,
    Extension(authenticated): Extension<Authenticated>,
    headers: HeaderMap,
    Body(request): Body<GrantRequest>,
) -> Result<Response, ApiError> {
    let command = GrantAdministrator {
        identity: request.identity,
        can_manage_administrators: request.can_manage_administrators,
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

pub async fn update_grant<S: AdminStore>(
    State(state): State<Arc<AdminState<S>>>,
    Extension(authenticated): Extension<Authenticated>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Body(change): Body<GrantChange>,
) -> Result<Response, ApiError> {
    let command = UpdateAdministrator {
        identity_id: path_uuid(&id)?,
        expected_version: if_match(&headers)?,
        active: change.active,
        can_manage_administrators: change.can_manage_administrators,
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
