use super::*;
use crate::postgres::authoring::{DraftChange, DraftInput};
use crate::template_validation::{StrictValue, problem, validate};
pub(super) fn routes() -> Router<Arc<AdminState>> {
    Router::new()
        .route("/api/v1/template-drafts", get(list).post(create))
        .route("/api/v1/template-drafts/{id}", get(detail).patch(update))
        .route("/api/v1/template-drafts/{id}/publish", post(publish))
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
        .dashboard_list(&state.scope, DashboardList::Drafts, &query)
        .await
    {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(error),
    }
}
async fn detail(State(state): State<Arc<AdminState>>, Path(id): Path<Uuid>) -> Response {
    match state.store.dashboard_drafts(&state.scope, Some(id)).await {
        Ok(mut items) => {
            let draft = items.remove(0);
            let version = draft.version;
            versioned(draft, version)
        }
        Err(error) => api_error(error),
    }
}
async fn save(
    state: &AdminState,
    actor: &VerifiedPrincipal,
    headers: &HeaderMap,
    change: DraftChange,
) -> Response {
    let key = match one_header(headers, "idempotency-key") {
        Ok(key) => key,
        Err(error) => return api_error(error),
    };
    let problems = crate::template_validation::validate(
        &serde_json::to_value(&change.input.definition).unwrap_or(Value::Null),
    );
    if !problems.is_empty() {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"error":"invalid_configuration","problems":problems})),
        )
            .into_response();
    }
    match state
        .store
        .dashboard_save_draft(&state.scope, actor, key, change)
        .await
    {
        Ok(value) => {
            let mut result = versioned(&value, value["draft"]["version"].as_i64().unwrap_or(1));
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
    body: Result<Json<StrictValue>, JsonRejection>,
) -> Response {
    let input = match parse_input(body) {
        Ok(input) => input,
        Err(response) => return *response,
    };
    save(
        &state,
        &actor,
        &headers,
        DraftChange {
            id: None,
            expected: None,
            input,
            publish: false,
        },
    )
    .await
}
async fn update(
    State(state): State<Arc<AdminState>>,
    Extension(actor): Extension<VerifiedPrincipal>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Result<Json<StrictValue>, JsonRejection>,
) -> Response {
    let input = match parse_input(body) {
        Ok(input) => input,
        Err(response) => return *response,
    };
    let expected = match expected_version(&headers) {
        Ok(value) => value,
        Err(error) => return api_error(error),
    };
    save(
        &state,
        &actor,
        &headers,
        DraftChange {
            id: Some(id),
            expected: Some(expected),
            input,
            publish: false,
        },
    )
    .await
}
async fn publish(
    State(state): State<Arc<AdminState>>,
    Extension(actor): Extension<VerifiedPrincipal>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Result<Json<Empty>, JsonRejection>,
) -> Response {
    if body.is_err() {
        return api_error(Error::Invalid);
    }
    let expected = match expected_version(&headers) {
        Ok(value) => value,
        Err(error) => return api_error(error),
    };
    let draft = match state.store.dashboard_drafts(&state.scope, Some(id)).await {
        Ok(mut items) => items.remove(0),
        Err(error) => return api_error(error),
    };
    save(
        &state,
        &actor,
        &headers,
        DraftChange {
            id: Some(id),
            expected: Some(expected),
            input: DraftInput {
                template_id: draft.template_id,
                definition: draft.definition,
            },
            publish: true,
        },
    )
    .await
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Empty {}

fn parse_input(
    body: Result<Json<StrictValue>, JsonRejection>,
) -> Result<DraftInput, Box<Response>> {
    let invalid = |problems| {
        Box::new(
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({"error":"invalid_configuration","problems":problems})),
            )
                .into_response(),
        )
    };
    let Ok(Json(StrictValue(value))) = body else {
        return Err(invalid(vec![problem("".into(), "syntax")]));
    };
    let problems = validate(value.get("definition").unwrap_or(&Value::Null));
    if !problems.is_empty() {
        return Err(invalid(problems));
    }
    serde_json::from_value(value)
        .map_err(|_| invalid(vec![problem("/templateId".into(), "schema")]))
}
