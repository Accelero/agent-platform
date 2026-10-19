//! Session-only router adapter. No registry credentials or commissioning access.
use crate::{
    browser_auth::{BrowserAuth, BrowserIdentity},
    browser_http,
    http::{api_error, one_header},
    server::PeerIdentity,
    session_crypto::Purpose,
};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Request, State, rejection::JsonRejection},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use hermes_application::Error;
use serde::Deserialize;
use serde_json::json;
use std::{sync::Arc, time::Duration};
use url::Url;

#[derive(Clone)]
pub struct RouterAuthConfig {
    pub forward_auth_identity: String,
    pub session_plugin_identity: String,
}
struct AuthState {
    auth: BrowserAuth,
    config: RouterAuthConfig,
}

async fn bounded_public(request: Request, next: Next) -> Response {
    bounded(request, next, Duration::from_secs(15)).await
}
async fn bounded_internal(request: Request, next: Next) -> Response {
    bounded(request, next, Duration::from_millis(1800)).await
}
async fn bounded(request: Request, next: Next, timeout: Duration) -> Response {
    let size: usize = request
        .headers()
        .iter()
        .map(|(k, v)| k.as_str().len() + v.as_bytes().len())
        .sum();
    let mut response = if size > 16 * 1024 || request.uri().to_string().len() > 8192 {
        api_error(Error::Invalid)
    } else {
        match tokio::time::timeout(timeout, next.run(request)).await {
            Ok(r) => r,
            Err(_) => api_error(Error::Unavailable),
        }
    };
    if response.status().is_client_error()
        && !response
            .headers()
            .get(header::CONTENT_TYPE)
            .is_some_and(|v| v.as_bytes().starts_with(b"application/json"))
    {
        response = api_error(if response.status() == StatusCode::NOT_FOUND {
            Error::NotFound
        } else {
            Error::Invalid
        });
    }
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    response
}
async fn missing() -> Response {
    api_error(Error::NotFound)
}
async fn health() -> &'static str {
    "ok"
}
async fn identity(auth: &BrowserAuth, headers: &HeaderMap) -> Result<BrowserIdentity, Error> {
    let session = browser_http::cookie(headers, auth.session_cookie())?
        .ok_or(Error::InvalidIdentityEvidence)?;
    let browser = browser_http::cookie(headers, auth.browser_cookie())?
        .ok_or(Error::InvalidIdentityEvidence)?;
    auth.authenticate(session, browser).await
}
async fn me(State(s): State<Arc<AuthState>>, headers: HeaderMap) -> Response {
    match identity(&s.auth, &headers).await {
        Ok(v) => {
            Json(json!({"identity":v.access.identity(),"csrfToken":v.csrf_token})).into_response()
        }
        Err(e) => api_error(e),
    }
}

pub fn routers(auth: BrowserAuth, config: RouterAuthConfig) -> Result<(Router, Router), Error> {
    if auth.purpose() != Purpose::Router {
        return Err(Error::Invalid);
    }
    if config.forward_auth_identity == config.session_plugin_identity {
        return Err(Error::Invalid);
    }
    for value in [
        &config.forward_auth_identity,
        &config.session_plugin_identity,
    ] {
        let uri = Url::parse(value).map_err(|_| Error::Invalid)?;
        if uri.scheme() != "spiffe"
            || uri.host_str().is_none()
            || uri.query().is_some()
            || uri.fragment().is_some()
            || !uri.username().is_empty()
            || uri.password().is_some()
        {
            return Err(Error::Invalid);
        }
    }
    let state = Arc::new(AuthState {
        auth: auth.clone(),
        config,
    });
    let public = browser_http::router(auth)
        .merge(
            Router::new()
                .route("/_platform/me", get(me))
                .with_state(state.clone()),
        )
        .route("/healthz", get(health))
        .fallback(missing)
        .layer(DefaultBodyLimit::max(16 * 1024))
        .layer(middleware::from_fn(bounded_public));
    let internal = Router::new()
        .route("/internal/v1/forward-auth", get(forward_auth))
        .route("/internal/v1/sessions/authorize", post(authorize_session))
        .route("/healthz", get(health))
        .fallback(missing)
        .with_state(state)
        .layer(DefaultBodyLimit::max(16 * 1024))
        .layer(middleware::from_fn(bounded_internal));
    Ok((public, internal))
}

async fn forward_auth(
    State(s): State<Arc<AuthState>>,
    peer: Option<axum::Extension<PeerIdentity>>,
    headers: HeaderMap,
) -> Response {
    if !peer.is_some_and(|p| p.0.0.as_deref() == Some(s.config.forward_auth_identity.as_str())) {
        return api_error(Error::Forbidden);
    }
    let request = || -> Result<(&str, &str), Error> {
        let origin = Url::parse(s.auth.public_origin()).map_err(|_| Error::Invalid)?;
        let expected = origin[url::Position::BeforeHost..url::Position::AfterPort].to_string();
        if one_header(&headers, "x-forwarded-proto")? != "https"
            || one_header(&headers, "x-forwarded-host")? != expected
        {
            return Err(Error::Forbidden);
        }
        let method = one_header(&headers, "x-forwarded-method")?;
        if !matches!(
            method,
            "GET" | "HEAD" | "POST" | "PUT" | "PATCH" | "DELETE" | "OPTIONS"
        ) {
            return Err(Error::Invalid);
        }
        let uri = one_header(&headers, "x-forwarded-uri")?;
        if uri.len() > 8192
            || !uri.starts_with('/')
            || uri.starts_with("//")
            || uri.contains('\\')
            || uri.chars().any(|c| c.is_control())
        {
            return Err(Error::Invalid);
        }
        Ok((method, uri))
    };
    let (method, uri) = match request() {
        Ok(v) => v,
        Err(e) => return api_error(e),
    };
    match identity(&s.auth, &headers).await {
        Ok(v) => {
            let owner = v.access.identity();
            let mut response = StatusCode::OK.into_response();
            for (name, value) in [
                ("x-hermes-verified-issuer", owner.issuer.as_str()),
                ("x-hermes-verified-subject", owner.subject.as_str()),
                ("x-hermes-verified-evidence", v.access_token.as_str()),
                ("x-hermes-verified-session", v.session_handle.as_str()),
            ] {
                let Ok(value) = HeaderValue::from_str(value) else {
                    return api_error(Error::InvalidIdentityEvidence);
                };
                response
                    .headers_mut()
                    .insert(axum::http::HeaderName::from_static(name), value);
            }
            response
        }
        Err(Error::InvalidIdentityEvidence)
            if method == "GET"
                && one_header(&headers, "accept")
                    .is_ok_and(|a| a.split(',').any(|v| v.trim().starts_with("text/html"))) =>
        {
            // Native callbacks and API mutations are never replayed after login.
            if s.auth.safe_return_path(uri).is_err() {
                return api_error(Error::InvalidIdentityEvidence);
            }
            let query = url::form_urlencoded::Serializer::new(String::new())
                .append_pair("returnTo", uri)
                .finish();
            let location = format!("{}?{query}", s.auth.login_path());
            (StatusCode::SEE_OTHER, [(header::LOCATION, location)]).into_response()
        }
        Err(e) => api_error(e),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SessionRequest {
    session_id: String,
}
async fn authorize_session(
    State(s): State<Arc<AuthState>>,
    peer: Option<axum::Extension<PeerIdentity>>,
    body: Result<Json<SessionRequest>, JsonRejection>,
) -> Response {
    if !peer.is_some_and(|p| p.0.0.as_deref() == Some(s.config.session_plugin_identity.as_str())) {
        return api_error(Error::Forbidden);
    }
    let Ok(Json(body)) = body else {
        return api_error(Error::Invalid);
    };
    match s.auth.authenticate_handle(&body.session_id).await {
        Ok(v) => {
            Json(json!({"identity":v.access.identity(),"accessToken":v.access_token.as_str()}))
                .into_response()
        }
        Err(e) => api_error(e),
    }
}
