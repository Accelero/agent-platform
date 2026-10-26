//! The manager's route resolver as the gateway sees it: an mTLS client with a
//! 2-second deadline whose replies are accepted only when consistent.

use std::time::Duration;

use async_trait::async_trait;
use hermes_core::domain::identity::Identity;
use hermes_core::domain::installation::Installation;
use hermes_core::domain::routing::Route;
use hermes_identity::AuthError;
use hermes_wire::routing::{DecisionBody, PendingCode, ResolveRequest};

/// The gateway's view of a decision. Anything it cannot verify is
/// `Unavailable`, never a route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Ready(Route),
    Pending(PendingCode),
    Denied,
    Unavailable,
}

#[async_trait]
pub trait Resolve: Send + Sync {
    async fn resolve(&self, identity: &Identity) -> Verdict;
}

const DEADLINE: Duration = Duration::from_secs(2);
const MAX_REPLY: usize = 16 * 1024;

pub struct ResolverClient {
    client: reqwest::Client,
    url: String,
    installation: Installation,
}

impl ResolverClient {
    /// `identity_pem` holds the gateway's client key and certificate; `ca_pem`
    /// is the only trust anchor for the resolver's server certificate.
    pub fn new(
        base_url: &str,
        ca_pem: &[u8],
        identity_pem: &[u8],
        installation: Installation,
    ) -> Result<Self, AuthError> {
        let base = url::Url::parse(base_url).map_err(|_| AuthError::Configuration)?;
        if base.scheme() != "https" || base.path() != "/" || base.query().is_some() {
            return Err(AuthError::Configuration);
        }
        let url = base
            .join("/internal/v1/routes/resolve")
            .map_err(|_| AuthError::Configuration)?;
        let ca = reqwest::Certificate::from_pem(ca_pem).map_err(|_| AuthError::Configuration)?;
        let identity =
            reqwest::Identity::from_pem(identity_pem).map_err(|_| AuthError::Configuration)?;
        let client = reqwest::Client::builder()
            .https_only(true)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .tls_certs_only([ca])
            .identity(identity)
            .connect_timeout(Duration::from_secs(1))
            .timeout(DEADLINE)
            .pool_idle_timeout(Duration::from_secs(30))
            .build()
            .map_err(|_| AuthError::Configuration)?;
        Ok(Self {
            client,
            url: url.to_string(),
            installation,
        })
    }

    async fn call(&self, identity: &Identity) -> Option<(u16, Vec<u8>)> {
        let request = ResolveRequest {
            identity: identity.clone(),
        };
        let mut response = self
            .client
            .post(&self.url)
            .json(&request)
            .send()
            .await
            .ok()?;
        let status = response.status().as_u16();
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.ok()? {
            body.extend_from_slice(&chunk);
            if body.len() > MAX_REPLY {
                return None;
            }
        }
        Some((status, body))
    }
}

/// A reply is accepted only when its status and decision agree.
pub fn verdict(installation: &Installation, status: u16, body: &[u8]) -> Verdict {
    let Ok(decision) = hermes_wire::strict::from_slice::<DecisionBody>(body) else {
        return Verdict::Unavailable;
    };
    match (status, &decision) {
        (200, DecisionBody::Ready { .. }) => decision
            .route_for(installation)
            .map_or(Verdict::Unavailable, Verdict::Ready),
        (202, DecisionBody::Pending { reason }) => Verdict::Pending(*reason),
        (403, DecisionBody::Denied { .. }) => Verdict::Denied,
        _ => Verdict::Unavailable,
    }
}

#[async_trait]
impl Resolve for ResolverClient {
    async fn resolve(&self, identity: &Identity) -> Verdict {
        match tokio::time::timeout(DEADLINE, self.call(identity)).await {
            Ok(Some((status, body))) => verdict(&self.installation, status, &body),
            _ => Verdict::Unavailable,
        }
    }
}

#[cfg(test)]
mod tests {
    use hermes_core::app::testing::installation;
    use hermes_core::domain::routing::Destination;
    use serde_json::json;
    use uuid::Uuid;

    use super::*;

    fn route() -> Route {
        let instance_id = Uuid::new_v4();
        Route {
            instance_id,
            route_generation: 2,
            authorization_version: 1,
            destination: Destination {
                namespace: installation().instance_namespace(instance_id),
                service: "hermes",
                port: 8080,
            },
        }
    }

    fn ready(route: &Route) -> Vec<u8> {
        json!({"status": "ready", "instanceId": route.instance_id, "routeGeneration": 2,
               "authorizationVersion": 1, "destination": {"namespace": route.destination.namespace,
               "service": "hermes", "port": 8080, "scheme": "http"}})
        .to_string()
        .into_bytes()
    }

    #[test]
    fn only_a_consistent_reply_for_this_installation_is_a_route() {
        let installation = installation();
        let route = route();
        assert_eq!(
            verdict(&installation, 200, &ready(&route)),
            Verdict::Ready(route.clone())
        );
        let pending = br#"{"status":"pending","reason":"instance_blocked"}"#;
        assert_eq!(
            verdict(&installation, 202, pending),
            Verdict::Pending(PendingCode::InstanceBlocked)
        );
        let denied = br#"{"status":"denied","reason":"access_denied"}"#;
        assert_eq!(verdict(&installation, 403, denied), Verdict::Denied);
        let mut foreign = route.clone();
        foreign.destination.namespace = "kube-system".into();
        let duplicated = String::from_utf8(ready(&route)).unwrap().replacen(
            "\"port\":8080",
            "\"port\":8080,\"port\":9119",
            1,
        );
        for (status, body) in [
            // A ready body under any other status, and the reverse.
            (403, ready(&route)),
            (202, ready(&route)),
            (200, denied.to_vec()),
            (200, ready(&foreign)),
            (200, duplicated.into_bytes()),
            (200, b"{}".to_vec()),
            (
                503,
                br#"{"status":"unavailable","reason":"dependency_unavailable"}"#.to_vec(),
            ),
            (302, Vec::new()),
        ] {
            assert_eq!(verdict(&installation, status, &body), Verdict::Unavailable);
        }
    }
}
