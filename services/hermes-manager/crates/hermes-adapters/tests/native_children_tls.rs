//! Real TLS transport for durable native child inputs; SQL ownership/registration
//! boundaries are exercised independently in native_children.rs.
use axum::{
    Json, Router,
    body::to_bytes,
    extract::{Request, State},
    http::{Method, StatusCode},
};
use hermes_adapters::{
    kubernetes::{KubernetesDriver, Manifest},
    ownership::OwnershipSigner,
};
use hermes_application::{
    Error, OperationClaim,
    native_children::{
        NativeChildAction, NativeChildInput, NativeChildObservation, PreparedNativeChild,
        START_GATE,
    },
    resources::{Creation, OwnedResource},
};
use hermes_domain::resources::{ResourceKind, ResourceSpec, Retention};
use kube::Config;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use uuid::Uuid;

#[allow(dead_code)]
mod support;

fn signer() -> OwnershipSigner {
    OwnershipSigner::new(
        "fixture".into(),
        BTreeMap::from([("fixture".into(), vec![42; 32])]),
    )
    .unwrap()
}
struct ApiState {
    objects: BTreeMap<String, Value>,
    pod: Option<Value>,
    pod_path: String,
    pod_list: String,
    rs_path: String,
    rs_gets: usize,
    patches: Vec<Value>,
    deletes: Vec<Value>,
    lose_patch_reply: bool,
}
type Shared = Arc<Mutex<ApiState>>;
fn failure(status: StatusCode) -> (StatusCode, Json<Value>) {
    let reason = match status {
        StatusCode::NOT_FOUND => "NotFound",
        StatusCode::CONFLICT => "Conflict",
        StatusCode::GATEWAY_TIMEOUT => "Timeout",
        _ => "MethodNotAllowed",
    };
    (
        status,
        Json(
            json!({"kind":"Status","apiVersion":"v1","status":"Failure","reason":reason,"code":status.as_u16()}),
        ),
    )
}
async fn api(State(state): State<Shared>, request: Request) -> (StatusCode, Json<Value>) {
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let bytes = to_bytes(request.into_body(), 65536).await.unwrap();
    let mut s = state.lock().unwrap();
    if method == Method::GET {
        if path == s.rs_path {
            s.rs_gets += 1;
        }
        if path == s.pod_list {
            return (
                StatusCode::OK,
                Json(
                    json!({"apiVersion":"v1","kind":"PodList","metadata":{},"items":s.pod.iter().collect::<Vec<_>>()}),
                ),
            );
        }
        if path == s.pod_path {
            return s
                .pod
                .clone()
                .map(|p| (StatusCode::OK, Json(p)))
                .unwrap_or_else(|| failure(StatusCode::NOT_FOUND));
        }
        return s
            .objects
            .get(&path)
            .cloned()
            .map(|p| (StatusCode::OK, Json(p)))
            .unwrap_or_else(|| failure(StatusCode::NOT_FOUND));
    }
    if path != s.pod_path {
        return failure(StatusCode::METHOD_NOT_ALLOWED);
    }
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    let Some(pod) = s.pod.as_ref() else {
        return failure(StatusCode::NOT_FOUND);
    };
    if method == Method::PATCH {
        if body["metadata"]["uid"] != pod["metadata"]["uid"]
            || body["metadata"]["resourceVersion"] != pod["metadata"]["resourceVersion"]
        {
            return failure(StatusCode::CONFLICT);
        }
        assert_eq!(body["spec"], json!({"schedulingGates":[]}));
        s.patches.push(body);
        let pod = s.pod.as_mut().unwrap();
        pod["spec"]["schedulingGates"] = json!([]);
        let version = pod["metadata"]["resourceVersion"]
            .as_str()
            .unwrap()
            .parse::<u64>()
            .unwrap()
            + 1;
        pod["metadata"]["resourceVersion"] = json!(version.to_string());
        if std::mem::take(&mut s.lose_patch_reply) {
            return failure(StatusCode::GATEWAY_TIMEOUT);
        }
        return (StatusCode::OK, Json(s.pod.clone().unwrap()));
    }
    if method == Method::DELETE {
        if body["preconditions"]["uid"] != pod["metadata"]["uid"]
            || body["preconditions"]["resourceVersion"] != pod["metadata"]["resourceVersion"]
        {
            return failure(StatusCode::CONFLICT);
        }
        assert_eq!(body["gracePeriodSeconds"], 45);
        s.deletes.push(body);
        s.pod = None;
        return (
            StatusCode::OK,
            Json(json!({"apiVersion":"v1","kind":"Status","status":"Success","code":200})),
        );
    }
    failure(StatusCode::METHOD_NOT_ALLOWED)
}
fn prepared(
    owned: &OwnedResource,
    claim: &OperationClaim,
    observation: NativeChildObservation,
    action: NativeChildAction,
) -> PreparedNativeChild {
    let input = NativeChildInput {
        installation_id: owned.creation.installation_id,
        instance_id: claim.instance_id,
        operation_id: claim.id,
        deployment_id: owned.creation.id,
        fence_key: if action == NativeChildAction::Start {
            "open"
        } else {
            "closed"
        }
        .into(),
        namespace: owned.creation.spec.namespace.clone().unwrap(),
        namespace_uid: observation.namespace_uid,
        deployment_uid: observation.deployment_uid,
        replica_set_name: observation.replica_set_name,
        replica_set_uid: observation.replica_set_uid,
        pod_name: observation.pod_name,
        pod_uid: observation.pod_uid,
        resource_version: observation.resource_version,
        spec_digest: observation.spec_digest,
        generation: claim.generation,
        epoch: claim.epoch,
        action,
    };
    let prepared = PreparedNativeChild {
        deployment: owned.clone(),
        input,
        completed: false,
    };
    prepared.validate(claim).unwrap();
    prepared
}

#[tokio::test]
async fn tls_gate_lost_reply_known_uid_stop_and_name_replacement_are_safe() {
    let installation = Uuid::new_v4();
    let instance = Uuid::new_v4();
    let namespace = hermes_domain::instance_namespace(installation, instance);
    let mut claim = OperationClaim {
        id: Uuid::new_v4(),
        instance_id: instance,
        generation: 1,
        epoch: 1,
        worker: Uuid::new_v4(),
        fence: 1,
        kind: "commission".into(),
    };
    let selector =
        json!({"app.kubernetes.io/component":"hermes","agent-platform.io/instance":instance});
    let mut labels = selector.clone();
    labels["agent-platform.io/policy-generation"] = json!("1");
    let template = json!({"metadata":{"labels":labels},"spec":{"containers":[{"name":"hermes","image":format!("ghcr.io/example/native@sha256:{}","a".repeat(64))}],"schedulingGates":[{"name":START_GATE}],"restartPolicy":"Always"}});
    let value = json!({"apiVersion":"apps/v1","kind":"Deployment","metadata":{"name":"hermes","namespace":namespace},"spec":{"replicas":1,"selector":{"matchLabels":selector},"template":template}});
    let manifest = Manifest::new(value.clone()).unwrap();
    let creation = Creation {
        id: Uuid::new_v4(),
        installation_id: installation,
        instance_id: instance,
        cluster_identity: "native-child-fixture".into(),
        epoch: 1,
        generation: 1,
        operation_id: claim.id,
        spec: ResourceSpec {
            kind: ResourceKind::Deployment,
            namespace: Some(namespace.clone()),
            namespace_uid: Some("namespace-uid".into()),
            name: "hermes".into(),
            manifest_digest: manifest.digest().unwrap(),
            purpose: "hermes-workload".into(),
            retention: Retention::Delete,
            encrypted_material_ref: None,
        },
    };
    let owned = OwnedResource {
        creation,
        uid: Some("deployment-uid".into()),
        resource_version: Some("10".into()),
        observed_digest: Some(manifest.digest().unwrap()),
        state: "Bound".into(),
    };
    let mut deployment = value;
    deployment["metadata"]["uid"] = json!("deployment-uid");
    deployment["metadata"]["resourceVersion"] = json!("10");
    deployment["metadata"]["labels"] = json!({"agent-platform.io/installation":installation,"agent-platform.io/instance":instance,"agent-platform.io/resource":owned.creation.id});
    deployment["metadata"]["annotations"] = json!({"agent-platform.io/creation-proof":signer().sign(&owned.creation).unwrap(),"agent-platform.io/applied-intent":manifest.digest().unwrap()});
    let namespace_path = format!("/api/v1/namespaces/{namespace}");
    let pod_list = format!("/api/v1/namespaces/{namespace}/pods");
    let pod_path = format!("{pod_list}/hermes-pod");
    let rs_path = format!("/apis/apps/v1/namespaces/{namespace}/replicasets/hermes-replica");
    let parent = json!({"apiVersion":"apps/v1","kind":"ReplicaSet","name":"hermes-replica","uid":"rs-uid","controller":true});
    let pod = json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"hermes-pod","namespace":namespace,"uid":"pod-uid","resourceVersion":"20","labels":labels,"ownerReferences":[parent]},"spec":template["spec"]});
    let set = json!({"apiVersion":"apps/v1","kind":"ReplicaSet","metadata":{"name":"hermes-replica","namespace":namespace,"uid":"rs-uid","ownerReferences":[{"apiVersion":"apps/v1","kind":"Deployment","name":"hermes","uid":"deployment-uid","controller":true}]},"spec":{"selector":{"matchLabels":selector},"template":template}});
    let ns = json!({"apiVersion":"v1","kind":"Namespace","metadata":{"name":namespace,"uid":"namespace-uid","labels":{"agent-platform.io/installation":installation,"agent-platform.io/instance":instance,"agent-platform.io/admission":"open","agent-platform.io/access":"enabled","agent-platform.io/fence-generation":"1","agent-platform.io/fence-epoch":"1"}}});
    let state = Arc::new(Mutex::new(ApiState {
        objects: BTreeMap::from([
            (
                "/api/v1/namespaces/kube-system".into(),
                json!({"apiVersion":"v1","kind":"Namespace","metadata":{"name":"kube-system","uid":"native-child-fixture"}}),
            ),
            (namespace_path.clone(), ns),
            (
                format!("/apis/apps/v1/namespaces/{namespace}/deployments/hermes"),
                deployment,
            ),
            (rs_path.clone(), set),
        ]),
        pod: Some(pod),
        pod_path,
        pod_list,
        rs_path: rs_path.clone(),
        rs_gets: 0,
        patches: vec![],
        deletes: vec![],
        lose_patch_reply: true,
    }));
    let secrets = support::Secrets::new().await;
    let _tls = support::start(
        &secrets,
        Router::new().fallback(api).with_state(state.clone()),
        false,
    )
    .await;
    let mut config = Config::new(_tls.url.parse().unwrap());
    config.root_cert = Some(
        rustls_pemfile::certs(&mut secrets.read("ca.crt").await.as_slice())
            .map(|c| c.unwrap().as_ref().to_vec())
            .collect(),
    );
    let driver =
        KubernetesDriver::from_config(config, signer(), "native-child-fixture".into()).unwrap();
    let observation = driver
        .discover_native_children(&owned, Some(&manifest), &claim)
        .await
        .unwrap()
        .remove(0);
    let stale = prepared(&owned, &claim, observation, NativeChildAction::Start);
    state.lock().unwrap().pod.as_mut().unwrap()["metadata"]["resourceVersion"] = json!("21");
    assert!(matches!(
        driver.apply_native_child(&stale, &claim).await,
        Err(Error::PreconditionFailed)
    ));
    assert!(state.lock().unwrap().patches.is_empty());
    let observation = driver
        .discover_native_children(&owned, Some(&manifest), &claim)
        .await
        .unwrap()
        .remove(0);
    let start = prepared(
        &owned,
        &claim,
        observation.clone(),
        NativeChildAction::Start,
    );
    assert_ne!(start.input.digest().unwrap(), stale.input.digest().unwrap());
    assert!(matches!(
        driver.apply_native_child(&start, &claim).await,
        Err(Error::Unavailable)
    ));
    assert_eq!(state.lock().unwrap().patches.len(), 1);
    let recovered = driver
        .apply_native_child(&start, &claim)
        .await
        .unwrap()
        .unwrap();
    assert!(!recovered.absent);
    assert_eq!(recovered.input_digest, start.input.digest().unwrap());
    assert_eq!(
        state.lock().unwrap().patches.len(),
        1,
        "lost reply caused repeated gate removal"
    );
    assert_eq!(
        state.lock().unwrap().patches[0]["metadata"]["resourceVersion"],
        "21"
    );
    // Independent HTTP snapshots for a pending start whose exact Pod UID has
    // gone. Neither case completes SQL here or authorizes a replacement start.
    let present = state.lock().unwrap().pod.clone();
    let mut replacement = present.clone().unwrap();
    replacement["metadata"]["uid"] = json!("replacement-before-start");
    for alternative in [None, Some(replacement)] {
        state.lock().unwrap().pod = alternative;
        let gone = driver
            .apply_native_child(&start, &claim)
            .await
            .unwrap()
            .unwrap();
        assert!(gone.absent);
        assert_eq!(gone.pod_uid, start.input.pod_uid);
        assert_eq!(gone.input_digest, start.input.digest().unwrap());
        assert_eq!(state.lock().unwrap().patches.len(), 1);
    }
    state.lock().unwrap().pod = present;
    // New discovery/start always requires the live complete UID lineage.
    state.lock().unwrap().objects.get_mut(&rs_path).unwrap()["metadata"]["ownerReferences"][0]["uid"] =
        json!("foreign-deployment");
    assert!(matches!(
        driver
            .discover_native_children(&owned, Some(&manifest), &claim)
            .await,
        Err(Error::OwnershipConflict)
    ));
    // A registered exact Pod UID remains identifiable after ReplicaSet GC.
    state.lock().unwrap().objects.remove(&rs_path);
    let rs_gets = state.lock().unwrap().rs_gets;
    let present = state.lock().unwrap().pod.clone();
    state.lock().unwrap().pod.as_mut().unwrap()["metadata"]["deletionTimestamp"] =
        json!("2026-01-01T00:00:00Z");
    assert!(
        driver
            .apply_native_child(&start, &claim)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        driver
            .discover_native_children(&owned, Some(&manifest), &claim)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        state.lock().unwrap().rs_gets,
        rs_gets,
        "terminating start depended on a collected ReplicaSet"
    );
    assert_eq!(state.lock().unwrap().patches.len(), 1);
    assert!(state.lock().unwrap().deletes.is_empty());
    state.lock().unwrap().pod = None;
    let absent = driver
        .apply_native_child(&start, &claim)
        .await
        .unwrap()
        .unwrap();
    assert!(absent.absent);
    assert_eq!(absent.pod_uid, start.input.pod_uid);
    assert_eq!(state.lock().unwrap().rs_gets, rs_gets);
    // Restore the independent known-stop fixture snapshot; no SQL completion
    // from the preceding pending-start transport scenarios was persisted here.
    state.lock().unwrap().pod = present;
    let known = driver
        .observe_native_child(&owned, &observation)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(known.resource_version, "22");
    assert_eq!(state.lock().unwrap().rs_gets, rs_gets);
    claim.id = Uuid::new_v4();
    claim.generation = 2;
    claim.kind = "suspend".into();
    {
        let mut s = state.lock().unwrap();
        let labels = &mut s.objects.get_mut(&namespace_path).unwrap()["metadata"]["labels"];
        labels["agent-platform.io/admission"] = json!("closed");
        labels["agent-platform.io/access"] = json!("suspended");
        labels["agent-platform.io/fence-generation"] = json!("2");
    }
    let stop = prepared(&owned, &claim, known, NativeChildAction::Stop);
    let stopped = driver
        .apply_native_child(&stop, &claim)
        .await
        .unwrap()
        .unwrap();
    assert!(stopped.absent);
    assert_eq!(state.lock().unwrap().rs_gets, rs_gets);
    assert_eq!(state.lock().unwrap().deletes.len(), 1);
    assert_eq!(
        state.lock().unwrap().deletes[0]["preconditions"],
        json!({"uid":"pod-uid","resourceVersion":"22"})
    );
    let mut replacement = json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"hermes-pod","namespace":namespace,"uid":"replacement-uid","resourceVersion":"23","labels":labels,"ownerReferences":[parent]},"spec":template["spec"]});
    replacement["spec"]["schedulingGates"] = json!([]);
    state.lock().unwrap().pod = Some(replacement);
    assert!(
        driver
            .observe_native_child(&owned, &observation)
            .await
            .unwrap()
            .is_none()
    );
    let stopped = driver
        .apply_native_child(&stop, &claim)
        .await
        .unwrap()
        .unwrap();
    assert!(stopped.absent);
    assert_eq!(
        state.lock().unwrap().deletes.len(),
        1,
        "exact-name replacement was deleted"
    );
    assert_eq!(
        state.lock().unwrap().pod.as_ref().unwrap()["metadata"]["uid"],
        "replacement-uid"
    );
    assert!(
        matches!(
            driver.discover_native_children(&owned, None, &claim).await,
            Err(Error::OwnershipConflict)
        ),
        "unknown child skipped live ReplicaSet lineage"
    );
    // An enabled retry can update the Deployment before an old writer has a
    // deletionTimestamp. It is neither a new start nor an ownership failure.
    let mut next_template = template.clone();
    next_template["metadata"]["labels"]["agent-platform.io/policy-generation"] = json!("2");
    let next_value = json!({"apiVersion":"apps/v1","kind":"Deployment","metadata":{"name":"hermes","namespace":namespace},"spec":{"replicas":1,"selector":{"matchLabels":selector},"template":next_template}});
    let next_manifest = Manifest::new(next_value.clone()).unwrap();
    let mut next_owned = owned.clone();
    next_owned.observed_digest = Some(next_manifest.digest().unwrap());
    claim.id = Uuid::new_v4();
    claim.kind = "commission".into();
    {
        let mut state = state.lock().unwrap();
        let deployment = state
            .objects
            .get_mut(&format!(
                "/apis/apps/v1/namespaces/{namespace}/deployments/hermes"
            ))
            .unwrap();
        deployment["spec"] = next_value["spec"].clone();
        deployment["metadata"]["annotations"]["agent-platform.io/applied-intent"] =
            json!(next_manifest.digest().unwrap());
        let labels = &mut state.objects.get_mut(&namespace_path).unwrap()["metadata"]["labels"];
        labels["agent-platform.io/admission"] = json!("open");
        labels["agent-platform.io/access"] = json!("enabled");
    }
    let rs_gets = state.lock().unwrap().rs_gets;
    assert!(
        driver
            .discover_native_children(&next_owned, Some(&next_manifest), &claim)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(state.lock().unwrap().rs_gets, rs_gets);
    for generation in [
        Some("3"),
        Some("0"),
        Some("01"),
        Some("-1"),
        Some("nonnumeric"),
        None,
    ] {
        {
            let mut state = state.lock().unwrap();
            let labels = state.pod.as_mut().unwrap()["metadata"]["labels"]
                .as_object_mut()
                .unwrap();
            match generation {
                Some(value) => {
                    labels.insert("agent-platform.io/policy-generation".into(), json!(value));
                }
                None => {
                    labels.remove("agent-platform.io/policy-generation");
                }
            }
        }
        assert!(matches!(
            driver
                .discover_native_children(&next_owned, Some(&next_manifest), &claim)
                .await,
            Err(Error::OwnershipConflict)
        ));
    }
    assert_eq!(state.lock().unwrap().rs_gets, rs_gets);
    assert_eq!(state.lock().unwrap().patches.len(), 1);
    assert_eq!(state.lock().unwrap().deletes.len(), 1);
}
