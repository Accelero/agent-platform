//! Assignments, their instances, rollouts and the audit log.

use std::sync::Arc;

use axum::Extension;
use axum::Json;
use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use hermes_core::app::assignments::{
    self, AssignmentChanges, CreateAssignment, RetryInstance, UpdateAssignment,
};
use hermes_core::app::queries::{AssignmentFilter, AuditFilter, EnvironmentList, search};
use hermes_core::domain::assignments::Access;
use hermes_wire::ErrorBody;
use hermes_wire::admin::{self as wire, Preview, RolloutItem, RolloutResult};
use serde::Deserialize;

use super::auth::Authenticated;
use super::support::{
    Body, NOT_FOUND, idempotency_key, if_match, page, path_uuid, query, request_id, tagged,
    with_status,
};
use super::{AdminState, AdminStore};
use crate::error::ApiError;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssignmentQuery {
    q: Option<String>,
    access: Option<Access>,
    template_id: Option<String>,
    after: Option<String>,
    limit: Option<u16>,
}

pub async fn assignments<S: AdminStore>(
    State(state): State<Arc<AdminState<S>>>,
    RawQuery(raw): RawQuery,
) -> Result<Response, ApiError> {
    let params: AssignmentQuery = query(raw.as_deref())?;
    let filter = AssignmentFilter {
        q: search(params.q)?,
        access: params.access,
        template_id: params.template_id,
    };
    let result = state
        .store
        .assignments(&filter, &page(params.after, params.limit)?)
        .await?;
    Ok(Json(result).into_response())
}

pub async fn assignment<S: AdminStore>(
    State(state): State<Arc<AdminState<S>>>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let assignment = state
        .store
        .assignment(path_uuid(&id)?)
        .await?
        .ok_or(NOT_FOUND)?;
    Ok(tagged(assignment.version, &assignment))
}

pub async fn create<S: AdminStore>(
    State(state): State<Arc<AdminState<S>>>,
    Extension(authenticated): Extension<Authenticated>,
    headers: HeaderMap,
    Body(request): Body<wire::CreateAssignment>,
) -> Result<Response, ApiError> {
    let command = CreateAssignment {
        identity: request.identity,
        spec: request.spec,
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
    Ok(with_status(StatusCode::ACCEPTED, &receipt))
}

pub async fn update<S: AdminStore>(
    State(state): State<Arc<AdminState<S>>>,
    Extension(authenticated): Extension<Authenticated>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Body(changes): Body<AssignmentChanges>,
) -> Result<Response, ApiError> {
    let command = UpdateAssignment {
        assignment_id: path_uuid(&id)?,
        expected_version: if_match(&headers)?,
        changes,
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
    Ok(with_status(StatusCode::ACCEPTED, &receipt))
}

pub async fn preview<S: AdminStore>(
    State(state): State<Arc<AdminState<S>>>,
    Path(id): Path<String>,
    Body(changes): Body<AssignmentChanges>,
) -> Result<Response, ApiError> {
    let preview =
        assignments::preview(&state.store, &state.settings, path_uuid(&id)?, &changes).await?;
    Ok(Json(Preview {
        valid: preview.problems.is_empty(),
        problems: hermes_wire::problems(&preview.problems),
        restarts_hermes: preview.restarts_hermes,
        recreates_terminals: preview.recreates_terminals,
    })
    .into_response())
}

pub async fn retry<S: AdminStore>(
    State(state): State<Arc<AdminState<S>>>,
    Extension(authenticated): Extension<Authenticated>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let command = RetryInstance {
        assignment_id: path_uuid(&id)?,
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
    Ok(with_status(StatusCode::ACCEPTED, &receipt))
}

pub async fn rollout<S: AdminStore>(
    State(state): State<Arc<AdminState<S>>>,
    Extension(authenticated): Extension<Authenticated>,
    headers: HeaderMap,
    Body(request): Body<wire::Rollout>,
) -> Result<Response, ApiError> {
    let key = idempotency_key(&headers)?;
    let outcomes = assignments::rollout(
        &state.envelope,
        &authenticated.actor,
        request_id(),
        &key,
        &request.template,
        &request.items,
    )
    .await?;
    let results = outcomes
        .into_iter()
        .map(|(assignment_id, result)| match result {
            Ok(receipt) => RolloutItem {
                assignment_id,
                outcome: "accepted",
                receipt: Some(receipt),
                error: None,
            },
            Err(error) => RolloutItem {
                assignment_id,
                outcome: "rejected",
                receipt: None,
                error: Some(ErrorBody {
                    error: error.code,
                    problems: hermes_wire::problems(&error.problems),
                }),
            },
        })
        .collect();
    Ok(Json(RolloutResult { results }).into_response())
}

pub async fn instance<S: AdminStore>(
    State(state): State<Arc<AdminState<S>>>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let instance = state
        .store
        .instance(path_uuid(&id)?)
        .await?
        .ok_or(NOT_FOUND)?;
    Ok(Json(instance).into_response())
}

pub async fn environments<S: AdminStore>(
    State(state): State<Arc<AdminState<S>>>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let items = state
        .store
        .environments(path_uuid(&id)?)
        .await?
        .ok_or(NOT_FOUND)?;
    Ok(Json(EnvironmentList { items }).into_response())
}

pub async fn orphans<S: AdminStore>(
    State(state): State<Arc<AdminState<S>>>,
) -> Result<Response, ApiError> {
    Ok(Json(state.store.orphans().await?).into_response())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuditQuery {
    target_kind: Option<String>,
    target_id: Option<String>,
    after: Option<String>,
    limit: Option<u16>,
}

pub async fn audit<S: AdminStore>(
    State(state): State<Arc<AdminState<S>>>,
    RawQuery(raw): RawQuery,
) -> Result<Response, ApiError> {
    let params: AuditQuery = query(raw.as_deref())?;
    let filter = AuditFilter {
        target_kind: search(params.target_kind)?,
        target_id: search(params.target_id)?,
    };
    let result = state
        .store
        .audit(&filter, &page(params.after, params.limit)?)
        .await?;
    Ok(Json(result).into_response())
}

pub async fn audit_event<S: AdminStore>(
    State(state): State<Arc<AdminState<S>>>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let event = state
        .store
        .audit_event(path_uuid(&id)?)
        .await?
        .ok_or(NOT_FOUND)?;
    Ok(Json(event).into_response())
}
