//! Authentication adapters: Keycloak access-token verification, the OIDC
//! authorization-code flow, sealed server-side browser sessions and the
//! read-only Keycloak directory.
//!
//! Nothing here decides authorization. Callers supply an [`browser::Admission`]
//! hook; the manager's hook checks PostgreSQL grants.

pub mod browser;
pub mod crypto;
pub mod directory;
pub mod error;
pub mod oidc;
pub mod sessions;
pub mod tokens;

use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub use error::AuthError;
/// The HTTP client type used for provider calls.
pub use reqwest::Client as HttpClient;

/// HTTPS-only client without redirects or environment proxies, with short
/// deadlines. `ca_pem` adds one private trust anchor.
pub fn http_client(ca_pem: Option<&[u8]>) -> Result<reqwest::Client, AuthError> {
    let mut builder = reqwest::Client::builder()
        .https_only(true)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(1))
        .timeout(Duration::from_secs(2));
    if let Some(pem) = ca_pem {
        let certificate =
            reqwest::Certificate::from_pem(pem).map_err(|_| AuthError::Configuration)?;
        builder = builder.add_root_certificate(certificate);
    }
    builder.build().map_err(|_| AuthError::Unavailable)
}

pub fn unix_time() -> Result<u64, AuthError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .map_err(|_| AuthError::Unavailable)
}
