//! The execution API over real TLS and PostgreSQL, as the `managed_execution`
//! plugin uses it. Every response is checked against
//! `contracts/hermes-manager/execution.openapi.yaml`.

use std::collections::BTreeSet;

use hermes_core::app::assignments::AssignmentReceipt;
use hermes_core::app::controller::{InstanceStore, NewMaterial};
use hermes_core::app::execution::{Execution, credential_digest};
use hermes_core::app::testing::contracts::{bob_assignment, set_access};
use hermes_core::app::testing::installation;
use hermes_core::domain::assignments::Access;
use hermes_core::domain::execution::EnvironmentState;
use hermes_core::domain::materials::MaterialPurpose;
use hermes_http::execution::router;
use hermes_http::server::{bind, serve_until, tls_config};
use hermes_store::PgStore;
use hermes_testkit::Secrets;
use hermes_testkit::contract::Contract;
use hermes_testkit::raw::{Response, chunked, head};
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::task::JoinHandle;
use uuid::Uuid;

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const HOST_KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIFixture";

struct Fixture {
    store: PgStore,
    receipt: AssignmentReceipt,
    instance: Uuid,
    url: String,
    client: reqwest::Client,
    contract: Contract,
    _secrets: Secrets,
    server: JoinHandle<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

struct Reply {
    status: u16,
    retry_after: Option<String>,
    authenticate: Option<String>,
    body: Value,
}

impl Fixture {
    async fn new(pool: PgPool) -> Self {
        let options = (*pool.connect_options()).clone();
        let store = PgStore::connect_with(options).await.unwrap();
        store
            .initialize(&installation(), "fixture-cluster")
            .await
            .unwrap();
        let receipt = bob_assignment(&store).await;
        for (purpose, public) in [
            (
                MaterialPurpose::ExecutionCredential,
                credential_digest(TOKEN).unwrap(),
            ),
            (MaterialPurpose::WorkerHost, HOST_KEY.to_owned()),
        ] {
            store
                .insert_material(
                    receipt.instance_id,
                    purpose,
                    &NewMaterial {
                        sealed: b"sealed".to_vec(),
                        public: Some(public),
                    },
                )
                .await
                .unwrap();
        }
        let secrets = Secrets::new().await;
        let app = router(Execution::new(store.clone()));
        let (listener, address) = bind("127.0.0.1:0").await.unwrap();
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
            store,
            instance: receipt.instance_id,
            receipt,
            url: format!("https://{address}"),
            client: secrets.client(None),
            contract: Contract::load("execution.openapi.yaml"),
            _secrets: secrets,
            server,
        }
    }

    async fn call(
        &self,
        operation: &str,
        path: &str,
        body: Option<Value>,
        token: Option<&str>,
    ) -> Reply {
        let mut request = match &body {
            Some(body) => self.client.post(format!("{}{path}", self.url)).json(body),
            None => self.client.get(format!("{}{path}", self.url)),
        };
        if let Some(token) = token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        let response = request.send().await.unwrap();
        let status = response.status().as_u16();
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .map(|v| v.to_str().unwrap().to_owned())
        };
        assert_eq!(
            header("cache-control").as_deref(),
            Some("no-store"),
            "{operation}"
        );
        let (retry_after, authenticate) = (header("retry-after"), header("www-authenticate"));
        let body: Value = response.json().await.unwrap();
        self.contract.check(operation, status, &body);
        Reply {
            status,
            retry_after,
            authenticate,
            body,
        }
    }

    async fn acquire(&self, request: &str, session: &str) -> Reply {
        self.call(
            "acquireExecutionLease",
            "/execution/v1/leases/acquire",
            Some(
                json!({"session": session, "mode": "per_session", "request": request,
                        "runtime": {"hermes_home": "/opt/data", "workdir": "/workspace"}}),
            ),
            Some(TOKEN),
        )
        .await
    }

    /// What the controller does once the worker answers over SSH.
    async fn make_ready(&self) {
        let pending = self.store.worker_environments(self.instance).await.unwrap();
        let id = pending[0].id;
        self.store
            .transition_environment(
                id,
                EnvironmentState::Pending,
                EnvironmentState::Starting,
                Some("pod-uid"),
            )
            .await
            .unwrap();
        self.store
            .transition_environment(
                id,
                EnvironmentState::Starting,
                EnvironmentState::Ready,
                Some("10.42.0.9"),
            )
            .await
            .unwrap();
    }
}

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn a_failed_backend_is_refused_with_its_contract_error(pool: PgPool) {
    let fixture = Fixture::new(pool).await;
    assert_eq!(fixture.acquire("r1", "s1").await.status, 409);
    let environment = fixture
        .store
        .worker_environments(fixture.instance)
        .await
        .unwrap()[0]
        .id;
    assert!(
        fixture
            .store
            .fail_environment(
                environment,
                EnvironmentState::Pending,
                hermes_core::domain::execution::WorkerFailure::ImageUnavailable,
            )
            .await
            .unwrap()
    );
    for (request, session) in [("r1", "s1"), ("r2", "s1")] {
        let refused = fixture.acquire(request, session).await;
        assert_eq!(
            (refused.status, refused.body["error"].as_str()),
            (422, Some("backend_failed"))
        );
        assert_eq!(refused.retry_after, None);
    }
}

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn the_plugin_acquires_renews_releases_and_closes_through_the_contract(pool: PgPool) {
    let fixture = Fixture::new(pool).await;
    let pending = fixture.acquire("r1", "s1").await;
    assert_eq!(
        (pending.status, pending.body["error"].as_str()),
        (409, Some("environment_pending"))
    );
    assert_eq!(pending.retry_after.as_deref(), Some("1"));
    fixture.make_ready().await;

    let granted = fixture.acquire("r1", "s1").await;
    assert_eq!(granted.status, 200);
    assert_eq!(
        granted.body["ssh"],
        json!({"host": "10.42.0.9", "port": 2222, "user": "worker", "host_key": HOST_KEY})
    );
    assert_eq!(granted.body["generation"], "1");
    let lease = granted.body["lease"].clone();
    let replay = fixture.acquire("r1", "s1").await;
    assert_eq!(replay.body["lease"], lease);

    let renewed = fixture
        .call(
            "heartbeatExecutionLease",
            "/execution/v1/leases/heartbeat",
            Some(json!({"lease": lease})),
            Some(TOKEN),
        )
        .await;
    assert_eq!(
        (renewed.status, renewed.body["ttl"].as_i64()),
        (200, Some(120))
    );
    let listed = fixture
        .call(
            "listNativeExecutionEnvironments",
            "/execution/v1/environments",
            None,
            Some(TOKEN),
        )
        .await;
    assert_eq!(listed.body[0]["leases"], 1);
    assert_eq!(listed.body[0]["provisioned"], true);
    let released = fixture
        .call(
            "releaseExecutionLease",
            "/execution/v1/leases/release",
            Some(json!({"lease": lease})),
            Some(TOKEN),
        )
        .await;
    assert_eq!(released.body, json!({"released": true}));
    let dead = fixture
        .call(
            "heartbeatExecutionLease",
            "/execution/v1/leases/heartbeat",
            Some(json!({"lease": lease})),
            Some(TOKEN),
        )
        .await;
    assert_eq!(
        (dead.status, dead.body["error"].as_str()),
        (409, Some("lease_not_live"))
    );
    assert_eq!(
        dead.retry_after, None,
        "Retry-After is only for pending acquisitions"
    );
    let unknown = fixture
        .call(
            "releaseExecutionLease",
            "/execution/v1/leases/release",
            Some(json!({"lease": Uuid::new_v4()})),
            Some(TOKEN),
        )
        .await;
    assert_eq!(unknown.status, 404);

    let closed = fixture
        .call(
            "closeExecutionSession",
            "/execution/v1/sessions/close",
            Some(json!({"session": "s1", "runtime": {"hermes_home": "/opt/data", "workdir": "/workspace"}})),
            Some(TOKEN),
        )
        .await;
    assert_eq!(closed.body, json!({"closed": true}));
    let stale = fixture
        .call(
            "listNativeExecutionEnvironments",
            "/execution/v1/environments",
            None,
            Some(TOKEN),
        )
        .await;
    assert_eq!(stale.body[0]["stale"], true);
}

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn credentials_scope_and_strictness_are_enforced(pool: PgPool) {
    let fixture = Fixture::new(pool).await;
    for token in [None, Some("f".repeat(64)), Some("short".to_owned())] {
        let reply = fixture
            .call(
                "listNativeExecutionEnvironments",
                "/execution/v1/environments",
                None,
                token.as_deref(),
            )
            .await;
        assert_eq!(
            (reply.status, reply.body["error"].as_str()),
            (401, Some("invalid_identity_evidence"))
        );
        assert_eq!(reply.authenticate.as_deref(), Some("Bearer"));
    }
    let query = fixture
        .call(
            "listNativeExecutionEnvironments",
            "/execution/v1/environments?instance=x",
            None,
            Some(TOKEN),
        )
        .await;
    assert_eq!(query.status, 422, "no caller-selected scope");
    for body in [
        json!({"session": "s", "mode": "per_session", "request": "r", "image": "attacker"}),
        json!({"session": "s", "mode": "per_session", "request": "r", "runtime": {"hermes_home": "/x", "workdir": "/y", "mount": "/"}}),
        json!({"session": "s", "mode": "per_session"}),
        json!({"session": "s", "mode": "per_session", "request": "r", "parent_lease": Uuid::nil()}),
    ] {
        let reply = fixture
            .call(
                "acquireExecutionLease",
                "/execution/v1/leases/acquire",
                Some(body.clone()),
                Some(TOKEN),
            )
            .await;
        assert_eq!(
            (reply.status, reply.body["error"].as_str()),
            (422, Some("invalid_request")),
            "{body}"
        );
        assert!(!reply.body.to_string().contains("attacker"));
    }
    let foreign = fixture
        .call(
            "acquireExecutionLease",
            "/execution/v1/leases/acquire",
            Some(json!({"session": "s", "mode": "per_session", "request": "r", "runtime_id": "gpu-tools"})),
            Some(TOKEN),
        )
        .await;
    assert_eq!(
        (foreign.status, foreign.body["error"].as_str()),
        (403, Some("access_denied"))
    );

    set_access(&fixture.store, &fixture.receipt, Access::Revoked).await;
    let revoked = fixture.acquire("r9", "s9").await;
    assert_eq!(
        revoked.status, 401,
        "a revoked instance's credential is refused at once"
    );
}

/// `value` as compact JSON followed by spaces up to exactly `size` bytes.
fn padded(value: &Value, size: usize) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(value).unwrap();
    assert!(bytes.len() <= size);
    bytes.resize(size, b' ');
    bytes
}

impl Fixture {
    /// A heartbeat on its own connection with exactly these header lines.
    async fn heartbeat_with(&self, headers: &[(&str, &[u8])], body: &[u8]) -> Response {
        let mut raw = self._secrets.raw(&self.url).await;
        raw.send(&head("POST", "/execution/v1/leases/heartbeat", headers))
            .await;
        raw.send(body).await;
        raw.response().await
    }

    async fn heartbeat_sized(&self, body: &[u8]) -> Response {
        let length = body.len().to_string();
        let bearer = format!("Bearer {TOKEN}");
        self.heartbeat_with(
            &[
                ("Authorization", bearer.as_bytes()),
                ("Content-Type", b"application/json"),
                ("Content-Length", length.as_bytes()),
            ],
            body,
        )
        .await
    }
}

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn execution_bodies_are_bounded_at_exactly_16_kib_sized_or_streamed(pool: PgPool) {
    let fixture = Fixture::new(pool).await;
    let request = json!({"lease": Uuid::new_v4()});
    let baseline = fixture.heartbeat_sized(&padded(&request, 100)).await;
    assert_eq!(baseline.status, 404, "an unknown lease, not a refusal");
    fixture
        .contract
        .check("heartbeatExecutionLease", baseline.status, &baseline.json());

    for size in [16 * 1024 - 1, 16 * 1024] {
        let response = fixture.heartbeat_sized(&padded(&request, size)).await;
        assert_eq!(response.status, baseline.status, "{size} bytes");
        assert_eq!(response.json(), baseline.json(), "{size} bytes");
    }
    let over = fixture
        .heartbeat_sized(&padded(&request, 16 * 1024 + 1))
        .await;
    assert_eq!(
        (over.status, over.json()),
        (422, json!({"error": "invalid_request"}))
    );
    fixture
        .contract
        .check("heartbeatExecutionLease", over.status, &over.json());

    let bearer = format!("Bearer {TOKEN}");
    for (size, accepted) in [(16 * 1024, true), (16 * 1024 + 1, false)] {
        let response = fixture
            .heartbeat_with(
                &[
                    ("Authorization", bearer.as_bytes()),
                    ("Content-Type", b"application/json"),
                    ("Transfer-Encoding", b"chunked"),
                ],
                &chunked(&padded(&request, size), 1000),
            )
            .await;
        let expected = if accepted { 404 } else { 422 };
        assert_eq!(response.status, expected, "{size} streamed bytes");
    }
}

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn execution_requests_with_repeated_credentials_or_content_types_are_refused(pool: PgPool) {
    let fixture = Fixture::new(pool).await;
    let body = serde_json::to_vec(&json!({"lease": Uuid::new_v4()})).unwrap();
    let length = body.len().to_string();
    let valid = format!("Bearer {TOKEN}");
    let wrong = format!("Bearer {}", "f".repeat(64));

    // Parser order must never choose between two credentials, whichever is
    // the valid one.
    for (first, second) in [(&valid, &wrong), (&wrong, &valid), (&valid, &valid)] {
        let response = fixture
            .heartbeat_with(
                &[
                    ("Authorization", first.as_bytes()),
                    ("Authorization", second.as_bytes()),
                    ("Content-Type", b"application/json"),
                    ("Content-Length", length.as_bytes()),
                ],
                &body,
            )
            .await;
        assert_eq!(
            (
                response.status,
                response.json()["error"].as_str().map(str::to_owned)
            ),
            (401, Some("invalid_identity_evidence".into()))
        );
        assert_eq!(response.header("www-authenticate"), Some(&b"Bearer"[..]));
    }
    let response = fixture
        .heartbeat_with(
            &[
                ("Authorization", valid.as_bytes()),
                ("Content-Type", b"application/json"),
                ("Content-Type", b"text/plain"),
                ("Content-Length", length.as_bytes()),
            ],
            &body,
        )
        .await;
    assert_eq!(
        (response.status, response.json()),
        (422, json!({"error": "invalid_request"}))
    );
}
