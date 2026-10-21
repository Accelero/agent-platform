//! Opaque browser cookies and OIDC endpoints shared by the two public origins.
use std::sync::Arc;

use axum::{
    Router,
    extract::{Query, State, rejection::QueryRejection},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use hermes_application::Error;
use serde::Deserialize;

use crate::{
    browser_auth::BrowserAuth,
    http::{api_error, one_header},
    session_crypto::Purpose,
};

/// Reject duplicates rather than letting parser order select a credential.
pub fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Result<Option<&'a str>, Error> {
    let mut found = None;
    for header in headers.get_all(header::COOKIE) {
        let header = header
            .to_str()
            .map_err(|_| Error::InvalidIdentityEvidence)?;
        for item in header.split(';') {
            let Some((key, value)) = item.trim().split_once('=') else {
                continue;
            };
            if key == name && found.replace(value).is_some() {
                return Err(Error::InvalidIdentityEvidence);
            }
        }
    }
    Ok(found)
}

fn set_cookie(response: &mut Response, name: &str, value: &str, age: u32) {
    // Names are static and values are generated lowercase hex, never provider data.
    let header = HeaderValue::from_str(&format!(
        "{name}={value}; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age={age}"
    ))
    .expect("static cookie and opaque handle");
    response.headers_mut().append(header::SET_COOKIE, header);
}
pub fn clear_cookies(response: &mut Response, auth: &BrowserAuth) {
    set_cookie(response, auth.session_cookie(), "", 0);
    set_cookie(response, auth.browser_cookie(), "", 0);
}

fn redirect(location: &str) -> Response {
    match HeaderValue::from_str(location) {
        Ok(location) => (StatusCode::SEE_OTHER, [(header::LOCATION, location)]).into_response(),
        Err(_) => api_error(Error::Invalid),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LoginQuery {
    return_to: Option<String>,
}

async fn login(
    State(auth): State<Arc<BrowserAuth>>,
    headers: HeaderMap,
    query: Result<Query<LoginQuery>, QueryRejection>,
) -> Response {
    let Ok(Query(query)) = query else {
        return api_error(Error::Invalid);
    };
    let browser = match cookie(&headers, auth.browser_cookie()) {
        Ok(value) => value,
        Err(e) => return api_error(e),
    };
    match auth
        .begin(browser, query.return_to.as_deref().unwrap_or("/"))
        .await
    {
        Ok(start) => {
            let mut response = redirect(&start.location);
            set_cookie(
                &mut response,
                auth.browser_cookie(),
                &start.browser,
                8 * 60 * 60,
            );
            set_cookie(&mut response, auth.session_cookie(), "", 0);
            response
        }
        Err(e) => api_error(e),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Callback {
    state: String,
    code: Option<String>,
    iss: Option<String>,
    session_state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}
async fn callback(
    State(auth): State<Arc<BrowserAuth>>,
    headers: HeaderMap,
    query: Result<Query<Callback>, QueryRejection>,
) -> Response {
    let Ok(Query(query)) = query else {
        return api_error(Error::InvalidIdentityEvidence);
    };
    // Never reflect or log codes, provider errors or callback query strings.
    if query.error.is_some()
        || query.error_description.is_some()
        || query
            .iss
            .as_deref()
            .is_some_and(|issuer| issuer != auth.issuer())
        || query.session_state.as_ref().is_some_and(|s| s.len() > 1024)
    {
        return api_error(Error::InvalidIdentityEvidence);
    }
    let Some(code) = query.code else {
        return api_error(Error::InvalidIdentityEvidence);
    };
    let browser = match cookie(&headers, auth.browser_cookie()) {
        Ok(Some(value)) => value,
        _ => return api_error(Error::InvalidIdentityEvidence),
    };
    match auth.complete(browser, &query.state, &code).await {
        Ok(complete) => {
            let mut response = redirect(&complete.location);
            set_cookie(
                &mut response,
                auth.browser_cookie(),
                &complete.browser,
                8 * 60 * 60,
            );
            set_cookie(
                &mut response,
                auth.session_cookie(),
                &complete.session,
                8 * 60 * 60,
            );
            response
        }
        Err(e) => api_error(e),
    }
}

async fn logout(State(auth): State<Arc<BrowserAuth>>, headers: HeaderMap) -> Response {
    if let Err(e) = one_header(&headers, "origin")
        .map_err(|_| Error::Forbidden)
        .and_then(|origin| auth.check_origin(origin))
    {
        return api_error(e);
    }
    let result = async {
        let session =
            cookie(&headers, auth.session_cookie())?.ok_or(Error::InvalidIdentityEvidence)?;
        let browser =
            cookie(&headers, auth.browser_cookie())?.ok_or(Error::InvalidIdentityEvidence)?;
        auth.logout(
            session,
            browser,
            one_header(&headers, "x-csrf-token").map_err(|_| Error::Forbidden)?,
        )
        .await
    }
    .await;
    let mut response = match result {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => api_error(e),
    };
    clear_cookies(&mut response, &auth);
    response
}

async fn signed_out() -> Response {
    let mut response = axum::response::Html(
        "<!doctype html><html lang=\"en\"><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>Signed out · Hermes Manager</title><main><h1>Signed out</h1><p>Your manager session has ended.</p><a href=\"/auth/login\">Sign in with Keycloak</a></main></html>"
    ).into_response();
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'none'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'",
        ),
    );
    response
}

pub fn router(auth: BrowserAuth) -> Router {
    let (login_path, callback_path, logout_path) = match auth.purpose() {
        Purpose::Administration => ("/auth/login", "/auth/oidc/callback", "/auth/logout"),
        Purpose::Router => (
            "/_platform/login",
            "/_platform/oidc/callback",
            "/_platform/logout",
        ),
    };
    let router = Router::new()
        .route(login_path, get(login))
        .route(callback_path, get(callback))
        .route(logout_path, post(logout))
        .with_state(Arc::new(auth.clone()));
    if auth.purpose() == Purpose::Administration {
        router.route("/auth/signed-out", get(signed_out))
    } else {
        router
    }
}
