use hermes_adapters::{bootstrap::BootstrapPolicy, workloads::WorkloadConfig};
use hermes_application::{Error, InstallationScope};
use k8s_openapi::api::{
    authentication::v1::{TokenRequest, TokenRequestSpec},
    core::v1::{Namespace, Secret, ServiceAccount},
    rbac::v1::{ClusterRole, ClusterRoleBinding, RoleBinding},
};
use kube::{
    Api, Client, Config,
    api::{Patch, PatchParams, PostParams},
    config::{KubeConfigOptions, Kubeconfig},
    core::{ApiResource, DynamicObject, GroupVersionKind},
};
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use uuid::Uuid;

fn api(client: &Client, object: &Value) -> Api<DynamicObject> {
    let kind = object["kind"].as_str().unwrap();
    let plural = match kind {
        "ClusterRole" => "clusterroles",
        "ClusterRoleBinding" => "clusterrolebindings",
        "ValidatingAdmissionPolicy" => "validatingadmissionpolicies",
        "ValidatingAdmissionPolicyBinding" => "validatingadmissionpolicybindings",
        "CustomResourceDefinition" => "customresourcedefinitions",
        _ => panic!("unexpected fixture kind"),
    };
    let (group, version) = object["apiVersion"]
        .as_str()
        .unwrap()
        .split_once('/')
        .unwrap();
    Api::all_with(
        client.clone(),
        &ApiResource::from_gvk_with_plural(&GroupVersionKind::gvk(group, version, kind), plural),
    )
}
fn denied<T>(response: Result<T, kube::Error>) {
    assert!(
        matches!(response,Err(kube::Error::Api(response)) if response.code==403),
        "scope escape was not denied"
    );
}
fn namespace(scope: &InstallationScope, instance: Uuid) -> Namespace {
    serde_json::from_value(json!({"apiVersion":"v1","kind":"Namespace","metadata":{"name":hermes_domain::instance_namespace(scope.id,instance),"labels":{"agent-platform.io/installation":scope.id.to_string(),"agent-platform.io/instance":instance.to_string(),"pod-security.kubernetes.io/enforce":"restricted"}}})).unwrap()
}
#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
#[ignore = "requires HERMES_TEST_KUBECONFIG from the disposable Kubernetes fixture"]
async fn actual_controller_service_account_cannot_escape_namespace_bootstrap_scope(
    pool: sqlx::PgPool,
) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let path = std::env::var("HERMES_TEST_KUBECONFIG").expect("disposable fixture only");
    let mut config = Config::from_custom_kubeconfig(
        Kubeconfig::read_from(path).unwrap(),
        &KubeConfigOptions {
            context: Some("personal-hermes-manager-tests".into()),
            ..KubeConfigOptions::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(config.cluster_url.host(), Some("127.0.0.1"));
    config.connect_timeout = Some(Duration::from_secs(2));
    config.read_timeout = Some(Duration::from_secs(5));
    config.write_timeout = Some(Duration::from_secs(5));
    config.default_retry = false;
    let administrator = Client::try_from(config.clone()).unwrap();
    let scope = InstallationScope {
        id: Uuid::new_v4(),
        epoch: 1,
        issuer: "https://sso.example.org/realms/agents".into(),
    };
    let control = format!("control-{}", scope.id.simple());
    let namespaces: Api<Namespace> = Api::all(administrator.clone());
    namespaces
        .create(
            &PostParams::default(),
            &serde_json::from_value(json!({"metadata":{"name":control}})).unwrap(),
        )
        .await
        .unwrap();
    let accounts: Api<ServiceAccount> = Api::namespaced(administrator.clone(), &control);
    accounts.create(&PostParams::default(),&serde_json::from_value(json!({"metadata":{"name":"hermes-controller"},"automountServiceAccountToken":false})).unwrap()).await.unwrap();
    let token = accounts
        .create_token_request(
            "hermes-controller",
            &PostParams::default(),
            &TokenRequest {
                spec: Some(TokenRequestSpec {
                    expiration_seconds: Some(600),
                    ..TokenRequestSpec::default()
                }),
                ..TokenRequest::default()
            },
        )
        .await
        .unwrap()
        .status
        .unwrap()
        .token
        .unwrap();
    config.auth_info = Default::default();
    config.auth_info.token = Some(token.into());
    let controller_config = config.clone();
    let controller = Client::try_from(config).unwrap();
    let workload = WorkloadConfig {
        control_namespace: control.clone(),
        controller_service_account: "hermes-controller".into(),
        reconciler_role: BootstrapPolicy::reconciler_role(&scope),
        public_origin: "https://hermes.example.org".into(),
        oidc_client: "native-hermes".into(),
        execution_port: 8443,
        execution_ca_pem: include_str!("../fixtures/template-ca.crt").into(),
        oidc_ca_pem: None,
        service_egress: vec![],
        public_https: false,
    };
    let policy = BootstrapPolicy::new(&scope, &workload).unwrap();
    assert!(policy.verify(&controller).await.is_err());
    for manifest in policy.manifests() {
        let data: DynamicObject = serde_json::from_value(manifest.clone()).unwrap();
        api(&administrator, manifest)
            .create(&PostParams::default(), &data)
            .await
            .unwrap();
        if manifest["kind"] == "CustomResourceDefinition" {
            let deadline = Instant::now() + Duration::from_secs(20);
            loop {
                let current = api(&administrator, manifest)
                    .get(manifest["metadata"]["name"].as_str().unwrap())
                    .await
                    .unwrap();
                let current = serde_json::to_value(current).unwrap();
                if current["status"]["conditions"]
                    .as_array()
                    .is_some_and(|conditions| {
                        conditions
                            .iter()
                            .any(|c| c["type"] == "Established" && c["status"] == "True")
                    })
                {
                    break;
                }
                assert!(Instant::now() < deadline, "fixture CRD was not established");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
    let reviews: Api<k8s_openapi::api::authentication::v1::SelfSubjectReview> =
        Api::all(controller.clone());
    let review = reviews
        .create(&PostParams::default(), &Default::default())
        .await
        .unwrap();
    assert_eq!(
        review.status.unwrap().user_info.unwrap().username,
        Some(policy.subject().to_string())
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let result = policy.verify(&controller).await;
        if result.is_ok() {
            break;
        }
        if Instant::now() >= deadline {
            // These manifests contain only release-owned policy/RBAC/schema,
            // never Secrets or owner content. Report schema paths, not values.
            for expected in policy.manifests() {
                let actual = api(&administrator, expected)
                    .get(expected["metadata"]["name"].as_str().unwrap())
                    .await
                    .unwrap();
                let actual = serde_json::to_value(actual).unwrap();
                eprintln!(
                    "policy kind={} differences={:?} warnings={}",
                    expected["kind"],
                    differences(&actual["spec"], &expected["spec"], "spec"),
                    actual["status"]["typeChecking"]
                );
            }
        }
        assert!(
            Instant::now() < deadline,
            "bootstrap policy did not become verifiable: {result:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(matches!(
        policy.verify(&administrator).await,
        Err(Error::Forbidden)
    ));
    let controller_namespaces: Api<Namespace> = Api::all(controller.clone());
    super::controller::verify(&pool, &scope, controller_config.clone(), &workload).await;
    let own = namespace(&scope, Uuid::new_v4());
    let own_name = own.metadata.name.clone().unwrap();
    controller_namespaces
        .create(&PostParams::default(), &own)
        .await
        .unwrap();
    let mut foreign = namespace(&scope, Uuid::new_v4());
    foreign.metadata.labels.as_mut().unwrap().insert(
        "agent-platform.io/installation".into(),
        Uuid::new_v4().to_string(),
    );
    denied(
        controller_namespaces
            .create(&PostParams::default(), &foreign)
            .await,
    );
    denied(
        controller_namespaces
            .create(
                &PostParams::default(),
                &namespace(
                    &InstallationScope {
                        id: Uuid::new_v4(),
                        ..scope.clone()
                    },
                    Uuid::new_v4(),
                ),
            )
            .await,
    );
    let binding:RoleBinding=serde_json::from_value(json!({"metadata":{"name":"instance-reconciler"},"roleRef":{"apiGroup":"rbac.authorization.k8s.io","kind":"ClusterRole","name":workload.reconciler_role},"subjects":[{"kind":"ServiceAccount","name":"hermes-controller","namespace":control}]})).unwrap();
    let own_secrets: Api<Secret> = Api::namespaced(controller.clone(), &own_name);
    denied(own_secrets.get("fixture").await);
    let controls: Api<RoleBinding> = Api::namespaced(controller.clone(), &control);
    denied(controls.create(&PostParams::default(), &binding).await);
    let own_bindings: Api<RoleBinding> = Api::namespaced(controller.clone(), &own_name);
    let mut wrong = binding.clone();
    wrong.subjects.as_mut().unwrap()[0].name = "other-controller".into();
    denied(own_bindings.create(&PostParams::default(), &wrong).await);
    own_bindings
        .create(&PostParams::default(), &binding)
        .await
        .unwrap();
    super::workloads::verify(
        &controller,
        &administrator,
        &scope,
        &workload,
        &controller_namespaces.get(&own_name).await.unwrap(),
        controller_config,
    )
    .await;
    let secret:Secret=serde_json::from_value(json!({"metadata":{"name":"fixture"},"type":"Opaque","stringData":{"value":"fixture-only"}})).unwrap();
    own_secrets
        .create(&PostParams::default(), &secret)
        .await
        .unwrap();
    own_secrets.get("fixture").await.unwrap();
    let controls: Api<Secret> = Api::namespaced(controller.clone(), &control);
    denied(controls.get("fixture").await);
    let all: Api<Secret> = Api::all(controller.clone());
    denied(all.list(&Default::default()).await);
    denied(controller_namespaces.patch(&own_name,&PatchParams::default(),&Patch::Merge(json!({"metadata":{"labels":{"agent-platform.io/installation":Uuid::new_v4().to_string()}}}))).await);
    // An additional, unrelated binding must not silently elevate this account.
    let excessive = format!("excessive-{}", scope.id.simple());
    let roles: Api<ClusterRole> = Api::all(administrator.clone());
    let bindings: Api<ClusterRoleBinding> = Api::all(administrator.clone());
    roles.create(&PostParams::default(),&serde_json::from_value(json!({"metadata":{"name":excessive},"rules":[{"apiGroups":["rbac.authorization.k8s.io"],"resources":["clusterrolebindings"],"verbs":["create"]}]})).unwrap()).await.unwrap();
    bindings.create(&PostParams::default(),&serde_json::from_value(json!({"metadata":{"name":excessive},"roleRef":{"apiGroup":"rbac.authorization.k8s.io","kind":"ClusterRole","name":excessive},"subjects":[{"kind":"ServiceAccount","name":"hermes-controller","namespace":control}]})).unwrap()).await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if matches!(policy.verify(&controller).await, Err(Error::Forbidden)) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "unexpected additional RBAC was not detected"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    bindings
        .delete(&excessive, &Default::default())
        .await
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if policy.verify(&controller).await.is_ok() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "verified scope was not restored after fixture binding removal"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // Relaxation of a deployed policy is detected, even if negative probes
    // might still fail for an unrelated RBAC reason.
    let manifest = policy
        .manifests()
        .iter()
        .find(|m| m["kind"] == "ValidatingAdmissionPolicy")
        .unwrap();
    api(&administrator, manifest)
        .patch(
            manifest["metadata"]["name"].as_str().unwrap(),
            &PatchParams::default(),
            &Patch::Merge(json!({"spec":{"failurePolicy":"Ignore"}})),
        )
        .await
        .unwrap();
    assert!(matches!(
        policy.verify(&controller).await,
        Err(Error::OwnershipConflict)
    ));
}

fn differences(actual: &Value, expected: &Value, path: &str) -> Vec<String> {
    if actual == expected {
        return vec![];
    }
    if let (Some(actual), Some(expected)) = (actual.as_object(), expected.as_object()) {
        let keys: std::collections::BTreeSet<_> = actual.keys().chain(expected.keys()).collect();
        return keys
            .into_iter()
            .flat_map(|key| {
                differences(
                    actual.get(key).unwrap_or(&Value::Null),
                    expected.get(key).unwrap_or(&Value::Null),
                    &format!("{path}.{key}"),
                )
            })
            .collect();
    }
    if let (Some(actual), Some(expected)) = (actual.as_array(), expected.as_array())
        && actual.len() == expected.len()
    {
        return actual
            .iter()
            .zip(expected)
            .enumerate()
            .flat_map(|(i, (a, e))| differences(a, e, &format!("{path}[{i}]")))
            .collect();
    }
    vec![path.to_string()]
}
