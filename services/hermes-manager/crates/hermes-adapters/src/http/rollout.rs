use super::*;
use crate::postgres::rollout::{BatchMember, TemplateBatch, TemplateSelection};
pub(super) fn routes() -> Router<Arc<AdminState>> {
    Router::new()
        .route("/api/v1/template-batches/{id}/resume", post(resume))
        .route("/api/v1/assignments/{id}/template-preview", get(preview))
        .route("/api/v1/assignments/{id}/template", post(apply))
        .route("/api/v1/template-batches/preview", post(batch_preview))
        .route("/api/v1/template-batches", post(batch))
        .route("/api/v1/template-batches/{id}", get(batch_detail))
}
async fn preview(
    State(state): State<Arc<AdminState>>,
    Path(id): Path<Uuid>,
    selection: Result<Query<TemplateSelection>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let Ok(Query(selection)) = selection else {
        return api_error(Error::Invalid);
    };
    match state
        .store
        .dashboard_template_preview(&state.scope, id, &selection)
        .await
    {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(error),
    }
}
async fn apply(
    State(state): State<Arc<AdminState>>,
    Extension(actor): Extension<VerifiedPrincipal>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Result<Json<TemplateSelection>, JsonRejection>,
) -> Response {
    let Ok(Json(selection)) = body else {
        return api_error(Error::Invalid);
    };
    let expected = match expected_version(&headers) {
        Ok(value) => value,
        Err(error) => return api_error(error),
    };
    let key = match one_header(&headers, "idempotency-key") {
        Ok(value) => value,
        Err(error) => return api_error(error),
    };
    match state
        .store
        .dashboard_apply_template(
            &state.scope,
            &actor,
            key,
            &BatchMember {
                assignment_id: id,
                expected_version: expected,
            },
            &selection,
        )
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
async fn batch_preview(
    State(state): State<Arc<AdminState>>,
    body: Result<Json<TemplateBatch>, JsonRejection>,
) -> Response {
    let Ok(Json(input)) = body else {
        return api_error(Error::Invalid);
    };
    if input.members.is_empty() || input.members.len() > 50 {
        return api_error(Error::Invalid);
    }
    let mut items = Vec::new();
    for member in input.members {
        match state
            .store
            .dashboard_template_preview(&state.scope, member.assignment_id, &input.selection)
            .await
        {
            Ok(mut value) => {
                if value["expectedVersion"] != member.expected_version {
                    value["eligible"] = json!(false);
                    value["blockedReasons"]
                        .as_array_mut()
                        .expect("preview reasons")
                        .push(json!("stale_version"));
                }
                items.push(value)
            }
            Err(error) => return api_error(error),
        }
    }
    Json(json!({"items":items})).into_response()
}
async fn batch(
    State(state): State<Arc<AdminState>>,
    Extension(actor): Extension<VerifiedPrincipal>,
    headers: HeaderMap,
    body: Result<Json<TemplateBatch>, JsonRejection>,
) -> Response {
    let Ok(Json(input)) = body else {
        return api_error(Error::Invalid);
    };
    let key = match one_header(&headers, "idempotency-key") {
        Ok(value) => value,
        Err(error) => return api_error(error),
    };
    let id = match state
        .store
        .dashboard_start_batch(&state.scope, &actor, key, &input)
        .await
    {
        Ok(id) => id,
        Err(error) => return api_error(error),
    };
    let store = state.store.clone();
    let scope = state.scope.clone();
    tokio::spawn(async move {
        let _ = tokio::time::timeout(
            Duration::from_secs(600),
            store.dashboard_dispatch_batch(&scope, &actor, id, &input),
        )
        .await;
    });
    (
        StatusCode::ACCEPTED,
        Json(json!({"id":id,"operationId":id})),
    )
        .into_response()
}
async fn batch_detail(State(state): State<Arc<AdminState>>, Path(id): Path<Uuid>) -> Response {
    match state.store.dashboard_batch(&state.scope, id).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(error),
    }
}
async fn resume(
    State(state): State<Arc<AdminState>>,
    Extension(actor): Extension<VerifiedPrincipal>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Result<Json<super::authoring::Empty>, JsonRejection>,
) -> Response {
    if one_header(&headers, "idempotency-key").is_err() || body.is_err() {
        return api_error(Error::Invalid);
    }
    let input = match state
        .store
        .dashboard_resume_batch(&state.scope, &actor, id)
        .await
    {
        Ok(input) => input,
        Err(error) => return api_error(error),
    };
    let store = state.store.clone();
    let scope = state.scope.clone();
    tokio::spawn(async move {
        let _ = tokio::time::timeout(
            Duration::from_secs(600),
            store.dashboard_dispatch_batch(&scope, &actor, id, &input),
        )
        .await;
    });
    (
        StatusCode::ACCEPTED,
        Json(json!({"id":id,"operationId":id})),
    )
        .into_response()
}
