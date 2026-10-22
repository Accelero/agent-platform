//! Immutable startup-loaded UI assets on the existing administration boundary.
use super::*;
use std::{collections::BTreeMap, path::Path, sync::Arc};

#[derive(Clone)]
pub struct DashboardAssets {
    files: Arc<BTreeMap<String, (HeaderValue, Bytes)>>,
}
impl DashboardAssets {
    /// Only the Vite HTML entry and flat JS/CSS assets are accepted. Requests
    /// never access the filesystem; unknown API/auth/internal paths stay JSON.
    pub async fn load(directory: &Path) -> Result<Self, Error> {
        let mut files = BTreeMap::new();
        let mut total = 0usize;
        let mut paths = vec![(
            "/".to_owned(),
            directory.join("index.html"),
            "text/html; charset=utf-8",
        )];
        let mut entries = tokio::fs::read_dir(directory.join("assets"))
            .await
            .map_err(|_| Error::Invalid)?;
        while let Some(entry) = entries.next_entry().await.map_err(|_| Error::Invalid)? {
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| Error::Invalid)?;
            let mime = match Path::new(&name).extension().and_then(|s| s.to_str()) {
                Some("js") => "text/javascript; charset=utf-8",
                Some("css") => "text/css; charset=utf-8",
                _ => return Err(Error::Invalid),
            };
            if name.len() > 160
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
                || paths.len() >= 32
            {
                return Err(Error::Invalid);
            }
            paths.push((format!("/assets/{name}"), entry.path(), mime));
        }
        if paths.len() < 2 {
            return Err(Error::Invalid);
        }
        for (url, path, mime) in paths {
            let metadata = tokio::fs::symlink_metadata(&path)
                .await
                .map_err(|_| Error::Invalid)?;
            if !metadata.is_file() || metadata.len() > 4 * 1024 * 1024 {
                return Err(Error::Invalid);
            }
            let content = tokio::fs::read(path).await.map_err(|_| Error::Invalid)?;
            total += content.len();
            if total > 16 * 1024 * 1024 {
                return Err(Error::Invalid);
            }
            files.insert(url, (HeaderValue::from_static(mime), Bytes::from(content)));
        }
        Ok(Self {
            files: Arc::new(files),
        })
    }
}

async fn asset(State(assets): State<DashboardAssets>, request: Request) -> Response {
    let path = request.uri().path();
    if let Some((prefix, id)) = path.rsplit_once('/')
        && matches!(
            prefix,
            "/users"
                | "/operations"
                | "/activity"
                | "/assignments"
                | "/storage-bindings"
                | "/terminal-runtimes"
                | "/templates"
                | "/template-drafts"
                | "/administrators"
                | "/template-batches"
        )
    {
        let valid = if matches!(prefix, "/terminal-runtimes" | "/templates") {
            !id.is_empty()
                && id.len() <= 63
                && id.as_bytes()[0].is_ascii_lowercase()
                && id
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        } else {
            Uuid::parse_str(id).is_ok()
        };
        if !valid {
            return api_error(Error::NotFound);
        }
    }
    let key = if path.starts_with("/assets/") {
        path
    } else {
        "/"
    };
    let Some((mime, bytes)) = assets.files.get(key) else {
        return api_error(Error::NotFound);
    };
    let nonce = match crate::session_crypto::random_handle() {
        Ok(nonce) => nonce,
        Err(error) => return api_error(error),
    };
    let content = if mime
        .to_str()
        .is_ok_and(|value| value.starts_with("text/html"))
    {
        let Ok(html) = std::str::from_utf8(bytes) else {
            return api_error(Error::Invalid);
        };
        Bytes::from(html.replace("__MANAGER_STYLE_NONCE__", &nonce))
    } else {
        bytes.clone()
    };
    let mut result = content.into_response();
    result
        .headers_mut()
        .insert(header::CONTENT_TYPE, mime.clone());
    let policy = format!(
        "default-src 'none'; script-src 'self'; style-src 'self' 'nonce-{nonce}'; worker-src 'self'; img-src 'self' data:; connect-src 'self'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'"
    );
    let Ok(policy) = HeaderValue::from_str(&policy) else {
        return api_error(Error::Invalid);
    };
    result
        .headers_mut()
        .insert(header::CONTENT_SECURITY_POLICY, policy);
    result
}

async fn authorize(State(auth): State<AdminAuth>, request: Request, next: Next) -> Response {
    // Only unauthenticated page navigation redirects. Assets and API errors
    // retain their status; invalid credentials never turn into SPA HTML.
    if !request.uri().path().starts_with("/assets/")
        && !request.headers().contains_key(header::AUTHORIZATION)
    {
        let Some(browser) = auth.browser.as_ref() else {
            return api_error(Error::Invalid);
        };
        match browser_http::cookie(request.headers(), browser.session_cookie()) {
            Ok(None) => {
                let mut url =
                    url::Url::parse("https://unused.invalid/auth/login").expect("static URL");
                url.query_pairs_mut()
                    .append_pair("returnTo", request.uri().path());
                let location = &url[url::Position::BeforePath..];
                return (StatusCode::SEE_OTHER, [(header::LOCATION, location)]).into_response();
            }
            Ok(Some(_)) => {}
            Err(error) => return api_error(error),
        }
    }
    admin_auth(State(auth), request, next).await
}

pub(super) fn router(assets: DashboardAssets, auth: AdminAuth) -> Router {
    Router::new()
        .route("/", get(asset))
        .route("/assignments", get(asset))
        .route("/assignments/{id}", get(asset))
        .route("/users", get(asset))
        .route("/users/{id}", get(asset))
        .route("/operations/{id}", get(asset))
        .route("/activity/{id}", get(asset))
        .route("/administrators", get(asset))
        .route("/administrators/{id}", get(asset))
        .route("/template-batches/{id}", get(asset))
        .route("/templates", get(asset))
        .route("/templates/{id}", get(asset))
        .route("/template-drafts/{id}", get(asset))
        .route("/storage-bindings", get(asset))
        .route("/storage-bindings/{id}", get(asset))
        .route("/terminal-runtimes", get(asset))
        .route("/terminal-runtimes/{id}", get(asset))
        .route("/activity", get(asset))
        .route("/assets/{*path}", get(asset))
        .route_layer(middleware::from_fn_with_state(auth, authorize))
        .with_state(assets)
}
