//! The gateway against fake sessions, a fake resolver and two loopback
//! instances. Proves routing by verified identity only, request and response
//! hygiene, status pages, uploads, logout, and the revocation timing of open
//! WebSockets and streams.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use hermes_core::app::testing::{identity, installation};
use hermes_core::domain::identity::Identity;
use hermes_core::domain::routing::{Destination, Route};
use hermes_identity::AuthError;
use hermes_proxy::gateway::{Dial, Gateway, MAX_UPLOAD, platform, router};
use hermes_proxy::resolver::{Resolve, Verdict};
use hermes_proxy::sessions::{Sessions, SignedIn};
use hermes_proxy::supervise::{Checks, Timing};
use hermes_testkit::contract::Contract;
use hermes_wire::routing::PendingCode;
use hyper::body::Frame;
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use uuid::Uuid;

const ORIGIN: &str = "https://hermes.example.org";
const BROWSER: &str = "__Host-hermes-platform-browser";
const SESSION: &str = "__Host-hermes-platform";

fn handle(subject: &str) -> String {
    format!("{subject:0>64}")
}

/// Sessions whose cookie value is the subject's handle.
#[derive(Default)]
struct FakeSessions {
    ended: Mutex<HashSet<String>>,
    unavailable: AtomicBool,
    authentications: AtomicUsize,
    logouts: Mutex<Vec<String>>,
}

impl FakeSessions {
    fn identity(&self, handle: &str) -> Result<Identity, AuthError> {
        if self.unavailable.load(Ordering::SeqCst) {
            return Err(AuthError::Unavailable);
        }
        if self.ended.lock().unwrap().contains(handle) {
            return Err(AuthError::Evidence);
        }
        let subject = handle.trim_start_matches('0');
        if subject.is_empty() || handle.len() != 64 {
            return Err(AuthError::Evidence);
        }
        Ok(identity(subject))
    }
}

#[async_trait]
impl Sessions for FakeSessions {
    async fn authenticate(&self, session: &str, browser: &str) -> Result<SignedIn, AuthError> {
        self.authentications.fetch_add(1, Ordering::SeqCst);
        if browser != "proof" {
            return Err(AuthError::Evidence);
        }
        let identity = self.identity(session)?;
        Ok(SignedIn {
            identity,
            handle: session.to_owned(),
            csrf: "c".repeat(64),
        })
    }

    async fn recheck(&self, handle: &str) -> Result<Identity, AuthError> {
        self.identity(handle)
    }

    async fn logout(&self, session: &str, _: &str, csrf: &str) -> Result<(), AuthError> {
        assert_eq!(csrf, "c".repeat(64));
        self.logouts.lock().unwrap().push(session.to_owned());
        self.ended.lock().unwrap().insert(session.to_owned());
        Ok(())
    }
}

#[derive(Default)]
struct FakeResolver {
    verdicts: Mutex<HashMap<String, Verdict>>,
    calls: AtomicUsize,
    /// The next this many resolutions fail.
    failures: AtomicUsize,
}

#[async_trait]
impl Resolve for FakeResolver {
    async fn resolve(&self, identity: &Identity) -> Verdict {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let failing = self
            .failures
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok();
        if failing {
            return Verdict::Unavailable;
        }
        self.verdicts
            .lock()
            .unwrap()
            .get(&identity.subject)
            .cloned()
            .unwrap_or(Verdict::Denied)
    }
}

/// A stream that yields a chunk every 20 ms, forever.
struct Ticker(tokio::time::Interval);

impl hyper::body::Body for Ticker {
    type Data = Bytes;
    type Error = std::convert::Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        self.0
            .poll_tick(cx)
            .map(|_| Some(Ok(Frame::data(Bytes::from_static(b"data: tick\n\n")))))
    }
}

#[derive(Clone)]
struct Upstream {
    name: &'static str,
    hits: Arc<Mutex<Vec<String>>>,
}

async fn echo(State(upstream): State<Upstream>, request: Request) -> Response {
    let path = request
        .uri()
        .path_and_query()
        .map(|p| p.as_str().to_owned())
        .unwrap_or_default();
    upstream
        .hits
        .lock()
        .unwrap()
        .push(format!("{} {path}", upstream.name));
    let headers: serde_json::Map<String, Value> = request
        .headers()
        .iter()
        .map(|(k, v)| (k.as_str().to_owned(), json!(v.to_str().unwrap_or("?"))))
        .collect();
    let size = http_body_util::BodyExt::collect(request.into_body())
        .await
        .map(|b| b.to_bytes().len())
        .unwrap_or(0);
    let mut response = axum::Json(
        json!({"instance": upstream.name, "path": path, "headers": headers, "bytes": size}),
    )
    .into_response();
    for cookie in [
        "__Host-hermes-platform=forged; Path=/; Secure",
        "__Host-hermes_session_at=native; Path=/; Secure; HttpOnly",
    ] {
        response
            .headers_mut()
            .append(header::SET_COOKIE, HeaderValue::from_static(cookie));
    }
    response
}

async fn stream(State(upstream): State<Upstream>) -> Response {
    upstream
        .hits
        .lock()
        .unwrap()
        .push(format!("{} /events", upstream.name));
    let ticker = Ticker(tokio::time::interval(Duration::from_millis(20)));
    (
        [(header::CONTENT_TYPE, "text/event-stream")],
        Body::new(ticker),
    )
        .into_response()
}

/// Accepts the upgrade and echoes bytes.
async fn websocket(State(upstream): State<Upstream>, mut request: Request) -> Response {
    upstream
        .hits
        .lock()
        .unwrap()
        .push(format!("{} /api/ws", upstream.name));
    let upgrade = request
        .extensions_mut()
        .remove::<hyper::upgrade::OnUpgrade>()
        .expect("upgrade");
    tokio::spawn(async move {
        let upgraded = upgrade.await.expect("upgraded");
        let (mut reader, mut writer) = tokio::io::split(TokioIo::new(upgraded));
        let _ = tokio::io::copy(&mut reader, &mut writer).await;
    });
    Response::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header(header::CONNECTION, "upgrade")
        .header(header::UPGRADE, "websocket")
        .header("sec-websocket-accept", "fixture")
        .body(Body::empty())
        .unwrap()
}

async fn start_upstream(name: &'static str, hits: Arc<Mutex<Vec<String>>>) -> SocketAddr {
    let state = Upstream { name, hits };
    let app = Router::new()
        .route("/events", get(stream))
        .route("/api/ws", get(websocket))
        .route("/auth/logout", post(echo))
        .fallback(any(echo))
        .with_state(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    address
}

struct Fixture {
    url: String,
    address: SocketAddr,
    sessions: Arc<FakeSessions>,
    resolver: Arc<FakeResolver>,
    hits: Arc<Mutex<Vec<String>>>,
    routes: HashMap<&'static str, Route>,
    client: reqwest::Client,
}

fn route() -> Route {
    let instance_id = Uuid::new_v4();
    Route {
        instance_id,
        route_generation: 1,
        authorization_version: 1,
        destination: Destination {
            namespace: installation().instance_namespace(instance_id),
            service: "hermes",
            port: 8080,
        },
    }
}

impl Fixture {
    async fn new(timing: Timing) -> Self {
        let hits = Arc::new(Mutex::new(Vec::new()));
        let mut routes = HashMap::new();
        let mut addresses = HashMap::new();
        for name in ["bob", "carol"] {
            let route = route();
            addresses.insert(
                route.destination.namespace.clone(),
                start_upstream(name, hits.clone()).await,
            );
            routes.insert(name, route);
        }
        // A registered instance whose Pod is gone.
        let down = route();
        addresses.insert(
            down.destination.namespace.clone(),
            "127.0.0.1:9".parse().unwrap(),
        );
        routes.insert("dave", down);
        let dial: Dial = Arc::new(move |destination: &Destination| {
            addresses
                .get(&destination.namespace)
                .map(|a| a.to_string())
                .unwrap_or_else(|| "127.0.0.1:9".into())
        });
        let sessions = Arc::new(FakeSessions::default());
        let resolver = Arc::new(FakeResolver::default());
        for (name, route) in &routes {
            resolver
                .verdicts
                .lock()
                .unwrap()
                .insert((*name).to_owned(), Verdict::Ready(route.clone()));
        }
        let checks = Checks {
            sessions: sessions.clone(),
            resolver: resolver.clone(),
            timing,
        };
        let gateway = Arc::new(Gateway::new(checks, dial, ORIGIN, 1024 * 1024).unwrap());
        let app = router(gateway.clone(), platform(gateway));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            url: format!("http://{address}"),
            address,
            sessions,
            resolver,
            hits,
            routes,
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .no_proxy()
                .build()
                .unwrap(),
        }
    }

    fn cookies(subject: &str) -> String {
        format!("{SESSION}={}; {BROWSER}=proof", handle(subject))
    }

    fn get(&self, subject: Option<&str>, path: &str) -> reqwest::RequestBuilder {
        let request = self.client.get(format!("{}{path}", self.url));
        match subject {
            Some(subject) => request.header("cookie", Self::cookies(subject)),
            None => request,
        }
    }

    fn set(&self, subject: &str, verdict: Verdict) {
        self.resolver
            .verdicts
            .lock()
            .unwrap()
            .insert(subject.to_owned(), verdict);
    }

    fn hits(&self) -> Vec<String> {
        self.hits.lock().unwrap().clone()
    }

    /// Opens a WebSocket through the gateway; returns the raw stream after
    /// the `101` response.
    async fn websocket(&self, subject: &str, origin: &str) -> Result<TcpStream, String> {
        let mut stream = TcpStream::connect(self.address).await.unwrap();
        let request = format!(
            "GET /api/ws?ticket=t HTTP/1.1\r\nHost: hermes.example.org\r\nOrigin: {origin}\r\n\
             Connection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\n\
             Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nCookie: {}\r\n\r\n",
            Self::cookies(subject)
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            if stream.read(&mut byte).await.unwrap() == 0 {
                break;
            }
            head.push(byte[0]);
        }
        let head = String::from_utf8_lossy(&head).to_string();
        if head.starts_with("HTTP/1.1 101") {
            assert!(!head.to_ascii_lowercase().contains("hermes-platform"));
            Ok(stream)
        } else {
            Err(head)
        }
    }
}

fn fast() -> Timing {
    Timing {
        recheck: Duration::from_millis(150),
        retry: Duration::from_millis(50),
        deadline: Duration::from_millis(100),
        cache: Duration::from_millis(100),
    }
}

async fn json(response: reqwest::Response) -> Value {
    response.json().await.unwrap()
}

#[tokio::test]
async fn each_session_reaches_only_its_own_instance_with_forged_headers_removed() {
    let fixture = Fixture::new(Timing::default()).await;
    for subject in ["bob", "carol"] {
        let response = fixture
            .get(Some(subject), "/api/chat?x=1")
            .header("x-hermes-verified-subject", "carol")
            .header("x-forwarded-host", "kube-system")
            .header("x-forwarded-prefix", "/other")
            .header("forwarded", "for=1.2.3.4")
            .header("authorization", "Bearer native")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let cookies: Vec<_> = response
            .headers()
            .get_all("set-cookie")
            .iter()
            .map(|v| v.to_str().unwrap().to_owned())
            .collect();
        assert_eq!(
            cookies,
            ["__Host-hermes_session_at=native; Path=/; Secure; HttpOnly"],
            "an instance cannot set a platform cookie"
        );
        let body = json(response).await;
        assert_eq!(body["instance"], subject);
        assert_eq!(body["path"], "/api/chat?x=1");
        let headers = body["headers"].as_object().unwrap();
        assert_eq!(headers["host"], "hermes.example.org");
        assert_eq!(headers["x-forwarded-host"], "hermes.example.org");
        assert_eq!(headers["x-forwarded-proto"], "https");
        assert_eq!(headers["authorization"], "Bearer native");
        for forged in [
            "x-hermes-verified-subject",
            "x-forwarded-prefix",
            "forwarded",
            "cookie",
        ] {
            assert!(!headers.contains_key(forged), "{forged}");
        }
    }
    // Native cookies pass; platform cookies do not.
    let response = fixture
        .client
        .get(format!("{}/", fixture.url))
        .header(
            "cookie",
            format!("{}; hermes_session_at=native", Fixture::cookies("bob")),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(
        json(response).await["headers"]["cookie"],
        "hermes_session_at=native"
    );
}

#[tokio::test]
async fn requests_without_a_valid_session_reach_nothing() {
    let fixture = Fixture::new(Timing::default()).await;
    let navigation = fixture
        .get(None, "/sessions/42?tab=files")
        .header("accept", "text/html")
        .send()
        .await
        .unwrap();
    assert_eq!(navigation.status(), 303);
    assert_eq!(
        navigation.headers()["location"],
        "/_platform/login?returnTo=%2Fsessions%2F42%3Ftab%3Dfiles"
    );
    // Login, native auth and API paths never become return paths.
    for path in ["/auth/callback?code=x", "/_platform/me", "/api/x"] {
        let response = fixture
            .get(None, path)
            .header("accept", "text/html")
            .send()
            .await
            .unwrap();
        let location = response
            .headers()
            .get("location")
            .map(|v| v.to_str().unwrap().to_owned());
        assert!(
            location.is_none() || location.as_deref() == Some("/_platform/login?returnTo=%2F"),
            "{path}: {location:?}"
        );
    }
    let unauthenticated = [
        fixture.get(None, "/api/chat"),
        fixture
            .client
            .get(format!("{}/api/chat", fixture.url))
            .header(
                "cookie",
                format!("{SESSION}={}; {BROWSER}=wrong", handle("bob")),
            ),
        fixture
            .client
            .get(format!("{}/api/chat", fixture.url))
            .header(
                "cookie",
                format!("{}; {SESSION}={}", Fixture::cookies("bob"), handle("carol")),
            ),
        fixture
            .client
            .get(format!("{}/api/chat", fixture.url))
            .header("x-hermes-verified-subject", "bob"),
    ];
    for request in unauthenticated {
        let response = request.send().await.unwrap();
        assert_eq!(response.status(), 401);
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert_eq!(
            json(response).await,
            json!({"error": "invalid_identity_evidence"})
        );
    }
    fixture.sessions.unavailable.store(true, Ordering::SeqCst);
    let unavailable = fixture.get(Some("bob"), "/api/chat").send().await.unwrap();
    assert_eq!(unavailable.status(), 503);
    // Liveness needs no session and checks no dependency.
    let health = fixture
        .get(None, "/_platform/healthz")
        .send()
        .await
        .unwrap();
    assert_eq!(health.status(), 200);
    assert_eq!(health.headers()["cache-control"], "no-store");
    assert_eq!(health.text().await.unwrap(), "ok");
    assert!(fixture.hits().is_empty());
}

#[tokio::test]
async fn pending_denied_and_unavailable_decisions_never_reach_an_instance() {
    let fixture = Fixture::new(Timing::default()).await;
    fixture.set("bob", Verdict::Pending(PendingCode::InstanceNotReady));
    fixture.set("carol", Verdict::Pending(PendingCode::InstanceBlocked));
    fixture.set("erin", Verdict::Unavailable);
    let cases = [
        ("bob", 202, "Starting your Hermes"),
        ("carol", 202, "needs an administrator"),
        ("mallory", 403, "No Hermes for this account"),
        ("erin", 503, "Hermes is unavailable"),
        ("dave", 502, "did not respond"),
    ];
    for (subject, status, text) in cases {
        let page = fixture
            .get(Some(subject), "/")
            .header("accept", "text/html")
            .send()
            .await
            .unwrap();
        assert_eq!(page.status(), status, "{subject}");
        assert_eq!(page.headers()["cache-control"], "no-store");
        assert!(
            page.headers()["content-security-policy"]
                .to_str()
                .unwrap()
                .starts_with("default-src 'none'")
        );
        let html = page.text().await.unwrap();
        assert!(html.contains(text), "{subject}: {html}");
        assert!(!html.contains("hi-"), "pages never name a namespace");
        let api = fixture
            .get(Some(subject), "/api/chat")
            .send()
            .await
            .unwrap();
        assert_eq!(api.status(), status);
    }
    assert!(fixture.hits().is_empty());
    assert_eq!(
        json(fixture.get(Some("bob"), "/api/x").send().await.unwrap()).await,
        json!({"status": "pending", "reason": "instance_not_ready"})
    );
}

#[tokio::test]
async fn ready_decisions_are_cached_per_session_for_the_cache_lifetime_only() {
    let fixture = Fixture::new(fast()).await;
    for _ in 0..5 {
        assert_eq!(
            fixture.get(Some("bob"), "/").send().await.unwrap().status(),
            200
        );
    }
    assert_eq!(fixture.resolver.calls.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.sessions.authentications.load(Ordering::SeqCst), 1);
    fixture.set("bob", Verdict::Denied);
    tokio::time::sleep(Duration::from_millis(120)).await;
    assert_eq!(
        fixture.get(Some("bob"), "/").send().await.unwrap().status(),
        403
    );
    // Denials are never cached.
    fixture.set("bob", Verdict::Ready(fixture.routes["bob"].clone()));
    assert_eq!(
        fixture.get(Some("bob"), "/").send().await.unwrap().status(),
        200
    );
}

#[tokio::test]
async fn uploads_are_bounded_and_bodies_reach_the_instance() {
    let fixture = Fixture::new(Timing::default()).await;
    let accepted = fixture
        .client
        .post(format!("{}/api/upload", fixture.url))
        .header("cookie", Fixture::cookies("bob"))
        .body(vec![7u8; 512 * 1024])
        .send()
        .await
        .unwrap();
    assert_eq!(json(accepted).await["bytes"], 512 * 1024);
    let refused = fixture
        .client
        .post(format!("{}/api/upload", fixture.url))
        .header("cookie", Fixture::cookies("bob"))
        .body(vec![7u8; 2 * 1024 * 1024])
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), 413);
    assert!(
        Gateway::new(
            Checks {
                sessions: fixture.sessions.clone(),
                resolver: fixture.resolver.clone(),
                timing: Timing::default(),
            },
            Arc::new(|_: &Destination| String::new()),
            ORIGIN,
            MAX_UPLOAD + 1,
        )
        .is_err()
    );
}

#[tokio::test]
async fn websockets_need_the_public_origin_and_carry_bytes_unchanged() {
    let fixture = Fixture::new(Timing::default()).await;
    let refused = fixture
        .websocket("bob", "https://attacker.example.org")
        .await
        .unwrap_err();
    assert!(refused.starts_with("HTTP/1.1 403"), "{refused}");
    let denied = fixture.websocket("mallory", ORIGIN).await.unwrap_err();
    assert!(denied.starts_with("HTTP/1.1 403"), "{denied}");
    let mut stream = fixture.websocket("bob", ORIGIN).await.unwrap();
    stream.write_all(b"\x81\x05hello").await.unwrap();
    let mut echoed = [0u8; 7];
    stream.read_exact(&mut echoed).await.unwrap();
    assert_eq!(&echoed, b"\x81\x05hello");
    assert_eq!(fixture.hits(), ["bob /api/ws"]);
}

/// Waits until the server closes `stream`; returns how long that took.
async fn closed_within(mut stream: TcpStream, bound: Duration) -> Duration {
    let started = Instant::now();
    let mut buffer = [0u8; 64];
    let closed = tokio::time::timeout(bound, async {
        loop {
            match stream.read(&mut buffer).await {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
        }
    })
    .await;
    assert!(closed.is_ok(), "still open after {bound:?}");
    started.elapsed()
}

#[tokio::test]
async fn open_websockets_close_when_access_changes_within_the_bound() {
    let timing = fast();
    // One interval plus one check deadline, with scheduling slack.
    let bound = timing.recheck + timing.deadline + Duration::from_millis(300);
    let fixture = Fixture::new(timing).await;

    let stream = fixture.websocket("bob", ORIGIN).await.unwrap();
    fixture.set("bob", Verdict::Denied);
    closed_within(stream, bound).await;

    let mut changed = fixture.routes["carol"].clone();
    let stream = fixture.websocket("carol", ORIGIN).await.unwrap();
    changed.route_generation += 1;
    fixture.set("carol", Verdict::Ready(changed));
    closed_within(stream, bound).await;

    fixture.set("bob", Verdict::Ready(fixture.routes["bob"].clone()));
    tokio::time::sleep(fixture_cache()).await;
    let stream = fixture.websocket("bob", ORIGIN).await.unwrap();
    fixture.sessions.ended.lock().unwrap().insert(handle("bob"));
    closed_within(stream, bound).await;
}

fn fixture_cache() -> Duration {
    fast().cache + Duration::from_millis(20)
}

#[tokio::test]
async fn one_failed_check_is_retried_and_two_close_the_connection() {
    let timing = fast();
    let fixture = Fixture::new(timing).await;
    let mut stream = fixture.websocket("bob", ORIGIN).await.unwrap();
    // A single failed check, then recovery: the connection stays open.
    fixture.resolver.failures.store(1, Ordering::SeqCst);
    tokio::time::sleep(timing.recheck * 3).await;
    assert_eq!(fixture.resolver.failures.load(Ordering::SeqCst), 0);
    stream.write_all(b"ping").await.unwrap();
    let mut echoed = [0u8; 4];
    stream.read_exact(&mut echoed).await.unwrap();
    assert_eq!(&echoed, b"ping");
    // Persistent failure closes after the second consecutive failed check.
    fixture.sessions.unavailable.store(true, Ordering::SeqCst);
    let bound = timing.recheck
        + timing.deadline
        + timing.retry
        + timing.deadline
        + Duration::from_millis(300);
    let took = closed_within(stream, bound).await;
    assert!(took >= timing.retry, "{took:?}");
}

#[tokio::test]
async fn streamed_responses_end_when_access_is_revoked() {
    let timing = fast();
    let fixture = Fixture::new(timing).await;
    let mut response = fixture.get(Some("bob"), "/events").send().await.unwrap();
    assert_eq!(response.status(), 200);
    assert!(response.chunk().await.unwrap().is_some());
    fixture.set("bob", Verdict::Denied);
    let ended = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match response.chunk().await {
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => return,
            }
        }
    })
    .await;
    assert!(ended.is_ok(), "the stream outlived its authorization");
}

#[tokio::test]
async fn logout_ends_the_session_and_expires_every_cookie_even_when_the_instance_is_down() {
    let fixture = Fixture::new(Timing::default()).await;
    let contract = Contract::load("gateway.openapi.yaml");
    let me = fixture
        .get(Some("bob"), "/_platform/me")
        .send()
        .await
        .unwrap();
    assert_eq!(me.status(), 200);
    assert_eq!(me.headers()["cache-control"], "no-store");
    let body = json(me).await;
    contract.check("getPlatformSession", 200, &body);
    assert_eq!(body["identity"]["subject"], "bob");
    let anonymous = json(fixture.get(None, "/_platform/me").send().await.unwrap()).await;
    contract.check("getPlatformSession", 401, &anonymous);

    let foreign = fixture
        .client
        .post(format!("{}/auth/logout", fixture.url))
        .header("cookie", Fixture::cookies("bob"))
        .header("origin", "https://attacker.example.org")
        .send()
        .await
        .unwrap();
    assert_eq!(foreign.status(), 403);
    contract.check("logoutNative", 403, &json(foreign).await);
    assert!(fixture.sessions.logouts.lock().unwrap().is_empty());

    for subject in ["bob", "dave"] {
        let response = fixture
            .client
            .post(format!("{}/auth/logout", fixture.url))
            .header("cookie", Fixture::cookies(subject))
            .header("origin", ORIGIN)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 303, "{subject}");
        assert_eq!(response.headers()["location"], "/_platform/signed-out");
        let expired: Vec<_> = response
            .headers()
            .get_all("set-cookie")
            .iter()
            .map(|v| v.to_str().unwrap().to_owned())
            .collect();
        for name in [
            SESSION,
            BROWSER,
            "__Host-hermes_session_at",
            "hermes_session_rt",
        ] {
            assert!(
                expired
                    .iter()
                    .any(|c| c.starts_with(&format!("{name}=;")) && c.ends_with("Max-Age=0")),
                "{name}"
            );
        }
    }
    // The ready instance saw its own logout; the session ended either way.
    assert!(fixture.hits().contains(&"bob /auth/logout".to_owned()));
    assert_eq!(
        *fixture.sessions.logouts.lock().unwrap(),
        [handle("bob"), handle("dave")]
    );
    assert_eq!(
        fixture.get(Some("bob"), "/").send().await.unwrap().status(),
        401
    );
    let signed_out = fixture
        .get(None, "/_platform/signed-out")
        .send()
        .await
        .unwrap();
    assert_eq!(signed_out.status(), 200);
    assert!(
        signed_out
            .text()
            .await
            .unwrap()
            .contains("/_platform/login")
    );
}
