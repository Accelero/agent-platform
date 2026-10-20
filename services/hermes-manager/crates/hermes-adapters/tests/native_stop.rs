//! Persisted retained stop inputs and the real bounded TLS Kubernetes transport.
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
    lifecycle::LifecycleRepository,
    namespace_fence::{
        ACCESS, ADMISSION, Admission, EPOCH, GENERATION, NamespaceFenceObservation,
        NamespaceFenceRepository,
    },
    native_stop::{
        NativeStopInspection, NativeStopObservation, NativeStopRepository, PreparedNativeStop,
    },
    resources::{OwnedResource, ResourceObservation, ResourceRepository, ResourceVersion},
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
    deployment: OwnedResource,
}
async fn fixture(pool: &PgPool, disabled: bool) -> Fixture {
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
                cluster_identity: "stop-test".into(),
                digest: "a".repeat(64),
                registered_instances: 0,
                unexplained_resources: 0,
            },
        )
        .await
        .unwrap();
    sqlx::query("INSERT INTO manager.instance_templates(id,version,schema_version,image,content_digest,approved) VALUES('personal-default',1,1,$1,$2,true)").bind(format!("ghcr.io/example/native@sha256:{}","a".repeat(64))).bind("b".repeat(64)).execute(pool).await.unwrap();
    let actor = VerifiedPrincipal {
        identity: Identity {
            issuer: scope.issuer.clone(),
            subject: "admin".into(),
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
    let accepted = assignments
        .execute(
            &actor,
            Uuid::new_v4(),
            "create",
            AssignmentCommand::Create {
                identity: Identity {
                    issuer: scope.issuer.clone(),
                    subject: "owner".into(),
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
    let mut claim = store
        .claim(&scope, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    let namespace_name = hermes_domain::instance_namespace(scope.id, claim.instance_id);
    let namespace_spec = ResourceSpec {
        kind: ResourceKind::Namespace,
        namespace: None,
        namespace_uid: None,
        name: namespace_name.clone(),
        manifest_digest: "b".repeat(64),
        purpose: "namespace".into(),
        retention: Retention::Retain,
        encrypted_material_ref: None,
    };
    let mut deployment_spec = namespace_spec.clone();
    deployment_spec.kind = ResourceKind::Deployment;
    deployment_spec.namespace = Some(namespace_name);
    deployment_spec.namespace_uid = Some("namespace-uid".into());
    deployment_spec.name = "hermes".into();
    deployment_spec.purpose = "native".into();
    for (key, spec, uid) in [
        ("namespace", namespace_spec.clone(), "namespace-uid"),
        ("native", deployment_spec.clone(), "deployment-uid"),
    ] {
        store
            .prepare_resource(&scope, &claim, key, spec.clone())
            .await
            .unwrap();
        store
            .bind_resource(
                &scope,
                &claim,
                key,
                ResourceObservation {
                    uid: uid.into(),
                    resource_version: "1".into(),
                    manifest_digest: spec.manifest_digest,
                },
            )
            .await
            .unwrap();
    }
    let namespace = store
        .find_resource(&scope, &claim, &namespace_spec)
        .await
        .unwrap()
        .unwrap();
    let deployment = store
        .find_resource(&scope, &claim, &deployment_spec)
        .await
        .unwrap()
        .unwrap();
    if disabled {
        assignments
            .execute(
                &actor,
                Uuid::new_v4(),
                "suspend",
                AssignmentCommand::SetAccess {
                    assignment_id: accepted.assignment_id,
                    expected_version: 1,
                    access: Access::Suspended,
                },
            )
            .await
            .unwrap();
        claim = store
            .claim(&scope, Uuid::new_v4(), 30)
            .await
            .unwrap()
            .unwrap();
    }
    Fixture {
        store,
        scope,
        claim,
        assignments,
        actor,
        namespace,
        deployment,
    }
}
async fn close(f: &Fixture, key: &str) {
    close_at(f, key, "1").await;
}
async fn close_at(f: &Fixture, key: &str, rv: &str) {
    let prepared = f
        .store
        .prepare_namespace_fence(
            &f.scope,
            &f.claim,
            key,
            Admission::Closed,
            ResourceVersion {
                uid: "namespace-uid".into(),
                resource_version: rv.into(),
            },
        )
        .await
        .unwrap();
    f.store
        .complete_namespace_fence(
            &f.scope,
            &f.claim,
            key,
            NamespaceFenceObservation {
                uid: "namespace-uid".into(),
                resource_version: (rv.parse::<u64>().unwrap() + 1).to_string(),
                input_digest: prepared.input.digest().unwrap(),
                state: prepared.input.state,
            },
        )
        .await
        .unwrap();
}
fn inspected(rv: &str) -> NativeStopInspection {
    NativeStopInspection {
        uid: "deployment-uid".into(),
        resource_version: rv.into(),
        preserved_spec_digest: "c".repeat(64),
    }
}
fn observed(prepared: &PreparedNativeStop, rv: &str) -> NativeStopObservation {
    NativeStopObservation {
        uid: prepared.input.deployment_uid.clone(),
        resource_version: rv.into(),
        input_digest: prepared.input.digest().unwrap(),
        preserved_spec_digest: prepared.input.preserved_spec_digest.clone(),
    }
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn stop_requires_current_committed_close_and_preserves_immutable_intent(pool: PgPool) {
    let f = fixture(&pool, true).await;
    assert!(matches!(
        f.store
            .prepare_native_stop(&f.scope, &f.claim, "stop", "close", inspected("10"))
            .await,
        Err(Error::Forbidden)
    ));
    close(&f, "close").await;
    let prepared = f
        .store
        .prepare_native_stop(&f.scope, &f.claim, "stop", "close", inspected("10"))
        .await
        .unwrap();
    assert_eq!(prepared.input.drain_generation, 2);
    assert_eq!(prepared.input.fence_state.access, Access::Suspended);
    assert!(matches!(prepared.step.result, StepResult::Unknown));
    let retry = f
        .store
        .prepare_native_stop(&f.scope, &f.claim, "stop", "close", inspected("11"))
        .await
        .unwrap();
    assert_eq!(retry.input, prepared.input);
    let mut changed = inspected("11");
    changed.preserved_spec_digest = "d".repeat(64);
    assert!(matches!(
        f.store
            .prepare_native_stop(&f.scope, &f.claim, "stop", "close", changed)
            .await,
        Err(Error::OwnershipConflict)
    ));
    assert_eq!(
        f.store
            .record_result(
                &f.scope,
                &f.claim,
                "stop",
                StepResult::Verified {
                    uid: "deployment-uid".into(),
                    resource_version: "11".into()
                }
            )
            .await,
        Err(Error::OwnershipConflict)
    );
    let mut wrong = observed(&prepared, "11");
    wrong.preserved_spec_digest = "d".repeat(64);
    assert_eq!(
        f.store
            .complete_native_stop(&f.scope, &f.claim, "stop", wrong)
            .await,
        Err(Error::OwnershipConflict)
    );
    for statement in [
        "UPDATE manager.native_stop_inputs SET input='{}'",
        "DELETE FROM manager.native_stop_inputs",
    ] {
        assert!(sqlx::query(statement).execute(&pool).await.is_err());
    }
    f.store
        .complete_native_stop(&f.scope, &f.claim, "stop", observed(&prepared, "11"))
        .await
        .unwrap();
    let row=sqlx::query("SELECT s.result,s.verified_at IS NOT NULL AS verified,i.drain_completed_at IS NULL AS draining,r.state AS resource_state FROM manager.operation_steps s JOIN manager.instances i ON i.id=$1 JOIN manager.owned_resources r ON r.id=$2 WHERE s.operation_id=$3 AND s.step_key='stop'")
        .bind(f.claim.instance_id).bind(f.deployment.creation.id).bind(f.claim.id).fetch_one(&pool).await.unwrap();
    assert!(row.get::<bool, _>("verified"));
    assert!(row.get::<bool, _>("draining"));
    assert_eq!(row.get::<String, _>("resource_state"), "Bound");
    assert_eq!(row.get::<Value, _>("result")["status"], "verified");
    let resources: i64 = sqlx::query_scalar("SELECT count(*) FROM manager.owned_resources")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(resources, 2);
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn enabled_stop_needs_original_active_drain_and_new_closed_generation(pool: PgPool) {
    let mut f = fixture(&pool, false).await;
    close(&f, "initial-close").await;
    assert!(matches!(
        f.store
            .prepare_native_stop(
                &f.scope,
                &f.claim,
                "no-drain",
                "initial-close",
                inspected("1")
            )
            .await,
        Err(Error::Forbidden)
    ));
    let assignment = f
        .store
        .lifecycle_plan(&f.scope, &f.claim)
        .await
        .unwrap()
        .assignment
        .id;
    f.assignments
        .execute(
            &f.actor,
            Uuid::new_v4(),
            "disable",
            AssignmentCommand::SetAccess {
                assignment_id: assignment,
                expected_version: 1,
                access: Access::Revoked,
            },
        )
        .await
        .unwrap();
    let old = f
        .store
        .claim(&f.scope, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    f.assignments
        .execute(
            &f.actor,
            Uuid::new_v4(),
            "reenable",
            AssignmentCommand::SetAccess {
                assignment_id: assignment,
                expected_version: 2,
                access: Access::Enabled,
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
    assert!(matches!(
        f.store
            .prepare_native_stop(&f.scope, &old, "stale", "initial-close", inspected("1"))
            .await,
        Err(Error::StaleFence)
    ));
    assert!(matches!(
        f.store
            .prepare_native_stop(
                &f.scope,
                &f.claim,
                "not-current-close",
                "initial-close",
                inspected("1")
            )
            .await,
        Err(Error::Forbidden | Error::OwnershipConflict)
    ));
    close(&f, "current-close").await;
    let prepared = f
        .store
        .prepare_native_stop(&f.scope, &f.claim, "stop", "current-close", inspected("1"))
        .await
        .unwrap();
    assert_eq!(prepared.input.fence_state.access, Access::Enabled);
    assert_eq!(prepared.input.fence_state.generation, 3);
    assert_eq!(prepared.input.drain_generation, 2);
    let opened = f
        .store
        .prepare_namespace_fence(
            &f.scope,
            &f.claim,
            "opened",
            Admission::Open,
            ResourceVersion {
                uid: "namespace-uid".into(),
                resource_version: "2".into(),
            },
        )
        .await
        .unwrap();
    f.store
        .complete_namespace_fence(
            &f.scope,
            &f.claim,
            "opened",
            NamespaceFenceObservation {
                uid: "namespace-uid".into(),
                resource_version: "3".into(),
                input_digest: opened.input.digest().unwrap(),
                state: opened.input.state,
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        f.store.load_pending_native_stops(&f.scope, &f.claim).await,
        Err(Error::OwnershipConflict)
    ));
    close_at(&f, "closed-again", "3").await;
    let pending = f
        .store
        .load_pending_native_stops(&f.scope, &f.claim)
        .await
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].input, prepared.input);
    sqlx::query("UPDATE manager.installations SET mode='maintenance'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        f.store
            .complete_native_stop(&f.scope, &f.claim, "stop", observed(&prepared, "2"))
            .await,
        Err(Error::Maintenance)
    );
}

struct ApiState {
    namespace: Value,
    deployment: Value,
    patches: Vec<Value>,
    lose_response: bool,
    mutate_after_patch: bool,
}
type Shared = Arc<Mutex<ApiState>>;
fn error(code: StatusCode) -> (StatusCode, Json<Value>) {
    (
        code,
        Json(
            json!({"apiVersion":"v1","kind":"Status","status":"Failure","code":code.as_u16(),"reason":"Fixture"}),
        ),
    )
}
async fn namespace_get(
    State(state): State<Shared>,
    Path(name): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if name == "kube-system" {
        return Ok(Json(
            json!({"apiVersion":"v1","kind":"Namespace","metadata":{"name":"kube-system","uid":"stop-test"}}),
        ));
    }
    let state = state.lock().unwrap();
    if state.namespace["metadata"]["name"] == name {
        Ok(Json(state.namespace.clone()))
    } else {
        Err(error(StatusCode::NOT_FOUND))
    }
}
async fn deployment_get(
    State(state): State<Shared>,
    Path((namespace, name)): Path<(String, String)>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let state = state.lock().unwrap();
    if state.deployment["metadata"]["namespace"] == namespace && name == "hermes" {
        Ok(Json(state.deployment.clone()))
    } else {
        Err(error(StatusCode::NOT_FOUND))
    }
}
async fn scale_patch(
    State(state): State<Shared>,
    Path((namespace, name)): Path<(String, String)>,
    Json(patch): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mut state = state.lock().unwrap();
    if state.deployment["metadata"]["namespace"] != namespace
        || name != "hermes"
        || state.deployment["metadata"]["uid"] != patch["metadata"]["uid"]
        || state.deployment["metadata"]["resourceVersion"] != patch["metadata"]["resourceVersion"]
    {
        return Err(error(StatusCode::CONFLICT));
    }
    assert_eq!(patch.as_object().unwrap().len(), 2);
    assert_eq!(patch["metadata"].as_object().unwrap().len(), 2);
    assert_eq!(patch["spec"], json!({"replicas":0}));
    state.patches.push(patch);
    state.deployment["spec"]["replicas"] = json!(0);
    let rv = state.deployment["metadata"]["resourceVersion"]
        .as_str()
        .unwrap()
        .parse::<u64>()
        .unwrap()
        + 1;
    state.deployment["metadata"]["resourceVersion"] = json!(rv.to_string());
    if state.mutate_after_patch {
        state.deployment["spec"]["template"]["metadata"]["labels"]["changed"] = json!("yes");
    }
    if std::mem::take(&mut state.lose_response) {
        return Err(error(StatusCode::GATEWAY_TIMEOUT));
    }
    Ok(Json(
        json!({"apiVersion":"autoscaling/v1","kind":"Scale","metadata":state.deployment["metadata"],"spec":{"replicas":0}}),
    ))
}
fn object(f: &Fixture, owned: &OwnedResource, signer: &OwnershipSigner) -> Value {
    let mut value = json!({"apiVersion":owned.creation.spec.kind.api_version(),"kind":owned.creation.spec.kind.as_str(),"metadata":{"name":owned.creation.spec.name,"uid":owned.uid,"resourceVersion":"1","labels":{"agent-platform.io/installation":f.scope.id,"agent-platform.io/instance":f.claim.instance_id,"agent-platform.io/resource":owned.creation.id},"annotations":{"agent-platform.io/creation-proof":signer.sign(&owned.creation).unwrap(),"agent-platform.io/applied-intent":owned.creation.spec.manifest_digest}}});
    if let Some(ns) = &owned.creation.spec.namespace {
        value["metadata"]["namespace"] = json!(ns);
    }
    if owned.creation.spec.kind == ResourceKind::Namespace {
        let labels = value["metadata"]["labels"].as_object_mut().unwrap();
        for (key, value) in [
            (ACCESS, "suspended".into()),
            (ADMISSION, "closed".into()),
            (GENERATION, f.claim.generation.to_string()),
            (EPOCH, f.scope.epoch.to_string()),
        ] {
            labels.insert(key.into(), json!(value));
        }
    } else {
        value["spec"] = json!({"replicas":1,"selector":{"matchLabels":{"app":"hermes"}},"strategy":{"type":"RollingUpdate"},"template":{"metadata":{"labels":{"app":"hermes"}},"spec":{"containers":[{"name":"hermes","image":format!("ghcr.io/example/native@sha256:{}","a".repeat(64))}]}}});
    }
    value
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn tls_scale_lost_response_reopens_without_replay_or_template_replacement(pool: PgPool) {
    let mut f = fixture(&pool, true).await;
    close(&f, "close").await;
    let secrets = support::Secrets::new().await;
    let signer = OwnershipSigner::new(
        "stop-key".into(),
        BTreeMap::from([("stop-key".into(), vec![42; 32])]),
    )
    .unwrap();
    let original = object(&f, &f.deployment, &signer);
    let state = Arc::new(Mutex::new(ApiState {
        namespace: object(&f, &f.namespace, &signer),
        deployment: original.clone(),
        patches: vec![],
        lose_response: true,
        mutate_after_patch: false,
    }));
    let running = support::start(
        &secrets,
        Router::new()
            .route("/api/v1/namespaces/{name}", get(namespace_get))
            .route(
                "/apis/apps/v1/namespaces/{namespace}/deployments/{name}",
                get(deployment_get),
            )
            .route(
                "/apis/apps/v1/namespaces/{namespace}/deployments/{name}/scale",
                axum::routing::patch(scale_patch),
            )
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
    let driver = KubernetesDriver::from_config(config, signer, "stop-test".into()).unwrap();
    let inspected = driver.inspect_native_stop(&f.deployment).await.unwrap();
    let prepared = f
        .store
        .prepare_native_stop(&f.scope, &f.claim, "stop", "close", inspected)
        .await
        .unwrap();
    let original_claim = f.claim.clone();
    assert!(matches!(
        driver.stop_native(&prepared, &f.claim).await,
        Err(Error::Unavailable)
    ));
    assert_eq!(state.lock().unwrap().patches.len(), 1);
    sqlx::query("UPDATE manager.operations SET lease_until=clock_timestamp()-interval '1 second' WHERE id=$1").bind(f.claim.id).execute(&pool).await.unwrap();
    let reopened = PgStore::connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    let claim = reopened
        .claim(&f.scope, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    f.claim = claim.clone();
    close_at(&f, "later-close", "2").await;
    let mut pending = reopened
        .load_pending_native_stops(&f.scope, &claim)
        .await
        .unwrap();
    assert_eq!(pending.len(), 1);
    let retry = pending.remove(0);
    assert_eq!(retry.key, "stop");
    assert_eq!(prepared.input, retry.input);
    let result = driver.stop_native(&retry, &claim).await.unwrap();
    reopened
        .complete_native_stop(&f.scope, &claim, &retry.key, result)
        .await
        .unwrap();
    assert_eq!(state.lock().unwrap().patches.len(), 1);
    assert!(
        reopened
            .load_pending_native_stops(&f.scope, &claim)
            .await
            .unwrap()
            .is_empty()
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM manager.native_stop_inputs")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
    let mut expected = original.clone();
    expected["metadata"]["resourceVersion"] = json!("2");
    expected["spec"]["replicas"] = json!(0);
    assert_eq!(state.lock().unwrap().deployment, expected);
    assert_eq!(
        reopened
            .complete_native_stop(&f.scope, &original_claim, "stop", observed(&prepared, "2"))
            .await,
        Err(Error::StaleFence)
    );
    // Refuse a replaced UID, even if it retained the old labels and HMAC.
    state.lock().unwrap().deployment["metadata"]["uid"] = json!("replacement");
    assert!(matches!(
        driver.stop_native(&retry, &claim).await,
        Err(Error::OwnershipConflict)
    ));
    state.lock().unwrap().deployment = original.clone();
    state.lock().unwrap().deployment["metadata"]["resourceVersion"] = json!("99");
    assert!(matches!(
        driver.stop_native(&retry, &claim).await,
        Err(Error::PreconditionFailed)
    ));
    state.lock().unwrap().deployment = original.clone();
    state.lock().unwrap().deployment["spec"]["template"]["metadata"]["labels"]["changed"] =
        json!("yes");
    assert!(matches!(
        driver.stop_native(&retry, &claim).await,
        Err(Error::OwnershipConflict)
    ));
    // A rollout/template mutation racing AFTER Scale is also unacknowledged.
    state.lock().unwrap().deployment = original;
    state.lock().unwrap().mutate_after_patch = true;
    assert!(matches!(
        driver.stop_native(&retry, &claim).await,
        Err(Error::OwnershipConflict)
    ));
    drop(running);
}
