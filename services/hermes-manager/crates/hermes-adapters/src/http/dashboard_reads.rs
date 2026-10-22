use super::*;
pub(super) fn routes() -> Router<Arc<AdminState>> {
    Router::new()
        .route(
            "/api/v1/assignments/{id}/template-observation",
            get(template_observation),
        )
        .route("/api/v1/template-versions", get(catalog_versions))
        .route(
            "/api/v1/terminal-runtimes/{id}/version-history",
            get(runtime_history),
        )
        .route("/api/v1/terminal-runtimes/{id}", get(runtime))
        .route(
            "/api/v1/configuration-references/storage-bindings",
            get(bindings),
        )
        .route("/api/v1/identities/{id}", get(identity))
        .route("/api/v1/assignments/{id}/operations", get(history))
        .route("/api/v1/operations/{id}/details", get(operation_details))
        .route("/api/v1/audit-events/{id}", get(audit_detail))
        .route(
            "/api/v1/template-definitions/{id}/versions/{version}",
            get(template_version),
        )
        .route(
            "/api/v1/configuration-references/storage-classes",
            get(storage_classes),
        )
}
async fn identity(State(s): State<Arc<AdminState>>, Path(id): Path<Uuid>) -> Response {
    match s.store.dashboard_identity(&s.scope, id).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => api_error(e),
    }
}
async fn operation_details(State(s): State<Arc<AdminState>>, Path(id): Path<Uuid>) -> Response {
    match s.store.dashboard_operation_details(&s.scope, id).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => api_error(e),
    }
}
async fn audit_detail(State(s): State<Arc<AdminState>>, Path(id): Path<Uuid>) -> Response {
    match s.store.dashboard_audit_detail(&s.scope, id).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => api_error(e),
    }
}
async fn template_version(
    State(s): State<Arc<AdminState>>,
    Path((id, version)): Path<(String, i64)>,
) -> Response {
    match s
        .store
        .dashboard_template_version(&s.scope, &id, version)
        .await
    {
        Ok(v) => Json(v).into_response(),
        Err(e) => api_error(e),
    }
}
async fn history(
    State(s): State<Arc<AdminState>>,
    Path(id): Path<Uuid>,
    q: Result<Query<DashboardQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let Ok(Query(mut q)) = q else {
        return api_error(Error::Invalid);
    };
    if q.assignment_id.is_some() {
        return api_error(Error::Invalid);
    }
    if let Err(e) = s.store.get_assignment(&s.scope, id).await {
        return api_error(e);
    }
    q.assignment_id = Some(id);
    match s
        .store
        .dashboard_list(&s.scope, DashboardList::Operations, &q)
        .await
    {
        Ok(v) => Json(v).into_response(),
        Err(e) => api_error(e),
    }
}
async fn storage_classes(
    State(s): State<Arc<AdminState>>,
    q: Result<Query<DashboardQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let Ok(Query(q)) = q else {
        return api_error(Error::Invalid);
    };
    match s
        .store
        .dashboard_list(&s.scope, DashboardList::StorageClasses, &q)
        .await
    {
        Ok(v) => Json(v).into_response(),
        Err(e) => api_error(e),
    }
}

async fn runtime(State(s): State<Arc<AdminState>>, Path(id): Path<String>) -> Response {
    match s.store.dashboard_runtime(&s.scope, &id).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => api_error(e),
    }
}
async fn bindings(
    State(s): State<Arc<AdminState>>,
    q: Result<Query<DashboardQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let Ok(Query(q)) = q else {
        return api_error(Error::Invalid);
    };
    let Some(id) = q.instance_id else {
        return api_error(Error::Invalid);
    };
    if let Err(e) = s.store.instance(&s.scope, id).await {
        return api_error(e);
    }
    match s
        .store
        .dashboard_list(&s.scope, DashboardList::Bindings, &q)
        .await
    {
        Ok(v) => Json(v).into_response(),
        Err(e) => api_error(e),
    }
}

async fn catalog_versions(
    State(s): State<Arc<AdminState>>,
    q: Result<Query<DashboardQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let Ok(Query(q)) = q else {
        return api_error(Error::Invalid);
    };
    match s
        .store
        .dashboard_list(&s.scope, DashboardList::CatalogVersions, &q)
        .await
    {
        Ok(v) => Json(v).into_response(),
        Err(e) => api_error(e),
    }
}
async fn runtime_history(
    State(s): State<Arc<AdminState>>,
    Path(id): Path<String>,
    q: Result<Query<DashboardQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let Ok(Query(mut q)) = q else {
        return api_error(Error::Invalid);
    };
    if q.runtime_id.is_some() || hermes_domain::validate_name(&id).is_err() {
        return api_error(Error::Invalid);
    }
    q.runtime_id = Some(id);
    match s
        .store
        .dashboard_list(&s.scope, DashboardList::RuntimeVersions, &q)
        .await
    {
        Ok(v) => Json(v).into_response(),
        Err(e) => api_error(e),
    }
}

async fn template_observation(State(s): State<Arc<AdminState>>, Path(id): Path<Uuid>) -> Response {
    match s.store.dashboard_template_observation(&s.scope, id).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => api_error(e),
    }
}
