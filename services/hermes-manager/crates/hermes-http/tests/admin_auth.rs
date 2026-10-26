//! Administrator authentication: bearer tokens, browser sessions, CSRF,
//! revocation, the dashboard and request bounds.

#[allow(dead_code)]
mod support;

use hermes_core::app::access::{GrantAdministrator, UpdateAdministrator};
use hermes_core::app::actor::Actor;
use hermes_core::app::envelope::IdempotencyKey;
use serde_json::{Value, json};
use sqlx::PgPool;
use support::{Fixture, PUBLIC, body};
use uuid::Uuid;

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn bearer_administrators_get_their_session_without_csrf(pool: PgPool) {
    let fixture = Fixture::new(pool).await;
    let response = fixture
        .get("/api/v1/session")
        .header("authorization", fixture.bearer("owner-a"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let session = body(response).await;
    assert_eq!(session["actor"]["subject"], "owner-a");
    assert_eq!(session["csrfToken"], Value::Null);
    assert!(
        session["permittedActions"]
            .as_array()
            .unwrap()
            .contains(&json!("administrator.manage"))
    );
}

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn grants_decide_on_every_request_and_revocation_is_immediate(pool: PgPool) {
    let fixture = Fixture::new(pool).await;
    let session = |subject: &'static str| {
        let request = fixture
            .get("/api/v1/session")
            .header("authorization", fixture.bearer(subject));
        async move { request.send().await.unwrap() }
    };
    let without_grant = session("owner-b").await;
    assert_eq!(without_grant.status(), 403);
    assert_eq!(body(without_grant).await, json!({"error": "access_denied"}));

    let owner_a = Actor::Administrator(fixture.identity("owner-a"));
    let granted = fixture
        .envelope()
        .execute(
            &owner_a,
            Uuid::new_v4(),
            &IdempotencyKey::parse("grant-b").unwrap(),
            &GrantAdministrator {
                identity: fixture.identity("owner-b"),
                can_manage_administrators: false,
            },
        )
        .await
        .expect("the refused request recorded owner-b's login");
    let plain = body(session("owner-b").await).await;
    assert!(
        !plain["permittedActions"]
            .as_array()
            .unwrap()
            .contains(&json!("administrator.manage"))
    );

    fixture
        .envelope()
        .execute(
            &owner_a,
            Uuid::new_v4(),
            &IdempotencyKey::parse("revoke-b").unwrap(),
            &UpdateAdministrator {
                identity_id: granted.identity_id,
                expected_version: granted.version,
                active: false,
                can_manage_administrators: false,
            },
        )
        .await
        .unwrap();
    assert_eq!(session("owner-b").await.status(), 403);
}

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn invalid_or_ambiguous_credentials_are_unauthenticated(pool: PgPool) {
    let fixture = Fixture::new(pool).await;
    let mut wrong_audience = fixture.provider.access_claims("owner-a");
    wrong_audience["aud"] = json!("route-api");
    let cases = [
        format!("Bearer {}", fixture.provider.sign(&wrong_audience)),
        "Bearer not-a-token".into(),
        "Basic b3duZXItYTpzZWNyZXQ=".into(),
    ];
    for authorization in cases {
        let response = fixture
            .get("/api/v1/session")
            .header("authorization", authorization)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 401);
        assert_eq!(response.headers()["www-authenticate"], "Bearer");
        assert_eq!(
            body(response).await,
            json!({"error": "invalid_identity_evidence"})
        );
    }
    let both = fixture
        .get("/api/v1/session")
        .header("authorization", fixture.bearer("owner-a"))
        .header(
            "cookie",
            format!(
                "__Host-hermes-admin-browser={0}; __Host-hermes-admin={0}",
                "a".repeat(64)
            ),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(both.status(), 401);
    let duplicate = fixture
        .get("/api/v1/session")
        .header(
            "cookie",
            format!(
                "__Host-hermes-admin={0}; __Host-hermes-admin={0}",
                "a".repeat(64)
            ),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(duplicate.status(), 401);
    assert_eq!(
        fixture
            .get("/api/v1/session")
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
}

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn browser_sessions_carry_csrf_and_logout_needs_origin_and_token(pool: PgPool) {
    let fixture = Fixture::new(pool).await;
    let cookies = fixture.browser_login().await.expect("administrator login");
    let session = body(
        fixture
            .get("/api/v1/session")
            .header("cookie", &cookies)
            .send()
            .await
            .unwrap(),
    )
    .await;
    let csrf = session["csrfToken"].as_str().unwrap().to_owned();
    assert_eq!(csrf.len(), 64);

    let logout = |origin: Option<&str>, token: Option<&str>| {
        let mut request = fixture
            .client
            .post(format!("{}/auth/logout", fixture.url))
            .header("cookie", &cookies);
        if let Some(origin) = origin {
            request = request.header("origin", origin);
        }
        if let Some(token) = token {
            request = request.header("x-csrf-token", token);
        }
        async move { request.send().await.unwrap() }
    };
    assert_eq!(logout(None, Some(&csrf)).await.status(), 403);
    assert_eq!(
        logout(Some("https://evil.example.org"), Some(&csrf))
            .await
            .status(),
        403
    );
    assert_eq!(
        logout(Some(PUBLIC), Some(&"0".repeat(64))).await.status(),
        403
    );
    let done = logout(Some(PUBLIC), Some(&csrf)).await;
    assert_eq!(done.status(), 204);
    let after = fixture
        .get("/api/v1/session")
        .header("cookie", &cookies)
        .send()
        .await
        .unwrap();
    assert_eq!(after.status(), 401);
}

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn identities_without_a_grant_cannot_create_a_browser_session(pool: PgPool) {
    let fixture = Fixture::new(pool).await;
    fixture.provider.set_subject("owner-b");
    let refused = fixture.browser_login().await.expect_err("no session");
    assert_eq!(refused.status(), 403);
    assert!(
        refused
            .headers()
            .get_all("set-cookie")
            .iter()
            .all(|v| !v.to_str().unwrap().starts_with("__Host-hermes-admin="))
    );
}

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn dashboard_pages_need_a_session_and_unknown_paths_stay_json(pool: PgPool) {
    let fixture = Fixture::new(pool).await;
    let anonymous = fixture.get("/assignments").send().await.unwrap();
    assert_eq!(anonymous.status(), 303);
    assert_eq!(
        anonymous.headers()["location"],
        "/auth/login?returnTo=%2Fassignments"
    );

    let cookies = fixture.browser_login().await.unwrap();
    let page = fixture
        .get("/assignments")
        .header("cookie", &cookies)
        .send()
        .await
        .unwrap();
    assert_eq!(page.status(), 200);
    let policy = page.headers()["content-security-policy"]
        .to_str()
        .unwrap()
        .to_owned();
    let html = page.text().await.unwrap();
    assert!(!html.contains("__MANAGER_STYLE_NONCE__"));
    let nonce = policy
        .split("'nonce-")
        .nth(1)
        .unwrap()
        .split('\'')
        .next()
        .unwrap();
    assert!(html.contains(nonce));

    let script = fixture
        .get("/assets/app.js")
        .header("cookie", &cookies)
        .send()
        .await
        .unwrap();
    assert_eq!(script.status(), 200);
    assert_eq!(
        script.headers()["content-type"],
        "text/javascript; charset=utf-8"
    );
    for path in [
        "/assignments/not-a-uuid",
        "/unknown",
        "/assets/missing.js",
        "/api/v1/unknown",
    ] {
        let response = fixture
            .get(path)
            .header("cookie", &cookies)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 404, "{path}");
        assert_eq!(
            body(response).await,
            json!({"error": "not_found"}),
            "{path}"
        );
    }
}

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn requests_are_bounded_and_health_needs_no_credentials(pool: PgPool) {
    let fixture = Fixture::new(pool).await;
    let health = fixture.get("/healthz").send().await.unwrap();
    assert_eq!(health.status(), 200);
    let oversized = fixture
        .client
        .post(format!("{}/api/v1/session", fixture.url))
        .header("authorization", fixture.bearer("owner-a"))
        .body(vec![b'x'; 17 * 1024])
        .send()
        .await
        .unwrap();
    assert_eq!(oversized.status(), 422);
    assert_eq!(body(oversized).await, json!({"error": "invalid_request"}));
    let long_header = fixture
        .get("/api/v1/session")
        .header("x-padding", "a".repeat(15 * 1024))
        .header("x-more", "a".repeat(2 * 1024))
        .send()
        .await
        .unwrap();
    // The HTTP/1 read buffer (16 KiB) refuses these before the middleware does.
    assert!(
        matches!(long_header.status().as_u16(), 422 | 431),
        "{}",
        long_header.status()
    );
}
