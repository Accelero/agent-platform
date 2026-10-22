//! Dedicated execution listener uses native opaque credentials over real TLS.
//! These tests commit desired policy only; no fake worker readiness or leases.
#[path = "support/grants.rs"]
mod grant_fixture;
use std::collections::{BTreeMap, BTreeSet};

use axum::{Json, Router, routing::get};
use hermes_adapters::{
    auth::{AccessTokenVerifier, TokenPolicy, http_client, unix_time},
    execution_http::execution_router,
    http::{admin_router, resolver_router},
    materials::MaterialVault,
    postgres::{FreshInventory, PgStore},
    session_crypto::Sealer,
};
use hermes_application::{
    AdminPolicy, AssignmentCommand, Assignments, InstallationScope, OperationQueue,
    VerifiedPrincipal,
    execution::{
        AssignmentExecution, ExecutionAdministration, ExecutionCommand, RuntimeDefinition,
    },
    routing::RoutePolicy,
    terminal::clients::{ExecutionClientRepository, ExecutionTokenHash},
};
use hermes_domain::{Access, Identity, TemplateVersion};
use jsonwebtoken::{Algorithm, Header, encode};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

#[allow(dead_code)]
mod support;
use support::{ROUTER, Running, Secrets, start};

struct Owner {
    assignment: Uuid,
    instance: Uuid,
    token: String,
}
struct Fixture {
    secrets: Secrets,
    scope: InstallationScope,
    store: PgStore,
    actor: VerifiedPrincipal,
    assignments: Assignments<PgStore>,
    owners: [Owner; 2],
    execution: Running,
    admin: Running,
    resolver: Running,
    _jwks: Running,
    client: reqwest::Client,
    router_client: reqwest::Client,
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
                    cluster_identity: "execution-https-fixture".into(),
                    digest: "a".repeat(64),
                    registered_instances: 0,
                    unexplained_resources: 0,
                },
            )
            .await
            .unwrap();
        grant_fixture::grant(pool, &scope, &["administrator"]).await;
        sqlx::query("INSERT INTO manager.instance_templates(id,version,schema_version,image,content_digest,approved) VALUES('personal-default',1,1,$1,$2,true)")
            .bind(format!("ghcr.io/example/hermes@sha256:{}","a".repeat(64))).bind("a".repeat(64)).execute(pool).await.unwrap();
        let admin_policy = AdminPolicy {
            issuer: scope.issuer.clone(),
            audience: "manager-api".into(),
            management_client: "manager-admin".into(),
            administrator_role: "administrator".into(),
        };
        let actor = VerifiedPrincipal {
            identity: Identity {
                issuer: scope.issuer.clone(),
                subject: "administrator".into(),
            },
            audiences: BTreeSet::from(["manager-api".into()]),
            client_roles: BTreeMap::from([(
                "manager-admin".into(),
                BTreeSet::from(["administrator".into()]),
            )]),
        };
        let assignments =
            Assignments::new(store.clone(), scope.clone(), admin_policy.clone()).unwrap();
        let cipher = Sealer::new(
            "fixture".into(),
            BTreeMap::from([("fixture".into(), vec![43; 32])]),
        )
        .unwrap();
        let vault = MaterialVault::new(store.clone(), cipher);
        let template = TemplateVersion {
            id: "personal-default".into(),
            version: 1,
        };
        let mut owners = Vec::new();
        for subject in ["native-owner-a", "native-owner-b"] {
            let accepted = assignments
                .execute(
                    &actor,
                    Uuid::new_v4(),
                    subject,
                    AssignmentCommand::Create {
                        identity: Identity {
                            issuer: scope.issuer.clone(),
                            subject: subject.into(),
                        },
                        access: Access::Enabled,
                        template: template.clone(),
                    },
                )
                .await
                .unwrap();
            let claim = store
                .claim(&scope, Uuid::new_v4(), 30)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(claim.instance_id, accepted.instance_id);
            let (material, bootstrap) = vault
                .native_bootstrap(
                    &scope,
                    &claim,
                    template.clone(),
                    "https://manager.example.org/execution",
                )
                .await
                .unwrap();
            store
                .register_under_current_claim(
                    &scope,
                    &claim,
                    &material.context,
                    ExecutionTokenHash::from_token(&bootstrap.execution_client_token).unwrap(),
                )
                .await
                .unwrap();
            // A namespace identity allows desired reference resolution, without
            // claiming any Kubernetes resources or instance/worker readiness.
            sqlx::query("UPDATE manager.instances SET namespace_uid=$1 WHERE id=$2")
                .bind(Uuid::new_v4().to_string())
                .bind(accepted.instance_id)
                .execute(pool)
                .await
                .unwrap();
            owners.push(Owner {
                assignment: accepted.assignment_id,
                instance: accepted.instance_id,
                token: bootstrap.execution_client_token.clone(),
            });
        }
        let configuration =
            ExecutionAdministration::new(store.clone(), scope.clone(), admin_policy.clone())
                .unwrap();
        let definition:RuntimeDefinition=serde_json::from_value(json!({"schemaVersion":1,"runtime":{"id":"python-tools","version":1},
            "image":format!("ghcr.io/example/worker@sha256:{}","c".repeat(64)),"sshContract":"managed-ssh-v1","uid":1000,"gid":1000,"serviceAccount":"worker",
            "resources":{"cpuMillis":1000,"memoryMib":1024,"ephemeralStorageMib":1024},"locale":"C.UTF-8","egress":[],"pullSecretNames":[]})).unwrap();
        configuration
            .execute(
                &actor,
                Uuid::new_v4(),
                "register-runtime",
                ExecutionCommand::RegisterRuntime { definition },
            )
            .await
            .unwrap();
        for owner in &owners {
            let policy:AssignmentExecution=serde_json::from_value(json!({"defaultRuntimeId":"python-tools","allowed":[{"runtime":{"id":"python-tools","version":1},"resources":{"cpuMillis":1000,"memoryMib":1024,"ephemeralStorageMib":1024},"mounts":[]}]})).unwrap();
            configuration
                .execute(
                    &actor,
                    Uuid::new_v4(),
                    &format!("policy-{}", owner.instance),
                    ExecutionCommand::SetPolicy {
                        assignment_id: owner.assignment,
                        expected_version: 1,
                        policy,
                    },
                )
                .await
                .unwrap();
        }
        let oidc_client = http_client(Some(&secrets.read("ca.crt").await)).unwrap();
        let tokens = |audience: &str, client: &str| {
            AccessTokenVerifier::new(
                TokenPolicy {
                    issuer: scope.issuer.clone(),
                    jwks_url: format!("{}/jwks", jwks.url),
                    audience: audience.into(),
                    authorized_clients: BTreeSet::from([client.into()]),
                    algorithms: vec![Algorithm::RS256],
                    maximum_age_seconds: None,
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
                admin_policy,
                tokens("manager-api", "manager-admin"),
            )
            .unwrap(),
            false,
        )
        .await;
        let resolver = start(
            &secrets,
            resolver_router(
                store.clone(),
                scope.clone(),
                RoutePolicy {
                    issuer: scope.issuer.clone(),
                    audience: "route-api".into(),
                    authorized_client: "hermes-router".into(),
                    router_service_identity: ROUTER.into(),
                },
                tokens("route-api", "hermes-router"),
            )
            .unwrap(),
            true,
        )
        .await;
        let execution = start(
            &secrets,
            execution_router(store.clone(), scope.clone()).unwrap(),
            false,
        )
        .await;
        let client = secrets.client(None).await;
        let router_client = secrets.client(Some("router")).await;
        Self {
            secrets,
            scope,
            store,
            actor,
            assignments,
            owners: owners.try_into().ok().unwrap(),
            execution,
            admin,
            resolver,
            _jwks: jwks,
            client,
            router_client,
        }
    }
    fn jwt(&self) -> String {
        let now = unix_time().unwrap();
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some("fixture".into());
        encode(&header,&json!({"iss":self.scope.issuer,"sub":"administrator","aud":"manager-api","azp":"manager-admin","typ":"Bearer","sid":"fixture-session","iat":now,"exp":now+300,"resource_access":{"manager-admin":{"roles":["administrator"]}}}),&self.secrets.signer).unwrap()
    }
    async fn acquire(&self, owner: usize, body: &Value) -> reqwest::Response {
        self.client
            .post(format!(
                "{}/execution/v1/leases/acquire",
                self.execution.url
            ))
            .bearer_auth(&self.owners[owner].token)
            .json(body)
            .send()
            .await
            .unwrap()
    }
    async fn environments(&self, owner: usize) -> reqwest::Response {
        self.client
            .get(format!("{}/execution/v1/environments", self.execution.url))
            .bearer_auth(&self.owners[owner].token)
            .send()
            .await
            .unwrap()
    }
}
fn acquire_body() -> Value {
    json!({"session":"native-session","mode":"per_session","request":"request-one","runtime_id":"python-tools","runtime":{"hermes_home":"/var/lib/hermes","workdir":"/workspace"}})
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn opaque_execution_credentials_and_human_credentials_have_separate_tls_boundaries(
    pool: PgPool,
) {
    let f = Fixture::new(&pool).await;
    let native = &f.owners[0].token;
    for owner in &f.owners {
        let verified = f
            .store
            .authenticate_execution_client(
                &f.scope,
                ExecutionTokenHash::from_token(&owner.token).unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(verified.instance_id, owner.instance);
    }
    assert_eq!(f.environments(0).await.status(), 200);
    let admin_session = format!("{}/api/v1/session", f.admin.url);
    assert_eq!(
        f.client
            .get(&admin_session)
            .bearer_auth(f.jwt())
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        f.client
            .get(&admin_session)
            .bearer_auth(native)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let route = f
        .router_client
        .post(format!("{}/internal/v1/routes/resolve", f.resolver.url))
        .bearer_auth(native)
        .json(&json!({"identity":{"issuer":f.scope.issuer,"subject":"native-owner-a"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(route.status(), 401);
    let url = format!("{}/execution/v1/environments", f.execution.url);
    for authorization in [
        None,
        Some("Basic ignored".into()),
        Some(format!("bearer {native}")),
        Some(format!("Bearer {native} extra")),
        Some("Bearer short".into()),
        Some(format!("Bearer {}", "F".repeat(64))),
        Some(format!("Bearer {}", "0".repeat(64))),
        Some(format!("Bearer {}", f.jwt())),
    ] {
        let mut request = f.client.get(&url);
        if let Some(value) = authorization {
            request = request.header("Authorization", value);
        }
        let response = request.send().await.unwrap();
        assert_eq!(response.status(), 401);
        assert_eq!(response.headers()["www-authenticate"], "Bearer");
        assert_eq!(response.headers()["cache-control"], "no-store");
    }
    assert_eq!(
        f.client
            .get(&url)
            .header(
                "Cookie",
                format!("__Host-hermes-admin={native}; __Host-hermes-admin-browser={native}")
            )
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        f.client
            .get(&url)
            .bearer_auth(native)
            .header("Authorization", format!("Bearer {native}"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        f.client
            .get(format!("{}/api/v1/session", f.execution.url))
            .bearer_auth(native)
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    assert_eq!(
        f.client
            .get(format!("{}/execution/v1/environments", f.admin.url))
            .bearer_auth(f.jwt())
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn pending_policy_never_grants_and_current_disable_immediately_denies_native_access(
    pool: PgPool,
) {
    let f = Fixture::new(&pool).await;
    let request = acquire_body();
    for owner in 0..2 {
        for _ in 0..2 {
            let response = f.acquire(owner, &request).await;
            let status = response.status();
            if status != 409 {
                panic!(
                    "pending acquisition returned {status}: {}",
                    response.text().await.unwrap()
                );
            }
            assert_eq!(response.headers()["retry-after"], "1");
            assert_eq!(
                response.json::<Value>().await.unwrap(),
                json!({"error":"environment_pending"})
            );
        }
        assert_eq!(
            f.environments(owner).await.json::<Value>().await.unwrap(),
            json!([])
        );
    }
    // No caller-supplied scope exists; the credential alone selects the instance.
    let mut foreign = request.clone();
    foreign["profile"] = json!("default");
    assert_eq!(f.acquire(0, &foreign).await.status(), 422);
    foreign = request.clone();
    foreign["runtime_id"] = json!("not-approved");
    assert_eq!(f.acquire(0, &foreign).await.status(), 403);
    for table in [
        "execution_environments",
        "execution_requests",
        "execution_leases",
    ] {
        let sql = format!("SELECT count(*) FROM manager.{table}");
        let count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }
    let applied: Vec<i64> = sqlx::query_scalar(
        "SELECT applied_generation FROM manager.execution_policy_status ORDER BY instance_id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(applied, vec![0, 0]);
    f.assignments
        .execute(
            &f.actor,
            Uuid::new_v4(),
            "disable-owner-a",
            AssignmentCommand::SetAccess {
                assignment_id: f.owners[0].assignment,
                expected_version: 2,
                access: Access::Suspended,
            },
        )
        .await
        .unwrap();
    assert_eq!(f.environments(0).await.status(), 401);
    assert_eq!(f.acquire(0, &request).await.status(), 401);
    assert_eq!(f.environments(1).await.status(), 200);
    assert_eq!(f.acquire(1, &request).await.status(), 409);
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn native_protocol_rejects_unknown_duplicate_and_oversized_requests(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let request = acquire_body();
    for name in [
        "image",
        "mounts",
        "template",
        "instance_id",
        "environment",
        "pod_spec",
    ] {
        let mut rejected = request.clone();
        rejected[name] = json!("untrusted-input-marker");
        let response = f.acquire(0, &rejected).await;
        assert_eq!(response.status(), 422);
        assert!(
            !response
                .text()
                .await
                .unwrap()
                .contains("untrusted-input-marker")
        );
    }
    let mut context = request.clone();
    context["runtime"]["image"] = json!("forbidden");
    assert_eq!(f.acquire(0, &context).await.status(), 422);
    context = request.clone();
    context["runtime"]["workdir"] = json!("relative/path");
    assert_eq!(f.acquire(0, &context).await.status(), 422);
    let duplicate = request.to_string().replacen(
        "\"session\":\"native-session\"",
        "\"session\":\"native-session\",\"session\":\"foreign\"",
        1,
    );
    let oversized = format!("{{\"session\":\"{}\"}}", "x".repeat(16 * 1024));
    let response = f
        .client
        .get(format!("{}/execution/v1/environments", f.execution.url))
        .bearer_auth(&f.owners[0].token)
        .body(oversized.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 422);
    assert_eq!(
        response.json::<Value>().await.unwrap(),
        json!({"error":"invalid_request"})
    );
    for raw in [duplicate, oversized] {
        let response = f
            .client
            .post(format!("{}/execution/v1/leases/acquire", f.execution.url))
            .bearer_auth(&f.owners[0].token)
            .header("Content-Type", "application/json")
            .body(raw)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 422);
        assert!(response.bytes().await.unwrap().len() < 256);
    }
    for path in [
        "environments?instanceId=foreign",
        "environments?profile=foreign",
        "environments?limit=1",
        "leases/acquire?profile=default",
    ] {
        let response = f
            .client
            .request(
                if path.starts_with("leases/") {
                    reqwest::Method::POST
                } else {
                    reqwest::Method::GET
                },
                format!("{}/execution/v1/{path}", f.execution.url),
            )
            .bearer_auth(&f.owners[0].token)
            .json(&request)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 422);
    }
    for path in ["leases/heartbeat", "leases/release"] {
        let response = f
            .client
            .post(format!("{}/execution/v1/{path}", f.execution.url))
            .bearer_auth(&f.owners[0].token)
            .json(&json!({"lease":Uuid::new_v4(),"instance_id":f.owners[1].instance}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 422);
    }
    let response = f
        .client
        .post(format!("{}/execution/v1/sessions/close", f.execution.url))
        .bearer_auth(&f.owners[0].token)
        .json(&json!({"session":"native-session","runtime_id":"not-approved"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 403);
}

/// Historical inventory fixture only: it models a previously recorded Pod
/// awaiting reconciliation after restart. It sets no Ready/applied status and
/// creates no leases, requests, worker intents or readiness receipts.
async fn seed_historical_pending_inventory(pool: &PgPool, f: &Fixture, owner: usize) -> Uuid {
    let instance = f.owners[owner].instance;
    let (generation,digest):(i64,String)=sqlx::query_as("SELECT desired_generation,content_digest FROM manager.execution_policy_status s JOIN manager.execution_policy_versions p ON p.instance_id=s.instance_id AND p.generation=s.desired_generation WHERE s.instance_id=$1")
        .bind(instance).fetch_one(pool).await.unwrap();
    sqlx::query("INSERT INTO manager.execution_effective_policy_receipts(instance_id,generation,epoch,content_digest) VALUES($1,$2,$3,$4)")
        .bind(instance).bind(generation).bind(f.scope.epoch).bind(&digest).execute(pool).await.unwrap();
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO manager.execution_environments(id,installation_id,instance_id,identity_digest,epoch,assignment_generation,policy_generation,policy_digest,runtime_id,session,mode,limits,invocation,pod_name,pod_uid,ssh_host) VALUES($1,$2,$3,$4,$5,1,$6,$7,'python-tools',$8,'per_session',$9,$10,$11,$12,$13)")
        .bind(id).bind(f.scope.id).bind(instance).bind(vec![u8::try_from(owner+1).unwrap();32]).bind(f.scope.epoch).bind(generation).bind(digest)
        .bind(format!("historical-session-{owner}"))
        .bind(serde_json::to_value(hermes_application::terminal::leases::ExecutionLimits::default()).unwrap())
        .bind(json!({"hermes_home":"/private/historical-home","workdir":"/private/historical-workdir"}))
        .bind(format!("historical-private-pod-{owner}")).bind(format!("historical-private-uid-{owner}")).bind(format!("historical-private-host-{owner}.svc.cluster.local"))
        .execute(pool).await.unwrap();
    id
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn historical_environment_inventory_is_redacted_and_scoped_to_authenticated_instance(
    pool: PgPool,
) {
    let f = Fixture::new(&pool).await;
    let ids = [
        seed_historical_pending_inventory(&pool, &f, 0).await,
        seed_historical_pending_inventory(&pool, &f, 1).await,
    ];
    for (owner, id) in ids.iter().enumerate() {
        let response = f.environments(owner).await;
        assert_eq!(response.status(), 200);
        let body = response.text().await.unwrap();
        for private in [
            "historical-private",
            "/private/",
            f.owners[owner].token.as_str(),
        ] {
            assert!(!body.contains(private));
        }
        let inventory: Value = serde_json::from_str(&body).unwrap();
        let rows = inventory.as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["id"], id.to_string());
        assert_eq!(rows[0]["runtime_id"], "python-tools");
        assert_eq!(rows[0]["session"], format!("historical-session-{owner}"));
        assert_eq!(rows[0]["leases"], 0);
        let fields: BTreeSet<&str> = rows[0]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            fields,
            BTreeSet::from([
                "id",
                "session",
                "mode",
                "generation",
                "stale",
                "provisioned",
                "leases",
                "runtime_id"
            ])
        );
        assert_eq!(f.acquire(owner, &acquire_body()).await.status(), 409);
    }
    let applied: Vec<i64> =
        sqlx::query_scalar("SELECT applied_generation FROM manager.execution_policy_status")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(applied, vec![0, 0]);
    let leases: i64 = sqlx::query_scalar("SELECT count(*) FROM manager.execution_leases")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(leases, 0);
}
