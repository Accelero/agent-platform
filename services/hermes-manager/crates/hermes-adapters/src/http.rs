//! HTTPS/JSON transport. Browser/UI code is a client of these versioned routes.
use std::{sync::Arc, time::Duration};

use axum::{
    Extension, Json, Router,
    body::{Body, Bytes, to_bytes},
    extract::{DefaultBodyLimit, Path, Query, Request, State, rejection::JsonRejection},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use hermes_application::{
    Accepted, AdminPolicy, AssignmentCommand, AssignmentReader, Assignments, Error,
    InstallationScope, VerifiedPrincipal,
    administration::AdministrationReader,
    routing::{RouteDecision, RoutePolicy, Routes, VerifiedRouterService},
};
use hermes_domain::{Access, Identity, TemplateVersion};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    auth::{AccessTokenVerifier, unix_time},
    browser_auth::BrowserAuth,
    browser_http,
    postgres::PgStore,
    server::PeerIdentity,
    session_crypto::Purpose,
};

mod administrators;
mod authoring;
mod dashboard;
mod dashboard_reads;
mod rollout;
use crate::postgres::dashboard::{DashboardList, DashboardQuery};
mod execution;
mod validation;
pub use dashboard::DashboardAssets;

struct AdminState {
    store: PgStore,
    scope: InstallationScope,
    commands: Assignments<PgStore>,
    execution: hermes_application::execution::ExecutionAdministration<PgStore>,
}
#[derive(Clone)]
struct AdminAuth {
    store: PgStore,
    scope: InstallationScope,
    tokens: AccessTokenVerifier,
    policy: AdminPolicy,
    browser: Option<BrowserAuth>,
}
#[derive(Clone)]
struct BrowserCsrf(Option<String>);
#[derive(Clone)]
struct ManageAdministrators(bool);
struct ResolverState {
    routes: Routes<PgStore>,
    tokens: AccessTokenVerifier,
    service_identity: String,
}

fn response(status: StatusCode, body: Value) -> Response {
    (status, Json(body)).into_response()
}

pub(crate) fn api_error(error: Error) -> Response {
    let (status, code) = match error {
        Error::AdministratorExists => (StatusCode::CONFLICT, "administrator_grant_exists"),
        Error::UnverifiedIdentity => (StatusCode::UNPROCESSABLE_ENTITY, "identity_not_verified"),
        Error::LastAdministrator => (StatusCode::CONFLICT, "last_administrator"),
        Error::MigrationRequired => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "storage_migration_required",
        ),
        Error::OwnershipConflict => (StatusCode::CONFLICT, "ownership_conflict"),
        Error::Invalid => (StatusCode::UNPROCESSABLE_ENTITY, "invalid_request"),
        Error::InvalidIdentityEvidence => (StatusCode::UNAUTHORIZED, "invalid_identity_evidence"),
        Error::Forbidden => (StatusCode::FORBIDDEN, "access_denied"),
        Error::IdempotencyConflict => (StatusCode::CONFLICT, "idempotency_conflict"),
        Error::AlreadyExists => (StatusCode::CONFLICT, "assignment_exists"),
        Error::NotFound => (StatusCode::NOT_FOUND, "not_found"),
        Error::PreconditionFailed => (StatusCode::PRECONDITION_FAILED, "stale_version"),
        Error::TemplateUnavailable => (StatusCode::UNPROCESSABLE_ENTITY, "template_unavailable"),
        Error::Maintenance => (StatusCode::SERVICE_UNAVAILABLE, "maintenance"),
        Error::RecoveryRequired => (StatusCode::SERVICE_UNAVAILABLE, "registry_unavailable"),
        _ => (StatusCode::SERVICE_UNAVAILABLE, "dependency_unavailable"),
    };
    let mut result = response(status, json!({"error":code}));
    if status == StatusCode::UNAUTHORIZED {
        result
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    }
    result
}

pub(crate) fn one_header<'a>(headers: &'a HeaderMap, key: &str) -> Result<&'a str, Error> {
    let mut values = headers.get_all(key).iter();
    let value = values.next().ok_or(Error::Invalid)?;
    if values.next().is_some() {
        return Err(Error::Invalid);
    }
    value.to_str().map_err(|_| Error::Invalid)
}

fn bearer(headers: &HeaderMap) -> Result<&str, Error> {
    let value = one_header(headers, "authorization").map_err(|_| Error::InvalidIdentityEvidence)?;
    let token = value
        .strip_prefix("Bearer ")
        .filter(|token| {
            !token.is_empty()
                && token.len() <= 16 * 1024
                && token.bytes().all(|b| b.is_ascii_graphic())
        })
        .ok_or(Error::InvalidIdentityEvidence)?;
    Ok(token)
}

async fn admin_auth(State(auth): State<AdminAuth>, mut request: Request, next: Next) -> Response {
    let headers = request.headers().clone();
    let method = request.method().clone();
    let result = async {
        let session = match &auth.browser {
            Some(browser) => browser_http::cookie(&headers, browser.session_cookie())?,
            None => None,
        };
        if headers.contains_key(header::AUTHORIZATION) {
            if session.is_some() {
                return Err(Error::InvalidIdentityEvidence);
            }
            return Ok((
                auth.tokens.verify(bearer(&headers)?).await?.principal(),
                None,
            ));
        }
        let browser = auth
            .browser
            .as_ref()
            .ok_or(Error::InvalidIdentityEvidence)?;
        let session = session.ok_or(Error::InvalidIdentityEvidence)?;
        let proof = browser_http::cookie(&headers, browser.browser_cookie())?
            .ok_or(Error::InvalidIdentityEvidence)?;
        let verified = browser.authenticate(session, proof).await?;
        if !matches!(method.as_str(), "GET" | "HEAD" | "OPTIONS") {
            browser.check_origin(one_header(&headers, "origin").map_err(|_| Error::Forbidden)?)?;
            BrowserAuth::check_csrf(
                &verified.csrf_token,
                one_header(&headers, "x-csrf-token").map_err(|_| Error::Forbidden)?,
            )?;
        }
        Ok((verified.access.principal(), Some(verified.csrf_token)))
    }
    .await;
    let (principal, csrf) = match result {
        Ok(value) => value,
        Err(e) => return api_error(e),
    };
    if let Err(error) = auth.policy.authorize(&principal) {
        return api_error(error);
    }
    if let Err(error) = auth
        .store
        .note_verified_identity(&auth.scope, &principal.identity)
        .await
    {
        return api_error(error);
    }
    let manage = match auth
        .store
        .authorize_administrator(&auth.scope, &principal.identity)
        .await
    {
        Ok(manage) => manage,
        Err(error) => return api_error(error),
    };
    request
        .extensions_mut()
        .insert(ManageAdministrators(manage));
    request.extensions_mut().insert(principal);
    request.extensions_mut().insert(BrowserCsrf(csrf));
    next.run(request).await
}

async fn bounded_admin(request: Request, next: Next) -> Response {
    bounded(request, next, Duration::from_secs(15), false).await
}
async fn bounded_resolver(request: Request, next: Next) -> Response {
    bounded(request, next, Duration::from_millis(1800), true).await
}

pub(crate) async fn bounded(
    request: Request,
    next: Next,
    deadline: Duration,
    resolver: bool,
) -> Response {
    let header_size: usize = request
        .headers()
        .iter()
        .map(|(key, value)| key.as_str().len() + value.as_bytes().len())
        .sum();
    let mut result = if header_size > 16 * 1024 || request.uri().to_string().len() > 8192 {
        api_error(Error::Invalid)
    } else {
        match tokio::time::timeout(deadline, async {
            // DefaultBodyLimit is extractor-local. Read every body here so GET,
            // health and fallback routes have the same bound, including streams
            // without Content-Length. Reading is inside the overall deadline.
            let (parts, body) = request.into_parts();
            let bytes = match to_bytes(body, 16 * 1024).await {
                Ok(bytes) => bytes,
                Err(_) => return api_error(Error::Invalid),
            };
            next.run(Request::from_parts(parts, Body::from(bytes)))
                .await
        })
        .await
        {
            Ok(result) => result,
            Err(_) if resolver => response(
                StatusCode::SERVICE_UNAVAILABLE,
                json!({"status":"unavailable","reason":"dependency_unavailable"}),
            ),
            Err(_) => api_error(Error::Unavailable),
        }
    };
    // Axum extractor/method errors must not reflect submitted path/body values
    // or turn authentication/internal failures into an HTML/SPA response.
    if result.status().is_client_error()
        && !result
            .headers()
            .get(header::CONTENT_TYPE)
            .is_some_and(|value| value.as_bytes().starts_with(b"application/json"))
    {
        let status = match result.status() {
            StatusCode::BAD_REQUEST
            | StatusCode::UNSUPPORTED_MEDIA_TYPE
            | StatusCode::PAYLOAD_TOO_LARGE => StatusCode::UNPROCESSABLE_ENTITY,
            other => other,
        };
        result = response(
            status,
            json!({"error":if status==StatusCode::NOT_FOUND{"not_found"}else{"invalid_request"}}),
        );
    }
    result
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    result.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    result.headers_mut().insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    result
}

async fn not_found() -> Response {
    api_error(Error::NotFound)
}
async fn health() -> &'static str {
    "ok"
}

pub fn admin_router(
    store: PgStore,
    scope: InstallationScope,
    policy: AdminPolicy,
    tokens: AccessTokenVerifier,
) -> Result<Router, Error> {
    admin_router_with_browser(store, scope, policy, tokens, None)
}

pub fn admin_router_with_browser(
    store: PgStore,
    scope: InstallationScope,
    policy: AdminPolicy,
    tokens: AccessTokenVerifier,
    browser: Option<BrowserAuth>,
) -> Result<Router, Error> {
    admin_router_with_dashboard(store, scope, policy, tokens, browser, None)
}

pub fn admin_router_with_dashboard(
    store: PgStore,
    scope: InstallationScope,
    policy: AdminPolicy,
    tokens: AccessTokenVerifier,
    browser: Option<BrowserAuth>,
    dashboard: Option<DashboardAssets>,
) -> Result<Router, Error> {
    if dashboard.is_some() && browser.is_none() {
        return Err(Error::Invalid);
    }
    if browser
        .as_ref()
        .is_some_and(|auth| auth.purpose() != Purpose::Administration)
    {
        return Err(Error::Invalid);
    }
    let state = Arc::new(AdminState {
        commands: Assignments::new(store.clone(), scope.clone(), policy.clone())?,
        execution: hermes_application::execution::ExecutionAdministration::new(
            store.clone(),
            scope.clone(),
            policy.clone(),
        )?,
        store,
        scope,
    });
    let auth = AdminAuth {
        store: state.store.clone(),
        scope: state.scope.clone(),
        tokens,
        policy,
        browser: browser.clone(),
    };
    let protected = Router::new()
        .route("/api/v1/session", get(session))
        .route("/api/v1/identities", get(identities))
        .route("/api/v1/templates", get(templates))
        .route(
            "/api/v1/configuration-schemas",
            get(validation::configuration_schemas),
        )
        .route("/api/v1/template-definitions", get(template_definitions))
        .route(
            "/api/v1/template-definitions/validate",
            post(validation::validate_template),
        )
        .route(
            "/api/v1/template-definitions/{id}",
            get(template_definition),
        )
        .route(
            "/api/v1/assignments",
            get(assignments).post(create_assignment),
        )
        .route(
            "/api/v1/assignments/{id}",
            get(assignment).patch(set_access),
        )
        .route("/api/v1/instances/{id}", get(instance))
        .route("/api/v1/instances/{id}/reconcile", post(reconcile))
        .route("/api/v1/operations/{id}", get(operation))
        .route("/api/v1/audit-events", get(audit))
        .merge(execution::routes())
        .merge(dashboard_reads::routes())
        .merge(authoring::routes())
        .merge(administrators::routes())
        .merge(rollout::routes())
        .route_layer(middleware::from_fn_with_state(auth.clone(), admin_auth))
        .with_state(state);
    let protected = match dashboard {
        Some(assets) => protected.merge(dashboard::router(assets, auth)),
        None => protected,
    };
    let protected = match browser {
        Some(auth) => protected.merge(browser_http::router(auth)),
        None => protected,
    };
    Ok(protected
        .route("/healthz", get(health))
        .fallback(not_found)
        .layer(DefaultBodyLimit::max(16 * 1024))
        .layer(middleware::from_fn(bounded_admin)))
}

pub fn resolver_router(
    store: PgStore,
    scope: InstallationScope,
    policy: RoutePolicy,
    tokens: AccessTokenVerifier,
) -> Result<Router, Error> {
    let service_identity = policy.router_service_identity.clone();
    let state = Arc::new(ResolverState {
        routes: Routes::new(store, scope, policy)?,
        tokens,
        service_identity,
    });
    Ok(Router::new()
        .route("/internal/v1/routes/resolve", post(resolve))
        .route("/healthz", get(health))
        .fallback(not_found)
        .with_state(state)
        .layer(DefaultBodyLimit::max(16 * 1024))
        .layer(middleware::from_fn(bounded_resolver)))
}

async fn session(
    Extension(actor): Extension<VerifiedPrincipal>,
    Extension(csrf): Extension<BrowserCsrf>,
    Extension(manage): Extension<ManageAdministrators>,
) -> Json<Value> {
    let mut actions = vec![
        "template.draft.create",
        "template.draft.update",
        "template.publish",
        "assignment.set_template",
        "assignment.create",
        "assignment.set_access",
        "instance.retry",
        "execution.runtime.register",
        "execution.storage.register",
        "execution.storage.update",
        "execution.pull-secret.register",
        "execution.policy.set",
    ];
    if manage.0 {
        actions.push("administrator.manage");
    }
    Json(json!({"actor":actor.identity,"permittedActions":actions,"csrfToken":csrf.0}))
}

fn default_limit() -> u16 {
    50
}

fn page<T: Serialize>(items: Vec<T>, next: Option<Uuid>) -> Json<Value> {
    Json(json!({"items":items,"nextCursor":next}))
}

async fn identities(
    State(state): State<Arc<AdminState>>,
    query: Result<Query<DashboardQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let Ok(Query(query)) = query else {
        return api_error(Error::Invalid);
    };
    match state
        .store
        .dashboard_list(&state.scope, DashboardList::Identities, &query)
        .await
    {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(error),
    }
}

async fn template_definitions(
    State(state): State<Arc<AdminState>>,
    query: Result<Query<DashboardQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let Ok(Query(query)) = query else {
        return api_error(Error::Invalid);
    };
    match state
        .store
        .dashboard_list(&state.scope, DashboardList::TemplateDefinitions, &query)
        .await
    {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(error),
    }
}
async fn template_definition(
    State(state): State<Arc<AdminState>>,
    Path(id): Path<String>,
) -> Response {
    match state
        .store
        .dashboard_templates(&state.scope, Some(&id))
        .await
    {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(error),
    }
}

async fn templates(State(state): State<Arc<AdminState>>) -> Response {
    match state.store.templates(&state.scope).await {
        Ok(items) => page(items, None).into_response(),
        Err(error) => api_error(error),
    }
}

async fn assignments(
    State(state): State<Arc<AdminState>>,
    query: Result<Query<DashboardQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let Ok(Query(query)) = query else {
        return api_error(Error::Invalid);
    };
    match state
        .store
        .dashboard_list(&state.scope, DashboardList::Assignments, &query)
        .await
    {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(error),
    }
}

fn versioned<T: Serialize>(body: T, version: i64) -> Response {
    let mut response = Json(body).into_response();
    if let Ok(etag) = HeaderValue::from_str(&format!("\"{version}\"")) {
        response.headers_mut().insert(header::ETAG, etag);
    }
    response
}

async fn assignment(State(state): State<Arc<AdminState>>, Path(id): Path<Uuid>) -> Response {
    match state.store.get_assignment(&state.scope, id).await {
        Ok(view) => {
            let version = view.version;
            versioned(view, version)
        }
        Err(error) => api_error(error),
    }
}

async fn instance(State(state): State<Arc<AdminState>>, Path(id): Path<Uuid>) -> Response {
    match state.store.instance(&state.scope, id).await {
        Ok(view) => versioned(
            json!({"id":view.instance_id,"assignmentId":view.id,"namespace":view.namespace,"desiredGeneration":view.desired_generation,"observedGeneration":view.observed_generation,"state":view.state,"version":view.version}),
            view.version,
        ),
        Err(error) => api_error(error),
    }
}

async fn operation(State(state): State<Arc<AdminState>>, Path(id): Path<Uuid>) -> Response {
    match state.store.operation(&state.scope, id).await {
        Ok(view) => Json(view).into_response(),
        Err(error) => api_error(error),
    }
}

async fn audit(
    State(state): State<Arc<AdminState>>,
    query: Result<Query<DashboardQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let Ok(Query(query)) = query else {
        return api_error(Error::Invalid);
    };
    match state
        .store
        .dashboard_list(&state.scope, DashboardList::Audit, &query)
        .await
    {
        Ok(value) => Json(value).into_response(),
        Err(error) => api_error(error),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateAssignment {
    identity: Identity,
    access: Access,
    template_id: String,
    #[serde(default, deserialize_with = "optional_template_version")]
    template_version: Option<i64>,
}
fn optional_template_version<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<i64>, D::Error> {
    i64::deserialize(deserializer).map(Some)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetAccess {
    access: Access,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

fn expected_version(headers: &HeaderMap) -> Result<i64, Error> {
    let value = one_header(headers, "if-match")?;
    let number = value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .ok_or(Error::Invalid)?;
    if number.is_empty() || number.starts_with('0') || !number.bytes().all(|b| b.is_ascii_digit()) {
        return Err(Error::Invalid);
    }
    number
        .parse::<i64>()
        .ok()
        .filter(|v| *v > 0)
        .ok_or(Error::Invalid)
}

async fn mutate(
    state: &AdminState,
    actor: &VerifiedPrincipal,
    headers: &HeaderMap,
    command: AssignmentCommand,
) -> Response {
    let key = match one_header(headers, "idempotency-key") {
        Ok(key) => key,
        Err(error) => return api_error(error),
    };
    let request_id = if headers.contains_key("x-request-id") {
        match one_header(headers, "x-request-id")
            .ok()
            .and_then(|v| Uuid::parse_str(v).ok())
        {
            Some(id) => id,
            None => return api_error(Error::Invalid),
        }
    } else {
        Uuid::new_v4()
    };
    match state
        .commands
        .execute(actor, request_id, key, command)
        .await
    {
        Ok(accepted) => accepted_response(accepted),
        Err(error) => api_error(error),
    }
}

fn accepted_response(accepted: Accepted) -> Response {
    let version = accepted.version;
    let mut response = versioned(accepted, version);
    *response.status_mut() = StatusCode::ACCEPTED;
    response
}

async fn create_assignment(
    State(state): State<Arc<AdminState>>,
    Extension(actor): Extension<VerifiedPrincipal>,
    headers: HeaderMap,
    body: Result<Json<CreateAssignment>, JsonRejection>,
) -> Response {
    let Ok(Json(body)) = body else {
        return api_error(Error::Invalid);
    };
    let command = if let Some(version) = body.template_version {
        AssignmentCommand::Create {
            identity: body.identity,
            access: body.access,
            template: TemplateVersion {
                id: body.template_id,
                version,
            },
        }
    } else {
        AssignmentCommand::CreateDefaultTemplate {
            identity: body.identity,
            access: body.access,
            template_id: body.template_id,
        }
    };
    mutate(&state, &actor, &headers, command).await
}

async fn set_access(
    State(state): State<Arc<AdminState>>,
    Extension(actor): Extension<VerifiedPrincipal>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Result<Json<SetAccess>, JsonRejection>,
) -> Response {
    let Ok(Json(body)) = body else {
        return api_error(Error::Invalid);
    };
    let version = match expected_version(&headers) {
        Ok(version) => version,
        Err(error) => return api_error(error),
    };
    mutate(
        &state,
        &actor,
        &headers,
        AssignmentCommand::SetAccess {
            assignment_id: id,
            expected_version: version,
            access: body.access,
        },
    )
    .await
}

async fn reconcile(
    State(state): State<Arc<AdminState>>,
    Extension(actor): Extension<VerifiedPrincipal>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !body.is_empty() && serde_json::from_slice::<Empty>(&body).is_err() {
        return api_error(Error::Invalid);
    }
    let version = match expected_version(&headers) {
        Ok(version) => version,
        Err(error) => return api_error(error),
    };
    mutate(
        &state,
        &actor,
        &headers,
        AssignmentCommand::Retry {
            instance_id: id,
            expected_version: version,
        },
    )
    .await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Resolve {
    identity: Identity,
}

async fn resolve(
    State(state): State<Arc<ResolverState>>,
    Extension(peer): Extension<PeerIdentity>,
    headers: HeaderMap,
    body: Result<Json<Resolve>, JsonRejection>,
) -> Response {
    if peer.0.as_deref() != Some(&state.service_identity) {
        return response(
            StatusCode::FORBIDDEN,
            json!({"status":"denied","reason":"access_denied"}),
        );
    }
    let token = match bearer(&headers) {
        Ok(token) => token,
        Err(error) => return api_error(error),
    };
    let verified = match state.tokens.verify(token).await {
        Ok(value) => value,
        Err(Error::Unavailable) => {
            return response(
                StatusCode::SERVICE_UNAVAILABLE,
                json!({"status":"unavailable","reason":"dependency_unavailable"}),
            );
        }
        Err(error) => return api_error(error),
    };
    let Ok(Json(body)) = body else {
        return api_error(Error::Invalid);
    };
    let evidence = match verified.route_evidence() {
        Ok(value) => value,
        Err(error) => return api_error(error),
    };
    let now = match unix_time().ok().and_then(|value| i64::try_from(value).ok()) {
        Some(value) => value,
        None => return api_error(Error::Unavailable),
    };
    let decision = match state
        .routes
        .resolve(
            &VerifiedRouterService {
                identity: state.service_identity.clone(),
            },
            &evidence,
            &body.identity,
            now,
        )
        .await
    {
        Ok(value) => value,
        Err(error) => return api_error(error),
    };
    let status = match decision {
        RouteDecision::Ready { .. } => StatusCode::OK,
        RouteDecision::Pending { .. } => StatusCode::ACCEPTED,
        RouteDecision::Denied { .. } => StatusCode::FORBIDDEN,
        RouteDecision::Unavailable { .. } => StatusCode::SERVICE_UNAVAILABLE,
    };
    (status, Json(decision)).into_response()
}
