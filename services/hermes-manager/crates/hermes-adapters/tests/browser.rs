//! Real HTTPS OIDC protocol, opaque browser sessions and administration CSRF.
#[path = "support/grants.rs"]
mod grant_fixture;
use axum::{
    Form, Json, Router,
    extract::{Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hermes_adapters::{
    auth::{AccessTokenVerifier, TokenPolicy, http_client, unix_time},
    browser_auth::{BrowserAuth, BrowserConfig},
    http::{DashboardAssets, admin_router_with_dashboard},
    oidc::{OidcConfig, OidcProvider},
    postgres::{FreshInventory, PgStore},
    router_auth::{RouterAuthConfig, routers},
    session_crypto::{Purpose, Sealer, random_handle},
    sessions::PgSessions,
};
use hermes_application::{AdminPolicy, InstallationScope};
use jsonwebtoken::{Algorithm, Header, encode};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Mutex, RwLock};
use url::Url;
use uuid::Uuid;

#[allow(dead_code)]
mod support;
use support::{FORWARD, ROUTER, Running, Secrets, start, start_with_peers};

#[path = "browser/execution.rs"]
mod execution;

struct Grant {
    nonce: String,
    challenge: String,
    subject: String,
}
struct Provider {
    secrets: Secrets,
    client_id: String,
    audience: String,
    issuer: RwLock<String>,
    subject: RwLock<String>,
    fault: RwLock<String>,
    codes: Mutex<HashMap<String, Grant>>,
    refresh: Mutex<HashMap<String, String>>,
    exchanges: AtomicUsize,
    refreshes: AtomicUsize,
}
async fn metadata(State(p): State<Arc<Provider>>) -> Json<Value> {
    let issuer = p.issuer.read().await.clone();
    Json(
        json!({"issuer":issuer,"authorization_endpoint":format!("{issuer}/authorize"),
        "token_endpoint":format!("{issuer}/token"),"jwks_uri":format!("{issuer}/jwks"),
        "response_types_supported":["code"],"subject_types_supported":[if *p.fault.read().await=="pairwise"{"pairwise"}else{"public"}],
        "id_token_signing_alg_values_supported":["RS256"],"code_challenge_methods_supported":["S256"]}),
    )
}
async fn authorize(
    State(p): State<Arc<Provider>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    assert_eq!(q.get("client_id").unwrap(), &p.client_id);
    assert_eq!(q.get("response_type").unwrap(), "code");
    assert_eq!(q.get("code_challenge_method").unwrap(), "S256");
    let code = random_handle().unwrap();
    p.codes.lock().await.insert(
        code.clone(),
        Grant {
            nonce: q["nonce"].clone(),
            challenge: q["code_challenge"].clone(),
            subject: p.subject.read().await.clone(),
        },
    );
    let mut callback = Url::parse(&q["redirect_uri"]).unwrap();
    callback
        .query_pairs_mut()
        .append_pair("code", &code)
        .append_pair("state", &q["state"]);
    (
        StatusCode::SEE_OTHER,
        [(header::LOCATION, callback.to_string())],
    )
        .into_response()
}
async fn token(
    State(p): State<Arc<Provider>>,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    let grant_type = form["grant_type"].as_str();
    let (subject, nonce) = if grant_type == "authorization_code" {
        p.exchanges.fetch_add(1, Ordering::SeqCst);
        let Some(grant) = p.codes.lock().await.remove(&form["code"]) else {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"invalid_grant"})),
            )
                .into_response();
        };
        if URL_SAFE_NO_PAD.encode(Sha256::digest(form["code_verifier"].as_bytes()))
            != grant.challenge
        {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"invalid_grant"})),
            )
                .into_response();
        }
        (grant.subject, Some(grant.nonce))
    } else {
        assert_eq!(grant_type, "refresh_token");
        p.refreshes.fetch_add(1, Ordering::SeqCst);
        let subject = p.refresh.lock().await.remove(&form["refresh_token"]);
        if *p.fault.read().await == "revoked" || subject.is_none() {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"invalid_grant"})),
            )
                .into_response();
        }
        (subject.unwrap(), None)
    };
    let fault = p.fault.read().await.clone();
    let issuer = p.issuer.read().await.clone();
    let now = unix_time().unwrap();
    let subject = if fault == "refresh-subject" && grant_type == "refresh_token" {
        "owner-b".into()
    } else {
        subject
    };
    let mut access = json!({"iss":issuer,"sub":subject,"aud":p.audience,"azp":p.client_id,"iat":now,"exp":now+300,"nbf":now-1,"typ":"Bearer","sid":"provider-session-one","resource_access":{"manager-admin":{"roles":["platform-administrator"]}}});
    if fault == "access-subject" {
        access["sub"] = json!("foreign");
    }
    if fault == "audience" {
        access["aud"] = json!("wrong-api");
    }
    if fault == "client" {
        access["azp"] = json!("external-panel");
    }
    if fault == "role" {
        access["resource_access"] = json!({});
    }
    if fault == "stale" {
        access["iat"] = json!(now - 121);
    }
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some("fixture".into());
    let signer = if fault == "signature" {
        &p.secrets.foreign_signer
    } else {
        &p.secrets.signer
    };
    let access = encode(&header, &access, signer).unwrap();
    let digest = Sha256::digest(access.as_bytes());
    let mut id = json!({"iss":issuer,"sub":subject,"aud":p.client_id,"iat":now,"exp":now+300,"at_hash":URL_SAFE_NO_PAD.encode(&digest[..16])});
    if let Some(nonce) = nonce {
        id["nonce"] = json!(if fault == "nonce" {
            "incorrect".into()
        } else {
            nonce
        });
    }
    if fault == "hash" {
        id["at_hash"] = json!(URL_SAFE_NO_PAD.encode([0u8; 16]));
    }
    let id = encode(&header, &id, signer).unwrap();
    let refresh = random_handle().unwrap();
    p.refresh.lock().await.insert(refresh.clone(), subject);
    Json(json!({"access_token":access,"id_token":id,"token_type":"Bearer","expires_in":300,"refresh_token":refresh})).into_response()
}

struct Fixture {
    provider: Arc<Provider>,
    _oidc: Running,
    admin: Running,
    internal: Option<Running>,
    client: reqwest::Client,
    auth: BrowserAuth,
}
impl Fixture {
    async fn new(pool: &PgPool) -> Self {
        Self::for_purpose(pool, Purpose::Administration).await
    }
    async fn for_purpose(pool: &PgPool, purpose: Purpose) -> Self {
        Self::with_dashboard(pool, purpose, None).await
    }
    async fn with_dashboard(
        pool: &PgPool,
        purpose: Purpose,
        assets: Option<DashboardAssets>,
    ) -> Self {
        Self::with_dashboard_at(pool, purpose, assets, None).await
    }
    async fn with_dashboard_at(
        pool: &PgPool,
        purpose: Purpose,
        assets: Option<DashboardAssets>,
        listener: Option<tokio::net::TcpListener>,
    ) -> Self {
        let public_origin = listener
            .as_ref()
            .map(|listener| format!("https://{}", listener.local_addr().unwrap()))
            .unwrap_or_else(|| "https://localhost".into());
        let (client_id, audience, callback_path) = match purpose {
            Purpose::Administration => ("manager-admin", "manager-api", "/auth/oidc/callback"),
            Purpose::Router => ("hermes-router", "route-api", "/_platform/oidc/callback"),
        };
        let p = Arc::new(Provider {
            secrets: Secrets::new().await,
            client_id: client_id.into(),
            audience: audience.into(),
            issuer: RwLock::new(String::new()),
            subject: RwLock::new("owner-a".into()),
            fault: RwLock::new(String::new()),
            codes: Mutex::new(HashMap::new()),
            refresh: Mutex::new(HashMap::new()),
            exchanges: AtomicUsize::new(0),
            refreshes: AtomicUsize::new(0),
        });
        let jwks = json!({"keys":[p.secrets.jwk.clone()]});
        let oidc = start(
            &p.secrets,
            Router::new()
                .route(
                    "/realms/agents/.well-known/openid-configuration",
                    get(metadata),
                )
                .route(
                    "/realms/agents/jwks",
                    get(move || {
                        let data = jwks.clone();
                        async move { Json(data) }
                    }),
                )
                .route("/realms/agents/authorize", get(authorize))
                .route("/realms/agents/token", post(token))
                .with_state(p.clone()),
            false,
        )
        .await;
        let issuer = format!("{}/realms/agents", oidc.url);
        *p.issuer.write().await = issuer.clone();
        let scope = InstallationScope {
            id: Uuid::new_v4(),
            epoch: 1,
            issuer: issuer.clone(),
        };
        let store = PgStore::connect_with((*pool.connect_options()).clone())
            .await
            .unwrap();
        store
            .initialize_fresh(
                &scope,
                FreshInventory {
                    cluster_identity: "browser-test".into(),
                    digest: "a".repeat(64),
                    registered_instances: 0,
                    unexplained_resources: 0,
                },
            )
            .await
            .unwrap();
        grant_fixture::grant(pool, &scope, &["owner-a", "owner-b"]).await;
        sqlx::query("INSERT INTO manager.instance_templates(id,version,schema_version,image,content_digest,approved) VALUES('personal-default',1,1,$1,$2,true)").bind(format!("ghcr.io/example/hermes@sha256:{}","a".repeat(64))).bind("a".repeat(64)).execute(pool).await.unwrap();
        let http = http_client(Some(&p.secrets.read("ca.crt").await)).unwrap();
        let tokens = AccessTokenVerifier::new(
            TokenPolicy {
                issuer: issuer.clone(),
                jwks_url: format!("{issuer}/jwks"),
                audience: audience.into(),
                authorized_clients: if purpose == Purpose::Administration {
                    BTreeSet::from([client_id.into(), "external-panel".into()])
                } else {
                    BTreeSet::from([client_id.into()])
                },
                algorithms: vec![Algorithm::RS256],
                maximum_age_seconds: None,
            },
            http.clone(),
        )
        .unwrap();
        let policy = AdminPolicy {
            issuer: issuer.clone(),
            audience: "manager-api".into(),
            management_client: "manager-admin".into(),
            administrator_role: "platform-administrator".into(),
        };
        let provider = OidcProvider::new(
            OidcConfig {
                issuer,
                client_id: client_id.into(),
                callback_url: format!("{public_origin}{callback_path}"),
                scopes: vec!["openid".into()],
                client_secret: None,
                revocation_url: None,
            },
            http,
        )
        .unwrap();
        let sessions =
            PgSessions::connect((*pool.connect_options()).clone(), scope.clone(), purpose)
                .await
                .unwrap();
        let auth = BrowserAuth::new(
            sessions,
            provider,
            tokens.clone(),
            Sealer::new(
                "key-one".into(),
                BTreeMap::from([("key-one".into(), vec![42; 32])]),
            )
            .unwrap(),
            BrowserConfig {
                public_origin,
                other_origin: "https://hermes.example.org".into(),
            },
            (purpose == Purpose::Administration).then_some(policy.clone()),
        )
        .unwrap();
        let (public, internal) = if purpose == Purpose::Administration {
            (
                admin_router_with_dashboard(
                    store,
                    scope,
                    policy,
                    tokens,
                    Some(auth.clone()),
                    assets,
                )
                .unwrap(),
                None,
            )
        } else {
            let (public, internal) = routers(
                auth.clone(),
                RouterAuthConfig {
                    forward_auth_identity: FORWARD.into(),
                    session_plugin_identity: ROUTER.into(),
                },
            )
            .unwrap();
            let server = start_with_peers(
                &p.secrets,
                internal,
                true,
                BTreeSet::from([FORWARD.into(), ROUTER.into()]),
            )
            .await;
            (public, Some(server))
        };
        let admin = if let Some(listener) = listener {
            support::start_on_listener(&p.secrets, public, false, BTreeSet::new(), listener).await
        } else {
            start(&p.secrets, public, false).await
        };
        let client = p.secrets.client(None).await;
        Self {
            provider: p,
            _oidc: oidc,
            admin,
            internal,
            client,
            auth,
        }
    }
    async fn begin(&self, browser: Option<&str>) -> (String, String) {
        let mut request = self.client.get(format!(
            "{}{}?returnTo=%2Fassignments",
            self.admin.url,
            self.auth.login_path()
        ));
        if let Some(browser) = browser {
            request = request.header(
                header::COOKIE,
                format!("{}={browser}", self.auth.browser_cookie()),
            );
        }
        let response = request.send().await.unwrap();
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        let location = response.headers()[header::LOCATION]
            .to_str()
            .unwrap()
            .to_string();
        let browser = cookie_value(&response, self.auth.browser_cookie());
        (browser, location)
    }
    async fn callback(&self, authorization: &str) -> String {
        let response = self.client.get(authorization).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let url = Url::parse(response.headers()[header::LOCATION].to_str().unwrap()).unwrap();
        format!("{}{}?{}", self.admin.url, url.path(), url.query().unwrap())
    }
    async fn complete(&self, browser: &str, callback: &str) -> reqwest::Response {
        self.client
            .get(callback)
            .header(
                header::COOKIE,
                format!("{}={browser}", self.auth.browser_cookie()),
            )
            .send()
            .await
            .unwrap()
    }
    async fn login(&self) -> (String, String) {
        let (browser, authorization) = self.begin(None).await;
        let callback = self.callback(&authorization).await;
        let response = self.complete(&browser, &callback).await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(response.headers()[header::LOCATION], "/assignments");
        let session = cookie_value(&response, self.auth.session_cookie());
        (browser, session)
    }
    async fn session(&self, browser: &str, session: &str) -> reqwest::Response {
        self.client
            .get(format!("{}/api/v1/session", self.admin.url))
            .header(header::COOKIE, cookies(browser, session))
            .send()
            .await
            .unwrap()
    }
}
fn cookie_value(response: &reqwest::Response, name: &str) -> String {
    for value in response.headers().get_all(header::SET_COOKIE) {
        let value = value.to_str().unwrap();
        assert!(value.contains("; Secure; HttpOnly; SameSite=Lax;"));
        assert!(!value.contains("Domain="));
        assert!(value.contains("Path=/"));
        if let Some(value) = value.strip_prefix(&format!("{name}=")) {
            return value.split(';').next().unwrap().into();
        }
    }
    panic!("missing fixture cookie")
}
fn cookies(browser: &str, session: &str) -> String {
    format!("__Host-hermes-admin-browser={browser}; __Host-hermes-admin={session}")
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn dashboard_pages_assets_postgres_grants_logout_and_api_only_boundary(pool: PgPool) {
    let directory = tempfile::tempdir().unwrap();
    tokio::fs::create_dir(directory.path().join("assets"))
        .await
        .unwrap();
    tokio::fs::write(
        directory.path().join("index.html"),
        "<!doctype html><title>Dashboard fixture</title>",
    )
    .await
    .unwrap();
    tokio::fs::write(
        directory.path().join("assets/app.js"),
        "export const fixture = true;",
    )
    .await
    .unwrap();
    let assets = DashboardAssets::load(directory.path()).await.unwrap();
    let f = Fixture::with_dashboard(&pool, Purpose::Administration, Some(assets)).await;
    let root = f
        .client
        .get(format!("{}/", f.admin.url))
        .send()
        .await
        .unwrap();
    assert_eq!(root.status(), StatusCode::SEE_OTHER);
    assert_eq!(root.headers()[header::LOCATION], "/auth/login?returnTo=%2F");
    let script = f
        .client
        .get(format!("{}/assets/app.js", f.admin.url))
        .send()
        .await
        .unwrap();
    assert_eq!(script.status(), StatusCode::UNAUTHORIZED);
    // A valid signed identity without a PostgreSQL grant cannot create an
    // administration session or retrieve the dashboard.
    *f.provider.subject.write().await = "ungranted-fixture".into();
    let (browser, authorization) = f.begin(None).await;
    let callback = f.callback(&authorization).await;
    assert_eq!(
        f.complete(&browser, &callback).await.status(),
        StatusCode::FORBIDDEN
    );
    *f.provider.subject.write().await = "owner-a".into();
    *f.provider.fault.write().await = String::new();
    let (browser, session) = f.login().await;
    for path in [
        "/",
        "/assignments",
        "/assignments/00000000-0000-0000-0000-000000000001",
        "/users",
        "/users/00000000-0000-0000-0000-000000000001",
        "/operations/00000000-0000-0000-0000-000000000001",
        "/activity/00000000-0000-0000-0000-000000000001",
        "/templates",
        "/storage-bindings",
        "/storage-bindings/00000000-0000-0000-0000-000000000001",
        "/terminal-runtimes",
        "/terminal-runtimes/fixture-runtime",
        "/activity",
        "/assets/app.js",
    ] {
        let response = f
            .client
            .get(format!("{}{path}", f.admin.url))
            .header(header::COOKIE, cookies(&browser, &session))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        assert!(
            response.headers()[header::CONTENT_SECURITY_POLICY]
                .to_str()
                .unwrap()
                .contains("frame-ancestors 'none'")
        );
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            if path.starts_with("/assets/") {
                "text/javascript; charset=utf-8"
            } else {
                "text/html; charset=utf-8"
            }
        );
    }
    for path in [
        "/api/v1/missing",
        "/auth/missing",
        "/internal/missing",
        "/unknown",
        "/users/x/y",
        "/assignments/invalid",
        "/terminal-runtimes/INVALID",
        "/api/v1/nope",
        "/assets/missing.js",
        "/assets/%2e%2e%2findex.html",
    ] {
        let response = f
            .client
            .get(format!("{}{path}", f.admin.url))
            .header(header::COOKIE, cookies(&browser, &session))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        assert!(
            response.headers()[header::CONTENT_TYPE]
                .to_str()
                .unwrap()
                .starts_with("application/json")
        );
    }
    let identity: Value = f.session(&browser, &session).await.json().await.unwrap();
    let response = f
        .client
        .post(format!("{}/auth/logout", f.admin.url))
        .header(header::COOKIE, cookies(&browser, &session))
        .header(header::ORIGIN, "https://localhost")
        .header("x-csrf-token", identity["csrfToken"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let signed_out = f
        .client
        .get(format!("{}/auth/signed-out", f.admin.url))
        .send()
        .await
        .unwrap();
    assert_eq!(signed_out.status(), StatusCode::OK);
    assert_eq!(signed_out.headers()[header::CACHE_CONTROL], "no-store");
    assert!(!signed_out.headers().contains_key(header::LOCATION));
    assert!(
        signed_out
            .text()
            .await
            .unwrap()
            .contains("Your manager session has ended")
    );
    for path in ["/", "/assets/app.js"] {
        assert_eq!(
            f.client
                .get(format!("{}{path}", f.admin.url))
                .header(header::COOKIE, cookies(&browser, &session))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
    }
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn browser_oidc_pkce_login_session_csrf_mutation_and_logout(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    assert_eq!(
        f.client
            .get(format!("{}/", f.admin.url))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    let (browser, session) = f.login().await;
    assert_eq!(browser.len(), 64);
    assert_eq!(session.len(), 64);
    let response = f.session(&browser, &session).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["actor"]["subject"], "owner-a");
    let csrf = body["csrfToken"].as_str().unwrap();
    assert_eq!(csrf.len(), 64);
    assert!(!body.to_string().contains("access_token"));
    let assignment = json!({"identity":{"issuer":f.provider.issuer.read().await.clone(),"subject":"new-owner"},"access":"enabled","templateId":"personal-default"});
    for (origin, token, status) in [
        (None, None, StatusCode::FORBIDDEN),
        (
            Some("https://foreign.example.org"),
            Some(csrf),
            StatusCode::FORBIDDEN,
        ),
        (
            Some("https://localhost"),
            Some("incorrect"),
            StatusCode::FORBIDDEN,
        ),
        (Some("https://localhost"), Some(csrf), StatusCode::ACCEPTED),
    ] {
        let mut request = f
            .client
            .post(format!("{}/api/v1/assignments", f.admin.url))
            .header(header::COOKIE, cookies(&browser, &session))
            .header("idempotency-key", "browser-create")
            .json(&assignment);
        if let Some(origin) = origin {
            request = request.header(header::ORIGIN, origin);
        }
        if let Some(token) = token {
            request = request.header("x-csrf-token", token);
        }
        assert_eq!(request.send().await.unwrap().status(), status);
    }
    let audit: String = sqlx::query_scalar("SELECT actor_subject FROM manager.audit_events")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(audit, "owner-a");
    let logout = f
        .client
        .post(format!("{}/auth/logout", f.admin.url))
        .header(header::COOKIE, cookies(&browser, &session))
        .header(header::ORIGIN, "https://localhost")
        .header("x-csrf-token", csrf)
        .send()
        .await
        .unwrap();
    assert_eq!(logout.status(), StatusCode::NO_CONTENT);
    assert_eq!(cookie_value(&logout, "__Host-hermes-admin"), "");
    assert_eq!(
        f.session(&browser, &session).await.status(),
        StatusCode::UNAUTHORIZED
    );
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn callback_replay_wrong_browser_and_overlapping_identity_switch_fail_closed(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let (browser, authorization) = f.begin(None).await;
    let callback = f.callback(&authorization).await;
    assert_eq!(
        f.complete(&"a".repeat(64), &callback).await.status(),
        StatusCode::UNAUTHORIZED
    );
    let response = f.complete(&browser, &callback).await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let session = cookie_value(&response, "__Host-hermes-admin");
    assert_eq!(
        f.complete(&browser, &callback).await.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(f.provider.exchanges.load(Ordering::SeqCst), 1);
    let (_, authorization) = f.begin(Some(&browser)).await;
    let old_callback = f.callback(&authorization).await;
    assert_eq!(
        f.session(&browser, &session).await.status(),
        StatusCode::UNAUTHORIZED
    );
    *f.provider.subject.write().await = "owner-b".into();
    let (_, authorization) = f.begin(Some(&browser)).await;
    let new_callback = f.callback(&authorization).await;
    assert_eq!(
        f.complete(&browser, &old_callback).await.status(),
        StatusCode::UNAUTHORIZED
    );
    let response = f.complete(&browser, &new_callback).await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let new_session = cookie_value(&response, "__Host-hermes-admin");
    let body: Value = f
        .session(&browser, &new_session)
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(body["actor"]["subject"], "owner-b");
    assert_eq!(
        f.session(&browser, &session).await.status(),
        StatusCode::UNAUTHORIZED
    );
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn invalid_oidc_nonce_signature_subject_hash_audience_or_age_cannot_create_session(
    pool: PgPool,
) {
    let f = Fixture::new(&pool).await;
    for fault in [
        "nonce",
        "signature",
        "access-subject",
        "hash",
        "audience",
        "client",
        "stale",
    ] {
        *f.provider.fault.write().await = fault.into();
        let (browser, authorization) = f.begin(None).await;
        let callback = f.callback(&authorization).await;
        let response = f.complete(&browser, &callback).await;
        assert!(
            matches!(
                response.status(),
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
            ),
            "unexpected status for {fault}"
        );
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM manager_admin_auth.sessions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn refresh_rechecks_provider_and_subject_and_never_revives_revoked_session(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    for fault in ["", "revoked", "role", "refresh-subject", "stale"] {
        *f.provider.fault.write().await = String::new();
        let (browser, session) = f.login().await;
        sqlx::query("UPDATE manager_admin_auth.sessions SET verified_at=clock_timestamp()-interval '61 seconds' WHERE state='active'").execute(&pool).await.unwrap();
        *f.provider.fault.write().await = fault.into();
        let status = f.session(&browser, &session).await.status();
        if fault.is_empty() || fault == "role" {
            assert_eq!(status, StatusCode::OK);
        } else {
            assert!(matches!(
                status,
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
            ));
            *f.provider.fault.write().await = String::new();
            assert_eq!(
                f.session(&browser, &session).await.status(),
                StatusCode::UNAUTHORIZED
            );
        }
    }
    assert_eq!(f.provider.refreshes.load(Ordering::SeqCst), 5);
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn cookie_ambiguity_bearer_ambiguity_and_return_redirects_are_rejected(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let (browser, session) = f.login().await;
    let duplicate = f
        .client
        .get(format!("{}/api/v1/session", f.admin.url))
        .header(
            header::COOKIE,
            format!(
                "{}; __Host-hermes-admin={session}",
                cookies(&browser, &session)
            ),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(duplicate.status(), StatusCode::UNAUTHORIZED);
    let both = f
        .client
        .get(format!("{}/api/v1/session", f.admin.url))
        .header(header::COOKIE, cookies(&browser, &session))
        .bearer_auth("forged")
        .send()
        .await
        .unwrap();
    assert_eq!(both.status(), StatusCode::UNAUTHORIZED);
    for path in [
        "//evil.example.org",
        "/\\evil.example.org",
        "/auth/callback?code=secret",
        "/_platform/login",
        "/api/settings",
    ] {
        assert!(f.auth.safe_return_path(path).is_err());
    }
    assert!(
        f.auth
            .check_origin("https://localhost.evil.example.org")
            .is_err()
    );
    assert!(f.auth.check_origin("https://localhost:443").is_err());
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn router_forward_auth_and_session_recheck_require_distinct_mtls_callers(pool: PgPool) {
    let f = Fixture::for_purpose(&pool, Purpose::Router).await;
    let (browser, session) = f.login().await;
    let cookie = format!(
        "{}={browser}; {}={session}",
        f.auth.browser_cookie(),
        f.auth.session_cookie()
    );
    let base = &f.internal.as_ref().unwrap().url;
    let forward = f.provider.secrets.client(Some("forward")).await;
    let plugin = f.provider.secrets.client(Some("router")).await;
    let forward_request = |client: &reqwest::Client| {
        client
            .get(format!("{base}/internal/v1/forward-auth"))
            .header("x-forwarded-host", "localhost")
            .header("x-forwarded-proto", "https")
            .header("x-forwarded-method", "GET")
            .header("x-forwarded-uri", "/api/settings")
            .header(header::COOKIE, &cookie)
            .header("x-hermes-verified-subject", "forged")
    };
    let response = forward_request(&forward).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-hermes-verified-subject"], "owner-a");
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let handle = response.headers()["x-hermes-verified-session"]
        .to_str()
        .unwrap()
        .to_string();
    assert_eq!(handle.len(), 64);
    assert_ne!(handle, session);
    assert!(response.headers()["x-hermes-verified-evidence"].len() > 100);
    assert_eq!(
        forward_request(&plugin).send().await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    let endpoint = format!("{base}/internal/v1/sessions/authorize");
    assert_eq!(
        forward
            .post(&endpoint)
            .json(&json!({"sessionId":handle}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let response = plugin
        .post(&endpoint)
        .json(&json!({"sessionId":handle}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let evidence: Value = response.json().await.unwrap();
    assert_eq!(evidence["identity"]["subject"], "owner-a");
    assert!(evidence["accessToken"].as_str().unwrap().len() > 100);
    // Raw browser cookies cannot be used as internal session handles.
    assert_eq!(
        plugin
            .post(&endpoint)
            .json(&json!({"sessionId":session}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        plugin
            .post(&endpoint)
            .json(&json!({"sessionId":handle,"subject":"foreign"}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert!(
        f.client
            .post(&endpoint)
            .json(&json!({"sessionId":handle}))
            .send()
            .await
            .is_err()
    );
    let me = f
        .client
        .get(format!("{}/_platform/me", f.admin.url))
        .header(header::COOKIE, &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(me.status(), StatusCode::OK);
    let body: Value = me.json().await.unwrap();
    assert_eq!(body["identity"]["subject"], "owner-a");
    assert!(body.get("accessToken").is_none());
    assert_eq!(
        f.client
            .post(format!("{}/internal/v1/sessions/authorize", f.admin.url))
            .json(&json!({"sessionId":handle}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM manager.assignments")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    let logout = f
        .client
        .post(format!("{}/_platform/logout", f.admin.url))
        .header(header::COOKIE, &cookie)
        .header(header::ORIGIN, "https://localhost")
        .header("x-csrf-token", body["csrfToken"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(logout.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        plugin
            .post(&endpoint)
            .json(&json!({"sessionId":handle}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn missing_router_session_redirects_only_safe_html_navigation_never_native_callbacks(
    pool: PgPool,
) {
    let f = Fixture::for_purpose(&pool, Purpose::Router).await;
    let forward = f.provider.secrets.client(Some("forward")).await;
    let url = format!(
        "{}/internal/v1/forward-auth",
        f.internal.as_ref().unwrap().url
    );
    for (method, uri, status) in [
        ("GET", "/", StatusCode::SEE_OTHER),
        (
            "GET",
            "/auth/callback?code=private",
            StatusCode::UNAUTHORIZED,
        ),
        ("GET", "/api/ws?ticket=private", StatusCode::UNAUTHORIZED),
        ("POST", "/", StatusCode::UNAUTHORIZED),
        (
            "GET",
            "//foreign.example.org",
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
    ] {
        let response = forward
            .get(&url)
            .header("x-forwarded-host", "localhost")
            .header("x-forwarded-proto", "https")
            .header("x-forwarded-method", method)
            .header("x-forwarded-uri", uri)
            .header(header::ACCEPT, "text/html")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), status);
        if status == StatusCode::SEE_OTHER {
            assert_eq!(
                response.headers()[header::LOCATION],
                "/_platform/login?returnTo=%2F"
            );
        } else {
            let body = response.text().await.unwrap();
            assert!(!body.contains("private"));
        }
    }
    for (host, proto) in [
        ("foreign.example.org", "https"),
        ("localhost", "http"),
        ("localhost,foreign.example.org", "https"),
    ] {
        assert_eq!(
            forward
                .get(&url)
                .header("x-forwarded-host", host)
                .header("x-forwarded-proto", proto)
                .header("x-forwarded-method", "GET")
                .header("x-forwarded-uri", "/")
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
    }
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn provider_advertising_only_pairwise_subjects_cannot_start_login(pool: PgPool) {
    let f = Fixture::for_purpose(&pool, Purpose::Router).await;
    *f.provider.fault.write().await = "pairwise".into();
    let response = f
        .client
        .get(format!("{}/_platform/login", f.admin.url))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM manager_router_auth.flows")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
#[ignore = "run with MANAGER_UI_DIST and npm installed after building the production UI"]
async fn production_dashboard_browser_csp(pool: PgPool) {
    let directory = std::env::var("MANAGER_UI_DIST").expect("production assets directory");
    let assets = DashboardAssets::load(std::path::Path::new(&directory))
        .await
        .unwrap();
    let (listener, _) = hermes_adapters::server::bind("127.0.0.1:0").await.unwrap();
    let f =
        Fixture::with_dashboard_at(&pool, Purpose::Administration, Some(assets), Some(listener))
            .await;
    let (browser, session) = f.login().await;
    let mut command = tokio::process::Command::new("npm");
    command
        .current_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../../ui"))
        .args(["run", "test:production"])
        .env("MANAGER_E2E_ORIGIN", &f.admin.url)
        .env("MANAGER_E2E_SESSION", session)
        .env("MANAGER_E2E_BROWSER", browser)
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(120), command.output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        output.status.success(),
        "Production browser failed: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn postgres_revocation_denies_existing_cookie_assets_pages_and_bearer_immediately(
    pool: PgPool,
) {
    let directory = tempfile::tempdir().unwrap();
    tokio::fs::create_dir(directory.path().join("assets"))
        .await
        .unwrap();
    tokio::fs::write(
        directory.path().join("index.html"),
        "<!doctype html><title>Revocation fixture</title>",
    )
    .await
    .unwrap();
    tokio::fs::write(
        directory.path().join("assets/app.js"),
        "export const fixture=true;",
    )
    .await
    .unwrap();
    let assets = DashboardAssets::load(directory.path()).await.unwrap();
    let f = Fixture::with_dashboard(&pool, Purpose::Administration, Some(assets)).await;
    // OIDC roles are deliberately absent. Only this exact PostgreSQL identity grants access.
    *f.provider.fault.write().await = "role".into();
    let (browser, session) = f.login().await;
    let now = unix_time().unwrap();
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some("fixture".into());
    let claims = json!({"iss":f.provider.issuer.read().await.clone(),"sub":"owner-a","aud":"manager-api","azp":"manager-admin","iat":now,"exp":now+300,"nbf":now-1,"typ":"Bearer","sid":"provider-session-one"});
    let bearer = encode(&header, &claims, &f.provider.secrets.signer).unwrap();
    assert_eq!(
        f.client
            .get(format!("{}/api/v1/session", f.admin.url))
            .bearer_auth(&bearer)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    sqlx::query(
        "UPDATE manager_admin_auth.administrator_grants SET active=false WHERE subject='owner-a'",
    )
    .execute(&pool)
    .await
    .unwrap();
    for path in [
        "/",
        "/assignments",
        "/assets/app.js",
        "/api/v1/session",
        "/api/v1/assignments",
    ] {
        let response = f
            .client
            .get(format!("{}{path}", f.admin.url))
            .header(header::COOKIE, cookies(&browser, &session))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{path}");
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    }
    assert_eq!(
        f.client
            .get(format!("{}/api/v1/session", f.admin.url))
            .bearer_auth(&bearer)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
}
