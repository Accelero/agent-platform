//! The dashboard: startup-loaded, immutable UI assets behind the same
//! administrator authentication as the API.
//!
//! Only `index.html` and flat JS/CSS files under `assets/` are accepted.
//! Requests never touch the filesystem. Only the navigation routes listed in
//! [`PAGES`] receive the HTML shell; every other path stays a JSON 404.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use hermes_core::app::error::AppError;
use hermes_core::domain::ErrorKind;
use hermes_identity::crypto::random_handle;

use super::auth::authenticate;
use super::{AdminState, AdminStore};
use crate::cookies::cookie;
use crate::error::error_response;

const MAX_FILES: usize = 32;
const MAX_FILE: u64 = 4 * 1024 * 1024;
const MAX_TOTAL: usize = 16 * 1024 * 1024;

/// Client-side routes served with the HTML shell. `{id}` segments are checked
/// by [`valid_id`]. A new dashboard route needs an entry here.
pub const PAGES: &[&str] = &[
    "/",
    "/users",
    "/users/{id}",
    "/assignments",
    "/assignments/{id}",
    "/templates",
    "/templates/{id}",
    "/terminal-backends",
    "/terminal-backends/{id}",
    "/storage",
    "/secrets",
    "/activity",
    "/activity/{id}",
    "/administrators",
    "/administrators/{id}",
];

const NOT_FOUND: AppError = AppError::new(ErrorKind::NotFound, "not_found");

#[derive(Clone)]
pub struct DashboardAssets {
    files: Arc<BTreeMap<String, (HeaderValue, Bytes)>>,
}

impl DashboardAssets {
    pub async fn load(directory: &Path) -> Result<Self, AppError> {
        let mut paths = vec![(
            "/".to_owned(),
            directory.join("index.html"),
            "text/html; charset=utf-8",
        )];
        let mut entries = tokio::fs::read_dir(directory.join("assets"))
            .await
            .map_err(|_| AppError::invalid())?;
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|_| AppError::invalid())?
        {
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| AppError::invalid())?;
            let mime = match Path::new(&name).extension().and_then(|s| s.to_str()) {
                Some("js") => "text/javascript; charset=utf-8",
                Some("css") => "text/css; charset=utf-8",
                _ => return Err(AppError::invalid()),
            };
            let valid_name = name.len() <= 160
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b));
            if !valid_name || paths.len() >= MAX_FILES {
                return Err(AppError::invalid());
            }
            paths.push((format!("/assets/{name}"), entry.path(), mime));
        }
        if paths.len() < 2 {
            return Err(AppError::invalid());
        }
        let mut files = BTreeMap::new();
        let mut total = 0usize;
        for (url, path, mime) in paths {
            let metadata = tokio::fs::symlink_metadata(&path)
                .await
                .map_err(|_| AppError::invalid())?;
            if !metadata.is_file() || metadata.len() > MAX_FILE {
                return Err(AppError::invalid());
            }
            let content = tokio::fs::read(path)
                .await
                .map_err(|_| AppError::invalid())?;
            total += content.len();
            if total > MAX_TOTAL {
                return Err(AppError::invalid());
            }
            files.insert(url, (HeaderValue::from_static(mime), Bytes::from(content)));
        }
        Ok(Self {
            files: Arc::new(files),
        })
    }
}

/// Identifier segments of page routes: UUIDs, or catalog names for
/// templates and terminal backends.
fn valid_id(prefix: &str, id: &str) -> bool {
    if matches!(prefix, "/templates" | "/terminal-backends") {
        !id.is_empty()
            && id.len() <= 63
            && id.as_bytes()[0].is_ascii_lowercase()
            && id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    } else {
        uuid::Uuid::parse_str(id).is_ok()
    }
}

async fn asset(State(assets): State<DashboardAssets>, request: Request) -> Response {
    let path = request.uri().path();
    if let Some((prefix, id)) = path.rsplit_once('/')
        && !prefix.is_empty()
        && PAGES.contains(&format!("{prefix}/{{id}}").as_str())
        && !valid_id(prefix, id)
    {
        return error_response(NOT_FOUND);
    }
    let key = if path.starts_with("/assets/") {
        path
    } else {
        "/"
    };
    let Some((mime, bytes)) = assets.files.get(key) else {
        return error_response(NOT_FOUND);
    };
    let Ok(nonce) = random_handle() else {
        return error_response(AppError::unavailable());
    };
    let content = if key == "/" {
        let Ok(html) = std::str::from_utf8(bytes) else {
            return error_response(AppError::invalid());
        };
        Bytes::from(html.replace("__MANAGER_STYLE_NONCE__", &nonce))
    } else {
        bytes.clone()
    };
    let policy = format!(
        "default-src 'none'; script-src 'self'; style-src 'self' 'nonce-{nonce}'; worker-src 'self'; \
         img-src 'self' data:; connect-src 'self'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'"
    );
    let mut response = content.into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, mime.clone());
    if let Ok(policy) = HeaderValue::from_str(&policy) {
        response
            .headers_mut()
            .insert(header::CONTENT_SECURITY_POLICY, policy);
    }
    response
}

/// Page navigations without credentials go to login. Assets and invalid
/// credentials keep their status; nothing turns into an HTML shell unauthenticated.
async fn navigate<S: AdminStore>(
    State(state): State<Arc<AdminState<S>>>,
    request: Request,
    next: Next,
) -> Response {
    let page = !request.uri().path().starts_with("/assets/")
        && !request.headers().contains_key(header::AUTHORIZATION);
    if page && let Some(browser) = &state.browser {
        match cookie(request.headers(), browser.session_cookie()) {
            Ok(None) => {
                let mut url =
                    url::Url::parse("https://unused.invalid/auth/login").expect("static URL");
                url.query_pairs_mut()
                    .append_pair("returnTo", request.uri().path());
                let location = &url[url::Position::BeforePath..];
                return (StatusCode::SEE_OTHER, [(header::LOCATION, location)]).into_response();
            }
            Ok(Some(_)) => {}
            Err(error) => return error_response(error),
        }
    }
    authenticate(State(state), request, next).await
}

pub fn router<S: AdminStore>(assets: DashboardAssets, state: Arc<AdminState<S>>) -> Router {
    let mut router = Router::new();
    for page in PAGES {
        router = router.route(page, get(asset));
    }
    router
        .route("/assets/{*path}", get(asset))
        .route_layer(middleware::from_fn_with_state(state, navigate::<S>))
        .with_state(assets)
}
