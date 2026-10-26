//! Storage sources (installation configuration), assigned volumes and admin
//! secrets. Secret values are write-only.

use std::sync::Arc;

use axum::Extension;
use axum::Json;
use axum::extract::{Path, RawQuery, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use hermes_core::app::secrets::{self, DeleteSecret, PlainSecret};
use hermes_core::domain::names::validate_name;
use hermes_wire::admin::{SecretWrite, StorageSourceList};
use serde::Deserialize;

use super::auth::Authenticated;
use super::support::{Body, NOT_FOUND, idempotency_key, if_match, page, query, request_id};
use super::{AdminState, AdminStore};
use crate::error::ApiError;

pub async fn sources<S: AdminStore>(
    State(state): State<Arc<AdminState<S>>>,
) -> Json<StorageSourceList> {
    Json(StorageSourceList {
        items: state.settings.sources.iter().cloned().collect(),
        data_identity: state.settings.data_identity,
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VolumeQuery {
    source: Option<String>,
    after: Option<String>,
    limit: Option<u16>,
}

pub async fn volumes<S: AdminStore>(
    State(state): State<Arc<AdminState<S>>>,
    RawQuery(raw): RawQuery,
) -> Result<Response, ApiError> {
    let params: VolumeQuery = query(raw.as_deref())?;
    let result = state
        .store
        .volumes(params.source.as_deref(), &page(params.after, params.limit)?)
        .await?;
    Ok(Json(result).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Paging {
    after: Option<String>,
    limit: Option<u16>,
}

pub async fn secrets<S: AdminStore>(
    State(state): State<Arc<AdminState<S>>>,
    RawQuery(raw): RawQuery,
) -> Result<Response, ApiError> {
    let params: Paging = query(raw.as_deref())?;
    let result = state
        .store
        .secrets(&page(params.after, params.limit)?)
        .await?;
    Ok(Json(result).into_response())
}

pub async fn write_secret<S: AdminStore>(
    State(state): State<Arc<AdminState<S>>>,
    Extension(authenticated): Extension<Authenticated>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Body(write): Body<SecretWrite>,
) -> Result<Response, ApiError> {
    validate_name(&name).map_err(|_| NOT_FOUND)?;
    let receipt = secrets::write(
        &state.envelope,
        &state.sealer,
        &authenticated.actor,
        request_id(),
        &idempotency_key(&headers)?,
        PlainSecret {
            name: &name,
            kind: write.kind,
            value: write.value.as_bytes(),
        },
    )
    .await?;
    Ok(Json(receipt).into_response())
}

pub async fn delete_secret<S: AdminStore>(
    State(state): State<Arc<AdminState<S>>>,
    Extension(authenticated): Extension<Authenticated>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    validate_name(&name).map_err(|_| NOT_FOUND)?;
    let command = DeleteSecret {
        name,
        expected_version: if_match(&headers)?,
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
