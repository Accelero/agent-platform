//! The administration API over real TLS, against a fake realm and a fresh
//! PostgreSQL database. Every API response a test inspects is checked
//! against `contracts/hermes-manager/openapi.yaml`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use hermes_core::app::access::BootstrapAdministrator;
use hermes_core::app::actor::Actor;
use hermes_core::app::directory::{DirectoryResults, DirectoryUser, IdentityDirectory};
use hermes_core::app::envelope::{CommandEnvelope, IdempotencyKey};
use hermes_core::app::error::AppError;
use hermes_core::app::faults::NoFaults;
use hermes_core::app::settings::Settings;
use hermes_core::app::testing;
use hermes_core::domain::identity::Identity;
use hermes_core::domain::installation::Installation;
use hermes_http::admin::assets::DashboardAssets;
use hermes_http::admin::auth::AdministratorAdmission;
use hermes_http::admin::{AdminState, router};
use hermes_http::server::{bind, serve_until, tls_config};
use hermes_identity::browser::{BrowserAuth, BrowserConfig};
use hermes_identity::crypto::{Purpose, Sealer};
use hermes_identity::http_client;
use hermes_identity::oidc::{OidcConfig, OidcProvider};
use hermes_identity::sessions::PgSessions;
use hermes_identity::tokens::{AccessTokenVerifier, TokenPolicy};
use hermes_native::sealing::{PublicSealer, SecretOpener};
use hermes_store::PgStore;
use hermes_testkit::Secrets;
use hermes_testkit::contract::Contract;
use hermes_testkit::provider::FakeProvider;
use jsonwebtoken::Algorithm;
use reqwest::{Method, RequestBuilder};
use serde_json::Value;
use sqlx::PgPool;
use tokio::task::JoinHandle;
use url::Url;
use uuid::Uuid;

pub const PUBLIC: &str = "https://hermes-admin.localhost";
/// The bootstrap administrator, who may also manage administrators.
pub const OWNER: &str = "owner-a";

pub struct Fixture {
    pub secrets: Secrets,
    pub provider: FakeProvider,
    pub store: PgStore,
    /// The fixture database, for reading what the API stored.
    pub pool: PgPool,
    pub settings: Arc<Settings>,
    pub opener: SecretOpener,
    pub url: String,
    pub client: reqwest::Client,
    contract: Contract,
    _dashboard: tempfile::TempDir,
    server: JoinHandle<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

/// One API response, already checked against the contract.
#[derive(Debug)]
pub struct Reply {
    pub status: u16,
    pub etag: Option<String>,
    pub body: Value,
}

impl Reply {
    /// The `ETag` as an `If-Match` value.
    pub fn tag(&self) -> String {
        self.etag.clone().expect("ETag")
    }

    pub fn error(&self) -> &str {
        self.body["error"].as_str().expect("error body")
    }
}

/// A directory that knows two people.
pub struct FixtureDirectory {
    pub issuer: String,
}

#[async_trait]
impl IdentityDirectory for FixtureDirectory {
    async fn search(
        &self,
        query: &str,
        _after: Option<&str>,
        limit: u16,
    ) -> Result<DirectoryResults, AppError> {
        if query == "unavailable" {
            return Err(AppError::unavailable());
        }
        let items = ["bob", "carol"]
            .into_iter()
            .filter(|subject| subject.contains(query))
            .take(usize::from(limit))
            .map(|subject| DirectoryUser {
                identity: Identity {
                    issuer: self.issuer.clone(),
                    subject: subject.into(),
                },
                username: Some(subject.into()),
                display_name: None,
                email: Some(format!("{subject}@example.org")),
                enabled: true,
            })
            .collect();
        Ok(DirectoryResults {
            items,
            next_cursor: None,
        })
    }
}

async fn dashboard() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir(directory.path().join("assets")).unwrap();
    std::fs::write(
        directory.path().join("index.html"),
        "<!doctype html><style nonce=\"__MANAGER_STYLE_NONCE__\"></style><script src=\"/assets/app.js\"></script>",
    )
    .unwrap();
    std::fs::write(
        directory.path().join("assets/app.js"),
        "console.log('dashboard')",
    )
    .unwrap();
    directory
}

impl Fixture {
    pub async fn new(pool: PgPool) -> Self {
        Self::build(pool, true, None).await
    }

    /// Without a configured identity directory.
    pub async fn without_directory(pool: PgPool) -> Self {
        Self::build(pool, false, None).await
    }

    /// Serves the built dashboard in `assets`, with the listener's own URL as
    /// the public origin so a real browser can sign mutations.
    pub async fn serving(pool: PgPool, assets: &std::path::Path) -> Self {
        Self::build(pool, true, Some(assets)).await
    }

    async fn build(pool: PgPool, with_directory: bool, built: Option<&std::path::Path>) -> Self {
        let (listener, address) = bind("127.0.0.1:0").await.unwrap();
        let url = format!("https://{address}");
        let public = if built.is_some() {
            url.clone()
        } else {
            PUBLIC.to_owned()
        };
        let secrets = Secrets::new().await;
        let provider = FakeProvider::start(&secrets, "manager-admin", "manager-api").await;
        let installation = Installation {
            id: Uuid::new_v4(),
            issuer: provider.issuer.clone(),
        };
        let fixture = testing::settings();
        let settings = Arc::new(Settings {
            installation: installation.clone(),
            sources: fixture.sources.clone(),
            data_identity: fixture.data_identity,
        });
        let options = (*pool.connect_options()).clone();
        let database = pool.clone();
        let store = PgStore::connect_with(options.clone()).await.unwrap();
        store
            .initialize(&installation, "fixture-cluster")
            .await
            .unwrap();
        let envelope = CommandEnvelope::new(store.clone(), settings.clone(), Arc::new(NoFaults));
        envelope
            .execute(
                &Actor::Operator,
                Uuid::new_v4(),
                &IdempotencyKey::parse("bootstrap").unwrap(),
                &BootstrapAdministrator {
                    identity: Identity {
                        issuer: provider.issuer.clone(),
                        subject: OWNER.into(),
                    },
                    recover: false,
                },
            )
            .await
            .unwrap();

        let http = http_client(Some(&secrets.read("ca.crt"))).unwrap();
        let tokens = AccessTokenVerifier::new(
            TokenPolicy {
                issuer: provider.issuer.clone(),
                jwks_url: provider.jwks_url(),
                audience: "manager-api".into(),
                authorized_clients: BTreeSet::from(["manager-admin".into()]),
                algorithms: vec![Algorithm::RS256],
                maximum_age_seconds: None,
            },
            http.clone(),
        )
        .unwrap();
        let oidc = OidcProvider::new(
            OidcConfig {
                issuer: provider.issuer.clone(),
                client_id: "manager-admin".into(),
                callback_url: format!("{public}/auth/oidc/callback"),
                scopes: vec!["openid".into()],
                client_secret: None,
                revocation_url: None,
            },
            http,
        )
        .unwrap();
        let sessions = PgSessions::connect_with(options, Purpose::Administration)
            .await
            .unwrap();
        let cookie_sealer = Sealer::new(
            installation.id,
            "key-one".into(),
            BTreeMap::from([("key-one".into(), vec![7; 32])]),
        )
        .unwrap();
        let admission = Arc::new(AdministratorAdmission {
            reader: store.clone(),
            installation: installation.clone(),
        });
        let browser = BrowserAuth::new(
            sessions,
            oidc,
            tokens.clone(),
            cookie_sealer,
            BrowserConfig {
                public_origin: public.clone(),
                other_origin: "https://hermes.localhost".into(),
            },
            admission,
        )
        .unwrap();
        let opener = SecretOpener::new([9; 32]);
        let directory: Option<Arc<dyn IdentityDirectory>> = with_directory.then(|| {
            Arc::new(FixtureDirectory {
                issuer: provider.issuer.clone(),
            }) as Arc<dyn IdentityDirectory>
        });
        let dashboard = dashboard().await;
        let assets = DashboardAssets::load(built.unwrap_or(dashboard.path()))
            .await
            .unwrap();
        let app = router(
            AdminState {
                settings: settings.clone(),
                store: store.clone(),
                envelope,
                tokens,
                browser: Some(browser),
                sealer: Arc::new(PublicSealer::new(opener.public_key())),
                directory,
            },
            Some(assets),
        )
        .unwrap();
        let tls = tls_config(
            &secrets.read("server.crt"),
            &secrets.read("server.key"),
            None,
        )
        .unwrap();
        let server = tokio::spawn(async move {
            serve_until(listener, tls, BTreeSet::new(), app, std::future::pending())
                .await
                .unwrap();
        });
        Self {
            client: secrets.client(None),
            secrets,
            provider,
            store,
            pool: database,
            settings,
            opener,
            url,
            contract: Contract::load("openapi.yaml"),
            _dashboard: dashboard,
            server,
        }
    }

    pub fn get(&self, path: &str) -> RequestBuilder {
        self.client.get(format!("{}{path}", self.url))
    }

    pub fn bearer(&self, subject: &str) -> String {
        format!("Bearer {}", self.provider.access_token(subject))
    }

    pub fn identity(&self, subject: &str) -> Identity {
        Identity {
            issuer: self.provider.issuer.clone(),
            subject: subject.into(),
        }
    }

    pub fn identity_json(&self, subject: &str) -> Value {
        serde_json::to_value(self.identity(subject)).unwrap()
    }

    pub fn envelope(&self) -> CommandEnvelope<PgStore> {
        CommandEnvelope::new(
            self.store.clone(),
            self.settings.clone(),
            Arc::new(NoFaults),
        )
    }

    /// A bearer request as `subject`.
    pub fn as_(&self, subject: &str, method: Method, path: &str) -> RequestBuilder {
        self.client
            .request(method, format!("{}{path}", self.url))
            .header("authorization", self.bearer(subject))
    }

    /// A bearer request as the owner.
    pub fn api(&self, method: Method, path: &str) -> RequestBuilder {
        self.as_(OWNER, method, path)
    }

    /// A JSON mutation as the owner with an idempotency key.
    pub fn mutate(
        &self,
        method: Method,
        path: &str,
        key: &str,
        body: Option<&Value>,
    ) -> RequestBuilder {
        let request = self.api(method, path).header("idempotency-key", key);
        match body {
            Some(body) => request.json(body),
            None => request,
        }
    }

    /// Sends and checks the response against `operation` in the contract.
    pub async fn call(&self, operation: &str, request: RequestBuilder) -> Reply {
        let response = request.send().await.unwrap();
        let status = response.status().as_u16();
        let etag = response
            .headers()
            .get("etag")
            .map(|value| value.to_str().unwrap().to_owned());
        assert_eq!(
            response
                .headers()
                .get("cache-control")
                .map(|v| v.to_str().unwrap()),
            Some("no-store"),
            "{operation} {status}"
        );
        let bytes = response.bytes().await.unwrap();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        };
        self.contract.check(operation, status, &body);
        Reply { status, etag, body }
    }

    /// Signs in through `/auth/login` and the provider. Returns the cookie header.
    pub async fn browser_login(&self) -> Result<String, Box<reqwest::Response>> {
        let login = self
            .get("/auth/login?returnTo=%2Fassignments")
            .send()
            .await
            .unwrap();
        assert_eq!(login.status(), 303);
        let browser = cookie(&login, "__Host-hermes-admin-browser");
        let authorize = login.headers()["location"].to_str().unwrap().to_owned();
        let provider = self
            .secrets
            .client(None)
            .get(authorize)
            .send()
            .await
            .unwrap();
        let callback = Url::parse(provider.headers()["location"].to_str().unwrap()).unwrap();
        let callback = format!(
            "{}{}?{}",
            self.url,
            callback.path(),
            callback.query().unwrap()
        );
        let complete = self
            .client
            .get(callback)
            .header("cookie", format!("__Host-hermes-admin-browser={browser}"))
            .send()
            .await
            .unwrap();
        if complete.status() != 303 {
            return Err(Box::new(complete));
        }
        assert_eq!(complete.headers()["location"], "/assignments");
        let session = cookie(&complete, "__Host-hermes-admin");
        Ok(format!(
            "__Host-hermes-admin-browser={browser}; __Host-hermes-admin={session}"
        ))
    }

    /// Makes `subject` known to the manager by a (refused) sign-in.
    pub async fn sign_in_once(&self, subject: &str) {
        let response = self
            .as_(subject, Method::GET, "/api/v1/session")
            .send()
            .await
            .unwrap();
        assert!(matches!(response.status().as_u16(), 200 | 403));
    }
}

pub fn cookie(response: &reqwest::Response, name: &str) -> String {
    response
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .filter_map(|value| value.strip_prefix(&format!("{name}=")))
        .map(|value| value.split(';').next().unwrap().to_owned())
        .find(|value| !value.is_empty())
        .expect("cookie set")
}

pub async fn body(response: reqwest::Response) -> Value {
    response.json().await.unwrap()
}

/// The valid template fixture from the contract schemas.
pub fn template_fixture() -> Value {
    fixture("schemas/fixtures/valid/template.json")
}

pub fn backend_fixture() -> Value {
    fixture("schemas/fixtures/valid/terminal-backend.json")
}

fn fixture(name: &str) -> Value {
    let path = format!(
        "{}/../../../../contracts/hermes-manager/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}
