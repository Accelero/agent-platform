//! The gateway's resolver client against a resolver that misbehaves in time or
//! size. Resolution has a 2 s deadline and a failure denies: whatever the
//! server does, the answer is `Unavailable` within the deadline and never a
//! route.

use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use hermes_core::app::testing::{identity, installation};
use hermes_proxy::resolver::{Resolve, ResolverClient, Verdict};
use hermes_testkit::{Secrets, serve_tls};
use hyper::body::Frame;

/// The specified bound, with scheduling slack.
const DEADLINE: Duration = Duration::from_secs(2);
const SLACK: Duration = Duration::from_millis(400);

/// One byte of body, then silence forever.
struct StallsAfterOneByte(bool);

impl hyper::body::Body for StallsAfterOneByte {
    type Data = Bytes;
    type Error = std::convert::Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        if self.0 {
            return Poll::Pending;
        }
        self.0 = true;
        Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(b"{")))))
    }
}

async fn stalls_before_answering() -> Response {
    tokio::time::sleep(Duration::from_secs(60)).await;
    ().into_response()
}

async fn stalls_in_the_body() -> Response {
    Response::new(Body::new(StallsAfterOneByte(false)))
}

async fn answers_too_much() -> Response {
    Response::new(Body::from(vec![b' '; 20 * 1024]))
}

async fn client_for(router: Router) -> (ResolverClient, hermes_testkit::Running, Secrets) {
    let secrets = Secrets::new().await;
    let server = serve_tls(&secrets, router).await;
    let mut identity_pem = secrets.read("gateway.key");
    identity_pem.extend(secrets.read("gateway.crt"));
    let client = ResolverClient::new(
        &format!("{}/", server.url),
        &secrets.read("ca.crt"),
        &identity_pem,
        installation(),
    )
    .unwrap();
    (client, server, secrets)
}

async fn resolves_within_the_deadline_as_unavailable(router: Router) -> Duration {
    let (client, _server, _secrets) = client_for(router).await;
    let started = Instant::now();
    let verdict = client.resolve(&identity("bob")).await;
    let elapsed = started.elapsed();
    assert!(
        matches!(verdict, Verdict::Unavailable),
        "a misbehaving resolver is never a route: {verdict:?}"
    );
    elapsed
}

#[tokio::test]
async fn a_resolver_that_never_answers_is_unavailable_at_the_deadline() {
    let elapsed = resolves_within_the_deadline_as_unavailable(
        Router::new().route("/internal/v1/routes/resolve", post(stalls_before_answering)),
    )
    .await;
    assert!(elapsed <= DEADLINE + SLACK, "{elapsed:?}");
    assert!(
        elapsed >= DEADLINE - SLACK,
        "{elapsed:?}: not given up on before the deadline"
    );
}

#[tokio::test]
async fn a_resolver_that_stalls_in_the_body_is_unavailable_at_the_deadline() {
    let elapsed = resolves_within_the_deadline_as_unavailable(
        Router::new().route("/internal/v1/routes/resolve", post(stalls_in_the_body)),
    )
    .await;
    assert!(elapsed <= DEADLINE + SLACK, "{elapsed:?}");
}

#[tokio::test]
async fn a_reply_over_16_kib_is_unavailable_without_waiting() {
    let elapsed = resolves_within_the_deadline_as_unavailable(
        Router::new().route("/internal/v1/routes/resolve", post(answers_too_much)),
    )
    .await;
    assert!(elapsed < Duration::from_secs(1), "{elapsed:?}");
}
