//! The native auth-gate probe: unauthenticated requests to the instance's own
//! Service prove that native Hermes is up and that it admits nobody without
//! the owner's provider login. Bounded in time and size; no cookies, no
//! redirects, no credentials.

use std::time::Duration;

use async_trait::async_trait;
use hermes_core::app::controller::{InitBinding, Native, Plain, ProbeOutcome};
use hermes_core::domain::execution::SshCheck;
use reqwest::StatusCode;
use reqwest::header::LOCATION;
use serde::Deserialize;
use url::Url;

const BODY_LIMIT: usize = 64 * 1024;
const TOTAL_TIMEOUT: Duration = Duration::from_secs(4);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Providers {
    providers: Vec<Provider>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Provider {
    name: String,
    display_name: String,
    supports_password: bool,
}

/// Where the probe reaches an instance.
#[derive(Clone)]
enum Base {
    /// `http://hermes.<namespace>.svc.cluster.local:8080/`.
    ClusterDns,
    /// A fixed URL, for tests.
    Fixed(Url),
}

#[derive(Clone)]
pub struct NativeProbe {
    client: reqwest::Client,
    base: Base,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Failure {
    Unavailable,
    Invalid,
}

impl NativeProbe {
    pub fn new() -> Result<Self, hermes_core::app::error::AppError> {
        Self::build(Base::ClusterDns)
    }

    /// Probes `base` for every namespace; for tests.
    pub fn fixed(base: Url) -> Result<Self, hermes_core::app::error::AppError> {
        Self::build(Base::Fixed(base))
    }

    fn build(base: Base) -> Result<Self, hermes_core::app::error::AppError> {
        // No cookie store: a Set-Cookie from one request must not
        // authenticate the next.
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_millis(500))
            .read_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(3))
            .build()
            .map_err(|_| hermes_core::app::error::AppError::unavailable())?;
        Ok(Self { client, base })
    }

    /// Only an installation-shaped instance namespace is accepted; no caller
    /// URL, port, path or credential.
    pub async fn verify(&self, namespace: &str) -> ProbeOutcome {
        let base = match &self.base {
            Base::Fixed(url) => url.clone(),
            Base::ClusterDns => match service_url(namespace) {
                Some(url) => url,
                None => return ProbeOutcome::Invalid,
            },
        };
        match tokio::time::timeout(TOTAL_TIMEOUT, self.gate(&base)).await {
            Ok(Ok(())) => ProbeOutcome::Verified,
            Ok(Err(Failure::Invalid)) => ProbeOutcome::Invalid,
            Ok(Err(Failure::Unavailable)) | Err(_) => ProbeOutcome::Unavailable,
        }
    }

    async fn gate(&self, base: &Url) -> Result<(), Failure> {
        self.get(base, "/api/health", StatusCode::OK).await?;
        let (_, body) = self
            .get(base, "/api/auth/providers", StatusCode::OK)
            .await?;
        let providers: Providers = serde_json::from_slice(&body).map_err(|_| Failure::Invalid)?;
        let [provider] = providers.providers.as_slice() else {
            return Err(Failure::Invalid);
        };
        if provider.name != "self-hosted"
            || provider.supports_password
            || provider.display_name.is_empty()
        {
            return Err(Failure::Invalid);
        }
        let (location, _) = self.get(base, "/", StatusCode::FOUND).await?;
        login_redirect(base, location.as_deref().ok_or(Failure::Invalid)?)?;
        for path in ["/api/auth/me", "/api/sessions"] {
            self.get(base, path, StatusCode::UNAUTHORIZED).await?;
        }
        Ok(())
    }

    async fn get(
        &self,
        base: &Url,
        path: &str,
        expected: StatusCode,
    ) -> Result<(Option<String>, Vec<u8>), Failure> {
        let url = base.join(path).map_err(|_| Failure::Invalid)?;
        let mut response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|_| Failure::Unavailable)?;
        if response.status() != expected {
            return Err(if response.status().is_server_error() {
                Failure::Unavailable
            } else {
                Failure::Invalid
            });
        }
        if response.headers().get_all(LOCATION).iter().count() > 1 {
            return Err(Failure::Invalid);
        }
        let location = response
            .headers()
            .get(LOCATION)
            .map(|value| {
                value
                    .to_str()
                    .map(str::to_owned)
                    .map_err(|_| Failure::Invalid)
            })
            .transpose()?;
        if response
            .content_length()
            .is_some_and(|length| length > BODY_LIMIT as u64)
        {
            return Err(Failure::Invalid);
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| Failure::Unavailable)? {
            if chunk.len() > BODY_LIMIT - body.len() {
                return Err(Failure::Invalid);
            }
            body.extend_from_slice(&chunk);
        }
        Ok((location, body))
    }
}

/// `hi-<12 hex>-<32 hex>` only.
fn service_url(namespace: &str) -> Option<Url> {
    let (installation, instance) = namespace.strip_prefix("hi-")?.split_once('-')?;
    let hex = |s: &str| {
        s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    };
    if installation.len() != 12 || instance.len() != 32 || !hex(installation) || !hex(instance) {
        return None;
    }
    Url::parse(&format!(
        "http://hermes.{namespace}.svc.cluster.local:8080/"
    ))
    .ok()
}

fn login_redirect(base: &Url, location: &str) -> Result<(), Failure> {
    if !location.starts_with("/auth/login?provider=self-hosted&") {
        return Err(Failure::Invalid);
    }
    let target = base.join(location).map_err(|_| Failure::Invalid)?;
    let query: Vec<_> = target.query_pairs().collect();
    if target.origin() != base.origin()
        || target.path() != "/auth/login"
        || target.fragment().is_some()
        || query.len() != 2
        || query[0].0 != "provider"
        || query[0].1 != "self-hosted"
        || query[1].0 != "next"
        || query[1].1 != "/"
    {
        return Err(Failure::Invalid);
    }
    Ok(())
}

/// The controller's native checks.
#[derive(Clone)]
pub struct NativeChecks {
    pub probe: NativeProbe,
}

#[async_trait]
impl Native for NativeChecks {
    fn receipt_valid(&self, key: &Plain, binding: &InitBinding, receipt: &str) -> bool {
        crate::receipt::verify(key.expose(), binding, receipt)
    }

    async fn probe(&self, namespace: &str) -> ProbeOutcome {
        self.probe.verify(namespace).await
    }

    async fn ssh_ready(
        &self,
        address: &str,
        client_key: &Plain,
        host_key: &str,
        uid: u32,
    ) -> SshCheck {
        crate::ssh::ready(address, client_key.expose(), host_key, uid).await
    }
}

#[cfg(test)]
mod tests;
