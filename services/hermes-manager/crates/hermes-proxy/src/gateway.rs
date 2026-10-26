//! Request handling on the Hermes hostname (`gateway.openapi.yaml`): every
//! request outside `/_platform/*` needs a platform session whose identity
//! resolves to a ready route, and goes only to that route's instance.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, Version, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use hermes_core::domain::routing::{Destination, Route};
use hermes_identity::AuthError;
use hermes_identity::crypto::hash;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::{TokioExecutor, TokioIo};
use serde_json::json;

use crate::cache::TtlCache;
use crate::hygiene::{
    expire_cookies, filter_response, forward_request, is_websocket, platform_cookies,
};
use crate::pages::{Page, error, own_headers};
use crate::resolver::Verdict;
use crate::sessions::SignedIn;
use crate::supervise::{Checks, Revocable, splice};

/// Uploads are limited to this size unless a deployment lowers it.
pub const MAX_UPLOAD: u64 = 64 * 1024 * 1024;
/// Waiting for an instance's response headers.
const RESPONSE_DEADLINE: Duration = Duration::from_secs(120);
const LOGOUT_DEADLINE: Duration = Duration::from_secs(2);
const CACHE_CAPACITY: usize = 10_000;

/// Maps a destination to the `host:port` to connect to.
pub type Dial = Arc<dyn Fn(&Destination) -> String + Send + Sync>;

/// `<service>.<namespace>.svc.<cluster domain>:<port>`.
pub fn cluster_dial(cluster_domain: String) -> Dial {
    Arc::new(move |destination: &Destination| {
        format!(
            "{}.{}.svc.{cluster_domain}:{}",
            destination.service, destination.namespace, destination.port
        )
    })
}

pub struct Gateway {
    checks: Checks,
    routes: TtlCache<Route>,
    signed: TtlCache<SignedIn>,
    client: Client<HttpConnector, Body>,
    dial: Dial,
    public_origin: String,
    public_host: String,
    max_upload: u64,
}

impl Gateway {
    /// `public_origin` is the Hermes hostname's origin, such as
    /// `https://hermes.example.org`.
    pub fn new(
        checks: Checks,
        dial: Dial,
        public_origin: &str,
        max_upload: u64,
    ) -> Result<Self, AuthError> {
        let url = url::Url::parse(public_origin).map_err(|_| AuthError::Configuration)?;
        let host = url.host_str().ok_or(AuthError::Configuration)?;
        let public_host = match url.port() {
            Some(port) => format!("{host}:{port}"),
            None => host.to_owned(),
        };
        if url.scheme() != "https"
            || url.origin().ascii_serialization() != public_origin
            || max_upload == 0
            || max_upload > MAX_UPLOAD
        {
            return Err(AuthError::Configuration);
        }
        let mut connector = HttpConnector::new();
        connector.set_connect_timeout(Some(Duration::from_secs(2)));
        connector.enforce_http(true);
        let client = Client::builder(TokioExecutor::new())
            .pool_idle_timeout(Duration::from_secs(30))
            .build(connector);
        let ttl = checks.timing.cache;
        Ok(Self {
            checks,
            routes: TtlCache::new(ttl, CACHE_CAPACITY),
            signed: TtlCache::new(ttl, CACHE_CAPACITY),
            client,
            dial,
            public_origin: public_origin.to_owned(),
            public_host,
            max_upload,
        })
    }

    /// The request's session, from a decision at most one cache lifetime old.
    async fn signed_in(
        &self,
        headers: &HeaderMap,
    ) -> Result<(SignedIn, String, String), AuthError> {
        let (session, browser) = platform_cookies(headers)
            .map_err(|_| AuthError::Evidence)?
            .ok_or(AuthError::Evidence)?;
        let key = session_key(&session, &browser);
        if let Some(signed) = self.signed.get(&key) {
            return Ok((signed, session, browser));
        }
        let signed = self
            .checks
            .sessions
            .authenticate(&session, &browser)
            .await?;
        self.signed.put(&key, signed.clone());
        Ok((signed, session, browser))
    }

    async fn route(&self, signed: &SignedIn) -> Verdict {
        if let Some(route) = self.routes.get(&signed.handle) {
            return Verdict::Ready(route);
        }
        let verdict = self.checks.resolver.resolve(&signed.identity).await;
        match &verdict {
            Verdict::Ready(route) => self.routes.put(&signed.handle, route.clone()),
            Verdict::Pending(_) | Verdict::Denied | Verdict::Unavailable => {
                self.routes.remove(&signed.handle);
            }
        }
        verdict
    }

    /// Forgets the session these cookies name, if it is cached.
    fn forget_cookies(&self, headers: &HeaderMap) {
        if let Ok(Some((session, browser))) = platform_cookies(headers)
            && let Some(signed) = self.signed.get(&session_key(&session, &browser))
        {
            self.forget(&signed.handle);
        }
    }

    fn forget(&self, handle: &str) {
        self.routes.remove(handle);
        self.signed.remove_where(|signed| signed.handle == handle);
    }

    fn same_origin(&self, headers: &HeaderMap) -> bool {
        let mut origins = headers.get_all(header::ORIGIN).iter();
        matches!(
            (origins.next(), origins.next()),
            (Some(origin), None) if origin.as_bytes() == self.public_origin.as_bytes()
        )
    }

    fn upstream_request(
        &self,
        route: &Route,
        method: Method,
        uri: &Uri,
        headers: &HeaderMap,
        websocket: bool,
        body: Body,
    ) -> Option<Request> {
        let path = uri.path_and_query().map_or("/", |p| p.as_str());
        let target = format!("http://{}{path}", (self.dial)(&route.destination));
        let mut request = Request::builder()
            .method(method)
            .uri(target)
            .version(Version::HTTP_11)
            .body(body)
            .ok()?;
        *request.headers_mut() = forward_request(headers, &self.public_host, websocket);
        Some(request)
    }
}

/// The gateway: `platform` (the gateway's own routes, with their bounds
/// already applied) and the proxy for every other request.
pub fn router(gateway: Arc<Gateway>, platform: Router) -> Router {
    platform.fallback_service(axum::routing::any(proxy).with_state(gateway))
}

/// Platform routes the gateway answers itself. The binary adds the login
/// routes and the request bounds.
pub fn platform(gateway: Arc<Gateway>) -> Router {
    Router::new()
        .route("/_platform/me", get(me))
        .route("/_platform/signed-out", get(signed_out))
        .route("/_platform/healthz", get(healthz))
        .route("/auth/logout", post(native_logout).fallback(proxy))
        .with_state(gateway)
}

/// Cache key of a cookie pair; the cookie values themselves are not kept.
fn session_key(session: &str, browser: &str) -> String {
    hash(&format!("{session}\0{browser}"))
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// On `/_platform/logout`: forgets the ended session at once and expires
/// native cookies, whatever the outcome.
pub async fn expire_on_logout(
    State(gateway): State<Arc<Gateway>>,
    request: Request,
    next: Next,
) -> Response {
    let logout = request.method() == Method::POST && request.uri().path() == "/_platform/logout";
    let headers = logout.then(|| request.headers().clone());
    let mut response = next.run(request).await;
    if let Some(headers) = headers {
        if response.status() == StatusCode::NO_CONTENT {
            gateway.forget_cookies(&headers);
        }
        expire_cookies(response.headers_mut());
    }
    response
}

async fn healthz() -> Response {
    let mut response = "ok".into_response();
    own_headers(&mut response);
    response
}

async fn signed_out() -> Response {
    Page::SignedOut.respond(true)
}

fn auth_failure(error: AuthError) -> Response {
    match error {
        AuthError::Unavailable => error_response(StatusCode::SERVICE_UNAVAILABLE),
        AuthError::Evidence | AuthError::Forbidden | AuthError::Configuration => {
            error_response(StatusCode::UNAUTHORIZED)
        }
    }
}

fn error_response(status: StatusCode) -> Response {
    let code = match status {
        StatusCode::UNAUTHORIZED => "invalid_identity_evidence",
        StatusCode::FORBIDDEN => "access_denied",
        StatusCode::SERVICE_UNAVAILABLE => "dependency_unavailable",
        _ => "invalid_request",
    };
    error(status, code)
}

async fn me(State(gateway): State<Arc<Gateway>>, headers: HeaderMap) -> Response {
    match gateway.signed_in(&headers).await {
        Ok((signed, _, _)) => {
            let mut response = axum::Json(json!({
                "identity": signed.identity,
                "csrfToken": signed.csrf,
            }))
            .into_response();
            own_headers(&mut response);
            response
        }
        Err(error) => auth_failure(error),
    }
}

/// Native logout: forwarded best-effort so the instance can revoke its own
/// tokens, then the platform session ends and every cookie is expired.
async fn native_logout(State(gateway): State<Arc<Gateway>>, request: Request) -> Response {
    if !gateway.same_origin(request.headers()) {
        return error_response(StatusCode::FORBIDDEN);
    }
    if let Ok((signed, session, browser)) = gateway.signed_in(request.headers()).await {
        if let Verdict::Ready(route) = gateway.route(&signed).await
            && let Some(upstream) = gateway.upstream_request(
                &route,
                Method::POST,
                request.uri(),
                request.headers(),
                false,
                Body::empty(),
            )
        {
            let _ = tokio::time::timeout(LOGOUT_DEADLINE, gateway.client.request(upstream)).await;
        }
        let _ = gateway
            .checks
            .sessions
            .logout(&session, &browser, &signed.csrf)
            .await;
        gateway.forget(&signed.handle);
    }
    let mut response = (
        StatusCode::SEE_OTHER,
        [(
            header::LOCATION,
            HeaderValue::from_static("/_platform/signed-out"),
        )],
    )
        .into_response();
    expire_cookies(response.headers_mut());
    own_headers(&mut response);
    response
}

/// A `GET` or `HEAD` that accepts HTML and is not a WebSocket.
fn navigation(request: &Request) -> bool {
    matches!(*request.method(), Method::GET | Method::HEAD)
        && !is_websocket(request.headers())
        && request
            .headers()
            .get_all(header::ACCEPT)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .any(|value| value.contains("text/html"))
}

/// A local path to return to after login, or `/`.
fn return_path(uri: &Uri) -> String {
    let path = uri.path_and_query().map_or("/", |p| p.as_str());
    let safe = path.len() <= 2048
        && path.starts_with('/')
        && !path.starts_with("//")
        && !path.contains('\\')
        && !path.chars().any(char::is_control)
        && !["/_platform", "/auth", "/api/"]
            .iter()
            .any(|prefix| path.starts_with(prefix));
    if safe {
        path.to_owned()
    } else {
        "/".to_owned()
    }
}

fn sign_in(uri: &Uri) -> Response {
    let target: String =
        url::form_urlencoded::byte_serialize(return_path(uri).as_bytes()).collect();
    let location = format!("/_platform/login?returnTo={target}");
    let mut response = match HeaderValue::from_str(&location) {
        Ok(location) => (StatusCode::SEE_OTHER, [(header::LOCATION, location)]).into_response(),
        Err(_) => error_response(StatusCode::UNAUTHORIZED),
    };
    own_headers(&mut response);
    response
}

/// Every request outside the platform routes.
pub async fn proxy(State(gateway): State<Arc<Gateway>>, mut request: Request) -> Response {
    let navigation = navigation(&request);
    let websocket = is_websocket(request.headers());
    let signed = match gateway.signed_in(request.headers()).await {
        Ok((signed, _, _)) => signed,
        Err(AuthError::Unavailable) => return Page::Unavailable.respond(navigation),
        Err(_) if navigation => return sign_in(request.uri()),
        Err(error) => return auth_failure(error),
    };
    if websocket && !gateway.same_origin(request.headers()) {
        return error_response(StatusCode::FORBIDDEN);
    }
    let route = match gateway.route(&signed).await {
        Verdict::Ready(route) => route,
        Verdict::Pending(reason) => return Page::Pending(reason).respond(navigation),
        Verdict::Denied => return Page::Denied.respond(navigation),
        Verdict::Unavailable => return Page::Unavailable.respond(navigation),
    };
    let too_large = request
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .parse::<u64>()
                .map_or(true, |n| n > gateway.max_upload)
        });
    if too_large {
        return error(StatusCode::PAYLOAD_TOO_LARGE, "invalid_request");
    }
    let client_upgrade = if websocket {
        match request
            .extensions_mut()
            .remove::<hyper::upgrade::OnUpgrade>()
        {
            Some(upgrade) => Some(upgrade),
            None => return error_response(StatusCode::BAD_REQUEST),
        }
    } else {
        None
    };
    let method = request.method().clone();
    let uri = request.uri().clone();
    let headers = std::mem::take(request.headers_mut());
    let body = if websocket {
        Body::empty()
    } else {
        Body::new(http_body_util::Limited::new(
            request.into_body(),
            gateway.max_upload as usize,
        ))
    };
    let Some(upstream) =
        gateway.upstream_request(&route, method.clone(), &uri, &headers, websocket, body)
    else {
        return error_response(StatusCode::BAD_REQUEST);
    };
    let mut response =
        match tokio::time::timeout(RESPONSE_DEADLINE, gateway.client.request(upstream)).await {
            Ok(Ok(response)) => response,
            _ => return Page::BadGateway.respond(navigation),
        };
    if let Some(client_upgrade) = client_upgrade
        && response.status() == StatusCode::SWITCHING_PROTOCOLS
    {
        let upstream_upgrade = hyper::upgrade::on(&mut response);
        let supervisor = gateway.checks.spawn(signed.handle.clone(), route);
        let watcher = gateway.clone();
        tokio::spawn(async move {
            let (Ok(client), Ok(upstream)) = tokio::join!(client_upgrade, upstream_upgrade) else {
                return;
            };
            if splice(TokioIo::new(client), TokioIo::new(upstream), supervisor)
                .await
                .is_some()
            {
                watcher.forget(&signed.handle);
            }
        });
        let (mut parts, _) = response.into_parts();
        filter_response(&mut parts.headers, true);
        return Response::from_parts(parts, Body::empty());
    }
    let (mut parts, body) = response.into_parts();
    filter_response(&mut parts.headers, false);
    let streamed = !parts.headers.contains_key(header::CONTENT_LENGTH)
        && method != Method::HEAD
        && !matches!(
            parts.status,
            StatusCode::NO_CONTENT | StatusCode::NOT_MODIFIED
        )
        && !parts.status.is_informational();
    let body = Body::new(body);
    let body = if streamed {
        Revocable::wrap(body, gateway.checks.spawn(signed.handle, route))
    } else {
        body
    };
    Response::from_parts(parts, body)
}
