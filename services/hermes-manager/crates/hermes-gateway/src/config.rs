//! Gateway configuration. Every secret is a file reference; files are read
//! with a size bound and never echoed.

use std::path::{Path, PathBuf};

use hermes_core::app::error::AppError;
use hermes_core::domain::installation::Installation;
use hermes_http::files::read;
use serde::Deserialize;

const MAX_CONFIG: usize = 64 * 1024;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    pub installation: Installation,
    pub listen: String,
    pub tls: Tls,
    /// The Hermes hostname's origin, such as `https://hermes.example.org`.
    pub public_origin: String,
    /// The administration origin; it must be another host.
    pub admin_origin: String,
    /// Connection for the `gateway_sessions` role.
    pub database_url_file: PathBuf,
    /// Private CA for the identity provider, if it is not publicly trusted.
    #[serde(default)]
    pub oidc_ca_file: Option<PathBuf>,
    pub oidc: Oidc,
    pub keyring_file: PathBuf,
    pub resolver: Resolver,
    #[serde(default = "cluster_domain")]
    pub cluster_domain: String,
    /// Upload limit; at most 64.
    #[serde(default = "upload_mib")]
    pub max_upload_mib: u64,
}

fn cluster_domain() -> String {
    "cluster.local".into()
}

fn upload_mib() -> u64 {
    64
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Tls {
    pub certificate_file: PathBuf,
    pub private_key_file: PathBuf,
}

/// The gateway's own OIDC client.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Oidc {
    pub client_id: String,
    #[serde(default)]
    pub client_secret_file: Option<PathBuf>,
    pub scopes: Vec<String>,
    pub jwks_url: String,
    pub audience: String,
    #[serde(default)]
    pub revocation_url: Option<String>,
}

/// The manager's resolver, reached with the gateway's client certificate.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Resolver {
    pub url: String,
    pub ca_file: PathBuf,
    pub certificate_file: PathBuf,
    pub private_key_file: PathBuf,
}

impl Config {
    pub async fn load(path: &Path) -> Result<Self, AppError> {
        let config: Self = serde_json::from_slice(&read(path, MAX_CONFIG).await?)
            .map_err(|_| AppError::invalid())?;
        config.installation.validate()?;
        let domain_valid = !config.cluster_domain.is_empty()
            && config.cluster_domain.len() <= 253
            && config
                .cluster_domain
                .split('.')
                .all(|label| hermes_core::domain::names::validate_name(label).is_ok());
        if !domain_valid || !(1..=64).contains(&config.max_upload_mib) {
            return Err(AppError::invalid());
        }
        Ok(config)
    }
}
