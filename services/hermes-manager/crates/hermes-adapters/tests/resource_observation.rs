//! PostgreSQL plus real TLS GET-only recovery of late native CREATE results.
#[path = "support/grants.rs"]
mod grant_fixture;
use axum::{
    Json, Router,
    extract::{Request, State},
    http::{Method, StatusCode},
};
use hermes_adapters::{
    kubernetes::{KubernetesDriver, Manifest},
    materials::MaterialVault,
    ownership::OwnershipSigner,
    postgres::{FreshInventory, PgStore},
    session_crypto::Sealer,
};
use hermes_application::{
    Action, AdminPolicy, AssignmentCommand, Assignments, Error, InstallationScope, OperationClaim,
    OperationQueue, StepIntent, StepResult, VerifiedPrincipal,
    resources::{
        OwnedResource, PreparedResource, ResourceObservation, ResourceReconciler,
        ResourceRepository,
    },
};
use hermes_domain::{
    Access, Identity, TemplateVersion,
    resources::{ResourceKind, ResourceSpec, Retention},
};
use kube::Config;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};
use uuid::Uuid;
use zeroize::Zeroizing;

#[allow(dead_code)]
mod support;

fn cipher() -> Sealer {
    Sealer::new(
        "fixture".into(),
        BTreeMap::from([("fixture".into(), vec![42; 32])]),
    )
    .unwrap()
}
fn signer() -> OwnershipSigner {
    OwnershipSigner::new(
        "fixture".into(),
        BTreeMap::from([("fixture".into(), vec![43; 32])]),
    )
    .unwrap()
}
fn policy(scope: &InstallationScope) -> AdminPolicy {
    AdminPolicy {
        issuer: scope.issuer.clone(),
        audience: "manager-api".into(),
        management_client: "manager-admin".into(),
        administrator_role: "administrator".into(),
    }
}
struct Fixture {
    store: PgStore,
    scope: InstallationScope,
    claim: OperationClaim,
    commands: Assignments<PgStore>,
    actor: VerifiedPrincipal,
    assignment: Uuid,
    namespace: OwnedResource,
    namespace_manifest: Manifest,
    namespace_value: Value,
}
impl Fixture {
    async fn new(pool: &PgPool, snapshot: bool) -> Self {
        let store = PgStore::connect_with((*pool.connect_options()).clone())
            .await
            .unwrap();
        let scope = InstallationScope {
            id: Uuid::new_v4(),
            epoch: 1,
            issuer: "https://sso.example.org/realms/agents".into(),
        };
        store
            .initialize_fresh(
                &scope,
                FreshInventory {
                    cluster_identity: "observation-fixture".into(),
                    digest: "a".repeat(64),
                    registered_instances: 0,
                    unexplained_resources: 0,
                },
            )
            .await
            .unwrap();
        grant_fixture::grant(pool, &scope, &["fixture-admin"]).await;
        sqlx::query("INSERT INTO manager.instance_templates(id,version,schema_version,image,content_digest,approved) VALUES('personal-default',1,1,$1,$2,true)")
            .bind(format!("ghcr.io/example/hermes@sha256:{}", "a".repeat(64))).bind("a".repeat(64)).execute(pool).await.unwrap();
        let actor = VerifiedPrincipal {
            identity: Identity {
                issuer: scope.issuer.clone(),
                subject: "fixture-admin".into(),
            },
            audiences: BTreeSet::from(["manager-api".into()]),
            client_roles: BTreeMap::from([(
                "manager-admin".into(),
                BTreeSet::from(["administrator".into()]),
            )]),
        };
        let commands = Assignments::new(store.clone(), scope.clone(), policy(&scope)).unwrap();
        let accepted = commands
            .execute(
                &actor,
                Uuid::new_v4(),
                "create",
                AssignmentCommand::Create {
                    identity: Identity {
                        issuer: scope.issuer.clone(),
                        subject: "fixture-owner".into(),
                    },
                    access: Access::Enabled,
                    template: TemplateVersion {
                        id: "personal-default".into(),
                        version: 1,
                    },
                },
            )
            .await
            .unwrap();
        let claim = store
            .claim(&scope, Uuid::new_v4(), 30)
            .await
            .unwrap()
            .unwrap();
        let value = json!({"apiVersion":"v1","kind":"Namespace","metadata":{"name":hermes_domain::instance_namespace(scope.id,claim.instance_id),"labels":{"pod-security.kubernetes.io/enforce":"restricted"}}});
        let manifest = Manifest::new(value.clone()).unwrap();
        let spec = ResourceSpec {
            kind: ResourceKind::Namespace,
            namespace: None,
            namespace_uid: None,
            name: value["metadata"]["name"].as_str().unwrap().into(),
            manifest_digest: manifest.digest().unwrap(),
            purpose: "namespace".into(),
            retention: Retention::Retain,
            encrypted_material_ref: None,
        };
        if snapshot {
            snapshot_material(&store, &scope, &claim, "render-namespace-g1", &spec, &value).await;
        }
        let prepared = store
            .prepare_resource(&scope, &claim, "namespace", spec)
            .await
            .unwrap();
        Self {
            store,
            scope,
            claim,
            commands,
            actor,
            assignment: accepted.assignment_id,
            namespace: prepared.resource,
            namespace_manifest: manifest,
            namespace_value: value,
        }
    }
    async fn disable(&mut self, access: Access) {
        self.commands
            .execute(
                &self.actor,
                Uuid::new_v4(),
                "disable",
                AssignmentCommand::SetAccess {
                    assignment_id: self.assignment,
                    expected_version: 1,
                    access,
                },
            )
            .await
            .unwrap();
        self.claim = self
            .store
            .claim(&self.scope, Uuid::new_v4(), 30)
            .await
            .unwrap()
            .unwrap();
    }
    fn vault(&self) -> MaterialVault<PgStore> {
        MaterialVault::new(self.store.clone(), cipher())
    }
    async fn original_manifest(&self, purpose: &str) -> Manifest {
        let material = self
            .vault()
            .read_retained(&self.scope, &self.claim, purpose)
            .await
            .unwrap()
            .unwrap();
        let frozen: Value = serde_json::from_slice(&material.value).unwrap();
        Manifest::new(frozen[0]["manifest"].clone()).unwrap()
    }
}
async fn snapshot_material(
    store: &PgStore,
    scope: &InstallationScope,
    claim: &OperationClaim,
    purpose: &str,
    spec: &ResourceSpec,
    value: &Value,
) {
    let bytes = serde_json::to_vec(&json!([{"spec":spec,"manifest":value}])).unwrap();
    MaterialVault::new(store.clone(), cipher())
        .get_or_create(scope, claim, purpose, || Ok(Zeroizing::new(bytes)))
        .await
        .unwrap();
}
fn live(owned: &OwnedResource, mut value: Value, uid: &str) -> Value {
    value["metadata"]["uid"] = json!(uid);
    value["metadata"]["resourceVersion"] = json!("1");
    value["metadata"]["labels"]["agent-platform.io/installation"] =
        json!(owned.creation.installation_id);
    value["metadata"]["labels"]["agent-platform.io/instance"] = json!(owned.creation.instance_id);
    value["metadata"]["labels"]["agent-platform.io/resource"] = json!(owned.creation.id);
    value["metadata"]["annotations"] = json!({"agent-platform.io/creation-proof":signer().sign(&owned.creation).unwrap(),"agent-platform.io/applied-intent":owned.creation.spec.manifest_digest});
    value
}
#[derive(Default)]
struct FakeApi {
    objects: BTreeMap<String, Value>,
    methods: Vec<Method>,
}
type ApiState = Arc<Mutex<FakeApi>>;
async fn api(State(state): State<ApiState>, request: Request) -> (StatusCode, Json<Value>) {
    let mut state = state.lock().unwrap();
    state.methods.push(request.method().clone());
    if request.method() != Method::GET {
        return (
            StatusCode::METHOD_NOT_ALLOWED,
            Json(
                json!({"apiVersion":"v1","kind":"Status","status":"Failure","reason":"MethodNotAllowed","code":405}),
            ),
        );
    }
    if let Some(value) = state.objects.get(request.uri().path()) {
        return (StatusCode::OK, Json(value.clone()));
    }
    (
        StatusCode::NOT_FOUND,
        Json(
            json!({"apiVersion":"v1","kind":"Status","status":"Failure","reason":"NotFound","code":404}),
        ),
    )
}
async fn driver(state: &ApiState) -> (KubernetesDriver, support::Running) {
    state.lock().unwrap().objects.insert("/api/v1/namespaces/kube-system".into(), json!({"apiVersion":"v1","kind":"Namespace","metadata":{"name":"kube-system","uid":"observation-fixture"}}));
    let secrets = support::Secrets::new().await;
    let running = support::start(
        &secrets,
        Router::new().fallback(api).with_state(state.clone()),
        false,
    )
    .await;
    let mut config = Config::new(running.url.parse().unwrap());
    config.root_cert = Some(
        rustls_pemfile::certs(&mut secrets.read("ca.crt").await.as_slice())
            .map(|c| c.unwrap().as_ref().to_vec())
            .collect(),
    );
    (
        KubernetesDriver::from_config(config, signer(), "observation-fixture".into()).unwrap(),
        running,
    )
}
fn observed(uid: &str, digest: &str) -> ResourceObservation {
    ResourceObservation {
        uid: uid.into(),
        resource_version: "1".into(),
        manifest_digest: digest.into(),
    }
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn tls_missing_then_late_namespace_create_recovers_while_disabled_without_any_write(
    pool: PgPool,
) {
    let mut f = Fixture::new(&pool, true).await;
    let original_claim = f.claim.clone();
    f.disable(Access::Suspended).await;
    let manifest = f.original_manifest("render-namespace-g1").await;
    let state = Arc::new(Mutex::new(FakeApi::default()));
    let (driver, _tls) = driver(&state).await;
    let reconciler = ResourceReconciler::new(f.store.clone(), driver.clone(), f.scope.clone());
    assert!(
        reconciler
            .observe_existing(&f.claim, "missing", f.namespace.creation.id, &manifest)
            .await
            .unwrap()
            .is_none()
    );
    let missing: Value = sqlx::query_scalar(
        "SELECT result FROM manager.operation_steps WHERE operation_id=$1 AND step_key='missing'",
    )
    .bind(f.claim.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(missing["status"], "observed_missing");
    let reservation: (String, Option<String>) =
        sqlx::query_as("SELECT state,uid FROM manager.owned_resources WHERE id=$1")
            .bind(f.namespace.creation.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(reservation, ("Reserved".into(), None));
    let path = format!("/api/v1/namespaces/{}", f.namespace.creation.spec.name);
    let original = live(
        &f.namespace,
        f.namespace_value.clone(),
        "late-namespace-uid",
    );
    state
        .lock()
        .unwrap()
        .objects
        .insert(path.clone(), original.clone());
    let count = state.lock().unwrap().methods.len();
    assert!(
        reconciler
            .observe_existing(&f.claim, "missing", f.namespace.creation.id, &manifest)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        state.lock().unwrap().methods.len(),
        count,
        "completed missing history was reinterpreted"
    );
    let prepared = f
        .store
        .prepare_observation(&f.scope, &f.claim, "late", f.namespace.creation.id)
        .await
        .unwrap();
    assert_eq!(prepared.resource.creation.operation_id, original_claim.id);
    assert!(prepared.resource.creation.generation < f.claim.generation);
    let mut altered = f.namespace_value.clone();
    altered["metadata"]["labels"]["pod-security.kubernetes.io/enforce"] = json!("privileged");
    let altered = Manifest::new(altered).unwrap();
    let requests = state.lock().unwrap().methods.len();
    assert!(matches!(
        driver.observe_reserved(&prepared, &altered).await,
        Err(Error::OwnershipConflict)
    ));
    assert_eq!(
        state.lock().unwrap().methods.len(),
        requests,
        "altered original input reached transport"
    );
    let mut wrong = original.clone();
    wrong["metadata"]["annotations"]["agent-platform.io/creation-proof"] = json!("foreign-proof");
    state.lock().unwrap().objects.insert(path.clone(), wrong);
    assert!(matches!(
        driver.observe_reserved(&prepared, &manifest).await,
        Err(Error::OwnershipConflict)
    ));
    let mut wrong = original.clone();
    wrong["metadata"]["labels"]["pod-security.kubernetes.io/enforce"] = json!("privileged");
    state.lock().unwrap().objects.insert(path.clone(), wrong);
    assert!(matches!(
        driver.observe_reserved(&prepared, &manifest).await,
        Err(Error::OwnershipConflict)
    ));
    state.lock().unwrap().objects.insert(path, original);
    let seen = driver
        .observe_reserved(&prepared, &manifest)
        .await
        .unwrap()
        .unwrap();
    // Lose the GET result before UID commit; a reclaimed claim must observe the
    // same original creation instead of replaying the earlier apply operation.
    sqlx::query("UPDATE manager.operations SET lease_until=clock_timestamp()-interval '1 second' WHERE id=$1").bind(f.claim.id).execute(&pool).await.unwrap();
    let next = f
        .store
        .claim(&f.scope, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        f.store
            .bind_observation(&f.scope, &f.claim, "late", seen)
            .await,
        Err(Error::StaleFence)
    ));
    f.claim = next;
    let seen = reconciler
        .observe_existing(&f.claim, "late", f.namespace.creation.id, &manifest)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(seen.uid, "late-namespace-uid");
    let ns_uid: Option<String> =
        sqlx::query_scalar("SELECT namespace_uid FROM manager.instances WHERE id=$1")
            .bind(f.claim.instance_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(ns_uid.as_deref(), Some("late-namespace-uid"));
    let original_result: Value = sqlx::query_scalar(
        "SELECT result FROM manager.operation_steps WHERE operation_id=$1 AND step_key='namespace'",
    )
    .bind(original_claim.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(original_result["status"], "unknown");
    assert!(matches!(
        f.store
            .prepare_resource(
                &f.scope,
                &f.claim,
                "forbidden-apply",
                f.namespace.creation.spec.clone()
            )
            .await,
        Err(Error::Forbidden)
    ));
    assert!(matches!(
        f.store
            .bind_resource(
                &f.scope,
                &f.claim,
                "late",
                observed("late-namespace-uid", &manifest.digest().unwrap())
            )
            .await,
        Err(Error::OwnershipConflict)
    ));
    for result in [
        StepResult::Verified {
            uid: "late-namespace-uid".into(),
            resource_version: "1".into(),
        },
        StepResult::ObservedMissing,
        StepResult::Unknown,
    ] {
        assert!(matches!(
            f.store
                .record_result(&f.scope, &f.claim, "late", result)
                .await,
            Err(Error::OwnershipConflict)
        ));
    }
    let replay = f
        .store
        .prepare_observation(&f.scope, &f.claim, "late", f.namespace.creation.id)
        .await
        .unwrap();
    assert!(matches!(
        driver
            .apply(
                &PreparedResource {
                    resource: replay.resource,
                    step: replay.step
                },
                &manifest
            )
            .await,
        Err(Error::OwnershipConflict)
    ));
    assert!(matches!(
        f.store
            .prepare_observation(&f.scope, &original_claim, "stale", f.namespace.creation.id)
            .await,
        Err(Error::StaleFence)
    ));
    assert!(
        state
            .lock()
            .unwrap()
            .methods
            .iter()
            .all(|method| method == Method::GET)
    );
    let resources: i64 = sqlx::query_scalar("SELECT count(*) FROM manager.owned_resources")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(resources, 1);
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn tls_enabled_later_generation_observation_checks_namespace_fields_and_bound_uid(
    pool: PgPool,
) {
    let mut f = Fixture::new(&pool, true).await;
    f.store
        .bind_resource(
            &f.scope,
            &f.claim,
            "namespace",
            observed("namespace-uid", &f.namespace_manifest.digest().unwrap()),
        )
        .await
        .unwrap();
    let namespace = live(&f.namespace, f.namespace_value.clone(), "namespace-uid");
    let ns = f.namespace.creation.spec.name.clone();
    let value = json!({"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":"native-config","namespace":ns},"data":{"fixture":"approved-original"}});
    let manifest = Manifest::new(value.clone()).unwrap();
    let spec = ResourceSpec {
        kind: ResourceKind::ConfigMap,
        namespace: Some(ns.clone()),
        namespace_uid: Some("namespace-uid".into()),
        name: "native-config".into(),
        manifest_digest: manifest.digest().unwrap(),
        purpose: "native-config".into(),
        retention: Retention::Delete,
        encrypted_material_ref: None,
    };
    snapshot_material(
        &f.store,
        &f.scope,
        &f.claim,
        "render-instance-g1",
        &spec,
        &value,
    )
    .await;
    let original = f
        .store
        .prepare_resource(&f.scope, &f.claim, "config", spec)
        .await
        .unwrap();
    f.commands
        .execute(
            &f.actor,
            Uuid::new_v4(),
            "retry",
            AssignmentCommand::Retry {
                instance_id: f.claim.instance_id,
                expected_version: 1,
            },
        )
        .await
        .unwrap();
    f.claim = f
        .store
        .claim(&f.scope, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    let frozen = f
        .vault()
        .get_or_create(&f.scope, &f.claim, "render-instance-g1", || {
            panic!("existing rendering regenerated")
        })
        .await
        .unwrap();
    let frozen: Value = serde_json::from_slice(&frozen.value).unwrap();
    let manifest = Manifest::new(frozen[0]["manifest"].clone()).unwrap();
    let path = format!("/api/v1/namespaces/{ns}/configmaps/native-config");
    let ns_path = format!("/api/v1/namespaces/{ns}");
    let object = live(&original.resource, value, "original-config-uid");
    let state = Arc::new(Mutex::new(FakeApi::default()));
    state
        .lock()
        .unwrap()
        .objects
        .insert(ns_path.clone(), namespace.clone());
    state
        .lock()
        .unwrap()
        .objects
        .insert(path.clone(), object.clone());
    let (driver, _tls) = driver(&state).await;
    let prepared = f
        .store
        .prepare_observation(
            &f.scope,
            &f.claim,
            "recover-config",
            original.resource.creation.id,
        )
        .await
        .unwrap();
    for (pointer, value) in [
        ("/data/fixture", json!("changed")),
        (
            "/metadata/annotations/agent-platform.io~1applied-intent",
            json!("0".repeat(64)),
        ),
        (
            "/metadata/labels/agent-platform.io~1instance",
            json!(Uuid::new_v4()),
        ),
    ] {
        let mut wrong = object.clone();
        *wrong.pointer_mut(pointer).unwrap() = value;
        state.lock().unwrap().objects.insert(path.clone(), wrong);
        assert!(matches!(
            driver.observe_reserved(&prepared, &manifest).await,
            Err(Error::OwnershipConflict)
        ));
    }
    state
        .lock()
        .unwrap()
        .objects
        .insert(path.clone(), object.clone());
    let mut wrong_namespace = namespace.clone();
    wrong_namespace["metadata"]["uid"] = json!("foreign-namespace-uid");
    state
        .lock()
        .unwrap()
        .objects
        .insert(ns_path.clone(), wrong_namespace);
    assert!(matches!(
        driver.observe_reserved(&prepared, &manifest).await,
        Err(Error::OwnershipConflict)
    ));
    state.lock().unwrap().objects.insert(ns_path, namespace);
    let seen = driver
        .observe_reserved(&prepared, &manifest)
        .await
        .unwrap()
        .unwrap();
    f.store
        .bind_observation(&f.scope, &f.claim, "recover-config", seen)
        .await
        .unwrap();
    let replay = f
        .store
        .prepare_observation(
            &f.scope,
            &f.claim,
            "recover-config",
            original.resource.creation.id,
        )
        .await
        .unwrap();
    let mut foreign = object;
    foreign["metadata"]["uid"] = json!("replacement-config-uid");
    state.lock().unwrap().objects.insert(path, foreign);
    assert!(matches!(
        driver.observe_reserved(&replay, &manifest).await,
        Err(Error::OwnershipConflict)
    ));
    assert!(matches!(
        f.store
            .bind_observation(
                &f.scope,
                &f.claim,
                "recover-config",
                observed("replacement-config-uid", &manifest.digest().unwrap())
            )
            .await,
        Err(Error::OwnershipConflict)
    ));
    assert!(
        state
            .lock()
            .unwrap()
            .methods
            .iter()
            .all(|method| method == Method::GET)
    );
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn missing_original_rendering_requires_recovery_without_reservation_or_generation(
    pool: PgPool,
) {
    let f = Fixture::new(&pool, false).await;
    assert!(matches!(
        f.store
            .prepare_observation(
                &f.scope,
                &f.claim,
                "missing-rendering",
                f.namespace.creation.id
            )
            .await,
        Err(Error::RecoveryRequired)
    ));
    assert!(matches!(
        f.store
            .prepare_observation(&f.scope, &f.claim, "foreign", Uuid::new_v4())
            .await,
        Err(Error::NotFound)
    ));
    let observations: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM manager.operation_steps WHERE intent->>'action'='observe'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(observations, 0);
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM manager.bootstrap_materials")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 0);
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn generic_observe_completion_and_old_epoch_first_adoption_are_rejected(pool: PgPool) {
    let mut f = Fixture::new(&pool, true).await;
    let spec = &f.namespace.creation.spec;
    f.store
        .prepare_step(
            &f.scope,
            &f.claim,
            "generic",
            StepIntent {
                action: Action::Observe,
                creation_id: f.namespace.creation.id,
                kind: "Namespace".into(),
                namespace: None,
                name: spec.name.clone(),
                namespace_uid: None,
                expected_uid: None,
                expected_resource_version: None,
                manifest_digest: spec.manifest_digest.clone(),
                encrypted_material_ref: None,
            },
        )
        .await
        .unwrap();
    for result in [
        StepResult::Verified {
            uid: "untrusted-uid".into(),
            resource_version: "1".into(),
        },
        StepResult::ObservedMissing,
    ] {
        assert!(matches!(
            f.store
                .record_result(&f.scope, &f.claim, "generic", result)
                .await,
            Err(Error::OwnershipConflict)
        ));
    }
    for (key, action) in [
        ("generic-close", Action::FenceClose),
        ("generic-open", Action::FenceOpen),
    ] {
        f.store
            .prepare_step(
                &f.scope,
                &f.claim,
                key,
                StepIntent {
                    action,
                    creation_id: f.namespace.creation.id,
                    kind: "Namespace".into(),
                    namespace: None,
                    name: f.namespace.creation.spec.name.clone(),
                    namespace_uid: None,
                    expected_uid: Some("untrusted-uid".into()),
                    expected_resource_version: Some("1".into()),
                    manifest_digest: f.namespace.creation.spec.manifest_digest.clone(),
                    encrypted_material_ref: None,
                },
            )
            .await
            .unwrap();
        assert!(matches!(
            f.store
                .record_result(
                    &f.scope,
                    &f.claim,
                    key,
                    StepResult::Verified {
                        uid: "untrusted-uid".into(),
                        resource_version: "2".into(),
                    }
                )
                .await,
            Err(Error::OwnershipConflict)
        ));
    }
    sqlx::query("UPDATE manager.installations SET epoch=2")
        .execute(&pool)
        .await
        .unwrap();
    f.scope.epoch = 2;
    f.commands = Assignments::new(f.store.clone(), f.scope.clone(), policy(&f.scope)).unwrap();
    f.disable(Access::Revoked).await;
    assert!(matches!(
        f.store
            .prepare_observation(&f.scope, &f.claim, "old-epoch", f.namespace.creation.id)
            .await,
        Err(Error::RecoveryRequired)
    ));
    let snapshots: i64 = sqlx::query_scalar("SELECT count(*) FROM manager.bootstrap_materials")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        snapshots, 1,
        "the original encrypted rendering must exist for this epoch proof"
    );
    let reserved: (String, Option<String>) =
        sqlx::query_as("SELECT state,uid FROM manager.owned_resources WHERE id=$1")
            .bind(f.namespace.creation.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(reserved, ("Reserved".into(), None));
}
