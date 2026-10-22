//! Administration transport for approved execution references and desired policy.
use super::*;
use hermes_application::execution::{
    AssignmentExecution, ExecutionCommand, ExecutionRepository, PullSecretBinding,
    RuntimeDefinition, StorageBinding,
};

pub(super) fn routes() -> Router<Arc<AdminState>> {
    Router::new()
        .route(
            "/api/v1/terminal-runtimes",
            get(runtime_catalog).post(register_runtime),
        )
        .route(
            "/api/v1/terminal-runtimes/{id}/versions",
            get(runtime_versions).post(register_runtime_version),
        )
        .route(
            "/api/v1/storage-bindings",
            get(storage_bindings).post(register_storage),
        )
        .route(
            "/api/v1/storage-bindings/{id}",
            get(storage_binding).patch(update_storage),
        )
        .route(
            "/api/v1/assignments/{id}/execution",
            get(policy).patch(set_policy),
        )
        .route(
            "/api/v1/instances/{id}/pull-secret-bindings",
            post(register_pull_secret),
        )
}

async fn runtime_catalog(
    State(state): State<Arc<AdminState>>,
    query: Result<Query<DashboardQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let Ok(Query(query)) = query else {
        return api_error(Error::Invalid);
    };
    match state
        .store
        .dashboard_list(&state.scope, DashboardList::Runtimes, &query)
        .await
    {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(error),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Versions {
    #[serde(default)]
    after: i64,
    #[serde(default = "default_limit")]
    limit: u16,
}

async fn runtime_versions(
    State(state): State<Arc<AdminState>>,
    Path(id): Path<String>,
    query: Result<Query<Versions>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let Ok(Query(query)) = query else {
        return api_error(Error::Invalid);
    };
    match state
        .store
        .runtime_versions(&state.scope, &id, query.after, query.limit)
        .await
    {
        Ok(items) => {
            let next = (items.len() == usize::from(query.limit))
                .then(|| items.last().map(|item| item.runtime.version))
                .flatten();
            Json(json!({"items":items,"nextCursor":next})).into_response()
        }
        Err(error) => api_error(error),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoragePage {
    instance_id: Uuid,
    after: Option<Uuid>,
    #[serde(default = "default_limit")]
    limit: u16,
}
async fn storage_bindings(
    State(state): State<Arc<AdminState>>,
    query: Result<Query<StoragePage>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let Ok(Query(query)) = query else {
        return api_error(Error::Invalid);
    };
    match state
        .store
        .owner_storage_bindings(&state.scope, query.instance_id, query.after, query.limit)
        .await
    {
        Ok(items) => {
            let next = if items.len() == usize::from(query.limit) {
                items.last().map(|item| item.id)
            } else {
                None
            };
            page(items, next).into_response()
        }
        Err(error) => api_error(error),
    }
}

async fn mutate_execution(
    state: &AdminState,
    actor: &VerifiedPrincipal,
    headers: &HeaderMap,
    command: ExecutionCommand,
) -> Response {
    let storage_version = match &command {
        ExecutionCommand::RegisterStorage { binding }
        | ExecutionCommand::UpdateStorage { binding, .. } => Some(binding.version),
        _ => None,
    };
    let key = match one_header(headers, "idempotency-key") {
        Ok(key) => key,
        Err(error) => return api_error(error),
    };
    let request_id = if headers.contains_key("x-request-id") {
        match one_header(headers, "x-request-id")
            .ok()
            .and_then(|value| Uuid::parse_str(value).ok())
        {
            Some(id) => id,
            None => return api_error(Error::Invalid),
        }
    } else {
        Uuid::new_v4()
    };
    match state
        .execution
        .execute(actor, request_id, key, command)
        .await
    {
        Ok(accepted) => {
            let mut result = if let Some(version) = accepted.assignment_version.or(storage_version)
            {
                versioned(&accepted, version)
            } else {
                Json(accepted).into_response()
            };
            *result.status_mut() = StatusCode::ACCEPTED;
            result
        }
        Err(error) => api_error(error),
    }
}

async fn register_runtime(
    State(state): State<Arc<AdminState>>,
    Extension(actor): Extension<VerifiedPrincipal>,
    headers: HeaderMap,
    body: Result<Json<RuntimeDefinition>, JsonRejection>,
) -> Response {
    let Ok(Json(definition)) = body else {
        return api_error(Error::Invalid);
    };
    mutate_execution(
        &state,
        &actor,
        &headers,
        ExecutionCommand::RegisterRuntime { definition },
    )
    .await
}
async fn register_runtime_version(
    State(state): State<Arc<AdminState>>,
    Extension(actor): Extension<VerifiedPrincipal>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Result<Json<RuntimeDefinition>, JsonRejection>,
) -> Response {
    let Ok(Json(definition)) = body else {
        return api_error(Error::Invalid);
    };
    if definition.runtime.id != id {
        return api_error(Error::Invalid);
    }
    mutate_execution(
        &state,
        &actor,
        &headers,
        ExecutionCommand::RegisterRuntime { definition },
    )
    .await
}
async fn register_storage(
    State(state): State<Arc<AdminState>>,
    Extension(actor): Extension<VerifiedPrincipal>,
    headers: HeaderMap,
    body: Result<Json<StorageBinding>, JsonRejection>,
) -> Response {
    let Ok(Json(binding)) = body else {
        return api_error(Error::Invalid);
    };
    mutate_execution(
        &state,
        &actor,
        &headers,
        ExecutionCommand::RegisterStorage { binding },
    )
    .await
}

async fn storage_binding(State(state): State<Arc<AdminState>>, Path(id): Path<Uuid>) -> Response {
    match state.store.storage_binding(&state.scope, id).await {
        Ok(binding) => {
            let version = binding.version;
            versioned(binding, version)
        }
        Err(error) => api_error(error),
    }
}
async fn update_storage(
    State(state): State<Arc<AdminState>>,
    Extension(actor): Extension<VerifiedPrincipal>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Result<Json<StorageBinding>, JsonRejection>,
) -> Response {
    let Ok(Json(binding)) = body else {
        return api_error(Error::Invalid);
    };
    if binding.id != id {
        return api_error(Error::Invalid);
    }
    let expected_version = match expected_version(&headers) {
        Ok(version) => version,
        Err(error) => return api_error(error),
    };
    mutate_execution(
        &state,
        &actor,
        &headers,
        ExecutionCommand::UpdateStorage {
            expected_version,
            binding,
        },
    )
    .await
}
async fn register_pull_secret(
    State(state): State<Arc<AdminState>>,
    Extension(actor): Extension<VerifiedPrincipal>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Result<Json<PullSecretBinding>, JsonRejection>,
) -> Response {
    let Ok(Json(binding)) = body else {
        return api_error(Error::Invalid);
    };
    if binding.instance_id != id {
        return api_error(Error::Invalid);
    }
    mutate_execution(
        &state,
        &actor,
        &headers,
        ExecutionCommand::RegisterPullSecret { binding },
    )
    .await
}
async fn policy(State(state): State<Arc<AdminState>>, Path(id): Path<Uuid>) -> Response {
    match state.store.execution_policy(&state.scope, id).await {
        Ok(view) => {
            let version = view.assignment_version;
            versioned(view, version)
        }
        Err(error) => api_error(error),
    }
}
async fn set_policy(
    State(state): State<Arc<AdminState>>,
    Extension(actor): Extension<VerifiedPrincipal>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Result<Json<AssignmentExecution>, JsonRejection>,
) -> Response {
    let Ok(Json(policy)) = body else {
        return api_error(Error::Invalid);
    };
    let expected_version = match expected_version(&headers) {
        Ok(version) => version,
        Err(error) => return api_error(error),
    };
    mutate_execution(
        &state,
        &actor,
        &headers,
        ExecutionCommand::SetPolicy {
            assignment_id: id,
            expected_version,
            policy,
        },
    )
    .await
}
