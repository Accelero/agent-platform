//! The route resolver over real mutual TLS and PostgreSQL, as the gateway
//! uses it. Every response is checked against
//! `contracts/hermes-manager/router.openapi.yaml`.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use hermes_core::app::assignments::AssignmentReceipt;
use hermes_core::app::controller::InstanceStore;
use hermes_core::app::routing::Routing;
use hermes_core::app::testing::contracts::{bob_assignment, set_access};
use hermes_core::app::testing::installation;
use hermes_core::domain::assignments::Access;
use hermes_core::domain::instances::{InstanceStatus, Phase};
use hermes_http::resolver::router;
use hermes_http::server::{bind, serve_until, tls_config};
use hermes_store::PgStore;
use hermes_testkit::contract::Contract;
use hermes_testkit::{FOREIGN_PEER, GATEWAY_PEER, Secrets};
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::task::JoinHandle;

const PATH: &str = "/internal/v1/routes/resolve";
const OPERATION: &str = "resolvePersonalHermesRoute";

struct Fixture {
    store: PgStore,
    receipt: AssignmentReceipt,
    url: String,
    secrets: Secrets,
    contract: Contract,
    server: JoinHandle<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

fn identity(subject: &str) -> Value {
    json!({"identity": {"issuer": installation().issuer, "subject": subject}})
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
        let secrets = Secrets::new().await;
        let app = router(
            Routing::new(store.clone(), installation()),
            GATEWAY_PEER.to_owned(),
        );
        let (listener, address) = bind("127.0.0.1:0").await.unwrap();
        let tls = tls_config(
            &secrets.read("server.crt"),
            &secrets.read("server.key"),
            Some(&secrets.read("ca.crt")),
        )
        .unwrap();
        let peers = BTreeSet::from([GATEWAY_PEER.to_owned(), FOREIGN_PEER.to_owned()]);
        let server = tokio::spawn(async move {
            serve_until(listener, tls, peers, app, std::future::pending())
                .await
                .unwrap();
        });
        Self {
            store,
            receipt,
            url: format!("https://{address}"),
            secrets,
            contract: Contract::load("router.openapi.yaml"),
            server,
        }
    }

    /// What the controller records once the instance is ready.
    async fn make_ready(&self) {
        let epoch = self.store.advance_epoch().await.unwrap();
        let instance = self.receipt.instance_id;
        self.store
            .bind_namespace(instance, "namespace-uid")
            .await
            .unwrap();
        let desired = self
            .store
            .desired_instance(instance)
            .await
            .unwrap()
            .unwrap();
        let status = InstanceStatus {
            phase: Phase::Ready,
            blocked: None,
            conditions: vec![],
            converged: true,
        };
        assert!(
            self.store
                .record_status(epoch, instance, desired.desired_generation, &status)
                .await
                .unwrap()
        );
    }

    async fn post(&self, request: reqwest::RequestBuilder) -> (u16, Value) {
        let response = request.send().await.expect("TLS with a client certificate");
        let status = response.status().as_u16();
        assert_eq!(
            response
                .headers()
                .get("cache-control")
                .map(|v| v.to_str().unwrap()),
            Some("no-store")
        );
        let body: Value = response.json().await.unwrap();
        (status, body)
    }

    async fn resolve(&self, peer: &str, body: &Value) -> (u16, Value) {
        let client = self.secrets.client(Some(peer));
        let request = client.post(format!("{}{PATH}", self.url)).json(body);
        let (status, body) = self.post(request).await;
        self.contract.check(OPERATION, status, &body);
        (status, body)
    }

    async fn raw(&self, path: &str, content_type: &str, body: &str) -> (u16, Value) {
        let client = self.secrets.client(Some("gateway"));
        let request = client
            .post(format!("{}{path}", self.url))
            .header("content-type", content_type)
            .body(body.to_owned());
        self.post(request).await
    }
}

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn the_resolver_routes_only_for_the_gateway_by_exact_identity(pool: PgPool) {
    let fixture = Fixture::new(pool).await;
    let bob = identity("bob");

    // Without a client certificate there is no TLS session at all; with
    // another service's certificate the answer is a denial.
    let anonymous = fixture
        .secrets
        .client(None)
        .post(format!("{}{PATH}", fixture.url))
        .json(&bob)
        .send()
        .await;
    assert!(anonymous.is_err());
    let (status, body) = fixture.resolve("foreign", &bob).await;
    assert_eq!(
        (status, body),
        (403, json!({"status": "denied", "reason": "access_denied"}))
    );

    let (status, body) = fixture.resolve("gateway", &bob).await;
    assert_eq!(
        (status, body),
        (
            202,
            json!({"status": "pending", "reason": "instance_not_ready"})
        )
    );
    fixture.make_ready().await;
    let (status, body) = fixture.resolve("gateway", &bob).await;
    assert_eq!(status, 200);
    assert_eq!(body["instanceId"], json!(fixture.receipt.instance_id));
    assert_eq!(
        body["destination"],
        json!({
            "namespace": installation().instance_namespace(fixture.receipt.instance_id),
            "service": "hermes",
            "port": 8080,
            "scheme": "http",
        })
    );
    assert_eq!(body["authorizationVersion"], json!(fixture.receipt.version));
    // Another service's certificate still gets nothing for a ready owner.
    assert_eq!(fixture.resolve("foreign", &bob).await.0, 403);

    for (subject, issuer) in [
        ("alice", installation().issuer),
        ("Bob", installation().issuer),
        ("bob", "https://other.example.org/realms/agents".to_owned()),
    ] {
        let request = json!({"identity": {"issuer": issuer, "subject": subject}});
        let (status, body) = fixture.resolve("gateway", &request).await;
        assert_eq!(
            (status, &body["status"]),
            (403, &json!("denied")),
            "{subject}"
        );
    }

    set_access(&fixture.store, &fixture.receipt, Access::Suspended).await;
    let (status, body) = fixture.resolve("gateway", &bob).await;
    assert_eq!((status, &body["status"]), (403, &json!("denied")));
}

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn resolver_requests_are_strict_and_never_reflected(pool: PgPool) {
    let fixture = Fixture::new(pool).await;
    let issuer = installation().issuer;
    let refused = [
        (
            PATH,
            "application/json",
            format!(r#"{{"identity":{{"issuer":"{issuer}","subject":"bob","subject":"carol"}}}}"#),
        ),
        (
            PATH,
            "application/json",
            format!(
                r#"{{"identity":{{"issuer":"{issuer}","subject":"bob","email":"bob@example.org"}}}}"#
            ),
        ),
        (
            PATH,
            "application/json",
            format!(
                r#"{{"identity":{{"issuer":"{issuer}","subject":"bob"}},"destination":"kube-system"}}"#
            ),
        ),
        (
            PATH,
            "application/json",
            format!(r#"{{"identity":{{"issuer":"{issuer}","subject":""}}}}"#),
        ),
        (
            PATH,
            "text/plain",
            format!(r#"{{"identity":{{"issuer":"{issuer}","subject":"bob"}}}}"#),
        ),
        (
            "/internal/v1/routes/resolve?subject=bob",
            "application/json",
            format!(r#"{{"identity":{{"issuer":"{issuer}","subject":"bob"}}}}"#),
        ),
        (
            PATH,
            "application/json",
            format!(
                r#"{{"identity":{{"issuer":"{issuer}","subject":"{}"}}}}"#,
                "b".repeat(17 * 1024)
            ),
        ),
    ];
    for (path, content_type, body) in refused {
        let (status, reply) = fixture.raw(path, content_type, &body).await;
        assert_eq!(
            (status, &reply),
            (422, &json!({"error": "invalid_request"})),
            "{path} {content_type}"
        );
        if path == PATH {
            fixture.contract.check(OPERATION, status, &reply);
        }
        assert!(!reply.to_string().contains("bob"));
    }
}

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn a_stalled_registry_is_unavailable_within_the_deadline(pool: PgPool) {
    let fixture = Fixture::new(pool.clone()).await;
    fixture.make_ready().await;
    assert_eq!(fixture.resolve("gateway", &identity("bob")).await.0, 200);

    let mut blocker = pool.begin().await.unwrap();
    sqlx::query("LOCK TABLE manager.instances IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *blocker)
        .await
        .unwrap();
    let started = Instant::now();
    let (status, body) = fixture.resolve("gateway", &identity("bob")).await;
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(
        (status, body),
        (
            503,
            json!({"status": "unavailable", "reason": "dependency_unavailable"})
        )
    );
    blocker.rollback().await.unwrap();
    assert_eq!(fixture.resolve("gateway", &identity("bob")).await.0, 200);
}
