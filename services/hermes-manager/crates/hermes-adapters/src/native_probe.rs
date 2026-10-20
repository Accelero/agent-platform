//! Unauthenticated native auth-gate proof against the fixed, owned Service.
//! Kubernetes resource ownership/readiness must be verified before this probe.
use std::time::Duration;

use hermes_application::Error;
use reqwest::{StatusCode, header::LOCATION};
use serde::Deserialize;
use url::Url;

#[cfg(test)]
mod tests;

const BODY_LIMIT: usize = 64 * 1024;
const TOTAL_TIMEOUT: Duration = Duration::from_secs(4);

#[derive(Clone)]
pub struct NativeBackendProbe {
    client: reqwest::Client,
}

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

impl NativeBackendProbe {
    pub fn new() -> Result<Self, Error> {
        // No cookie jar or default credentials are attached. A Set-Cookie from
        // one endpoint must not authenticate any subsequent probe request.
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_millis(500))
            .read_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(3))
            .build()
            .map_err(|_| Error::Unavailable)?;
        Ok(Self { client })
    }

    /// Only the stable manager-generated instance namespace is accepted. No
    /// caller URL, port, path, credential or forwarded identity is accepted.
    pub async fn verify(&self, namespace: &str) -> Result<(), Error> {
        let base = service_url(namespace)?;
        self.bounded_verify(&base).await
    }

    async fn bounded_verify(&self, base: &Url) -> Result<(), Error> {
        tokio::time::timeout(TOTAL_TIMEOUT, self.verify_backend(base))
            .await
            .map_err(|_| Error::Unavailable)?
    }

    async fn verify_backend(&self, base: &Url) -> Result<(), Error> {
        self.get(base, "/api/health", StatusCode::OK).await?;
        let (_, body) = self
            .get(base, "/api/auth/providers", StatusCode::OK)
            .await?;
        let providers: Providers =
            serde_json::from_slice(&body).map_err(|_| Error::NativeAuthInvalid)?;
        let [provider] = providers.providers.as_slice() else {
            return Err(Error::NativeAuthInvalid);
        };
        if provider.name != "self-hosted"
            || provider.supports_password
            || provider.display_name.is_empty()
        {
            return Err(Error::NativeAuthInvalid);
        }
        let (location, _) = self.get(base, "/", StatusCode::FOUND).await?;
        verify_login_redirect(base, location.as_deref().ok_or(Error::NativeAuthInvalid)?)?;
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
    ) -> Result<(Option<String>, Vec<u8>), Error> {
        let url = base.join(path).map_err(|_| Error::Invalid)?;
        let mut response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|_| Error::Unavailable)?;
        if response.status() != expected {
            return Err(if response.status().is_server_error() {
                Error::Unavailable
            } else {
                Error::NativeAuthInvalid
            });
        }
        if response.headers().get_all(LOCATION).iter().count() > 1 {
            return Err(Error::NativeAuthInvalid);
        }
        let location = response
            .headers()
            .get(LOCATION)
            .map(|value| {
                value
                    .to_str()
                    .map(str::to_owned)
                    .map_err(|_| Error::NativeAuthInvalid)
            })
            .transpose()?;
        if response
            .content_length()
            .is_some_and(|length| length > BODY_LIMIT as u64)
        {
            return Err(Error::NativeAuthInvalid);
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| Error::Unavailable)? {
            if chunk.len() > BODY_LIMIT - body.len() {
                return Err(Error::NativeAuthInvalid);
            }
            body.extend_from_slice(&chunk);
        }
        Ok((location, body))
    }
}

fn service_url(namespace: &str) -> Result<Url, Error> {
    hermes_domain::validate_name(namespace)?;
    let (installation, instance) = namespace
        .strip_prefix("hi-")
        .and_then(|name| name.split_once('-'))
        .ok_or(Error::Invalid)?;
    if installation.len() != 24
        || instance.len() != 32
        || !installation
            .bytes()
            .chain(instance.bytes())
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || uuid::Uuid::parse_str(instance)
            .map_err(|_| Error::Invalid)?
            .is_nil()
    {
        return Err(Error::Invalid);
    }
    Url::parse(&format!(
        "http://hermes.{namespace}.svc.cluster.local:8080/"
    ))
    .map_err(|_| Error::Invalid)
}

fn verify_login_redirect(base: &Url, location: &str) -> Result<(), Error> {
    if !location.starts_with("/auth/login?provider=self-hosted&") {
        return Err(Error::NativeAuthInvalid);
    }
    let target = base.join(location).map_err(|_| Error::NativeAuthInvalid)?;
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
        return Err(Error::NativeAuthInvalid);
    }
    Ok(())
}
