//! Process configuration. Every secret is a file reference; files are read with
//! a size bound and never echoed.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hermes_core::app::controller::ClusterError;
use hermes_core::app::directory::IdentityDirectory;
use hermes_core::app::error::AppError;
use hermes_core::app::settings::Settings;
use hermes_core::domain::installation::Installation;
use hermes_core::domain::storage::{DataIdentity, StorageSource, StorageSources};
use hermes_http::admin::auth::AdministratorAdmission;
pub use hermes_http::files::read;
use hermes_http::files::{keyring, read_text};
use hermes_identity::browser::{BrowserAuth, BrowserConfig};
use hermes_identity::crypto::{Purpose, Sealer};
use hermes_identity::directory::KeycloakDirectory;
use hermes_identity::oidc::{OidcConfig, OidcProvider};
use hermes_identity::sessions::PgSessions;
use hermes_identity::tokens::{AccessTokenVerifier, TokenPolicy};
use hermes_native::sealing::PublicSealer;
use hermes_render::{Peers, RenderConfig, ServiceEgress};
use hermes_store::PgStore;
use serde::Deserialize;
use zeroize::Zeroizing;

const MAX_CONFIG: usize = 64 * 1024;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    pub installation: Installation,
    pub database_url_file: PathBuf,
    /// Private CA for the identity provider, if it is not publicly trusted.
    #[serde(default)]
    pub oidc_ca_file: Option<PathBuf>,
    /// Where volumes may live. Rendered into the admission policies too.
    #[serde(default)]
    pub storage_sources: Vec<StorageSource>,
    #[serde(default)]
    pub data_identity: DataIdentity,
    #[serde(default)]
    pub administration: Option<Administration>,
    #[serde(default)]
    pub controller: Option<Controller>,
    /// The execution API for the `managed_execution` plugin.
    #[serde(default)]
    pub execution: Option<Listener>,
    /// Route resolution for the gateway, over mutual TLS.
    #[serde(default)]
    pub resolver: Option<Resolver>,
}

/// The resolver listener. Clients must present a certificate from
/// `clientCaFile`; only `gatewayIdentity` (its URI SAN) may resolve.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Resolver {
    pub listen: String,
    pub tls: Tls,
    pub client_ca_file: PathBuf,
    pub gateway_identity: String,
}

/// A TLS listener.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Listener {
    pub listen: String,
    pub tls: Tls,
}

/// The controller role: Kubernetes access, the sealing key and what every
/// instance is rendered with.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Controller {
    /// The manager's namespace; the leader Lease lives here.
    pub namespace: String,
    pub service_account: String,
    pub instance_role: String,
    /// Explicit kubeconfig for tools and tests; in-cluster credentials otherwise.
    #[serde(default)]
    pub kubeconfig: Option<KubeconfigSource>,
    /// The controller's 32-byte X25519 private key.
    pub secret_sealing_private_key_file: PathBuf,
    #[serde(default = "default_lease")]
    pub lease_name: String,
    pub peers: Peers,
    pub execution_url: String,
    pub execution_port: u16,
    pub execution_ca_file: PathBuf,
    pub native_client_id: String,
    pub public_origin: String,
    #[serde(default)]
    pub service_egress: Vec<ServiceEgress>,
    #[serde(default)]
    pub public_https: bool,
    #[serde(default = "default_workers")]
    pub workers: usize,
    #[serde(default = "default_resync")]
    pub resync_seconds: u64,
}

fn default_lease() -> String {
    "hermes-manager-controller".into()
}

fn default_workers() -> usize {
    4
}

fn default_resync() -> u64 {
    30
}

impl Controller {
    pub async fn render_config(&self, config: &Config) -> Result<RenderConfig, AppError> {
        let settings = config.settings()?;
        let execution_ca_pem =
            String::from_utf8(read(&self.execution_ca_file, 64 * 1024).await?.to_vec())
                .map_err(|_| AppError::invalid())?;
        let oidc_ca_pem = match &config.oidc_ca_file {
            Some(path) => Some(
                String::from_utf8(read(path, 64 * 1024).await?.to_vec())
                    .map_err(|_| AppError::invalid())?,
            ),
            None => None,
        };
        let render = RenderConfig {
            installation: settings.installation.clone(),
            sources: settings.sources.clone(),
            data_identity: settings.data_identity,
            control_namespace: self.namespace.clone(),
            controller_service_account: self.service_account.clone(),
            instance_role: self.instance_role.clone(),
            peers: self.peers.clone(),
            execution_url: self.execution_url.clone(),
            execution_port: self.execution_port,
            execution_ca_pem,
            oidc_ca_pem,
            native_client_id: self.native_client_id.clone(),
            public_origin: self.public_origin.clone(),
            service_egress: self.service_egress.clone(),
            public_https: self.public_https,
        };
        render.validate().map_err(|_| AppError::invalid())?;
        if self.workers == 0 || self.workers > 32 || !(5..=3600).contains(&self.resync_seconds) {
            return Err(AppError::invalid());
        }
        Ok(render)
    }

    pub async fn sealing_key(&self) -> Result<Zeroizing<[u8; 32]>, AppError> {
        let bytes = read(&self.secret_sealing_private_key_file, 32).await?;
        let key: [u8; 32] = bytes[..].try_into().map_err(|_| AppError::invalid())?;
        Ok(Zeroizing::new(key))
    }

    pub async fn client(&self) -> Result<hermes_kube::Client, AppError> {
        match &self.kubeconfig {
            Some(source) => hermes_kube::from_kubeconfig(&source.file, &source.context).await,
            None => hermes_kube::incluster(),
        }
        .map_err(|error| match error {
            ClusterError::Rejected => AppError::invalid(),
            _ => AppError::unavailable(),
        })
    }
}

/// A kubeconfig file and the one context in it to use.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KubeconfigSource {
    pub file: PathBuf,
    pub context: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Tls {
    pub certificate_file: PathBuf,
    pub private_key_file: PathBuf,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Administration {
    pub listen: String,
    pub tls: Tls,
    pub tokens: Tokens,
    /// The controller's 32-byte X25519 public key: admin secrets are sealed
    /// to it. The API never holds the private half.
    pub secret_sealing_public_key_file: PathBuf,
    #[serde(default)]
    pub browser: Option<Browser>,
    /// Read-only identity-provider lookup for picking people.
    #[serde(default)]
    pub directory: Option<Directory>,
    /// Built dashboard (`index.html` and `assets/`); needs `browser`.
    #[serde(default)]
    pub dashboard_directory: Option<PathBuf>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Tokens {
    pub jwks_url: String,
    pub audience: String,
    pub authorized_clients: BTreeSet<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Browser {
    /// Connection for the session role, which may differ from the registry's.
    pub database_url_file: PathBuf,
    pub public_origin: String,
    pub other_origin: String,
    pub client_id: String,
    #[serde(default)]
    pub client_secret_file: Option<PathBuf>,
    pub scopes: Vec<String>,
    #[serde(default)]
    pub revocation_url: Option<String>,
    pub keyring_file: PathBuf,
}

/// A Keycloak service client allowed only to view users.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Directory {
    pub client_id: String,
    pub client_secret_file: PathBuf,
}

impl Config {
    pub async fn load(path: &Path) -> Result<Self, AppError> {
        let config: Self = serde_json::from_slice(&read(path, MAX_CONFIG).await?)
            .map_err(|_| AppError::invalid())?;
        config.installation.validate()?;
        config.settings()?;
        Ok(config)
    }

    pub fn settings(&self) -> Result<Arc<Settings>, AppError> {
        let sources = StorageSources::new(self.storage_sources.clone())
            .map_err(AppError::invalid_configuration)?;
        Ok(Arc::new(Settings {
            installation: self.installation.clone(),
            sources,
            data_identity: self.data_identity,
        }))
    }

    /// The registry store. Connection failures never select another store.
    pub async fn store(&self) -> Result<PgStore, AppError> {
        let url = read_text(&self.database_url_file, 16 * 1024).await?;
        Ok(PgStore::connect(&url).await?)
    }

    pub async fn http_client(&self) -> Result<hermes_identity::HttpClient, AppError> {
        let ca = match &self.oidc_ca_file {
            Some(path) => Some(read(path, 64 * 1024).await?),
            None => None,
        };
        Ok(hermes_identity::http_client(
            ca.as_deref().map(|ca| &ca[..]),
        )?)
    }
}

impl Administration {
    pub async fn sealer(&self) -> Result<PublicSealer, AppError> {
        let key = read(&self.secret_sealing_public_key_file, 32).await?;
        let key: [u8; 32] = key[..].try_into().map_err(|_| AppError::invalid())?;
        Ok(PublicSealer::new(key))
    }

    pub async fn directory(
        &self,
        installation: &Installation,
        http: hermes_identity::HttpClient,
    ) -> Result<Option<Arc<dyn IdentityDirectory>>, AppError> {
        let Some(directory) = &self.directory else {
            return Ok(None);
        };
        let secret = read_text(&directory.client_secret_file, 4096).await?;
        let directory = KeycloakDirectory::new(
            &installation.issuer,
            directory.client_id.clone(),
            secret,
            http,
        )?;
        Ok(Some(directory))
    }

    pub fn token_policy(&self, installation: &Installation) -> TokenPolicy {
        TokenPolicy {
            issuer: installation.issuer.clone(),
            jwks_url: self.tokens.jwks_url.clone(),
            audience: self.tokens.audience.clone(),
            authorized_clients: self.tokens.authorized_clients.clone(),
            algorithms: vec![hermes_identity::tokens::Algorithm::RS256],
            maximum_age_seconds: None,
        }
    }

    pub async fn browser(
        &self,
        installation: &Installation,
        tokens: AccessTokenVerifier,
        http: hermes_identity::HttpClient,
        reader: PgStore,
    ) -> Result<Option<BrowserAuth>, AppError> {
        let Some(browser) = &self.browser else {
            return Ok(None);
        };
        if !self.tokens.authorized_clients.contains(&browser.client_id) {
            return Err(AppError::invalid());
        }
        let url = read_text(&browser.database_url_file, 16 * 1024).await?;
        let sessions = PgSessions::connect(&url, Purpose::Administration).await?;
        let (active, keys) = keyring(&browser.keyring_file).await?;
        let sealer = Sealer::new(installation.id, active, keys)?;
        let client_secret = match &browser.client_secret_file {
            Some(path) => Some(read_text(path, 4096).await?.to_string()),
            None => None,
        };
        let callback = url::Url::parse(&browser.public_origin)
            .and_then(|origin| origin.join(BrowserAuth::paths(Purpose::Administration).1))
            .map_err(|_| AppError::invalid())?;
        let provider = OidcProvider::new(
            OidcConfig {
                issuer: installation.issuer.clone(),
                client_id: browser.client_id.clone(),
                callback_url: callback.to_string(),
                scopes: browser.scopes.clone(),
                client_secret,
                revocation_url: browser.revocation_url.clone(),
            },
            http,
        )?;
        let admission = Arc::new(AdministratorAdmission {
            reader,
            installation: installation.clone(),
        });
        let config = BrowserConfig {
            public_origin: browser.public_origin.clone(),
            other_origin: browser.other_origin.clone(),
        };
        Ok(Some(BrowserAuth::new(
            sessions, provider, tokens, sealer, config, admission,
        )?))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// The configurations the chart generates (`tests/test_manager_chart.py`
    /// keeps these fixtures equal to the chart's output).
    #[tokio::test]
    async fn the_charts_role_configurations_load() {
        let fixture = |name: &str| {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/chart/{name}"))
        };
        let api = Config::load(&fixture("api.json")).await.unwrap();
        let administration = api.administration.as_ref().unwrap();
        assert!(administration.browser.is_some() && administration.directory.is_some());
        assert!(api.execution.is_some() && api.resolver.is_some() && api.controller.is_none());
        assert_eq!(api.settings().unwrap().sources.iter().count(), 3);
        let controller = Config::load(&fixture("controller.json")).await.unwrap();
        let section = controller.controller.as_ref().unwrap();
        assert!(controller.administration.is_none() && section.kubeconfig.is_none());
        assert_eq!(
            (section.namespace.as_str(), section.execution_port),
            ("agents", 8445)
        );
        assert_eq!(
            section.peers.gateway["app.kubernetes.io/name"],
            "hermes-gateway"
        );
    }

    #[test]
    fn kubernetes_access_is_explicit_and_takes_no_inline_secret() {
        let parse = |value| serde_json::from_value::<KubeconfigSource>(value);
        assert!(parse(json!({"file": "/run/kube/config", "context": "controller"})).is_ok());
        for refused in [
            json!({}),
            json!({"file": "/run/kube/config"}),
            json!({"file": "/run/kube/config", "context": "controller", "token": "inline-secret"}),
            json!({"source": "inCluster", "token": "inline-secret"}),
        ] {
            assert!(parse(refused).is_err());
        }
    }
}
