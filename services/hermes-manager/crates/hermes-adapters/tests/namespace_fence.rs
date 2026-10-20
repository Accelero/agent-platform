//! PostgreSQL fencing plus the actual TLS Kubernetes transport with lost replies.
#[allow(dead_code)]
mod support;
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::get,
};
use hermes_adapters::{
    kubernetes::KubernetesDriver,
    ownership::OwnershipSigner,
    postgres::{FreshInventory, PgStore},
};
use hermes_application::{
    AdminPolicy, AssignmentCommand, Assignments, Error, InstallationScope, OperationClaim,
    OperationQueue, StepResult, VerifiedPrincipal,
    namespace_fence::{
        ACCESS, ADMISSION, Admission, EPOCH, GENERATION, NamespaceFenceObservation,
        NamespaceFenceRepository, PreparedNamespaceFence,
    },
    resources::{OwnedResource, ResourceObservation, ResourceRepository, ResourceVersion},
    routing::{RouteDecision, RouteRegistry},
};
use hermes_domain::{
    Access, Identity, TemplateVersion,
    resources::{ResourceKind, ResourceSpec, Retention},
};
use kube::Config;
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};
use uuid::Uuid;

struct Fixture {
    store: PgStore,
    scope: InstallationScope,
    claim: OperationClaim,
    assignments: Assignments<PgStore>,
    actor: VerifiedPrincipal,
    namespace: OwnedResource,
}
async fn fixture(pool: &PgPool) -> Fixture {
    let scope = InstallationScope {
        id: Uuid::new_v4(),
        epoch: 1,
        issuer: "https://sso.example.org/realms/agents".into(),
    };
    let store = PgStore::connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    store
        .initialize_fresh(
            &scope,
            FreshInventory {
                cluster_identity: "fence-test".into(),
                digest: "a".repeat(64),
                registered_instances: 0,
                unexplained_resources: 0,
            },
        )
        .await
        .unwrap();
    sqlx::query("INSERT INTO manager.instance_templates(id,version,schema_version,image,content_digest,approved) VALUES('personal-default',1,1,$1,$2,true)").bind(format!("ghcr.io/example/native@sha256:{}","a".repeat(64))).bind("a".repeat(64)).execute(pool).await.unwrap();
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
    let assignments = Assignments::new(
        store.clone(),
        scope.clone(),
        AdminPolicy {
            issuer: scope.issuer.clone(),
            audience: "manager-api".into(),
            management_client: "manager-admin".into(),
            administrator_role: "administrator".into(),
        },
    )
    .unwrap();
    assignments
        .execute(
            &actor,
            Uuid::new_v4(),
            "create",
            AssignmentCommand::Create {
                identity: Identity {
                    issuer: scope.issuer.clone(),
                    subject: "owner-a".into(),
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
    let spec = ResourceSpec {
        kind: ResourceKind::Namespace,
        namespace: None,
        namespace_uid: None,
        name: hermes_domain::instance_namespace(scope.id, claim.instance_id),
        manifest_digest: "b".repeat(64),
        purpose: "namespace".into(),
        retention: Retention::Retain,
        encrypted_material_ref: None,
    };
    store
        .prepare_resource(&scope, &claim, "namespace", spec.clone())
        .await
        .unwrap();
    store
        .bind_resource(
            &scope,
            &claim,
            "namespace",
            ResourceObservation {
                uid: "namespace-uid".into(),
                resource_version: "1".into(),
                manifest_digest: spec.manifest_digest.clone(),
            },
        )
        .await
        .unwrap();
    let namespace = store
        .find_resource(&scope, &claim, &spec)
        .await
        .unwrap()
        .unwrap();
    Fixture {
        store,
        scope,
        claim,
        assignments,
        actor,
        namespace,
    }
}
fn version(rv: &str) -> ResourceVersion {
    ResourceVersion {
        uid: "namespace-uid".into(),
        resource_version: rv.into(),
    }
}
fn observation(prepared: &PreparedNamespaceFence, rv: &str) -> NamespaceFenceObservation {
    NamespaceFenceObservation {
        uid: prepared.input.namespace_uid.clone(),
        resource_version: rv.into(),
        input_digest: prepared.input.digest().unwrap(),
        state: prepared.input.state.clone(),
    }
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn namespace_fence_inputs_are_immutable_and_never_publish_readiness(pool: PgPool) {
    let f = fixture(&pool).await;
    let prepared = f
        .store
        .prepare_namespace_fence(&f.scope, &f.claim, "close", Admission::Closed, version("1"))
        .await
        .unwrap();
    let original = serde_json::to_value(&prepared.input).unwrap();
    assert!(matches!(
        f.store
            .prepare_namespace_fence(
                &f.scope,
                &f.claim,
                "namespace",
                Admission::Closed,
                version("1")
            )
            .await,
        Err(Error::OwnershipConflict)
    ));
    assert!(matches!(
        f.store
            .prepare_namespace_fence(&f.scope, &f.claim, "close", Admission::Open, version("1"))
            .await,
        Err(Error::OwnershipConflict)
    ));
    // A fresh observation does not rewrite an uncertain call's precondition.
    let replay = f
        .store
        .prepare_namespace_fence(
            &f.scope,
            &f.claim,
            "close",
            Admission::Closed,
            version("99"),
        )
        .await
        .unwrap();
    assert_eq!(replay.input.resource_version, "1");
    assert_eq!(serde_json::to_value(&replay.input).unwrap(), original);
    assert!(sqlx::query("UPDATE manager.namespace_fence_inputs SET input=jsonb_set(input,'{resourceVersion}',to_jsonb('99'::text))").execute(&pool).await.is_err());
    assert!(
        sqlx::query("DELETE FROM manager.namespace_fence_inputs")
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(matches!(
        f.store
            .record_result(
                &f.scope,
                &f.claim,
                "close",
                StepResult::Verified {
                    uid: "namespace-uid".into(),
                    resource_version: "2".into()
                }
            )
            .await,
        Err(Error::OwnershipConflict)
    ));
    let mut wrong = observation(&prepared, "2");
    wrong.input_digest = "c".repeat(64);
    assert!(matches!(
        f.store
            .complete_namespace_fence(&f.scope, &f.claim, "close", wrong)
            .await,
        Err(Error::OwnershipConflict)
    ));
    let mut wrong = observation(&prepared, "2");
    wrong.uid = "foreign-namespace-uid".into();
    assert!(matches!(
        f.store
            .complete_namespace_fence(&f.scope, &f.claim, "close", wrong)
            .await,
        Err(Error::OwnershipConflict)
    ));
    f.store
        .complete_namespace_fence(&f.scope, &f.claim, "close", observation(&prepared, "2"))
        .await
        .unwrap();
    // Duplicate response/re-observation preserves the first immutable result.
    f.store
        .complete_namespace_fence(&f.scope, &f.claim, "close", observation(&prepared, "3"))
        .await
        .unwrap();
    let row = sqlx::query(
        "SELECT creation,observed_digest,resource_version FROM manager.owned_resources WHERE id=$1",
    )
    .bind(f.namespace.creation.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        row.get::<Value, _>("creation"),
        serde_json::to_value(&f.namespace.creation).unwrap()
    );
    assert_eq!(row.get::<String, _>("observed_digest"), "b".repeat(64));
    assert_eq!(row.get::<String, _>("resource_version"), "3");
    let result: Value = sqlx::query_scalar(
        "SELECT result FROM manager.operation_steps WHERE operation_id=$1 AND step_key='close'",
    )
    .bind(f.claim.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(result["resource_version"], "2");
    let open = f
        .store
        .prepare_namespace_fence(&f.scope, &f.claim, "open", Admission::Open, version("3"))
        .await
        .unwrap();
    f.store
        .complete_namespace_fence(&f.scope, &f.claim, "open", observation(&open, "4"))
        .await
        .unwrap();
    let row = sqlx::query("SELECT state,observed_generation FROM manager.instances WHERE id=$1")
        .bind(f.claim.instance_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_ne!(row.get::<String, _>("state"), "Ready");
    assert_eq!(row.get::<i64, _>("observed_generation"), 0);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM manager.instance_routes")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    assert!(matches!(
        f.store
            .resolve(
                &f.scope,
                &Identity {
                    issuer: f.scope.issuer.clone(),
                    subject: "owner-a".into()
                }
            )
            .await
            .unwrap(),
        RouteDecision::Pending { .. }
    ));
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn revoke_fences_old_writes_and_only_current_disabled_close_is_allowed(pool: PgPool) {
    let f = fixture(&pool).await;
    let pending = f
        .store
        .prepare_namespace_fence(
            &f.scope,
            &f.claim,
            "pending",
            Admission::Closed,
            version("1"),
        )
        .await
        .unwrap();
    let assignment: Uuid =
        sqlx::query_scalar("SELECT assignment_id FROM manager.instances WHERE id=$1")
            .bind(f.claim.instance_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    f.assignments
        .execute(
            &f.actor,
            Uuid::new_v4(),
            "revoke",
            AssignmentCommand::SetAccess {
                assignment_id: assignment,
                expected_version: 1,
                access: Access::Revoked,
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        f.store
            .complete_namespace_fence(&f.scope, &f.claim, "pending", observation(&pending, "2"))
            .await,
        Err(Error::StaleFence)
    ));
    assert!(matches!(
        f.store
            .load_pending_namespace_fences(&f.scope, &f.claim, Admission::Closed)
            .await,
        Err(Error::StaleFence)
    ));
    assert!(matches!(
        f.store
            .prepare_namespace_fence(&f.scope, &f.claim, "stale", Admission::Closed, version("1"))
            .await,
        Err(Error::StaleFence)
    ));
    let current = f
        .store
        .claim(&f.scope, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        f.store
            .load_pending_namespace_fences(&f.scope, &current, Admission::Open)
            .await,
        Err(Error::Forbidden)
    ));
    assert!(
        f.store
            .load_pending_namespace_fences(&f.scope, &current, Admission::Closed)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        f.store
            .prepare_namespace_fence(&f.scope, &current, "open", Admission::Open, version("1"))
            .await,
        Err(Error::Forbidden)
    ));
    let mut spoof = current.clone();
    spoof.kind = "commission".into();
    assert!(matches!(
        f.store
            .prepare_namespace_fence(&f.scope, &spoof, "spoof", Admission::Closed, version("1"))
            .await,
        Err(Error::StaleFence)
    ));
    let closed = f
        .store
        .prepare_namespace_fence(&f.scope, &current, "close", Admission::Closed, version("1"))
        .await
        .unwrap();
    assert_eq!(closed.input.state.access, Access::Revoked);
    assert_eq!(closed.input.state.generation, 2);
    f.store
        .complete_namespace_fence(&f.scope, &current, "close", observation(&closed, "2"))
        .await
        .unwrap();
    // Bypassing the acknowledged command cannot silently change pinned access.
    sqlx::query("UPDATE manager.assignments SET access='enabled' WHERE id=$1")
        .bind(assignment)
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        f.store
            .prepare_namespace_fence(
                &f.scope,
                &current,
                "changed",
                Admission::Closed,
                version("2")
            )
            .await,
        Err(Error::StaleFence)
    ));
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn recovery_epoch_reuses_bound_namespace_but_refuses_old_claims(pool: PgPool) {
    let f = fixture(&pool).await;
    let closed = f
        .store
        .prepare_namespace_fence(&f.scope, &f.claim, "close", Admission::Closed, version("1"))
        .await
        .unwrap();
    f.store
        .complete_namespace_fence(&f.scope, &f.claim, "close", observation(&closed, "2"))
        .await
        .unwrap();
    f.store.finish(&f.scope, &f.claim).await.unwrap();
    sqlx::query("UPDATE manager.installations SET epoch=2")
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        f.store
            .complete_namespace_fence(&f.scope, &f.claim, "close", observation(&closed, "2"))
            .await,
        Err(Error::RecoveryRequired)
    ));
    let mut scope = f.scope.clone();
    scope.epoch = 2;
    sqlx::query("INSERT INTO manager.operations(id,installation_id,instance_id,kind,generation,epoch,input) VALUES($1,$2,$3,'resync',1,2,$4)")
        .bind(Uuid::new_v4()).bind(scope.id).bind(f.claim.instance_id).bind(json!({"access":"enabled","extraPublicInput":true})).execute(&pool).await.unwrap();
    let current = f
        .store
        .claim(&scope, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    let prepared = f
        .store
        .prepare_namespace_fence(
            &scope,
            &current,
            "new-epoch",
            Admission::Closed,
            version("2"),
        )
        .await
        .unwrap();
    assert_eq!(prepared.input.state.epoch, 2);
    assert_eq!(prepared.namespace.creation.epoch, 1);
    assert!(matches!(
        f.store
            .complete_namespace_fence(&scope, &f.claim, "close", observation(&closed, "2"))
            .await,
        Err(Error::StaleFence)
    ));
    f.store
        .complete_namespace_fence(&scope, &current, "new-epoch", observation(&prepared, "3"))
        .await
        .unwrap();
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn pending_fence_reclaim_is_bounded_and_separates_admission_states(pool: PgPool) {
    let f = fixture(&pool).await;
    for index in 0..32 {
        f.store
            .prepare_namespace_fence(
                &f.scope,
                &f.claim,
                &format!("close-{index}"),
                Admission::Closed,
                version("1"),
            )
            .await
            .unwrap();
    }
    assert_eq!(
        f.store
            .load_pending_namespace_fences(&f.scope, &f.claim, Admission::Closed)
            .await
            .unwrap()
            .len(),
        32
    );
    f.store
        .prepare_namespace_fence(
            &f.scope,
            &f.claim,
            "close-32",
            Admission::Closed,
            version("1"),
        )
        .await
        .unwrap();
    let original = f
        .store
        .prepare_namespace_fence(&f.scope, &f.claim, "open", Admission::Open, version("7"))
        .await
        .unwrap();
    sqlx::query("UPDATE manager.operations SET lease_until=clock_timestamp()-interval '1 second' WHERE id=$1")
        .bind(f.claim.id).execute(&pool).await.unwrap();
    let current = f
        .store
        .claim(&f.scope, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    let pending = f
        .store
        .load_pending_namespace_fences(&f.scope, &current, Admission::Open)
        .await
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].key, "open");
    assert_eq!(pending[0].input, original.input);
    assert_eq!(pending[0].step.intent, original.step.intent);
    assert!(matches!(
        f.store
            .load_pending_namespace_fences(&f.scope, &current, Admission::Closed)
            .await,
        Err(Error::OwnershipConflict)
    ));
    let fences: Vec<i64> = sqlx::query_scalar("SELECT fence FROM manager.operation_steps WHERE operation_id=$1 AND step_key LIKE 'close-%'")
        .bind(current.id).fetch_all(&pool).await.unwrap();
    assert_eq!(fences.len(), 33);
    assert!(fences.iter().all(|fence| *fence == f.claim.fence));
    let routes: i64 = sqlx::query_scalar("SELECT count(*) FROM manager.instance_routes")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(routes, 0);
}

struct NamespaceApi {
    object: Value,
    patches: Vec<Value>,
    lose_response: bool,
}
type ApiState = Arc<Mutex<NamespaceApi>>;
fn api_error(code: StatusCode) -> (StatusCode, Json<Value>) {
    (
        code,
        Json(
            json!({"kind":"Status","apiVersion":"v1","status":"Failure","message":"fixture request failed","reason":"Conflict","code":code.as_u16()}),
        ),
    )
}
async fn api_get(
    State(state): State<ApiState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if name == "kube-system" {
        return Ok(Json(
            json!({"apiVersion":"v1","kind":"Namespace","metadata":{"name":"kube-system","uid":"fence-test"}}),
        ));
    }
    let state = state.lock().unwrap();
    if state.object["metadata"]["name"] != name {
        return Err(api_error(StatusCode::NOT_FOUND));
    }
    Ok(Json(state.object.clone()))
}
async fn api_patch(
    State(state): State<ApiState>,
    Path(name): Path<String>,
    Json(patch): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mut state = state.lock().unwrap();
    if state.object["metadata"]["name"] != name
        || state.object["metadata"]["uid"] != patch["metadata"]["uid"]
        || state.object["metadata"]["resourceVersion"] != patch["metadata"]["resourceVersion"]
    {
        return Err(api_error(StatusCode::CONFLICT));
    }
    state.patches.push(patch.clone());
    for (key, value) in patch["metadata"]["labels"].as_object().unwrap() {
        state.object["metadata"]["labels"][key] = value.clone()
    }
    let rv = state.object["metadata"]["resourceVersion"]
        .as_str()
        .unwrap()
        .parse::<u64>()
        .unwrap()
        + 1;
    state.object["metadata"]["resourceVersion"] = json!(rv.to_string());
    if std::mem::take(&mut state.lose_response) {
        return Err(api_error(StatusCode::GATEWAY_TIMEOUT));
    }
    Ok(Json(state.object.clone()))
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn tls_driver_recovers_lost_patch_response_and_preserves_provenance(pool: PgPool) {
    let f = fixture(&pool).await;
    let secrets = support::Secrets::new().await;
    let signer = OwnershipSigner::new(
        "fence-key".into(),
        BTreeMap::from([("fence-key".into(), vec![42; 32])]),
    )
    .unwrap();
    let proof = signer.sign(&f.namespace.creation).unwrap();
    let original = json!({"apiVersion":"v1","kind":"Namespace","metadata":{"name":f.namespace.creation.spec.name,"uid":"namespace-uid","resourceVersion":"1","labels":{"agent-platform.io/installation":f.scope.id,"agent-platform.io/instance":f.claim.instance_id,"agent-platform.io/resource":f.namespace.creation.id},"annotations":{"agent-platform.io/creation-proof":proof,"agent-platform.io/applied-intent":"b".repeat(64)}}});
    let state = Arc::new(Mutex::new(NamespaceApi {
        object: original.clone(),
        patches: vec![],
        lose_response: true,
    }));
    let running = support::start(
        &secrets,
        Router::new()
            .route("/api/v1/namespaces/{name}", get(api_get).patch(api_patch))
            .with_state(state.clone()),
        false,
    )
    .await;
    let mut config = Config::new(running.url.parse().unwrap());
    config.root_cert = Some(
        rustls_pemfile::certs(&mut secrets.read("ca.crt").await.as_slice())
            .map(|c| c.unwrap().as_ref().to_vec())
            .collect(),
    );
    let driver = KubernetesDriver::from_config(config, signer, "fence-test".into()).unwrap();
    let (initial_version, initial_fence) =
        driver.current_namespace_fence(&f.namespace).await.unwrap();
    assert_eq!(initial_version.resource_version, "1");
    assert!(initial_fence.is_none());
    let close = f
        .store
        .prepare_namespace_fence(&f.scope, &f.claim, "close", Admission::Closed, version("1"))
        .await
        .unwrap();
    assert!(matches!(
        driver.apply_namespace_fence(&close, &f.claim).await,
        Err(Error::Unavailable)
    ));
    // The API committed before its reply was lost. Reopen the repository and
    // reclaim the expired worker; neither original input nor effect is replayed.
    sqlx::query("UPDATE manager.operations SET lease_until=clock_timestamp()-interval '1 second' WHERE id=$1").bind(f.claim.id).execute(&pool).await.unwrap();
    let reopened = PgStore::connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    let claim = reopened
        .claim(&f.scope, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        reopened
            .load_pending_namespace_fences(&f.scope, &f.claim, Admission::Closed)
            .await,
        Err(Error::StaleFence)
    ));
    let mut pending = reopened
        .load_pending_namespace_fences(&f.scope, &claim, Admission::Closed)
        .await
        .unwrap();
    assert_eq!(pending.len(), 1);
    let retry = pending.remove(0);
    assert_eq!(retry.key, "close");
    assert_eq!(retry.input, close.input);
    assert_eq!(retry.step.intent, close.step.intent);
    assert_eq!(retry.input.resource_version, "1");
    let fence: i64 = sqlx::query_scalar(
        "SELECT fence FROM manager.operation_steps WHERE operation_id=$1 AND step_key='close'",
    )
    .bind(claim.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(fence, claim.fence);
    assert!(
        reopened
            .load_pending_namespace_fences(&f.scope, &claim, Admission::Open)
            .await
            .unwrap()
            .is_empty()
    );
    let observed = driver.apply_namespace_fence(&retry, &claim).await.unwrap();
    assert_eq!(observed.resource_version, "2");
    reopened
        .complete_namespace_fence(&f.scope, &claim, "close", observed)
        .await
        .unwrap();
    assert_eq!(state.lock().unwrap().patches.len(), 1);
    assert!(
        reopened
            .load_pending_namespace_fences(&f.scope, &claim, Admission::Closed)
            .await
            .unwrap()
            .is_empty()
    );
    let open = reopened
        .prepare_namespace_fence(&f.scope, &claim, "open", Admission::Open, version("2"))
        .await
        .unwrap();
    state.lock().unwrap().object["metadata"]["resourceVersion"] = json!("3");
    assert!(matches!(
        driver.apply_namespace_fence(&open, &claim).await,
        Err(Error::PreconditionFailed)
    ));
    let repeated = reopened
        .prepare_namespace_fence(&f.scope, &claim, "open", Admission::Open, version("3"))
        .await
        .unwrap();
    assert_eq!(repeated.input.resource_version, "2");
    let open = reopened
        .prepare_namespace_fence(
            &f.scope,
            &claim,
            "open-new-rv",
            Admission::Open,
            version("3"),
        )
        .await
        .unwrap();
    let observed = driver.apply_namespace_fence(&open, &claim).await.unwrap();
    reopened
        .complete_namespace_fence(&f.scope, &claim, "open-new-rv", observed)
        .await
        .unwrap();
    // A fresh CAS establishes OPEN; the old uncertain version is completed only
    // by observing its exact desired state, without replaying that stale write.
    let mut pending = reopened
        .load_pending_namespace_fences(&f.scope, &claim, Admission::Open)
        .await
        .unwrap();
    assert_eq!(pending.len(), 1);
    let original_open = pending.remove(0);
    assert_eq!(original_open.key, "open");
    assert_eq!(original_open.input.resource_version, "2");
    let observed = driver
        .apply_namespace_fence(&original_open, &claim)
        .await
        .unwrap();
    reopened
        .complete_namespace_fence(&f.scope, &claim, &original_open.key, observed)
        .await
        .unwrap();
    assert!(
        reopened
            .load_pending_namespace_fences(&f.scope, &claim, Admission::Open)
            .await
            .unwrap()
            .is_empty()
    );
    {
        let snapshot = state.lock().unwrap();
        assert_eq!(snapshot.patches.len(), 2);
        for patch in &snapshot.patches {
            assert_eq!(patch.as_object().unwrap().len(), 1);
            assert_eq!(patch["metadata"].as_object().unwrap().len(), 3);
            let labels = patch["metadata"]["labels"].as_object().unwrap();
            assert_eq!(labels.len(), 4);
            for key in [ACCESS, ADMISSION, GENERATION, EPOCH] {
                assert!(labels.contains_key(key))
            }
        }
        assert_eq!(
            snapshot.object["metadata"]["annotations"],
            original["metadata"]["annotations"]
        );
        assert_eq!(snapshot.object["metadata"]["labels"][ADMISSION], "open");
    }
    let (version, fence) = driver.current_namespace_fence(&f.namespace).await.unwrap();
    assert_eq!(version.resource_version, "4");
    assert_eq!(fence.unwrap().admission, Admission::Open);
    state.lock().unwrap().object["metadata"]["labels"]
        .as_object_mut()
        .unwrap()
        .remove(EPOCH);
    assert!(matches!(
        driver.current_namespace_fence(&f.namespace).await,
        Err(Error::OwnershipConflict)
    ));
}
