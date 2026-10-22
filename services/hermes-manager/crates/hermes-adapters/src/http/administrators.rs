use super::*;
use crate::postgres::administrators::{AdministratorChange, AdministratorInput};
pub(super) fn routes() -> Router<Arc<AdminState>> {
    Router::new()
        .route("/api/v1/administrators", get(list).post(create))
        .route("/api/v1/administrator-identities", get(candidates))
        .route("/api/v1/administrators/{id}", get(detail).patch(update))
}
async fn list(
    State(state): State<Arc<AdminState>>,
    query: Result<Query<DashboardQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let Ok(Query(query)) = query else {
        return api_error(Error::Invalid);
    };
    match state
        .store
        .dashboard_list(&state.scope, DashboardList::Administrators, &query)
        .await
    {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(error),
    }
}
async fn candidates(
    State(state): State<Arc<AdminState>>,
    Extension(actor): Extension<VerifiedPrincipal>,
    query: Result<Query<DashboardQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let Ok(Query(query)) = query else {
        return api_error(Error::Invalid);
    };
    match state
        .store
        .administrator_candidates(&state.scope, &actor.identity, &query)
        .await
    {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(error),
    }
}
async fn detail(State(state): State<Arc<AdminState>>, Path(id): Path<Uuid>) -> Response {
    match state.store.administrator_grant(&state.scope, id).await {
        Ok(value) => versioned(&value, value["version"].as_i64().unwrap_or(1)),
        Err(error) => api_error(error),
    }
}
async fn save(
    state: &AdminState,
    actor: &VerifiedPrincipal,
    headers: &HeaderMap,
    change: AdministratorChange,
) -> Response {
    let key = match one_header(headers, "idempotency-key") {
        Ok(key) => key,
        Err(error) => return api_error(error),
    };
    match state
        .store
        .update_administrator(&state.scope, actor, key, change)
        .await
    {
        Ok(value) => {
            let mut result = versioned(&value, value["version"].as_i64().unwrap_or(1));
            *result.status_mut() = StatusCode::ACCEPTED;
            result
        }
        Err(error) => api_error(error),
    }
}
async fn create(
    State(state): State<Arc<AdminState>>,
    Extension(actor): Extension<VerifiedPrincipal>,
    headers: HeaderMap,
    body: Result<Json<AdministratorInput>, JsonRejection>,
) -> Response {
    let Ok(Json(input)) = body else {
        return api_error(Error::Invalid);
    };
    save(
        &state,
        &actor,
        &headers,
        AdministratorChange {
            id: None,
            expected: None,
            input,
        },
    )
    .await
}
async fn update(
    State(state): State<Arc<AdminState>>,
    Extension(actor): Extension<VerifiedPrincipal>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Result<Json<AdministratorInput>, JsonRejection>,
) -> Response {
    let Ok(Json(input)) = body else {
        return api_error(Error::Invalid);
    };
    let expected = match expected_version(&headers) {
        Ok(version) => version,
        Err(error) => return api_error(error),
    };
    save(
        &state,
        &actor,
        &headers,
        AdministratorChange {
            id: Some(id),
            expected: Some(expected),
            input,
        },
    )
    .await
}
