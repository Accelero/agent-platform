//! The gateway as configured from files: OIDC sign-in against a fake realm,
//! sealed sessions in PostgreSQL, the real resolver router over mutual TLS,
//! and two loopback instances on one hostname.

use std::collections::{BTreeSet, HashMap};
use std::net::SocketAddr;
use std::process::Command;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::Router;
use axum::extract::{Request, State};
use axum::response::IntoResponse;
use hermes_core::app::error::StoreError;
use hermes_core::app::routing::{RouteStore, Routing};
use hermes_core::domain::assignments::Access;
use hermes_core::domain::identity::Identity;
use hermes_core::domain::installation::Installation;
use hermes_core::domain::instances::Phase;
use hermes_core::domain::routing::{Destination, RouteRow};
use hermes_gateway::config::Config;
use hermes_http::server::{bind, serve_until, tls_config};
use hermes_proxy::gateway::Dial;
use hermes_testkit::contract::Contract;
use hermes_testkit::provider::FakeProvider;
use hermes_testkit::{GATEWAY_PEER, Secrets};
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::net::TcpListener;
use url::Url;
use uuid::Uuid;

const PUBLIC: &str = "https://hermes.localhost";
const SESSION: &str = "__Host-hermes-platform";
const BROWSER: &str = "__Host-hermes-platform-browser";

/// Registry rows by identity, as the controller would have recorded them.
#[derive(Clone, Default)]
struct Rows(Arc<Mutex<HashMap<Identity, RouteRow>>>);

#[async_trait]
impl RouteStore for Rows {
    async fn route_row(&self, identity: &Identity) -> Result<Option<RouteRow>, StoreError> {
        Ok(self.0.lock().unwrap().get(identity).cloned())
    }
}

fn ready(installation: &Installation) -> RouteRow {
    let instance_id = Uuid::new_v4();
    RouteRow {
        instance_id,
        namespace: installation.instance_namespace(instance_id),
        namespace_bound: true,
        access: Access::Enabled,
        phase: Phase::Ready,
        desired_generation: 1,
        observed_generation: 1,
        assignment_version: 1,
    }
}

async fn instance(name: &'static str) -> SocketAddr {
    let app = Router::new()
        .fallback(
            |State(name): State<&'static str>, request: Request| async move {
                let cookie = request
                    .headers()
                    .get("cookie")
                    .map(|v| v.to_str().unwrap().to_owned());
                axum::Json(
                    json!({"instance": name, "path": request.uri().path(), "cookie": cookie}),
                )
                .into_response()
            },
        )
        .with_state(name);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    address
}

fn cookie(response: &reqwest::Response, name: &str) -> String {
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

struct Fixture {
    url: String,
    secrets: Secrets,
    provider: FakeProvider,
    rows: Rows,
    installation: Installation,
    client: reqwest::Client,
    directory: tempfile::TempDir,
    config: Value,
}

impl Fixture {
    async fn new(pool: &PgPool) -> Self {
        let secrets = Secrets::new().await;
        let provider = FakeProvider::start(&secrets, "hermes-gateway", "hermes-gateway").await;
        let installation = Installation {
            id: Uuid::new_v4(),
            issuer: provider.issuer.clone(),
        };
        let rows = Rows::default();
        let mut addresses = HashMap::new();
        for name in ["bob", "carol"] {
            let row = ready(&installation);
            addresses.insert(row.namespace.clone(), instance(name).await);
            rows.0.lock().unwrap().insert(
                Identity {
                    issuer: installation.issuer.clone(),
                    subject: name.into(),
                },
                row,
            );
        }

        // The manager's resolver listener.
        let resolver = hermes_http::resolver::router(
            Routing::new(rows.clone(), installation.clone()),
            GATEWAY_PEER.to_owned(),
        );
        let (listener, address) = bind("127.0.0.1:0").await.unwrap();
        let tls = tls_config(
            &secrets.read("server.crt"),
            &secrets.read("server.key"),
            Some(&secrets.read("ca.crt")),
        )
        .unwrap();
        tokio::spawn(async move {
            serve_until(
                listener,
                tls,
                BTreeSet::from([GATEWAY_PEER.to_owned()]),
                resolver,
                std::future::pending(),
            )
            .await
            .unwrap();
        });

        let directory = tempfile::tempdir().unwrap();
        let path = |name: &str| directory.path().join(name);
        let options = pool.connect_options();
        let database = format!(
            "postgres://{}@{}:{}/{}",
            options.get_username(),
            options.get_host(),
            options.get_port(),
            options.get_database().unwrap()
        );
        std::fs::write(path("database-url"), database).unwrap();
        std::fs::write(path("key-one"), [7u8; 32]).unwrap();
        std::fs::write(
            path("keyring.json"),
            json!({"activeKeyId": "key-one", "keyFiles": {"key-one": path("key-one")}}).to_string(),
        )
        .unwrap();
        let config = json!({
            "installation": installation,
            "listen": "127.0.0.1:0",
            "tls": {"certificateFile": secrets.path("server.crt"), "privateKeyFile": secrets.path("server.key")},
            "publicOrigin": PUBLIC,
            "adminOrigin": "https://hermes-admin.localhost",
            "databaseUrlFile": path("database-url"),
            "oidcCaFile": secrets.path("ca.crt"),
            "oidc": {
                "clientId": "hermes-gateway",
                "scopes": ["openid"],
                "jwksUrl": provider.jwks_url(),
                "audience": "hermes-gateway",
            },
            "keyringFile": path("keyring.json"),
            "resolver": {
                "url": format!("https://{address}/"),
                "caFile": secrets.path("ca.crt"),
                "certificateFile": secrets.path("gateway.crt"),
                "privateKeyFile": secrets.path("gateway.key"),
            },
        });
        std::fs::write(path("gateway.json"), config.to_string()).unwrap();
        let loaded = Config::load(&path("gateway.json")).await.unwrap();
        let auth = hermes_gateway::sessions(&loaded).await.unwrap();
        let dial: Dial = Arc::new(move |destination: &Destination| {
            addresses
                .get(&destination.namespace)
                .map(|a| a.to_string())
                .unwrap_or_else(|| "127.0.0.1:9".into())
        });
        let app = hermes_gateway::app(&loaded, auth, dial).await.unwrap();
        let (listener, address) = bind("127.0.0.1:0").await.unwrap();
        let tls = tls_config(
            &secrets.read("server.crt"),
            &secrets.read("server.key"),
            None,
        )
        .unwrap();
        tokio::spawn(async move {
            serve_until(listener, tls, BTreeSet::new(), app, std::future::pending())
                .await
                .unwrap();
        });
        Self {
            url: format!("https://{address}"),
            client: secrets.client(None),
            secrets,
            provider,
            rows,
            installation,
            directory,
            config,
        }
    }

    /// Signs `subject` in from a protected page; returns the cookie header.
    async fn sign_in(&self, subject: &str) -> String {
        self.provider.set_subject(subject);
        let page = self
            .client
            .get(format!("{}/files/report?tab=1", self.url))
            .header("accept", "text/html")
            .send()
            .await
            .unwrap();
        assert_eq!(page.status(), 303);
        let login = page.headers()["location"].to_str().unwrap().to_owned();
        assert_eq!(
            login,
            "/_platform/login?returnTo=%2Ffiles%2Freport%3Ftab%3D1"
        );
        let login = self
            .client
            .get(format!("{}{login}", self.url))
            .send()
            .await
            .unwrap();
        assert_eq!(login.status(), 303);
        let browser = cookie(&login, BROWSER);
        let authorize = login.headers()["location"].to_str().unwrap().to_owned();
        let provider = self
            .secrets
            .client(None)
            .get(authorize)
            .send()
            .await
            .unwrap();
        let callback = Url::parse(provider.headers()["location"].to_str().unwrap()).unwrap();
        assert_eq!(
            callback.as_str().split('?').next().unwrap(),
            format!("{PUBLIC}/_platform/oidc/callback")
        );
        let complete = self
            .client
            .get(format!(
                "{}{}?{}",
                self.url,
                callback.path(),
                callback.query().unwrap()
            ))
            .header("cookie", format!("{BROWSER}={browser}"))
            .send()
            .await
            .unwrap();
        assert_eq!(complete.status(), 303);
        assert_eq!(complete.headers()["location"], "/files/report?tab=1");
        let session = cookie(&complete, SESSION);
        format!("{BROWSER}={browser}; {SESSION}={session}")
    }

    async fn get(&self, cookies: &str, path: &str) -> reqwest::Response {
        self.client
            .get(format!("{}{path}", self.url))
            .header("cookie", cookies)
            .send()
            .await
            .unwrap()
    }
}

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn two_owners_on_one_hostname_reach_only_their_own_instances(pool: PgPool) {
    let fixture = Fixture::new(&pool).await;
    let contract = Contract::load("gateway.openapi.yaml");
    let bob = fixture.sign_in("bob").await;
    let carol = fixture.sign_in("carol").await;
    for (cookies, owner) in [(&bob, "bob"), (&carol, "carol")] {
        let response = fixture
            .get(&format!("{cookies}; hermes_session_at=native"), "/api/chat")
            .await;
        assert_eq!(response.status(), 200);
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["instance"], owner);
        assert_eq!(body["cookie"], "hermes_session_at=native");
        let me = fixture.get(cookies, "/_platform/me").await;
        assert_eq!(me.headers()["cache-control"], "no-store");
        let me: Value = me.json().await.unwrap();
        contract.check("getPlatformSession", 200, &me);
        assert_eq!(me["identity"]["subject"], owner);
        assert_eq!(me["identity"]["issuer"], json!(fixture.installation.issuer));
    }
    // A session cookie with another browser's proof is no session.
    let mixed = format!(
        "{BROWSER}={}; {SESSION}={}",
        bob.split("; ").next().unwrap().split('=').nth(1).unwrap(),
        carol.split("; ").nth(1).unwrap().split('=').nth(1).unwrap()
    );
    assert_eq!(fixture.get(&mixed, "/api/chat").await.status(), 401);

    // A valid identity without an assignment gets the denial page.
    let mallory = fixture.sign_in("mallory").await;
    let denied = fixture
        .client
        .get(format!("{}/", fixture.url))
        .header("cookie", &mallory)
        .header("accept", "text/html")
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), 403);

    // Revoking Bob's access denies him once the cached decision expires.
    let bob_identity = Identity {
        issuer: fixture.installation.issuer.clone(),
        subject: "bob".into(),
    };
    fixture
        .rows
        .0
        .lock()
        .unwrap()
        .get_mut(&bob_identity)
        .unwrap()
        .access = Access::Suspended;
    tokio::time::sleep(std::time::Duration::from_millis(5100)).await;
    assert_eq!(fixture.get(&bob, "/api/chat").await.status(), 403);
    assert_eq!(fixture.get(&carol, "/api/chat").await.status(), 200);
}

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn platform_logout_needs_origin_and_csrf_and_expires_native_cookies(pool: PgPool) {
    let fixture = Fixture::new(&pool).await;
    let contract = Contract::load("gateway.openapi.yaml");
    let bob = fixture.sign_in("bob").await;
    let me: Value = fixture
        .get(&bob, "/_platform/me")
        .await
        .json()
        .await
        .unwrap();
    let csrf = me["csrfToken"].as_str().unwrap().to_owned();
    let logout = |origin: &'static str, token: String| {
        fixture
            .client
            .post(format!("{}/_platform/logout", fixture.url))
            .header("cookie", &bob)
            .header("origin", origin)
            .header("x-csrf-token", token)
            .send()
    };
    let wrong_origin = logout("https://attacker.example.org", csrf.clone())
        .await
        .unwrap();
    assert_eq!(wrong_origin.status(), 403);
    contract.check("logoutPlatform", 403, &wrong_origin.json().await.unwrap());
    let wrong_token = logout(PUBLIC, "0".repeat(64)).await.unwrap();
    assert_eq!(wrong_token.status(), 403);
    assert_eq!(fixture.get(&bob, "/api/chat").await.status(), 200);

    let done = logout(PUBLIC, csrf).await.unwrap();
    assert_eq!(done.status(), 204);
    let expired: Vec<_> = done
        .headers()
        .get_all("set-cookie")
        .iter()
        .map(|v| v.to_str().unwrap().to_owned())
        .collect();
    for name in [
        SESSION,
        BROWSER,
        "__Host-hermes_session_at",
        "hermes_session_rt",
    ] {
        assert!(
            expired
                .iter()
                .any(|c| c.starts_with(&format!("{name}=;")) && c.contains("Max-Age=0")),
            "{name}"
        );
    }
    let after = fixture.get(&bob, "/api/chat").await;
    assert_eq!(after.status(), 401);
    contract.check("getPlatformSession", 401, &after.json().await.unwrap());
}

#[test]
fn the_binary_reports_bounded_codes_without_paths() {
    let binary = env!("CARGO_BIN_EXE_hermes-gateway");
    let run = |args: &[&str]| {
        let output = Command::new(binary).args(args).output().unwrap();
        assert!(!output.status.success());
        String::from_utf8(output.stderr).unwrap()
    };
    assert_eq!(run(&[]), "hermes-gateway: invalid_request\n");
    assert_eq!(
        run(&["--config", "/run/missing/super-secret-path"]),
        "hermes-gateway: invalid_request\n"
    );
}

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn configuration_is_strict(pool: PgPool) {
    let fixture = Fixture::new(&pool).await;
    let write = |value: &Value| {
        let path = fixture.directory.path().join("variant.json");
        std::fs::write(&path, value.to_string()).unwrap();
        path
    };
    let mut unknown = fixture.config.clone();
    unknown["upstreamOverride"] = json!("127.0.0.1:1");
    let mut upload = fixture.config.clone();
    upload["maxUploadMib"] = json!(65);
    let mut domain = fixture.config.clone();
    domain["clusterDomain"] = json!("cluster.local/evil");
    for variant in [unknown, upload, domain] {
        assert!(Config::load(&write(&variant)).await.is_err());
    }
    let mut same_host = fixture.config.clone();
    same_host["adminOrigin"] = json!(PUBLIC);
    let loaded = Config::load(&write(&same_host)).await.unwrap();
    assert!(hermes_gateway::sessions(&loaded).await.is_err());
}

/// The configuration the chart generates (`tests/test_manager_chart.py` keeps
/// this fixture equal to the chart's output).
#[tokio::test]
async fn the_charts_gateway_configuration_loads() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/chart/gateway.json");
    let config = Config::load(&path).await.unwrap();
    assert_eq!(config.public_origin, "https://hermes.example.org");
    assert_eq!(config.cluster_domain, "cluster.local");
    assert_eq!(config.max_upload_mib, 64);
    assert!(
        config
            .resolver
            .url
            .starts_with("https://hermes-manager-api.")
    );
}
