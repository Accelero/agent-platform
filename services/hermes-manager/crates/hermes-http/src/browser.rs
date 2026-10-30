//! Login, callback and logout routes for one [`BrowserAuth`]: `/auth/*` for
//! the administration dashboard, `/_platform/*` for the gateway.

use std::sync::Arc;

use axum::Router;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use hermes_core::app::error::AppError;
use hermes_identity::AuthError;
use hermes_identity::browser::BrowserAuth;
use hermes_identity::crypto::Purpose;
use serde::Deserialize;

use crate::cookies::{cookie, one_header, set_cookie};
use crate::error::{ApiError, error_response};

/// Browser-proof and session cookies last as long as a session.
const COOKIE_SECONDS: u32 = 8 * 60 * 60;

pub fn router(auth: BrowserAuth) -> Router {
    let (login_path, callback_path, logout_path) = BrowserAuth::paths(auth.purpose());
    let purpose = auth.purpose();
    let router = Router::new()
        .route(login_path, get(login))
        .route(callback_path, get(callback))
        .route(logout_path, post(logout))
        .with_state(Arc::new(auth));
    match purpose {
        Purpose::Administration => router.route("/auth/signed-out", get(signed_out)),
        Purpose::Gateway => router,
    }
}

pub fn clear_cookies(response: &mut Response, auth: &BrowserAuth) {
    set_cookie(response, auth.session_cookie(), "", 0);
    set_cookie(response, auth.browser_cookie(), "", 0);
}

fn redirect(location: &str) -> Response {
    match HeaderValue::from_str(location) {
        Ok(location) => (StatusCode::SEE_OTHER, [(header::LOCATION, location)]).into_response(),
        Err(_) => error_response(AppError::invalid()),
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
) -> Result<Response, ApiError> {
    let Query(query) = query.map_err(|_| AppError::invalid())?;
    let browser = cookie(&headers, auth.browser_cookie())?;
    let start = auth
        .begin(browser, query.return_to.as_deref().unwrap_or("/"))
        .await?;
    let mut response = redirect(&start.location);
    set_cookie(
        &mut response,
        auth.browser_cookie(),
        &start.browser,
        COOKIE_SECONDS,
    );
    set_cookie(&mut response, auth.session_cookie(), "", 0);
    Ok(response)
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
) -> Result<Response, ApiError> {
    // Codes, provider errors and query strings are never reflected or logged.
    let Query(query) = query.map_err(|_| AuthError::Evidence)?;
    let refused = query.error.is_some()
        || query.error_description.is_some()
        || query
            .iss
            .as_deref()
            .is_some_and(|issuer| issuer != auth.issuer())
        || query.session_state.as_ref().is_some_and(|s| s.len() > 1024);
    if refused {
        return Err(AuthError::Evidence.into());
    }
    let code = query.code.ok_or(AuthError::Evidence)?;
    let browser = cookie(&headers, auth.browser_cookie())?.ok_or(AuthError::Evidence)?;
    let complete = auth.complete(browser, &query.state, &code).await?;
    let mut response = redirect(&complete.location);
    set_cookie(
        &mut response,
        auth.browser_cookie(),
        &complete.browser,
        COOKIE_SECONDS,
    );
    set_cookie(
        &mut response,
        auth.session_cookie(),
        &complete.session,
        COOKIE_SECONDS,
    );
    Ok(response)
}

async fn logout(State(auth): State<Arc<BrowserAuth>>, headers: HeaderMap) -> Response {
    let result: Result<(), AppError> = async {
        let origin = one_header(&headers, "origin").map_err(|_| AppError::forbidden())?;
        auth.check_origin(origin)?;
        let session = cookie(&headers, auth.session_cookie())?.ok_or(AuthError::Evidence)?;
        let browser = cookie(&headers, auth.browser_cookie())?.ok_or(AuthError::Evidence)?;
        let csrf = one_header(&headers, "x-csrf-token").map_err(|_| AppError::forbidden())?;
        auth.logout(session, browser, csrf).await?;
        Ok(())
    }
    .await;
    let mut response = match result {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => error_response(error),
    };
    // A refusal of the Origin or CSRF check changes nothing, so that a page of
    // another origin cannot end the administrator's session in their browser.
    // Any other outcome, a session that is already gone included, clears the
    // cookies.
    if response.status() != StatusCode::FORBIDDEN {
        clear_cookies(&mut response, &auth);
    }
    response
}

async fn signed_out() -> Response {
    let mut response = Html(
        "<!doctype html><html lang=\"en\"><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <title>Signed out · Hermes Manager</title><main><h1>Signed out</h1>\
         <p>Your manager session has ended.</p><a href=\"/auth/login\">Sign in with Keycloak</a></main></html>",
    )
    .into_response();
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'none'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'",
        ),
    );
    response
}
