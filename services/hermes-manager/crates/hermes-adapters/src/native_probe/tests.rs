use super::*;
use axum::{
    Router,
    body::Body,
    extract::{Request, State},
    http::{Response, header::SET_COOKIE},
};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinHandle,
};

#[derive(Clone)]
struct Reply {
    status: StatusCode,
    body: String,
    location: Option<String>,
    delay: Duration,
}
impl Reply {
    fn status(status: StatusCode) -> Self {
        Self {
            status,
            body: "{}".into(),
            location: None,
            delay: Duration::ZERO,
        }
    }
}
struct FixtureState {
    replies: BTreeMap<String, Reply>,
    paths: Mutex<Vec<String>>,
    credentials_sent: AtomicBool,
}
struct Fixture {
    base: Url,
    state: Arc<FixtureState>,
    server: JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

async fn respond(State(state): State<Arc<FixtureState>>, request: Request) -> Response<Body> {
    let path = request.uri().path();
    state.paths.lock().unwrap().push(path.into());
    if request.headers().contains_key("authorization") || request.headers().contains_key("cookie") {
        state.credentials_sent.store(true, Ordering::Relaxed);
    }
    let reply = state
        .replies
        .get(path)
        .cloned()
        .unwrap_or_else(|| Reply::status(StatusCode::IM_A_TEAPOT));
    tokio::time::sleep(reply.delay).await;
    let mut response = Response::builder().status(reply.status).header(
        SET_COOKIE,
        "native-probe-cookie=must-not-be-returned; Path=/",
    );
    if let Some(location) = reply.location {
        response = response.header(LOCATION, location);
    }
    response.body(Body::from(reply.body)).unwrap()
}

fn replies() -> BTreeMap<String, Reply> {
    BTreeMap::from([
        ("/api/health".into(), Reply::status(StatusCode::OK)),
        ("/api/auth/providers".into(), Reply {
            body: r#"{"providers":[{"name":"self-hosted","display_name":"Self-Hosted OIDC","supports_password":false}]}"#.into(),
            ..Reply::status(StatusCode::OK)
        }),
        ("/".into(), Reply {
            location: Some("/auth/login?provider=self-hosted&next=%2F".into()),
            ..Reply::status(StatusCode::FOUND)
        }),
        ("/api/auth/me".into(), Reply::status(StatusCode::UNAUTHORIZED)),
        ("/api/sessions".into(), Reply::status(StatusCode::UNAUTHORIZED)),
    ])
}

async fn fixture(replies: BTreeMap<String, Reply>) -> Fixture {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let state = Arc::new(FixtureState {
        replies,
        paths: Mutex::new(Vec::new()),
        credentials_sent: AtomicBool::new(false),
    });
    let router = Router::new().fallback(respond).with_state(state.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    Fixture {
        base,
        state,
        server,
    }
}

// Only tests may choose a loopback URL. The production entry point derives the
// fixed Service DNS name and port from a validated generated namespace.
async fn verify_at(probe: &NativeBackendProbe, base: &Url) -> Result<(), Error> {
    assert_eq!(base.host_str(), Some("127.0.0.1"));
    probe.bounded_verify(base).await
}

#[tokio::test]
async fn successful_probe_checks_native_contract_without_following_or_returning_cookies() {
    let fixture = fixture(replies()).await;
    let probe = NativeBackendProbe::new().unwrap();
    verify_at(&probe, &fixture.base).await.unwrap();
    verify_at(&probe, &fixture.base).await.unwrap();
    assert!(!fixture.state.credentials_sent.load(Ordering::Relaxed));
    assert_eq!(
        *fixture.state.paths.lock().unwrap(),
        [
            "/api/health",
            "/api/auth/providers",
            "/",
            "/api/auth/me",
            "/api/sessions",
            "/api/health",
            "/api/auth/providers",
            "/",
            "/api/auth/me",
            "/api/sessions",
        ]
    );
}

#[tokio::test]
async fn malformed_or_additional_interactive_providers_fail_closed() {
    let probe = NativeBackendProbe::new().unwrap();
    for body in [
        "not JSON",
        r#"{"providers":[]}"#,
        r#"{"providers":[{"name":"self-hosted","display_name":"OIDC","supports_password":true}]}"#,
        r#"{"providers":[{"name":"foreign","display_name":"Foreign","supports_password":false}]}"#,
        r#"{"providers":[{"name":"self-hosted","display_name":"OIDC"}]}"#,
        r#"{"providers":[{"name":"self-hosted","display_name":"OIDC","supports_password":false},{"name":"foreign","display_name":"Foreign","supports_password":false}]}"#,
    ] {
        let mut replies = replies();
        replies.get_mut("/api/auth/providers").unwrap().body = body.into();
        let fixture = fixture(replies).await;
        assert_eq!(
            verify_at(&probe, &fixture.base).await,
            Err(Error::NativeAuthInvalid)
        );
    }
}

#[tokio::test]
async fn health_failure_and_unprotected_native_apis_never_pass() {
    let probe = NativeBackendProbe::new().unwrap();
    for (path, status, error) in [
        (
            "/api/health",
            StatusCode::SERVICE_UNAVAILABLE,
            Error::Unavailable,
        ),
        (
            "/api/health",
            StatusCode::NOT_FOUND,
            Error::NativeAuthInvalid,
        ),
        ("/api/auth/me", StatusCode::OK, Error::NativeAuthInvalid),
        ("/api/sessions", StatusCode::OK, Error::NativeAuthInvalid),
    ] {
        let mut replies = replies();
        replies.get_mut(path).unwrap().status = status;
        let fixture = fixture(replies).await;
        assert_eq!(verify_at(&probe, &fixture.base).await, Err(error));
    }
}

#[tokio::test]
async fn redirects_cannot_follow_a_trap_or_point_login_at_another_origin() {
    let probe = NativeBackendProbe::new().unwrap();
    for location in [
        "http://127.0.0.1:1/trap",
        "//127.0.0.1:1/trap",
        "/auth/login?provider=self-hosted&next=https%3A%2F%2Fevil.example.org",
        "/auth/login?provider=self-hosted&next=%2F%2Fevil.example.org",
        "/auth/login?provider=self-hosted&next=%2F&provider=foreign",
        "/auth/login?provider=self-hosted&next=%2F#fragment",
    ] {
        let mut replies = replies();
        replies.get_mut("/").unwrap().location = Some(location.into());
        let fixture = fixture(replies).await;
        assert_eq!(
            verify_at(&probe, &fixture.base).await,
            Err(Error::NativeAuthInvalid)
        );
    }
    let mut replies = replies();
    let health = replies.get_mut("/api/health").unwrap();
    health.status = StatusCode::FOUND;
    health.location = Some("/redirect-trap".into());
    let fixture = fixture(replies).await;
    assert_eq!(
        verify_at(&probe, &fixture.base).await,
        Err(Error::NativeAuthInvalid)
    );
    assert_eq!(*fixture.state.paths.lock().unwrap(), ["/api/health"]);
}

#[tokio::test]
async fn streamed_body_without_content_length_is_stopped_at_the_byte_limit() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0u8; 4096];
        let mut received = 0;
        loop {
            let count = stream.read(&mut request[received..]).await.unwrap();
            assert!(count > 0);
            received += count;
            if request[..received]
                .windows(4)
                .any(|value| value == b"\r\n\r\n")
            {
                break;
            }
            assert!(received < request.len());
        }
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
            .await
            .unwrap();
        let mut chunk = b"400\r\n".to_vec();
        chunk.extend_from_slice(&[b'x'; 1024]);
        chunk.extend_from_slice(b"\r\n");
        for _ in 0..65 {
            if stream.write_all(&chunk).await.is_err() {
                return;
            }
        }
        let _ = stream.write_all(b"0\r\n\r\n").await;
    });
    let result = verify_at(&NativeBackendProbe::new().unwrap(), &base).await;
    server.abort();
    assert_eq!(result, Err(Error::NativeAuthInvalid));
}

#[tokio::test]
async fn complete_probe_has_one_four_second_deadline_across_all_requests() {
    let mut replies = replies();
    for reply in replies.values_mut() {
        reply.delay = Duration::from_millis(850);
    }
    let fixture = fixture(replies).await;
    let probe = NativeBackendProbe::new().unwrap();
    let start = Instant::now();
    assert_eq!(
        verify_at(&probe, &fixture.base).await,
        Err(Error::Unavailable)
    );
    assert!(start.elapsed() >= Duration::from_millis(3800));
    assert!(start.elapsed() < Duration::from_millis(4200));
}

#[test]
fn only_generated_instance_namespace_can_select_the_fixed_backend() {
    let namespace = hermes_domain::instance_namespace(uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
    let url = service_url(&namespace).unwrap();
    assert_eq!(url.scheme(), "http");
    assert_eq!(url.port(), Some(8080));
    assert_eq!(
        url.host_str(),
        Some(format!("hermes.{namespace}.svc.cluster.local").as_str())
    );
    for invalid in [
        "kube-system",
        "http://127.0.0.1",
        "../namespace",
        "hi-1234-deadbeef",
        "hi-000000000000000000000000-00000000000000000000000000000000",
    ] {
        assert!(service_url(invalid).is_err());
    }
}
