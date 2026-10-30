//! The static admission policies and the controller's RBAC against a
//! disposable API server, acting as the controller's real ServiceAccount.
//!
//! Every restriction has an independently valid positive control: a base
//! object that is admitted. Each negative case then differs from that base by
//! one field and must be refused for the intended reason (the policy's own
//! message, or Pod Security Admission where the policy delegates to it), so an
//! unrelated invalid fixture or a policy that refuses everything cannot pass.
//!
//! Needs `HERMES_TEST_KUBECONFIG`; run through
//! `tests/personal_hermes/kubernetes.py`, never against a real cluster.

#[allow(dead_code)]
mod support;

use std::fmt::Debug;

use hermes_core::app::controller::{ApplyRequest, Cluster};
use hermes_core::domain::instances::{Apply, Workload};
use k8s_openapi::api::core::v1::{Namespace, PersistentVolume, Pod, Secret, Service};
use k8s_openapi::api::rbac::v1::RoleBinding;
use kube::api::{Api, DeleteParams, ListParams, Patch, PatchParams, PostParams};
use serde_json::{Value, json};
use support::{Fixture, desired, materials};
use uuid::Uuid;

// The messages the policies carry (`hermes_render::admission`).
const NAMESPACE_WRITER: &str = "only the manager controller writes instance namespaces";
const NAMESPACE_LABELS: &str = "instance namespaces carry the installation's prefix and labels";
const WRITERS: &str = "only the manager controller writes in instance namespaces";
const SCOPE: &str = "the manager controller writes only in instance namespaces";
const NON_ROOT: &str = "Pods run as the installation's non-root data identity";
const CONTAINER_IDENTITY: &str = "containers keep the Pod's data identity";
const UNPRIVILEGED: &str = "containers are unprivileged and drop every capability";
const PINNED: &str = "images are pinned by digest";
const HOST: &str = "Pods use no host namespaces";
const TOKEN: &str = "Pods carry no Kubernetes token";
const VOLUMES: &str = "volumes are claims, Secrets, ConfigMaps or emptyDirs only";
const EPHEMERAL: &str = "no ephemeral containers";
const RETAINED: &str = "static volumes are retained and bind into instance namespaces";
const ROOTS: &str = "static volumes stay under approved storage roots";
const BINDING: &str = "instance namespaces bind only the instance role to the controller";
const SERVICES: &str = "Services stay inside the cluster";
/// A policy expression that errors (an absent label key) denies under
/// `failurePolicy: Fail`; the message then names the policy's expression.
const POLICY_ERROR: &str = "-namespaces' with binding";
/// Pod Security Admission, enforced by the `restricted` namespace label.
const POD_SECURITY: &str = "violates PodSecurity";
/// RBAC refuses before admission is reached.
const FORBIDDEN: &str = "forbidden";

fn suffix() -> String {
    Uuid::new_v4().simple().to_string()[..8].to_owned()
}

thread_local! {
    static MISMATCHES: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn note(message: String) {
    MISMATCHES.with(|found| found.borrow_mut().push(message));
}

/// The request was refused, and the refusal names one of `markers`. Outcomes
/// are collected so one run reports every mismatch; [`settle`] fails on them.
fn denied<T: Debug>(result: Result<T, kube::Error>, case: &str, markers: &[&str]) {
    match result {
        Err(kube::Error::Api(status)) => {
            if !markers.iter().any(|marker| status.message.contains(marker)) {
                note(format!(
                    "{case}: refused for another reason ({} {}): {}",
                    status.code, status.reason, status.message
                ));
            }
        }
        other => note(format!("{case} was not refused: {other:?}")),
    }
}

/// Fails with every unexpected outcome recorded so far.
fn settle() {
    let found = MISMATCHES.with(|found| found.take());
    assert!(
        found.is_empty(),
        "{} unexpected outcome(s):\n{}",
        found.len(),
        found.join("\n")
    );
}

/// Commissions one instance and returns its namespace.
async fn commissioned(fixture: &Fixture) -> String {
    let desired = desired(fixture);
    let materials = materials();
    fixture
        .cluster()
        .apply(&ApplyRequest {
            desired: &desired,
            apply: Apply::Everything(Workload::Run),
            materials: Some(&materials),
            secrets: &[],
        })
        .await
        .unwrap();
    desired.namespace
}

/// A Pod spec every rule admits.
fn safe() -> Value {
    json!({
        "automountServiceAccountToken": false,
        "securityContext": {
            "runAsNonRoot": true, "runAsUser": 1000, "runAsGroup": 1000,
            "seccompProfile": {"type": "RuntimeDefault"}
        },
        "containers": [{
            "name": "c",
            "image": format!("example.org/x@sha256:{}", "b".repeat(64)),
            "securityContext": {
                "allowPrivilegeEscalation": false,
                "capabilities": {"drop": ["ALL"]}
            }
        }],
    })
}

fn pod(spec: Value) -> Pod {
    serde_json::from_value(json!({
        "metadata": {"name": format!("probe-{}", suffix())},
        "spec": spec
    }))
    .unwrap()
}

type Mutation = fn(&mut Value);

/// One table row; the closure must not capture.
macro_rules! row {
    ($case:expr, $mutate:expr, $markers:expr $(,)?) => {
        (
            $case,
            $mutate as Mutation,
            $markers as &'static [&'static str],
        )
    };
}

fn pod_cases() -> Vec<(&'static str, Mutation, &'static [&'static str])> {
    vec![
        row!(
            "runs as root",
            |s| s["securityContext"]["runAsUser"] = json!(0),
            &[NON_ROOT, POD_SECURITY],
        ),
        row!(
            "runs as another non-root user",
            |s| s["securityContext"]["runAsUser"] = json!(2000),
            &[NON_ROOT],
        ),
        row!(
            "runs as another group",
            |s| s["securityContext"]["runAsGroup"] = json!(2000),
            &[NON_ROOT],
        ),
        row!(
            "may run as root",
            |s| s["securityContext"]["runAsNonRoot"] = json!(false),
            &[NON_ROOT, POD_SECURITY],
        ),
        row!(
            "has no Pod security context",
            |s| {
                s.as_object_mut().unwrap().remove("securityContext");
            },
            &[NON_ROOT, POD_SECURITY],
        ),
        row!(
            "has a privileged container",
            |s| {
                // The API refuses `privileged` with escalation disabled.
                s["containers"][0]["securityContext"]["privileged"] = json!(true);
                s["containers"][0]["securityContext"]["allowPrivilegeEscalation"] = json!(true);
            },
            &[UNPRIVILEGED, POD_SECURITY],
        ),
        row!(
            "allows privilege escalation",
            |s| s["containers"][0]["securityContext"]["allowPrivilegeEscalation"] = json!(true),
            &[UNPRIVILEGED, POD_SECURITY],
        ),
        row!(
            "leaves privilege escalation unset",
            |s| {
                s["containers"][0]["securityContext"]
                    .as_object_mut()
                    .unwrap()
                    .remove("allowPrivilegeEscalation");
            },
            &[UNPRIVILEGED, POD_SECURITY],
        ),
        row!(
            "adds a capability",
            |s| s["containers"][0]["securityContext"]["capabilities"]["add"] =
                json!(["NET_BIND_SERVICE"]),
            &[UNPRIVILEGED],
        ),
        row!(
            "drops too few capabilities",
            |s| s["containers"][0]["securityContext"]["capabilities"]["drop"] = json!(["NET_RAW"]),
            &[UNPRIVILEGED, POD_SECURITY],
        ),
        row!(
            "has a container without a security context",
            |s| {
                s["containers"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("securityContext");
            },
            &[UNPRIVILEGED, POD_SECURITY],
        ),
        row!(
            "has a privileged init container",
            |s| {
                s["initContainers"] = json!([{
                    "name": "i",
                    "image": format!("example.org/x@sha256:{}", "b".repeat(64)),
                    "securityContext": {"allowPrivilegeEscalation": true, "privileged": true,
                        "capabilities": {"drop": ["ALL"]}}
                }])
            },
            &[UNPRIVILEGED, POD_SECURITY],
        ),
        row!(
            "has an unpinned init container",
            |s| {
                s["initContainers"] = json!([{
                    "name": "i", "image": "example.org/x:latest",
                    "securityContext": {"allowPrivilegeEscalation": false,
                        "capabilities": {"drop": ["ALL"]}}
                }])
            },
            &[PINNED],
        ),
        row!(
            "uses an image tag",
            |s| s["containers"][0]["image"] = json!("example.org/x:latest"),
            &[PINNED],
        ),
        row!(
            "uses a tag together with a digest",
            |s| {
                s["containers"][0]["image"] =
                    json!(format!("example.org/x:1.0@sha256:{}", "b".repeat(64)))
            },
            // A tag before an `@digest` is a pinned image to the registry
            // client; the rule is that a digest is present and well formed.
            &[],
        ),
        row!(
            "uses a short digest",
            |s| s["containers"][0]["image"] = json!("example.org/x@sha256:abc123"),
            &[PINNED],
        ),
        row!(
            "uses an uppercase digest",
            |s| {
                s["containers"][0]["image"] =
                    json!(format!("example.org/x@sha256:{}", "B".repeat(64)))
            },
            &[PINNED],
        ),
        row!(
            "uses another digest algorithm",
            |s| {
                s["containers"][0]["image"] =
                    json!(format!("example.org/x@sha512:{}", "b".repeat(64)))
            },
            &[PINNED],
        ),
        row!(
            "uses the host network",
            |s| s["hostNetwork"] = json!(true),
            &[HOST, POD_SECURITY],
        ),
        row!(
            "shares the host PID namespace",
            |s| s["hostPID"] = json!(true),
            &[HOST, POD_SECURITY],
        ),
        row!(
            "shares the host IPC namespace",
            |s| s["hostIPC"] = json!(true),
            &[HOST, POD_SECURITY],
        ),
        row!(
            "automounts a token",
            |s| s["automountServiceAccountToken"] = json!(true),
            &[TOKEN],
        ),
        row!(
            "leaves the token automount unset",
            |s| {
                s.as_object_mut()
                    .unwrap()
                    .remove("automountServiceAccountToken");
            },
            &[TOKEN],
        ),
        row!(
            "mounts a host path",
            |s| s["volumes"] = json!([{"name": "v", "hostPath": {"path": "/"}}]),
            &[VOLUMES, POD_SECURITY],
        ),
        row!(
            "mounts NFS directly",
            |s| s["volumes"] =
                json!([{"name": "v", "nfs": {"server": "nas.internal", "path": "/export"}}]),
            &[VOLUMES, POD_SECURITY],
        ),
        row!(
            "projects a service account token",
            |s| {
                s["volumes"] = json!([{"name": "v", "projected": {"sources": [
                    {"serviceAccountToken": {"path": "t", "expirationSeconds": 3600}}]}}])
            },
            &[VOLUMES],
        ),
        row!(
            "mounts the downward API",
            |s| {
                s["volumes"] = json!([{"name": "v", "downwardAPI": {"items": [
                    {"path": "n", "fieldRef": {"fieldPath": "metadata.name"}}]}}])
            },
            &[VOLUMES],
        ),
        row!(
            "declares an ephemeral container",
            |s| {
                s["ephemeralContainers"] = json!([{
                    "name": "debug",
                    "image": format!("example.org/x@sha256:{}", "b".repeat(64)),
                    "securityContext": {"allowPrivilegeEscalation": false,
                        "capabilities": {"drop": ["ALL"]}}
                }])
            },
            // The API refuses ephemeral containers on create before admission.
            &[EPHEMERAL, "ephemeral", "Forbidden"],
        ),
        row!(
            "overrides the user in a container to another identity",
            |s| s["containers"][0]["securityContext"]["runAsUser"] = json!(2000),
            &[CONTAINER_IDENTITY],
        ),
        row!(
            "overrides the group in a container to another identity",
            |s| s["containers"][0]["securityContext"]["runAsGroup"] = json!(2000),
            &[CONTAINER_IDENTITY],
        ),
        row!(
            "overrides the user in an init container to another identity",
            |s| {
                s["initContainers"] = json!([{
                    "name": "i",
                    "image": format!("example.org/x@sha256:{}", "b".repeat(64)),
                    "securityContext": {"allowPrivilegeEscalation": false, "runAsUser": 2000,
                        "capabilities": {"drop": ["ALL"]}}
                }])
            },
            &[CONTAINER_IDENTITY],
        ),
        // The identity repeated at container level is the data identity.
        row!(
            "repeats the data identity in a container",
            |s| {
                s["containers"][0]["securityContext"]["runAsUser"] = json!(1000);
                s["containers"][0]["securityContext"]["runAsGroup"] = json!(1000);
                s["containers"][0]["securityContext"]["runAsNonRoot"] = json!(true);
            },
            &[],
        ),
        // Rules the policies leave to Pod Security Admission through the
        // namespace's `restricted` label.
        row!(
            "overrides the user in a container",
            |s| s["containers"][0]["securityContext"]["runAsUser"] = json!(0),
            &[POD_SECURITY, NON_ROOT, CONTAINER_IDENTITY],
        ),
        row!(
            "lets a container run as root",
            |s| s["containers"][0]["securityContext"]["runAsNonRoot"] = json!(false),
            &[POD_SECURITY, NON_ROOT, CONTAINER_IDENTITY],
        ),
        row!(
            "runs unconfined by seccomp",
            |s| s["securityContext"]["seccompProfile"] = json!({"type": "Unconfined"}),
            &[POD_SECURITY],
        ),
        row!(
            "publishes a host port",
            |s| s["containers"][0]["ports"] = json!([{"containerPort": 8080, "hostPort": 8080}]),
            &[POD_SECURITY],
        ),
    ]
}

#[tokio::test]
#[ignore = "needs HERMES_TEST_KUBECONFIG from the disposable Kubernetes fixture"]
async fn a_compliant_pod_is_admitted_and_every_rule_refuses_its_own_violation() {
    let fixture = Fixture::new().await;
    let namespace = commissioned(&fixture).await;
    let pods: Api<Pod> = Api::namespaced(fixture.controller.clone(), &namespace);
    let post = PostParams::default();

    // The positive control: the base every negative case varies.
    let control = pods
        .create(&post, &pod(safe()))
        .await
        .expect("a compliant Pod is admitted");
    let control = control.metadata.name.unwrap();

    let mut accepted = Vec::new();
    for (case, mutate, markers) in pod_cases() {
        let mut spec = safe();
        mutate(&mut spec);
        if markers.is_empty() {
            // A case recorded as admitted by design must really be admitted.
            let created = pods.create(&post, &pod(spec)).await;
            accepted.push((case, created.map(|p| p.metadata.name.unwrap())));
        } else {
            denied(pods.create(&post, &pod(spec)).await, case, markers);
        }
    }
    settle();
    for (case, created) in accepted {
        let name = created.unwrap_or_else(|error| panic!("{case} was refused: {error}"));
        pods.delete(&name, &DeleteParams::default()).await.unwrap();
    }

    // Updates are subject to the same rules as creation.
    let patch = |value: Value| strategic(&pods, &control, value);
    denied(
        patch(json!({"spec": {"containers": [{"name": "c", "image": "example.org/x:latest"}]}}))
            .await,
        "an image changed to a tag",
        &[PINNED],
    );
    let ephemeral = pods
        .patch_ephemeral_containers(
            &control,
            &PatchParams::default(),
            &Patch::Strategic(json!({"spec": {"ephemeralContainers": [{
                "name": "debug",
                "image": format!("example.org/x@sha256:{}", "b".repeat(64)),
                "securityContext": {"allowPrivilegeEscalation": false,
                    "capabilities": {"drop": ["ALL"]}}
            }]}})),
        )
        .await;
    // The controller's role does not include the subresource; the policy
    // refuses even a compliant ephemeral container.
    denied(ephemeral, "an ephemeral container", &[EPHEMERAL, FORBIDDEN]);
    assert!(
        patch(json!({"spec": {"activeDeadlineSeconds": 3600}}))
            .await
            .is_ok(),
        "a compliant update is admitted"
    );
    pods.delete(&control, &DeleteParams::default())
        .await
        .unwrap();
}

async fn strategic(pods: &Api<Pod>, name: &str, value: Value) -> Result<Pod, kube::Error> {
    pods.patch(name, &PatchParams::default(), &Patch::Strategic(value))
        .await
}

fn namespace_labels(fixture: &Fixture, instance: Uuid) -> Value {
    json!({
        "agent-platform.io/installation": fixture.installation().id.to_string(),
        "app.kubernetes.io/managed-by": "hermes-manager",
        "pod-security.kubernetes.io/enforce": "restricted",
        "agent-platform.io/instance": instance.to_string(),
    })
}

#[tokio::test]
#[ignore = "needs HERMES_TEST_KUBECONFIG from the disposable Kubernetes fixture"]
async fn only_the_controller_writes_instance_namespaces_and_only_with_the_installations_labels() {
    let fixture = Fixture::new().await;
    let post = PostParams::default();
    let instance = Uuid::new_v4();
    let name = fixture.installation().instance_namespace(instance);
    let make = |name: &str, labels: Value| -> Namespace {
        serde_json::from_value(json!({"metadata": {"name": name, "labels": labels}})).unwrap()
    };
    let right = namespace_labels(&fixture, instance);
    let controller: Api<Namespace> = Api::all(fixture.controller.clone());
    let admin: Api<Namespace> = Api::all(fixture.admin.clone());

    // An administrator, even a cluster-admin, cannot create or relabel an
    // instance namespace.
    denied(
        admin.create(&post, &make(&name, right.clone())).await,
        "an administrator creating an instance namespace",
        &[NAMESPACE_WRITER, WRITERS],
    );

    let mut cases: Vec<(&str, String, Value)> = Vec::new();
    let without = |label: &str| {
        let mut labels = right.clone();
        labels.as_object_mut().unwrap().remove(label);
        labels
    };
    cases.push((
        "no installation label",
        name.clone(),
        without("agent-platform.io/installation"),
    ));
    cases.push((
        "no manager label",
        name.clone(),
        without("app.kubernetes.io/managed-by"),
    ));
    cases.push((
        "no Pod Security label",
        name.clone(),
        without("pod-security.kubernetes.io/enforce"),
    ));
    cases.push((
        "no instance label",
        name.clone(),
        without("agent-platform.io/instance"),
    ));
    let mut baseline = right.clone();
    baseline["pod-security.kubernetes.io/enforce"] = json!("baseline");
    cases.push(("a weaker Pod Security level", name.clone(), baseline));
    let mut foreign = right.clone();
    foreign["agent-platform.io/installation"] = json!(Uuid::new_v4().to_string());
    cases.push(("another installation's label", name.clone(), foreign));
    let mut mismatched = right.clone();
    mismatched["agent-platform.io/instance"] = json!(Uuid::new_v4().to_string());
    cases.push((
        "a name that is not the instance's",
        name.clone(),
        mismatched,
    ));
    cases.push(("an instance name without labels", name.clone(), json!({})));
    for (case, name, labels) in cases {
        denied(
            controller.create(&post, &make(&name, labels)).await,
            case,
            &[NAMESPACE_LABELS, POLICY_ERROR],
        );
    }
    denied(
        controller
            .create(&post, &make(&format!("outside-{}", suffix()), json!({})))
            .await,
        "a namespace outside the installation's prefix",
        &[NAMESPACE_LABELS, SCOPE, FORBIDDEN],
    );

    // The same namespace with exactly the right name and labels is admitted,
    // and can then not be weakened.
    controller
        .create(&post, &make(&name, right.clone()))
        .await
        .expect("a namespace with the installation's name and labels is admitted");
    denied(
        controller
            .patch(
                &name,
                &PatchParams::default(),
                &Patch::Merge(json!({"metadata": {"labels": {"pod-security.kubernetes.io/enforce": "privileged"}}})),
            )
            .await,
        "weakening the Pod Security label",
        &[NAMESPACE_LABELS],
    );
    denied(
        controller.delete(&name, &DeleteParams::default()).await,
        "the controller deleting an instance namespace",
        &["never deletes", FORBIDDEN],
    );
    denied(
        admin
            .patch(
                &name,
                &PatchParams::default(),
                &Patch::Merge(json!({"metadata": {"labels": {"x": "y"}}})),
            )
            .await,
        "an administrator relabelling an instance namespace",
        &[NAMESPACE_WRITER, WRITERS],
    );
    settle();
}

#[tokio::test]
#[ignore = "needs HERMES_TEST_KUBECONFIG from the disposable Kubernetes fixture"]
async fn nobody_but_the_controller_writes_inside_an_instance_namespace() {
    let fixture = Fixture::new().await;
    let namespace = commissioned(&fixture).await;
    let post = PostParams::default();
    let secret = |name: &str| -> Secret {
        serde_json::from_value(json!({"metadata": {"name": name}, "stringData": {"k": "v"}}))
            .unwrap()
    };
    let controller: Api<Secret> = Api::namespaced(fixture.controller.clone(), &namespace);
    let admin: Api<Secret> = Api::namespaced(fixture.admin.clone(), &namespace);

    controller
        .create(&post, &secret(&format!("control-{}", suffix())))
        .await
        .expect("the controller writes inside its instance namespace");
    denied(
        admin.create(&post, &secret("planted")).await,
        "an administrator creating a Secret",
        &[WRITERS],
    );
    let existing = controller.list(&ListParams::default()).await.unwrap();
    let victim = existing.items.first().expect("a Secret exists").clone();
    denied(
        admin
            .patch(
                victim.metadata.name.as_deref().unwrap(),
                &PatchParams::default(),
                &Patch::Merge(json!({"stringData": {"k": "tampered"}})),
            )
            .await,
        "an administrator patching a Secret",
        &[WRITERS],
    );
    denied(
        admin
            .delete(
                victim.metadata.name.as_deref().unwrap(),
                &DeleteParams::default(),
            )
            .await,
        "an administrator deleting a Secret",
        &[WRITERS],
    );
    let pods: Api<Pod> = Api::namespaced(fixture.admin.clone(), &namespace);
    denied(
        pods.create(&PostParams::default(), &pod(safe())).await,
        "an administrator creating a Pod",
        &[WRITERS],
    );
    settle();
}

#[tokio::test]
#[ignore = "needs HERMES_TEST_KUBECONFIG from the disposable Kubernetes fixture"]
async fn services_and_role_bindings_keep_to_their_narrow_shapes() {
    let fixture = Fixture::new().await;
    let namespace = commissioned(&fixture).await;
    let post = PostParams::default();
    let services: Api<Service> = Api::namespaced(fixture.controller.clone(), &namespace);
    let service = |name: &str, spec: Value| -> Service {
        let mut spec = spec;
        spec["selector"] = json!({"app": "x"});
        spec["ports"] = json!([{"port": 80}]);
        serde_json::from_value(json!({"metadata": {"name": name}, "spec": spec})).unwrap()
    };
    services
        .create(
            &post,
            &service(&format!("ok-{}", suffix()), json!({"type": "ClusterIP"})),
        )
        .await
        .expect("a cluster-internal Service is admitted");
    for (case, spec) in [
        ("a LoadBalancer", json!({"type": "LoadBalancer"})),
        ("a NodePort", json!({"type": "NodePort"})),
        (
            "an ExternalName",
            json!({"type": "ExternalName", "externalName": "example.org"}),
        ),
        (
            "external IPs",
            json!({"type": "ClusterIP", "externalIPs": ["203.0.113.7"]}),
        ),
    ] {
        let mut object = service(&format!("bad-{}", suffix()), spec.clone());
        if case == "an ExternalName" {
            object.spec.as_mut().unwrap().selector = None;
            object.spec.as_mut().unwrap().ports = None;
        }
        denied(services.create(&post, &object).await, case, &[SERVICES]);
    }

    let bindings: Api<RoleBinding> = Api::namespaced(fixture.controller.clone(), &namespace);
    let role = fixture.config.instance_role.clone();
    let account = fixture.config.controller_service_account.clone();
    let control = fixture.config.control_namespace.clone();
    let binding = |name: &str, role_ref: Value, subjects: Value| -> RoleBinding {
        serde_json::from_value(json!({"metadata": {"name": name},
            "roleRef": role_ref, "subjects": subjects}))
        .unwrap()
    };
    let instance_role =
        json!({"apiGroup": "rbac.authorization.k8s.io", "kind": "ClusterRole", "name": role});
    let controller_subject =
        json!({"kind": "ServiceAccount", "name": account, "namespace": control});
    bindings
        .create(
            &post,
            &binding(
                &format!("ok-{}", suffix()),
                instance_role.clone(),
                json!([controller_subject.clone()]),
            ),
        )
        .await
        .expect("the instance role bound to the controller is admitted");
    for (case, role_ref, subjects) in [
        (
            "an additional subject",
            instance_role.clone(),
            json!([controller_subject.clone(),
                {"kind": "ServiceAccount", "name": "default", "namespace": "kube-system"}]),
        ),
        (
            "another service account",
            instance_role.clone(),
            json!([{"kind": "ServiceAccount", "name": "default", "namespace": control}]),
        ),
        (
            "the controller account from another namespace",
            instance_role.clone(),
            json!([{"kind": "ServiceAccount", "name": account, "namespace": "kube-system"}]),
        ),
        (
            "a user subject",
            instance_role.clone(),
            json!([{"kind": "User", "name": "mallory", "apiGroup": "rbac.authorization.k8s.io"}]),
        ),
        (
            "another role",
            json!({"apiGroup": "rbac.authorization.k8s.io", "kind": "ClusterRole", "name": "edit"}),
            json!([controller_subject.clone()]),
        ),
        (
            "the cluster-admin role",
            json!({"apiGroup": "rbac.authorization.k8s.io", "kind": "ClusterRole", "name": "cluster-admin"}),
            json!([controller_subject.clone()]),
        ),
        (
            "a namespaced role",
            json!({"apiGroup": "rbac.authorization.k8s.io", "kind": "Role", "name": role}),
            json!([controller_subject.clone()]),
        ),
    ] {
        denied(
            bindings
                .create(
                    &post,
                    &binding(&format!("bad-{}", suffix()), role_ref, subjects),
                )
                .await,
            case,
            // Binding a role the controller may not bind is refused by RBAC
            // before admission, and a Role that does not exist cannot be
            // checked for escalation at all.
            &[BINDING, FORBIDDEN, "not found"],
        );
    }
    settle();
}

fn volume(namespace: &str, source: Value) -> PersistentVolume {
    let mut spec = json!({
        "capacity": {"storage": "1Gi"},
        "accessModes": ["ReadWriteOnce"],
        "persistentVolumeReclaimPolicy": "Retain",
        "storageClassName": "",
        "claimRef": {"namespace": namespace, "name": "x"},
    });
    for (key, value) in source.as_object().unwrap() {
        spec[key] = value.clone();
    }
    serde_json::from_value(json!({
        "metadata": {"name": format!("{namespace}-{}", suffix())},
        "spec": spec
    }))
    .unwrap()
}

fn local(path: &str, node: Value) -> Value {
    json!({
        "local": {"path": path},
        "nodeAffinity": {"required": {"nodeSelectorTerms": [{"matchExpressions": [node]}]}}
    })
}

fn on(values: &[&str]) -> Value {
    json!({"key": "kubernetes.io/hostname", "operator": "In", "values": values})
}

fn nfs(server: &str, path: &str) -> Value {
    json!({"accessModes": ["ReadWriteMany"], "nfs": {"server": server, "path": path}})
}

#[tokio::test]
#[ignore = "needs HERMES_TEST_KUBECONFIG from the disposable Kubernetes fixture"]
async fn static_volumes_are_admitted_only_inside_the_approved_roots_and_retained() {
    let fixture = Fixture::new().await;
    let namespace = commissioned(&fixture).await;
    let post = PostParams::default();
    let volumes: Api<PersistentVolume> = Api::all(fixture.controller.clone());
    let admin: Api<PersistentVolume> = Api::all(fixture.admin.clone());
    let base = |source: Value| volume(&namespace, source);

    // Positive controls, one per approved source kind.
    let mut created = Vec::new();
    for (case, source) in [
        (
            "a directory under the approved local root and node",
            local("/srv/hermes-data/bob/x", on(&["server-1"])),
        ),
        (
            "a directory under the approved NFS export",
            nfs("nas.internal", "/export/homes/x"),
        ),
    ] {
        let volume = volumes
            .create(&post, &base(source))
            .await
            .unwrap_or_else(|error| panic!("{case} was refused: {error}"));
        created.push(volume.metadata.name.unwrap());
    }

    let mut cases: Vec<(&str, PersistentVolume, &[&str])> = vec![
        (
            "the local root itself",
            base(local("/srv/hermes-data", on(&["server-1"]))),
            &[ROOTS],
        ),
        (
            "a sibling that shares the root's prefix",
            base(local("/srv/hermes-data-evil/x", on(&["server-1"]))),
            &[ROOTS],
        ),
        (
            "a directory outside the root",
            base(local("/etc", on(&["server-1"]))),
            &[ROOTS],
        ),
        (
            "a local path that climbs out of the root",
            base(local("/srv/hermes-data/../../etc", on(&["server-1"]))),
            &[ROOTS, "must not contain"],
        ),
        (
            "another node",
            base(local("/srv/hermes-data/bob/x", on(&["server-2"]))),
            &[ROOTS],
        ),
        (
            "two nodes",
            base(local(
                "/srv/hermes-data/bob/x",
                on(&["server-1", "server-2"]),
            )),
            &[ROOTS],
        ),
        (
            "a node selector that excludes instead of selects",
            base(local(
                "/srv/hermes-data/bob/x",
                json!({"key": "kubernetes.io/hostname", "operator": "NotIn", "values": ["server-1"]}),
            )),
            &[ROOTS],
        ),
        (
            "a selector on another label",
            base(local(
                "/srv/hermes-data/bob/x",
                json!({"key": "topology.kubernetes.io/zone", "operator": "In", "values": ["server-1"]}),
            )),
            &[ROOTS],
        ),
        (
            "another NFS server",
            base(nfs("evil.internal", "/export/homes/x")),
            &[ROOTS],
        ),
        (
            "the NFS root itself",
            base(nfs("nas.internal", "/export/homes")),
            &[ROOTS],
        ),
        (
            "an NFS sibling that shares the export's prefix",
            base(nfs("nas.internal", "/export/homes-evil/x")),
            &[ROOTS],
        ),
        (
            "an NFS path outside the export",
            base(nfs("nas.internal", "/export/other")),
            &[ROOTS],
        ),
        (
            "a host path volume",
            base(json!({"hostPath": {"path": "/srv/hermes-data/bob/x"}})),
            &[ROOTS],
        ),
        (
            "a volume with no source",
            base(json!({})),
            &[ROOTS, "Required value"],
        ),
    ];
    let mut reclaim = base(local("/srv/hermes-data/bob/x", on(&["server-1"])));
    for (case, policy) in [("deleted on release", "Delete"), ("recycled", "Recycle")] {
        reclaim
            .spec
            .as_mut()
            .unwrap()
            .persistent_volume_reclaim_policy = Some(policy.into());
        reclaim.metadata.name = Some(format!("{namespace}-{}", suffix()));
        cases.push((case, reclaim.clone(), &[RETAINED]));
    }
    let mut unbound = base(local("/srv/hermes-data/bob/x", on(&["server-1"])));
    unbound.spec.as_mut().unwrap().claim_ref = None;
    cases.push(("a volume with no claim", unbound, &[RETAINED]));
    for (case, target) in [
        ("a claim in the default namespace", "default"),
        ("a claim in kube-system", "kube-system"),
        (
            "a claim in the control namespace",
            &fixture.config.control_namespace,
        ),
    ] {
        let mut claimed = base(local("/srv/hermes-data/bob/x", on(&["server-1"])));
        claimed
            .spec
            .as_mut()
            .unwrap()
            .claim_ref
            .as_mut()
            .unwrap()
            .namespace = Some(target.to_owned());
        cases.push((case, claimed, &[RETAINED]));
    }
    let mut classed = base(local("/srv/hermes-data/bob/x", on(&["server-1"])));
    classed.spec.as_mut().unwrap().storage_class_name = Some("local-path".into());
    cases.push(("a volume of a storage class", classed, &[RETAINED]));

    for (case, volume, markers) in cases {
        denied(volumes.create(&post, &volume).await, case, markers);
    }
    for name in created {
        admin.delete(&name, &DeleteParams::default()).await.unwrap();
    }
    settle();
}

#[tokio::test]
#[ignore = "needs HERMES_TEST_KUBECONFIG from the disposable Kubernetes fixture"]
async fn the_controllers_credentials_cannot_read_write_or_escalate_beyond_its_scope() {
    let fixture = Fixture::new().await;
    let namespace = commissioned(&fixture).await;
    let controller = &fixture.controller;
    let post = PostParams::default();
    let policy_name = format!(
        "hermes-manager-{}-pods",
        &fixture.installation().id.simple().to_string()[..12]
    );

    // What it must be able to do, so a credential that can do nothing fails.
    let namespaces: Api<Namespace> = Api::all(controller.clone());
    assert!(
        namespaces
            .list(&ListParams::default())
            .await
            .unwrap()
            .items
            .iter()
            .any(|item| item.metadata.name.as_deref() == Some(namespace.as_str())),
        "the controller sees its instance namespaces"
    );
    let own: Api<Secret> = Api::namespaced(controller.clone(), &namespace);
    assert!(
        !own.list(&ListParams::default())
            .await
            .unwrap()
            .items
            .is_empty(),
        "the controller reads Secrets in its instance namespaces"
    );

    // Reading other namespaces' Secrets, anywhere.
    for target in [
        "kube-system",
        "default",
        fixture.config.control_namespace.as_str(),
    ] {
        let secrets: Api<Secret> = Api::namespaced(controller.clone(), target);
        denied(
            secrets.list(&ListParams::default()).await,
            &format!("listing Secrets in {target}"),
            &[FORBIDDEN],
        );
    }
    let everywhere: Api<Secret> = Api::all(controller.clone());
    denied(
        everywhere.list(&ListParams::default()).await,
        "listing Secrets cluster-wide",
        &[FORBIDDEN],
    );

    // Editing the admission policies or its own rights.
    let policies = kube::core::ApiResource::from_gvk_with_plural(
        &kube::core::GroupVersionKind::gvk(
            "admissionregistration.k8s.io",
            "v1",
            "ValidatingAdmissionPolicy",
        ),
        "validatingadmissionpolicies",
    );
    let policies: Api<kube::core::DynamicObject> = Api::all_with(controller.clone(), &policies);
    denied(
        policies
            .patch(
                &policy_name,
                &PatchParams::default(),
                &Patch::Merge(json!({"spec": {"validations": []}})),
            )
            .await,
        "editing an admission policy",
        &[FORBIDDEN],
    );
    denied(
        policies
            .delete(&policy_name, &DeleteParams::default())
            .await,
        "deleting an admission policy",
        &[FORBIDDEN],
    );
    let roles = kube::core::ApiResource::from_gvk_with_plural(
        &kube::core::GroupVersionKind::gvk("rbac.authorization.k8s.io", "v1", "ClusterRole"),
        "clusterroles",
    );
    let roles: Api<kube::core::DynamicObject> = Api::all_with(controller.clone(), &roles);
    denied(
        roles
            .create(
                &post,
                &serde_json::from_value(json!({
                    "metadata": {"name": format!("escalate-{}", suffix())},
                    "rules": [{"apiGroups": ["*"], "resources": ["*"], "verbs": ["*"]}]
                }))
                .unwrap(),
            )
            .await,
        "creating a ClusterRole",
        &[FORBIDDEN],
    );
    let bindings = kube::core::ApiResource::from_gvk_with_plural(
        &kube::core::GroupVersionKind::gvk("rbac.authorization.k8s.io", "v1", "ClusterRoleBinding"),
        "clusterrolebindings",
    );
    let bindings: Api<kube::core::DynamicObject> = Api::all_with(controller.clone(), &bindings);
    denied(
        bindings
            .create(
                &post,
                &serde_json::from_value(json!({
                    "metadata": {"name": format!("escalate-{}", suffix())},
                    "roleRef": {"apiGroup": "rbac.authorization.k8s.io", "kind": "ClusterRole", "name": "cluster-admin"},
                    "subjects": [{"kind": "ServiceAccount",
                        "name": fixture.config.controller_service_account,
                        "namespace": fixture.config.control_namespace}]
                }))
                .unwrap(),
            )
            .await,
        "binding cluster-admin cluster-wide",
        &[FORBIDDEN],
    );

    // Acting outside instance namespaces and on what it may not delete.
    let elsewhere: Api<Pod> = Api::namespaced(controller.clone(), "default");
    denied(
        elsewhere.create(&post, &pod(safe())).await,
        "creating a Pod outside instance namespaces",
        &[FORBIDDEN, SCOPE],
    );
    let volumes: Api<PersistentVolume> = Api::all(controller.clone());
    denied(
        volumes.delete("any", &DeleteParams::default()).await,
        "deleting a PersistentVolume",
        &[FORBIDDEN],
    );
    let nodes: Api<k8s_openapi::api::core::v1::Node> = Api::all(controller.clone());
    denied(
        nodes.list(&ListParams::default()).await,
        "listing nodes",
        &[FORBIDDEN],
    );

    // Acting as someone else.
    let mut impersonating = fixture.controller_config.clone();
    impersonating.auth_info.impersonate = Some("system:admin".into());
    impersonating.auth_info.impersonate_groups = Some(vec!["system:masters".into()]);
    // `hermes_kube::client` refuses impersonation; the server must too.
    let impersonated = kube::Client::try_from(impersonating).unwrap();
    let secrets: Api<Secret> = Api::namespaced(impersonated, "kube-system");
    denied(
        secrets.list(&ListParams::default()).await,
        "impersonating a cluster administrator",
        &[FORBIDDEN],
    );
    settle();
}
