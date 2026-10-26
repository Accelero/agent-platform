//! Browser sign-in against a fake realm over real TLS and real PostgreSQL.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use hermes_core::domain::identity::Identity;
use hermes_identity::browser::{Admission, BrowserAuth, BrowserConfig};
use hermes_identity::crypto::{Purpose, Sealer};
use hermes_identity::oidc::{OidcConfig, OidcProvider};
use hermes_identity::sessions::PgSessions;
use hermes_identity::tokens::{AccessTokenVerifier, TokenPolicy};
use hermes_identity::{AuthError, http_client};
use hermes_testkit::Secrets;
use hermes_testkit::provider::{FakeProvider, Fault};
use jsonwebtoken::Algorithm;
use sqlx::PgPool;
use url::Url;
use uuid::Uuid;

const PUBLIC: &str = "https://hermes-admin.localhost";

/// Admits identities while `open` is set.
struct Switch {
    open: AtomicBool,
}

#[async_trait]
impl Admission for Switch {
    async fn admit(&self, _: &Identity) -> Result<(), AuthError> {
        if self.open.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(AuthError::Forbidden)
        }
    }
}

struct Fixture {
    secrets: Secrets,
    provider: FakeProvider,
    auth: BrowserAuth,
    admission: Arc<Switch>,
    pool: PgPool,
}

struct Login {
    browser: String,
    session: String,
}

impl Fixture {
    async fn new(pool: PgPool) -> Self {
        let secrets = Secrets::new().await;
        let provider = FakeProvider::start(&secrets, "manager-admin", "manager-api").await;
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
                callback_url: format!("{PUBLIC}/auth/oidc/callback"),
                scopes: vec!["openid".into()],
                client_secret: None,
                revocation_url: None,
            },
            http,
        )
        .unwrap();
        let sessions =
            PgSessions::connect_with((*pool.connect_options()).clone(), Purpose::Administration)
                .await
                .unwrap();
        let sealer = Sealer::new(
            Uuid::new_v4(),
            "key-one".into(),
            BTreeMap::from([("key-one".into(), vec![42; 32])]),
        )
        .unwrap();
        let admission = Arc::new(Switch {
            open: AtomicBool::new(true),
        });
        let auth = BrowserAuth::new(
            sessions,
            oidc,
            tokens,
            sealer,
            BrowserConfig {
                public_origin: PUBLIC.into(),
                other_origin: "https://hermes.localhost".into(),
            },
            admission.clone(),
        )
        .unwrap();
        Self {
            secrets,
            provider,
            auth,
            admission,
            pool,
        }
    }

    /// Begins a login and lets the provider authorize it. Returns the browser
    /// proof and the callback's `(state, code)`.
    async fn authorize(&self, browser: Option<&str>) -> (String, String, String) {
        let start = self.auth.begin(browser, "/assignments").await.unwrap();
        let response = self
            .secrets
            .client(None)
            .get(&start.location)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 303);
        let callback = Url::parse(response.headers()["location"].to_str().unwrap()).unwrap();
        assert_eq!(callback.path(), "/auth/oidc/callback");
        let query: BTreeMap<_, _> = callback.query_pairs().into_owned().collect();
        (start.browser, query["state"].clone(), query["code"].clone())
    }

    async fn login(&self) -> Login {
        let (browser, state, code) = self.authorize(None).await;
        let complete = self.auth.complete(&browser, &state, &code).await.unwrap();
        assert_eq!(complete.location, "/assignments");
        assert_eq!(complete.browser, browser);
        Login {
            browser,
            session: complete.session,
        }
    }

    /// Makes the next authentication refresh provider state.
    async fn make_refresh_due(&self) {
        sqlx::query("UPDATE admin_sessions.sessions SET verified_at = clock_timestamp() - interval '61 seconds'")
            .execute(&self.pool)
            .await
            .unwrap();
    }
}

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn login_creates_a_session_for_the_verified_subject(pool: PgPool) {
    let fixture = Fixture::new(pool).await;
    let login = fixture.login().await;
    let session = fixture
        .auth
        .authenticate(&login.session, &login.browser)
        .await
        .unwrap();
    assert_eq!(session.identity.subject, "owner-a");
    assert_eq!(session.identity.issuer, fixture.provider.issuer);
    assert_eq!(session.csrf_token.len(), 64);
    assert_eq!(fixture.provider.exchanges(), 1);
}

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn callbacks_are_single_use_and_bound_to_their_browser(pool: PgPool) {
    let fixture = Fixture::new(pool).await;
    let (browser, state, code) = fixture.authorize(None).await;
    let stranger = "b".repeat(64);
    assert_eq!(
        fixture.auth.complete(&stranger, &state, &code).await.err(),
        Some(AuthError::Evidence)
    );
    fixture
        .auth
        .complete(&browser, &state, &code)
        .await
        .unwrap();
    assert_eq!(
        fixture.auth.complete(&browser, &state, &code).await.err(),
        Some(AuthError::Evidence)
    );
}

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn a_new_login_invalidates_older_flows_and_sessions(pool: PgPool) {
    let fixture = Fixture::new(pool).await;
    let first = fixture.login().await;
    let (browser, old_state, old_code) = fixture.authorize(Some(&first.browser)).await;
    assert_eq!(browser, first.browser);
    assert!(
        fixture
            .auth
            .authenticate(&first.session, &first.browser)
            .await
            .is_err()
    );
    let (_, state, code) = fixture.authorize(Some(&first.browser)).await;
    assert_eq!(
        fixture
            .auth
            .complete(&browser, &old_state, &old_code)
            .await
            .err(),
        Some(AuthError::Evidence)
    );
    let second = fixture
        .auth
        .complete(&browser, &state, &code)
        .await
        .unwrap();
    assert!(
        fixture
            .auth
            .authenticate(&second.session, &browser)
            .await
            .is_ok()
    );
}

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn faulty_provider_responses_never_create_a_session(pool: PgPool) {
    let fixture = Fixture::new(pool).await;
    for fault in [
        Fault::Nonce,
        Fault::Hash,
        Fault::Signature,
        Fault::AccessSubject,
        Fault::Audience,
        Fault::Client,
        Fault::Stale,
    ] {
        fixture.provider.set_fault(fault);
        let (browser, state, code) = fixture.authorize(None).await;
        let result = fixture.auth.complete(&browser, &state, &code).await;
        assert_eq!(result.err(), Some(AuthError::Evidence), "{fault:?}");
    }
    let sessions: i64 = sqlx::query_scalar("SELECT count(*) FROM admin_sessions.sessions")
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(sessions, 0);
}

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn admission_is_checked_at_login_and_on_every_refresh(pool: PgPool) {
    let fixture = Fixture::new(pool).await;
    fixture.admission.open.store(false, Ordering::SeqCst);
    let (browser, state, code) = fixture.authorize(None).await;
    assert_eq!(
        fixture.auth.complete(&browser, &state, &code).await.err(),
        Some(AuthError::Forbidden)
    );

    fixture.admission.open.store(true, Ordering::SeqCst);
    let login = fixture.login().await;
    fixture.admission.open.store(false, Ordering::SeqCst);
    assert_eq!(
        fixture
            .auth
            .authenticate(&login.session, &login.browser)
            .await
            .err(),
        Some(AuthError::Forbidden)
    );
    fixture.make_refresh_due().await;
    assert!(
        fixture
            .auth
            .authenticate(&login.session, &login.browser)
            .await
            .is_err()
    );
    fixture.admission.open.store(true, Ordering::SeqCst);
    assert_eq!(
        fixture
            .auth
            .authenticate(&login.session, &login.browser)
            .await
            .err(),
        Some(AuthError::Evidence),
        "a refused refresh revokes the session"
    );
}

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn refresh_rechecks_the_provider_and_revokes_on_failure(pool: PgPool) {
    let fixture = Fixture::new(pool).await;
    let login = fixture.login().await;
    fixture.make_refresh_due().await;
    fixture
        .auth
        .authenticate(&login.session, &login.browser)
        .await
        .unwrap();
    assert_eq!(fixture.provider.refreshes(), 1);
    fixture
        .auth
        .authenticate(&login.session, &login.browser)
        .await
        .unwrap();
    assert_eq!(
        fixture.provider.refreshes(),
        1,
        "fresh evidence needs no refresh"
    );

    for fault in [Fault::Revoked, Fault::RefreshSubject] {
        let login = fixture.login().await;
        fixture.make_refresh_due().await;
        fixture.provider.set_fault(fault);
        assert!(
            fixture
                .auth
                .authenticate(&login.session, &login.browser)
                .await
                .is_err()
        );
        fixture.provider.set_fault(Fault::None);
        assert_eq!(
            fixture
                .auth
                .authenticate(&login.session, &login.browser)
                .await
                .err(),
            Some(AuthError::Evidence),
            "{fault:?} revokes the session"
        );
    }
}

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn interrupted_refresh_revokes_instead_of_replaying(pool: PgPool) {
    let fixture = Fixture::new(pool).await;
    let login = fixture.login().await;
    sqlx::query(
        "UPDATE admin_sessions.sessions SET state = 'refreshing', refresh_owner = gen_random_uuid(), \
         refresh_until = clock_timestamp() - interval '1 second'",
    )
    .execute(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(
        fixture
            .auth
            .authenticate(&login.session, &login.browser)
            .await
            .err(),
        Some(AuthError::Evidence)
    );
    let state: String = sqlx::query_scalar("SELECT state FROM admin_sessions.sessions")
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(state, "revoked");
}

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn logout_requires_the_session_csrf_token(pool: PgPool) {
    let fixture = Fixture::new(pool).await;
    let login = fixture.login().await;
    let session = fixture
        .auth
        .authenticate(&login.session, &login.browser)
        .await
        .unwrap();
    assert_eq!(
        fixture
            .auth
            .logout(&login.session, &login.browser, &"0".repeat(64))
            .await
            .err(),
        Some(AuthError::Forbidden)
    );
    fixture
        .auth
        .logout(&login.session, &login.browser, &session.csrf_token)
        .await
        .unwrap();
    assert_eq!(
        fixture
            .auth
            .authenticate(&login.session, &login.browser)
            .await
            .err(),
        Some(AuthError::Evidence)
    );
}

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn pairwise_subject_providers_cannot_start_a_login(pool: PgPool) {
    let fixture = Fixture::new(pool).await;
    fixture.provider.set_fault(Fault::Pairwise);
    assert_eq!(
        fixture.auth.begin(None, "/").await.err(),
        Some(AuthError::Evidence)
    );
}

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn return_paths_stay_local_and_outside_protected_routes(pool: PgPool) {
    let fixture = Fixture::new(pool).await;
    for path in [
        "https://evil.example.org",
        "//evil.example.org",
        "/auth/login",
        "/api/v1/session",
        "/_platform/me",
        "/a\\b",
    ] {
        assert_eq!(
            fixture.auth.begin(None, path).await.err(),
            Some(AuthError::Configuration),
            "{path}"
        );
    }
}

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn pruning_keeps_live_sessions(pool: PgPool) {
    let fixture = Fixture::new(pool).await;
    let live = fixture.login().await;
    let expired = fixture.login().await;
    sqlx::query("UPDATE admin_sessions.sessions SET expires_at = clock_timestamp() - interval '1 second' WHERE browser_hash = sha256($1::bytea)")
        .bind(expired.browser.as_bytes())
        .execute(&fixture.pool)
        .await
        .unwrap();
    assert!(fixture.auth.prune().await.unwrap() >= 1);
    assert!(
        fixture
            .auth
            .authenticate(&live.session, &live.browser)
            .await
            .is_ok()
    );
}
