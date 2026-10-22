//! HTTPS, mTLS and cryptographic authorization through real listeners and SQL.
#[path = "support/grants.rs"]
mod grant_fixture;
use std::{collections::BTreeSet, time::Duration};

use axum::{Json, Router, routing::get};
use hermes_adapters::{
    auth::{AccessTokenVerifier, TokenPolicy, http_client, unix_time},
    http::{admin_router, resolver_router},
    postgres::{FreshInventory, PgStore},
};
use hermes_application::routing::RoutePolicy;
use hermes_application::{AdminPolicy, InstallationScope};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

#[allow(dead_code)]
mod support;
use support::{ROUTER, Running, Secrets, start};

#[path = "https/body_limit.rs"]
mod body_limit;
#[path = "https/execution.rs"]
mod execution;

struct Fixture {
    secrets: Secrets,
    scope: InstallationScope,
    admin: Running,
    resolver: Running,
    _jwks: Running,
    client: reqwest::Client,
    router_client: reqwest::Client,
    foreign_client: reqwest::Client,
}
impl Fixture {
    async fn new(pool: &PgPool) -> Self {
        let secrets = Secrets::new().await;
        let jwks = json!({"keys":[secrets.jwk.clone()]});
        let jwks = start(
            &secrets,
            Router::new().route(
                "/jwks",
                get(move || {
                    let data = jwks.clone();
                    async move { Json(data) }
                }),
            ),
            false,
        )
        .await;
        let scope = InstallationScope {
            id: Uuid::new_v4(),
            epoch: 1,
            issuer: format!("{}/realms/agents", jwks.url),
        };
        let store = PgStore::connect_with((*pool.connect_options()).clone())
            .await
            .unwrap();
        store
            .initialize_fresh(
                &scope,
                FreshInventory {
                    cluster_identity: "https-fixture".into(),
                    digest: "a".repeat(64),
                    registered_instances: 0,
                    unexplained_resources: 0,
                },
            )
            .await
            .unwrap();
        grant_fixture::grant(pool, &scope, &["admin-a"]).await;
        store.check_existing(&scope).await.unwrap();
        sqlx::query("INSERT INTO manager.instance_templates (id,version,schema_version,image,content_digest,approved) VALUES ('personal-default',1,1,$1,$2,true)")
            .bind(format!("ghcr.io/example/hermes@sha256:{}","a".repeat(64))).bind("a".repeat(64)).execute(pool).await.unwrap();
        let client = secrets.client(None).await;
        let oidc_client = http_client(Some(&secrets.read("ca.crt").await)).unwrap();
        let tokens = |audience: &str, azp: &str, age| {
            AccessTokenVerifier::new(
                TokenPolicy {
                    issuer: scope.issuer.clone(),
                    jwks_url: format!("{}/jwks", jwks.url),
                    audience: audience.into(),
                    authorized_clients: BTreeSet::from([azp.into()]),
                    algorithms: vec![Algorithm::RS256],
                    maximum_age_seconds: age,
                },
                oidc_client.clone(),
            )
            .unwrap()
        };
        let admin = start(
            &secrets,
            admin_router(
                store.clone(),
                scope.clone(),
                AdminPolicy {
                    issuer: scope.issuer.clone(),
                    audience: "manager-api".into(),
                    management_client: "manager-admin".into(),
                    administrator_role: "platform-administrator".into(),
                },
                tokens("manager-api", "manager-admin", None),
            )
            .unwrap(),
            false,
        )
        .await;
        let resolver = start(
            &secrets,
            resolver_router(
                store,
                scope.clone(),
                RoutePolicy {
                    issuer: scope.issuer.clone(),
                    audience: "route-api".into(),
                    authorized_client: "hermes-router".into(),
                    router_service_identity: ROUTER.into(),
                },
                tokens("route-api", "hermes-router", Some(120)),
            )
            .unwrap(),
            true,
        )
        .await;
        let router_client = secrets.client(Some("router")).await;
        let foreign_client = secrets.client(Some("foreign")).await;
        Self {
            secrets,
            scope,
            admin,
            resolver,
            _jwks: jwks,
            client,
            router_client,
            foreign_client,
        }
    }
    fn claims(&self, subject: &str, audience: &str, client: &str, admin: bool) -> Value {
        let now = unix_time().unwrap();
        json!({"iss":self.scope.issuer,"sub":subject,"aud":audience,"azp":client,"typ":"Bearer","sid":"fixture-user-session","iat":now,"exp":now+300,
            "resource_access":{"manager-admin":{"roles":if admin{vec!["platform-administrator"]}else{vec!["realm-member"]}}}})
    }
    fn sign(&self, claims: &Value) -> String {
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some("fixture".into());
        encode(&header, claims, &self.secrets.signer).unwrap()
    }
    fn admin_token(&self) -> String {
        self.sign(&self.claims("admin-a", "manager-api", "manager-admin", true))
    }
    fn route_token(&self, subject: &str) -> String {
        self.sign(&self.claims(subject, "route-api", "hermes-router", false))
    }
    fn assignment(&self, subject: &str) -> Value {
        json!({"identity":{"issuer":self.scope.issuer,"subject":subject},"access":"enabled","templateId":"personal-default"})
    }
    async fn create(&self, subject: &str) -> Value {
        let response = self
            .client
            .post(format!("{}/api/v1/assignments", self.admin.url))
            .bearer_auth(self.admin_token())
            .header("Idempotency-Key", format!("create-{subject}"))
            .json(&self.assignment(subject))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 202);
        assert_eq!(response.headers()["cache-control"], "no-store");
        response.json().await.unwrap()
    }
    async fn resolve(&self, subject: &str) -> reqwest::Response {
        self.router_client
            .post(format!("{}/internal/v1/routes/resolve", self.resolver.url))
            .bearer_auth(self.route_token(subject))
            .json(&json!({"identity":{"issuer":self.scope.issuer,"subject":subject}}))
            .send()
            .await
            .unwrap()
    }
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn administration_api_only_workflow_authorizes_every_read_and_mutation(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let url = format!("{}/api/v1/assignments", f.admin.url);
    for request in [
        f.client.get(&url),
        f.client.post(&url).json(&f.assignment("owner-a")),
        f.client
            .get(&url)
            .header("Cookie", "__Host-hermes-admin=forged")
            .header("X-Platform-Administrator", "true"),
    ] {
        let response = request.send().await.unwrap();
        assert_eq!(response.status(), 401);
    }
    let owner = f.sign(&f.claims("owner-a", "manager-api", "manager-admin", false));
    assert_eq!(
        f.client
            .get(&url)
            .bearer_auth(owner)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    let session: Value = f
        .client
        .get(format!("{}/api/v1/session", f.admin.url))
        .bearer_auth(f.admin_token())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(session["actor"]["subject"], "admin-a");
    assert!(session["csrfToken"].is_null());
    let first = f.create("owner-a").await;
    assert_eq!(f.create("owner-a").await, first);
    let id = first["assignmentId"].as_str().unwrap();
    let instance_id = first["instanceId"].as_str().unwrap();
    let response = f
        .client
        .get(format!("{url}/{id}"))
        .bearer_auth(f.admin_token())
        .send()
        .await
        .unwrap();
    assert_eq!(response.headers()["etag"], "\"1\"");
    let view: Value = response.json().await.unwrap();
    assert_eq!(view["identity"]["subject"], "owner-a");
    assert_eq!(view["instanceId"], instance_id);
    // Without a configured directory, search is authorized but reports disabled.
    let response = f
        .client
        .get(format!("{}/api/v1/directory-users", f.admin.url))
        .bearer_auth(f.admin_token())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["enabled"], false);
    assert!(body["items"].as_array().unwrap().is_empty());
    for path in [
        "/api/v1/identities",
        "/api/v1/templates",
        "/api/v1/audit-events",
        "/api/v1/assignments?limit=1",
    ] {
        let response = f
            .client
            .get(format!("{}{path}", f.admin.url))
            .bearer_auth(f.admin_token())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let body: Value = response.json().await.unwrap();
        assert!(!body["items"].as_array().unwrap().is_empty());
    }
    for path in [
        format!(
            "/api/v1/operations/{}",
            first["operationId"].as_str().unwrap()
        ),
        format!("/api/v1/instances/{instance_id}"),
    ] {
        assert_eq!(
            f.client
                .get(format!("{}{path}", f.admin.url))
                .bearer_auth(f.admin_token())
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
    }
    let update = |access: &str, key: &str| {
        f.client
            .patch(format!("{url}/{id}"))
            .bearer_auth(f.admin_token())
            .header("If-Match", "\"1\"")
            .header("Idempotency-Key", key)
            .json(&json!({"access":access}))
    };
    let (left, right) = tokio::join!(
        update("suspended", "suspend").send(),
        update("revoked", "revoke").send()
    );
    let statuses = BTreeSet::from([
        left.unwrap().status().as_u16(),
        right.unwrap().status().as_u16(),
    ]);
    assert_eq!(statuses, BTreeSet::from([202, 412]));
    let retry = f
        .client
        .post(format!(
            "{}/api/v1/instances/{instance_id}/reconcile",
            f.admin.url
        ))
        .bearer_auth(f.admin_token())
        .header("If-Match", "\"2\"")
        .header("Idempotency-Key", "retry")
        .send()
        .await
        .unwrap();
    assert_eq!(retry.status(), 202);
    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM manager.audit_events")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(total, 3);
    for path in [
        "/internal/v1/routes/resolve",
        "/execution/v1/environments",
        "/api/unknown",
        "/auth/unknown",
    ] {
        assert_eq!(
            f.client
                .get(format!("{}{path}", f.admin.url))
                .bearer_auth(f.admin_token())
                .send()
                .await
                .unwrap()
                .status(),
            404
        );
    }
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn signed_but_wrong_claims_forged_headers_and_invalid_signatures_are_rejected(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let url = format!("{}/api/v1/session", f.admin.url);
    let base = f.claims("admin-a", "manager-api", "manager-admin", true);
    for (field, value) in [
        ("iss", json!("https://foreign.example.org")),
        ("aud", json!("route-api")),
        ("azp", json!("native-hermes")),
        ("typ", json!("ID")),
        ("sid", json!("")),
        ("gty", json!("client_credentials")),
        ("sub", json!("")),
        ("iat", json!(unix_time().unwrap() + 30)),
        ("exp", json!(unix_time().unwrap() - 1)),
    ] {
        let mut claims = base.clone();
        claims[field] = value;
        let response = f
            .client
            .get(&url)
            .bearer_auth(f.sign(&claims))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 401, "{field}");
    }
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some("fixture".into());
    let foreign = encode(&header, &base, &f.secrets.foreign_signer).unwrap();
    assert_eq!(
        f.client
            .get(&url)
            .bearer_auth(foreign)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    header.jku = Some("https://attacker.example.org/jwks".into());
    let untrusted = encode(&header, &base, &f.secrets.signer).unwrap();
    assert_eq!(
        f.client
            .get(&url)
            .bearer_auth(untrusted)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let hmac = encode(
        &Header::new(Algorithm::HS256),
        &base,
        &EncodingKey::from_secret(b"fixture-not-an-authorized-key"),
    )
    .unwrap();
    assert_eq!(
        f.client
            .get(&url)
            .bearer_auth(hmac)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        f.client
            .get(&url)
            .header("Authorization", format!("Bearer {}", f.admin_token()))
            .header("Authorization", format!("Bearer {}", f.admin_token()))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        f.client
            .get(&url)
            .header("X-Verified-Subject", "admin-a")
            .header("X-Verified-Roles", "platform-administrator")
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM manager.assignments")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn resolver_requires_real_mtls_and_fresh_exact_user_evidence_and_reads_live_access(
    pool: PgPool,
) {
    let f = Fixture::new(&pool).await;
    let url = format!("{}/internal/v1/routes/resolve", f.resolver.url);
    let body = json!({"identity":{"issuer":f.scope.issuer,"subject":"owner-a"}});
    assert!(
        f.client
            .post(&url)
            .bearer_auth(f.route_token("owner-a"))
            .json(&body)
            .send()
            .await
            .is_err()
    );
    assert_eq!(
        f.foreign_client
            .post(&url)
            .bearer_auth(f.route_token("owner-a"))
            .header("X-Forwarded-Client-Cert", ROUTER)
            .json(&body)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        f.router_client
            .post(&url)
            .json(&body)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        f.router_client
            .post(&url)
            .bearer_auth(f.admin_token())
            .json(&body)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        f.router_client
            .post(&url)
            .bearer_auth(f.route_token("owner-b"))
            .json(&body)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let mut stale = f.claims("owner-a", "route-api", "hermes-router", false);
    stale["iat"] = json!(unix_time().unwrap() - 121);
    assert_eq!(
        f.router_client
            .post(&url)
            .bearer_auth(f.sign(&stale))
            .json(&body)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(f.resolve("unassigned").await.status(), 403);
    let first = f.create("owner-a").await;
    let second = f.create("owner-b").await;
    assert_eq!(f.resolve("owner-a").await.status(), 202);
    for entry in [&first, &second] {
        let id = Uuid::parse_str(entry["instanceId"].as_str().unwrap()).unwrap();
        let uid = format!("namespace-{id}");
        sqlx::query("UPDATE manager.instances SET state='Ready',namespace_uid=$2,observed_generation=desired_generation,route_generation=1 WHERE id=$1").bind(id).bind(&uid).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO manager.instance_routes (instance_id,namespace_uid,service,port,scheme,generation,verified_owner,verified_storage,verified_policy,verified_backend) VALUES ($1,$2,'hermes',8080,'http',1,true,true,true,true)")
            .bind(id).bind(uid).execute(&pool).await.unwrap();
    }
    for (owner, expected) in [("owner-a", &first), ("owner-b", &second)] {
        let response = f.resolve(owner).await;
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["cache-control"], "no-store");
        let result: Value = response.json().await.unwrap();
        assert_eq!(result["instanceId"], expected["instanceId"]);
    }
    let revoke = f
        .client
        .patch(format!(
            "{}/api/v1/assignments/{}",
            f.admin.url,
            first["assignmentId"].as_str().unwrap()
        ))
        .bearer_auth(f.admin_token())
        .header("If-Match", "\"1\"")
        .header("Idempotency-Key", "revoke")
        .json(&json!({"access":"revoked"}))
        .send()
        .await
        .unwrap();
    assert_eq!(revoke.status(), 202);
    assert_eq!(f.resolve("owner-a").await.status(), 403);
    assert_eq!(f.resolve("owner-b").await.status(), 200);
    assert_eq!(
        f.router_client
            .get(format!("{}/api/v1/assignments", f.resolver.url))
            .bearer_auth(f.admin_token())
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn invalid_bodies_versions_and_unknown_fields_cannot_change_authorization(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let base = f.assignment("owner-a");
    let url = format!("{}/api/v1/assignments", f.admin.url);
    let mut invalid_version = base.clone();
    invalid_version["templateVersion"] = Value::Null;
    assert_eq!(
        f.client
            .post(&url)
            .bearer_auth(f.admin_token())
            .header("Idempotency-Key", "null-template-version")
            .json(&invalid_version)
            .send()
            .await
            .unwrap()
            .status(),
        422
    );
    for field in [
        "email",
        "instanceId",
        "namespace",
        "podSpec",
        "image",
        "mounts",
        "credentials",
    ] {
        let mut body = base.clone();
        body[field] = json!("untrusted");
        assert_eq!(
            f.client
                .post(&url)
                .bearer_auth(f.admin_token())
                .header("Idempotency-Key", format!("invalid-{field}"))
                .json(&body)
                .send()
                .await
                .unwrap()
                .status(),
            422
        );
    }
    assert_eq!(
        f.client
            .post(&url)
            .bearer_auth(f.admin_token())
            .json(&base)
            .send()
            .await
            .unwrap()
            .status(),
        422
    );
    let accepted = f.create("owner-a").await;
    let url = format!("{url}/{}", accepted["assignmentId"].as_str().unwrap());
    for version in [
        "1",
        "*",
        "W/\"1\"",
        "\"-1\"",
        "\"0\"",
        "\"01\"",
        "\"1\",\"2\"",
    ] {
        assert_eq!(
            f.client
                .patch(&url)
                .bearer_auth(f.admin_token())
                .header("Idempotency-Key", "invalid-version")
                .header("If-Match", version)
                .json(&json!({"access":"enabled"}))
                .send()
                .await
                .unwrap()
                .status(),
            422
        );
    }
    assert_eq!(
        f.client
            .get(format!("{}/api/v1/assignments?limit=101", f.admin.url))
            .bearer_auth(f.admin_token())
            .send()
            .await
            .unwrap()
            .status(),
        422
    );
    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM manager.audit_events")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(total, 1);
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn jwks_outage_is_unavailable_and_never_an_authentication_fallback(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    f._jwks.task.abort();
    tokio::task::yield_now().await;
    let response = f
        .client
        .get(format!("{}/api/v1/assignments", f.admin.url))
        .bearer_auth(f.admin_token())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 503);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"], "dependency_unavailable");
    let response = f.resolve("owner-a").await;
    assert_eq!(response.status(), 503);
    let body: Value = response.json().await.unwrap();
    assert_eq!(
        body,
        json!({"status":"unavailable","reason":"dependency_unavailable"})
    );
    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM manager.assignments")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(total, 0);
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn database_lock_outage_maintenance_and_changed_epoch_deny_routes_within_deadline(
    pool: PgPool,
) {
    let f = Fixture::new(&pool).await;
    // Warm only the key cache. Assignment authorization remains a live query.
    assert_eq!(f.resolve("owner-a").await.status(), 403);
    let mut lock = pool.begin().await.unwrap();
    sqlx::query("LOCK TABLE manager.installations IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *lock)
        .await
        .unwrap();
    let started = std::time::Instant::now();
    let response = f.resolve("owner-a").await;
    assert_eq!(response.status(), 503);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "resolver exceeded its total deadline"
    );
    let body: Value = response.json().await.unwrap();
    assert_eq!(
        body,
        json!({"status":"unavailable","reason":"dependency_unavailable"})
    );
    lock.rollback().await.unwrap();
    assert_eq!(f.resolve("owner-a").await.status(), 403);
    sqlx::query("UPDATE manager.installations SET mode='maintenance'")
        .execute(&pool)
        .await
        .unwrap();
    let response = f.resolve("owner-a").await;
    assert_eq!(response.status(), 503);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["reason"], "maintenance");
    sqlx::query("UPDATE manager.installations SET mode='active',epoch=epoch+1")
        .execute(&pool)
        .await
        .unwrap();
    let response = f.resolve("owner-a").await;
    assert_eq!(response.status(), 503);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["reason"], "registry_unavailable");
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn malformed_and_oversized_input_returns_bounded_json_without_reflecting_values(
    pool: PgPool,
) {
    let f = Fixture::new(&pool).await;
    let url = format!("{}/api/v1/assignments", f.admin.url);
    let secret = "fixture-private-value-do-not-reflect";
    for body in [
        json!({"credentials":secret}).to_string(),
        format!("{{\"unknown\":\"{}\"}}", secret.repeat(1000)),
        "{malformed".into(),
    ] {
        let response = f
            .client
            .post(&url)
            .bearer_auth(f.admin_token())
            .header("Idempotency-Key", "malformed")
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 422);
        assert_eq!(response.headers()["cache-control"], "no-store");
        let body = response.text().await.unwrap();
        assert!(!body.contains(secret));
        assert_eq!(
            serde_json::from_str::<Value>(&body).unwrap(),
            json!({"error":"invalid_request"})
        );
    }
    let response = f
        .client
        .get(format!("{url}/{secret}"))
        .bearer_auth(f.admin_token())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 422);
    let body = response.text().await.unwrap();
    assert!(!body.contains(secret));
    assert!(serde_json::from_str::<Value>(&body).is_ok());
    let response = f
        .client
        .post(&url)
        .header("Content-Type", "application/json")
        .body("{malformed")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM manager.audit_events")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(total, 0);
}
