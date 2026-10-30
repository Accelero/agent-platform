//! The disposable Kubernetes fixture shared by the cluster test targets: the
//! rendered RBAC and admission policies installed by the administrator, and a
//! client that acts as the controller's real ServiceAccount.

use std::collections::BTreeMap;
use std::time::Duration;

use hermes_core::app::controller::{DesiredInstance, InstanceMaterials, Plain};
use hermes_core::app::testing::settings;
use hermes_core::domain::assignments::Access;
use hermes_core::domain::installation::Installation;
use hermes_core::domain::instances::Workload;
use hermes_core::domain::materials::MaterialPurpose;
use hermes_core::domain::storage::{SourceKind, StorageSource, StorageSources};
use hermes_kube::{Client, KubeCluster};
use hermes_render::{Peers, RenderConfig, admission};
use k8s_openapi::api::authentication::v1::{TokenRequest, TokenRequestSpec};
use k8s_openapi::api::core::v1::{Namespace, ServiceAccount};
use kube::api::{Api, PostParams};
use kube::config::{KubeConfigOptions, Kubeconfig};
use serde_json::json;
use uuid::Uuid;

pub const CA: &str = "-----BEGIN CERTIFICATE-----\nMIIBfixture\n-----END CERTIFICATE-----\n";

pub struct Fixture {
    pub admin: Client,
    pub controller: Client,
    /// The controller's credentials, for probes that vary the request itself.
    pub controller_config: kube::Config,
    pub config: RenderConfig,
}

pub async fn config_from_env() -> kube::Config {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let path = std::env::var("HERMES_TEST_KUBECONFIG").expect("disposable Kubernetes fixture only");
    let config = kube::Config::from_custom_kubeconfig(
        Kubeconfig::read_from(path).unwrap(),
        &KubeConfigOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        config.cluster_url.host(),
        Some("127.0.0.1"),
        "only a local disposable server"
    );
    config
}

impl Fixture {
    pub async fn new() -> Self {
        Self::with(Installation {
            id: Uuid::new_v4(),
            issuer: settings().installation.issuer.clone(),
        })
        .await
    }

    pub async fn with(installation: Installation) -> Self {
        let config = config_from_env().await;
        let admin = hermes_kube::client(config.clone()).unwrap();
        let short = &installation.id.simple().to_string()[..12];
        let control = format!("hermes-test-{}", &Uuid::new_v4().simple().to_string()[..12]);
        let namespaces: Api<Namespace> = Api::all(admin.clone());
        namespaces
            .create(
                &PostParams::default(),
                &serde_json::from_value(json!({"metadata": {"name": control}})).unwrap(),
            )
            .await
            .unwrap();
        let accounts: Api<ServiceAccount> = Api::namespaced(admin.clone(), &control);
        accounts
            .create(
                &PostParams::default(),
                &serde_json::from_value(json!({"metadata": {"name": "hermes-controller"}, "automountServiceAccountToken": false}))
                    .unwrap(),
            )
            .await
            .unwrap();
        let fixture_settings = settings();
        let labels =
            |c: &str| BTreeMap::from([("app.kubernetes.io/component".to_owned(), c.to_owned())]);
        // Dynamic volumes use k3s's own provisioner, so claims can bind.
        let sources = StorageSources::new(
            fixture_settings
                .sources
                .iter()
                .map(|source| match &source.kind {
                    SourceKind::StorageClass { .. } => StorageSource {
                        id: source.id.clone(),
                        kind: SourceKind::StorageClass {
                            name: "local-path".into(),
                        },
                    },
                    _ => source.clone(),
                })
                .collect(),
        )
        .unwrap();
        let render = RenderConfig {
            installation,
            sources,
            data_identity: fixture_settings.data_identity,
            control_namespace: control.clone(),
            controller_service_account: "hermes-controller".into(),
            instance_role: format!("hermes-manager-{short}-{}", &control[12..]),
            peers: Peers {
                gateway: labels("hermes-gateway"),
                controller: labels("hermes-controller"),
                api: labels("hermes-manager-api"),
            },
            execution_url: "https://hermes-manager-api.example.svc:8443/execution".into(),
            execution_port: 8443,
            execution_ca_pem: CA.into(),
            oidc_ca_pem: None,
            native_client_id: "hermes-native".into(),
            public_origin: "https://hermes.example.org/".into(),
            service_egress: Vec::new(),
            public_https: false,
        };
        install(&admin, &admission::rbac(&render).unwrap()).await;
        install(&admin, &admission::policies(&render).unwrap()).await;
        let token = accounts
            .create_token_request(
                "hermes-controller",
                &PostParams::default(),
                &TokenRequest {
                    spec: Some(TokenRequestSpec {
                        expiration_seconds: Some(900),
                        ..TokenRequestSpec::default()
                    }),
                    ..TokenRequest::default()
                },
            )
            .await
            .unwrap()
            .status
            .and_then(|status| status.token)
            .expect("token");
        let mut controller_config = config;
        controller_config.auth_info = Default::default();
        controller_config.auth_info.token = Some(token.into());
        let controller = hermes_kube::client(controller_config.clone()).unwrap();
        // Admission policies take effect shortly after creation.
        tokio::time::sleep(Duration::from_secs(2)).await;
        Self {
            admin,
            controller,
            controller_config,
            config: render,
        }
    }

    pub fn cluster(&self) -> KubeCluster {
        KubeCluster::new(self.controller.clone(), self.config.clone()).unwrap()
    }

    pub fn installation(&self) -> &Installation {
        &self.config.installation
    }
}

/// Installs chart-owned objects as the administrator, reporting the server's
/// reason on failure (these objects hold no credentials).
pub async fn install(admin: &Client, objects: &[hermes_render::Object]) {
    for object in objects {
        let (group, version) = object
            .api_version
            .split_once('/')
            .unwrap_or(("", object.api_version));
        let plural = match object.kind {
            "ClusterRole" => "clusterroles",
            "ClusterRoleBinding" => "clusterrolebindings",
            "Role" => "roles",
            "RoleBinding" => "rolebindings",
            "ValidatingAdmissionPolicy" => "validatingadmissionpolicies",
            "ValidatingAdmissionPolicyBinding" => "validatingadmissionpolicybindings",
            other => panic!("unexpected {other}"),
        };
        let resource = kube::core::ApiResource::from_gvk_with_plural(
            &kube::core::GroupVersionKind::gvk(group, version, object.kind),
            plural,
        );
        let api: Api<kube::core::DynamicObject> = match &object.namespace {
            Some(ns) => Api::namespaced_with(admin.clone(), ns, &resource),
            None => Api::all_with(admin.clone(), &resource),
        };
        if let Err(error) = api
            .patch(
                &object.name,
                &kube::api::PatchParams::apply("chart").force(),
                &kube::api::Patch::Apply(&object.body),
            )
            .await
        {
            panic!("installing {object:?}: {error}");
        }
    }
}

/// Applies the rendered set as the controller object by object, so a refusal
/// names the object and the server's reason (fixture data only).
pub async fn diagnose(fixture: &Fixture, desired: &DesiredInstance, materials: &InstanceMaterials) {
    let objects =
        hermes_render::instance::render(&fixture.config, desired, materials, &[], Workload::Run)
            .unwrap();
    for object in &objects {
        let one = std::slice::from_ref(object);
        if hermes_kube::apply_objects(&fixture.controller, one)
            .await
            .is_err()
        {
            let (group, version) = object
                .api_version
                .split_once('/')
                .unwrap_or(("", object.api_version));
            let plural = format!("{}s", object.kind.to_lowercase()).replace("policys", "policies");
            let resource = kube::core::ApiResource::from_gvk_with_plural(
                &kube::core::GroupVersionKind::gvk(group, version, object.kind),
                &plural,
            );
            let api: Api<kube::core::DynamicObject> = match &object.namespace {
                Some(ns) => Api::namespaced_with(fixture.controller.clone(), ns, &resource),
                None => Api::all_with(fixture.controller.clone(), &resource),
            };
            let error = api
                .patch(
                    &object.name,
                    &kube::api::PatchParams::apply("hermes-manager").force(),
                    &kube::api::Patch::Apply(&object.body),
                )
                .await
                .unwrap_err();
            panic!("{object:?} refused: {error}");
        }
    }
}

pub fn materials() -> InstanceMaterials {
    InstanceMaterials {
        initialization_id: Uuid::new_v4(),
        plain: MaterialPurpose::ALL
            .into_iter()
            .map(|p| {
                (
                    p,
                    Plain::new(format!("fixture-{}", p.as_str()).into_bytes()),
                )
            })
            .collect(),
        public: BTreeMap::from([
            (
                MaterialPurpose::SshClient,
                "ssh-ed25519 AAAAclient".to_owned(),
            ),
            (
                MaterialPurpose::WorkerHost,
                "ssh-ed25519 AAAAhost".to_owned(),
            ),
        ]),
    }
}

pub fn desired(fixture: &Fixture) -> DesiredInstance {
    let instance = Uuid::new_v4();
    let definition = serde_json::from_value(json!({
        "schemaVersion": 2,
        "image": format!("ghcr.io/accelero/agent-platform/hermes@sha256:{}", "a".repeat(64)),
        "resources": {"cpuMillis": 100, "memoryMib": 128},
        "home": {"source": "nvme", "sizeGib": 1},
        "mounts": {"workspace": {"mountPath": "/workspace"}, "shared": {"mountPath": "/shared"}},
    }))
    .unwrap();
    DesiredInstance {
        instance_id: instance,
        assignment_id: Uuid::new_v4(),
        identity: hermes_core::app::testing::identity("bob"),
        namespace: fixture.installation().instance_namespace(instance),
        namespace_uid: None,
        access: Access::Enabled,
        desired_generation: 1,
        execution_generation: 1,
        template: hermes_core::domain::catalog::VersionRef {
            id: "personal-default".into(),
            version: 1,
        },
        admin_config: Default::default(),
        definition,
        volumes: serde_json::from_value(json!({
            "workspace": {"source": "nvme", "sizeGib": 2},
            "shared": {"source": "host-data", "path": "bob/shared"},
        }))
        .unwrap(),
        terminal: Default::default(),
        secrets: Vec::new(),
        backends: Default::default(),
    }
}
