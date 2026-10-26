//! Revocation of established exchanges: WebSockets and streamed responses
//! are re-checked against the session and the resolver, and end as soon as
//! either no longer yields the same ready route.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::{Body, Bytes};
use hermes_core::domain::routing::Route;
use hermes_identity::AuthError;
use http_body_util::BodyExt;
use hyper::body::{Frame, SizeHint};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::resolver::{Resolve, Verdict};
use crate::sessions::Sessions;

/// Re-check intervals and bounds. The defaults meet the specification: a
/// denial ends an exchange within one interval, and a dependency failure
/// within two checks (`recheck + deadline + retry + deadline` = 10 s).
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    pub recheck: Duration,
    /// Delay before re-checking after one failed check.
    pub retry: Duration,
    /// Deadline of one check, session and resolution together.
    pub deadline: Duration,
    /// Lifetime of a cached ready decision.
    pub cache: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            recheck: Duration::from_secs(5),
            retry: Duration::from_secs(1),
            deadline: Duration::from_secs(2),
            cache: Duration::from_secs(5),
        }
    }
}

/// Why an exchange ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    /// The session is no longer valid.
    Session,
    /// The decision is no longer this ready route.
    Decision,
    /// Two consecutive checks failed.
    Unavailable,
}

/// The parties a supervisor checks with.
#[derive(Clone)]
pub struct Checks {
    pub sessions: Arc<dyn Sessions>,
    pub resolver: Arc<dyn Resolve>,
    pub timing: Timing,
}

enum Check {
    Same,
    Ended(Ended),
    Failed,
}

impl Checks {
    async fn check(&self, handle: &str, route: &Route) -> Check {
        let run = async {
            let identity = match self.sessions.recheck(handle).await {
                Ok(identity) => identity,
                Err(AuthError::Unavailable) => return Check::Failed,
                Err(AuthError::Evidence | AuthError::Forbidden | AuthError::Configuration) => {
                    return Check::Ended(Ended::Session);
                }
            };
            match self.resolver.resolve(&identity).await {
                Verdict::Ready(current) if current == *route => Check::Same,
                Verdict::Ready(_) | Verdict::Pending(_) | Verdict::Denied => {
                    Check::Ended(Ended::Decision)
                }
                Verdict::Unavailable => Check::Failed,
            }
        };
        tokio::time::timeout(self.timing.deadline, run)
            .await
            .unwrap_or(Check::Failed)
    }

    /// Completes when the exchange of `handle` on `route` must end.
    pub async fn until_revoked(&self, handle: &str, route: &Route) -> Ended {
        let mut failed = false;
        loop {
            let wait = if failed {
                self.timing.retry
            } else {
                self.timing.recheck
            };
            tokio::time::sleep(wait).await;
            match self.check(handle, route).await {
                Check::Same => failed = false,
                Check::Ended(reason) => return reason,
                Check::Failed if failed => return Ended::Unavailable,
                Check::Failed => failed = true,
            }
        }
    }

    /// Starts supervising; the receiver completes when the exchange must end.
    pub fn spawn(&self, handle: String, route: Route) -> Supervisor {
        let (ended, receiver) = oneshot::channel();
        let checks = self.clone();
        let task = tokio::spawn(async move {
            let reason = checks.until_revoked(&handle, &route).await;
            let _ = ended.send(reason);
        });
        Supervisor { receiver, task }
    }
}

/// A running supervisor; dropping it stops the checks.
pub struct Supervisor {
    pub receiver: oneshot::Receiver<Ended>,
    task: JoinHandle<()>,
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// A streamed response body that fails, ending the connection, once its
/// supervisor says the exchange must end.
pub struct Revocable {
    inner: Body,
    supervisor: Supervisor,
}

impl Revocable {
    pub fn wrap(inner: Body, supervisor: Supervisor) -> Body {
        Body::new(Self { inner, supervisor })
    }
}

/// The error a revoked stream ends with. Carries nothing about the reason.
#[derive(Debug)]
pub struct Revoked;

impl std::fmt::Display for Revoked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("exchange revoked")
    }
}

impl std::error::Error for Revoked {}

impl hyper::body::Body for Revocable {
    type Data = Bytes;
    type Error = axum::BoxError;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        if Pin::new(&mut self.supervisor.receiver).poll(cx).is_ready() {
            return Poll::Ready(Some(Err(Box::new(Revoked))));
        }
        Pin::new(&mut self.inner).poll_frame(cx).map_err(Into::into)
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

/// Splices a client and an upstream connection until either closes or the
/// supervisor ends the exchange; dropping both then closes them.
pub async fn splice<C, U>(
    mut client: C,
    mut upstream: U,
    mut supervisor: Supervisor,
) -> Option<Ended>
where
    C: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    U: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    tokio::select! {
        _ = tokio::io::copy_bidirectional(&mut client, &mut upstream) => None,
        ended = &mut supervisor.receiver => Some(ended.unwrap_or(Ended::Unavailable)),
    }
}

/// Collects a small body, for pages and discarded replies.
pub async fn drain(body: Body, limit: usize) -> Option<Bytes> {
    http_body_util::Limited::new(body, limit)
        .collect()
        .await
        .ok()
        .map(|collected| collected.to_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_timing_meets_the_specified_bounds() {
        let timing = Timing::default();
        let seconds = Duration::from_secs;
        // HTTP decisions at most 5 s old; open connections re-checked every 5 s.
        assert!(timing.cache <= seconds(5) && timing.recheck <= seconds(5));
        // Resolution has a 2 s deadline.
        assert!(timing.deadline <= seconds(2));
        // Closed within 10 s of a denial, or of a failure seen by two checks.
        assert!(timing.recheck + timing.deadline <= seconds(10));
        assert!(timing.recheck + timing.deadline + timing.retry + timing.deadline <= seconds(10));
    }
}
