//! Administrator authentication for every `/api/v1` request and dashboard page.
//!
//! A request carries either a bearer access token or the browser session
//! cookies, never both. Browser mutations also need the exact `Origin` and the
//! session's CSRF token. Then the identity's *current* PostgreSQL grant must
//! permit administration; token roles grant nothing.

use std::sync::Arc;

use async_trait::async_trait;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, Method, header};
use axum::middleware::Next;
use axum::response::Response;
use hermes_core::app::access;
use hermes_core::app::actor::Actor;
use hermes_core::app::error::AppError;
use hermes_core::app::ports::{AccessReader, Store};
use hermes_core::domain::ErrorKind;
use hermes_core::domain::access::Grant;
use hermes_core::domain::identity::Identity;
use hermes_core::domain::installation::Installation;
use hermes_identity::AuthError;
use hermes_identity::browser::{Admission, BrowserAuth};

use super::{AdminState, AdminStore};
use crate::cookies::{cookie, one_header};
use crate::error::error_response;

/// The authenticated administrator, inserted into request extensions.
#[derive(Debug, Clone)]
pub struct Authenticated {
    pub actor: Actor,
    pub identity: Identity,
    pub grant: Grant,
    /// The browser session's CSRF token; `None` for bearer requests.
    pub csrf_token: Option<String>,
}

/// Admission for administrator browser sessions: records the login and
/// requires an active grant, at login and on every provider refresh.
pub struct AdministratorAdmission<R> {
    pub reader: R,
    pub installation: Installation,
}

#[async_trait]
impl<R: AccessReader> Admission for AdministratorAdmission<R> {
    async fn admit(&self, identity: &Identity) -> Result<(), AuthError> {
        match access::admit(&self.reader, &self.installation, identity).await {
            Ok(_) => Ok(()),
            Err(error) if error.kind == ErrorKind::Forbidden => Err(AuthError::Forbidden),
            Err(_) => Err(AuthError::Unavailable),
        }
    }
}

fn bearer(headers: &HeaderMap) -> Result<&str, AppError> {
    let value = one_header(headers, "authorization").map_err(|_| AuthError::Evidence)?;
    value
        .strip_prefix("Bearer ")
        .filter(|token| {
            !token.is_empty()
                && token.len() <= 16 * 1024
                && token.bytes().all(|b| b.is_ascii_graphic())
        })
        .ok_or_else(|| AuthError::Evidence.into())
}

fn safe_method(method: &Method) -> bool {
    matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

/// The verified identity and, for browser sessions, its CSRF token.
async fn verify(
    state: &AdminState<impl Store>,
    headers: &HeaderMap,
    method: &Method,
) -> Result<(Identity, Option<String>), AppError> {
    let session = match &state.browser {
        Some(browser) => cookie(headers, browser.session_cookie())?,
        None => None,
    };
    if headers.contains_key(header::AUTHORIZATION) {
        if session.is_some() {
            return Err(AuthError::Evidence.into());
        }
        let verified = state.tokens.verify(bearer(headers)?).await?;
        return Ok((verified.identity(), None));
    }
    let browser = state.browser.as_ref().ok_or(AuthError::Evidence)?;
    let session = session.ok_or(AuthError::Evidence)?;
    let proof = cookie(headers, browser.browser_cookie())?.ok_or(AuthError::Evidence)?;
    let verified = browser.authenticate(session, proof).await?;
    if !safe_method(method) {
        let origin = one_header(headers, "origin").map_err(|_| AppError::forbidden())?;
        browser.check_origin(origin)?;
        let csrf = one_header(headers, "x-csrf-token").map_err(|_| AppError::forbidden())?;
        BrowserAuth::check_csrf(&verified.csrf_token, csrf)?;
    }
    Ok((verified.identity, Some(verified.csrf_token)))
}

pub async fn authenticate<S: AdminStore>(
    State(state): State<Arc<AdminState<S>>>,
    mut request: Request,
    next: Next,
) -> Response {
    // Owned copies: a borrowed request is not `Send` across the awaits below.
    let (headers, method) = (request.headers().clone(), request.method().clone());
    let result = async {
        let (identity, csrf_token) = verify(&state, &headers, &method).await?;
        let grant = access::admit(&state.store, state.installation(), &identity).await?;
        Ok::<_, AppError>(Authenticated {
            actor: Actor::Administrator(identity.clone()),
            identity,
            grant,
            csrf_token,
        })
    }
    .await;
    match result {
        Ok(authenticated) => {
            request.extensions_mut().insert(authenticated);
            next.run(request).await
        }
        Err(error) => error_response(error),
    }
}
