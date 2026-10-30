//! The compiled controller against a disposable Kubernetes API server and
//! PostgreSQL: `initialize`, leadership, commissioning as the real
//! ServiceAccount, and an abrupt abort at every controller fault point
//! without regenerating material or recording status from a dead term.
//!
//! Needs `HERMES_TEST_KUBECONFIG` and a `fault-injection` build; run through
//! `tests/personal_hermes/kubernetes.py --workspace next`.

#![cfg(feature = "fault-injection")]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use hermes_core::app::controller::InstanceStore;
use hermes_core::app::testing::contracts::{bob_assignment, set_access};
use hermes_core::app::testing::{installation, settings};
use hermes_core::domain::assignments::Access;
use hermes_kube::Client;
use hermes_render::{Peers, RenderConfig, admission};
use hermes_store::PgStore;
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::authentication::v1::{TokenRequest, TokenRequestSpec};
use k8s_openapi::api::core::v1::{Namespace, ServiceAccount};
use kube::api::{Api, DeleteParams, Patch, PatchParams, PostParams};
use kube::config::{KubeConfigOptions, Kubeconfig};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

const BINARY: &str = env!("CARGO_BIN_EXE_hermes-manager");
const CA: &str = "-----BEGIN CERTIFICATE-----\nMIIBfixture\n-----END CERTIFICATE-----\n";

fn database_url(pool: &PgPool) -> String {
    let mut url = url::Url::parse(&std::env::var("DATABASE_URL").expect("DATABASE_URL")).unwrap();
    url.set_path(
        pool.connect_options()
            .get_database()
            .expect("database name"),
    );
    url.to_string()
}

/// The one context of the controller's kubeconfig.
const CONTEXT: &str = "controller";

struct Fixture {
    directory: tempfile::TempDir,
    admin: Client,
    control: String,
}

impl Fixture {
    async fn new(pool: &PgPool) -> Self {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let path =
            std::env::var("HERMES_TEST_KUBECONFIG").expect("disposable Kubernetes fixture only");
        let kubeconfig = Kubeconfig::read_from(&path).unwrap();
        let config =
            kube::Config::from_custom_kubeconfig(kubeconfig.clone(), &KubeConfigOptions::default())
                .await
                .unwrap();
        assert_eq!(
            config.cluster_url.host(),
            Some("127.0.0.1"),
            "only a local disposable server"
        );
        let admin = hermes_kube::client(config).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let control = format!(
            "hermes-process-{}",
            &Uuid::new_v4().simple().to_string()[..10]
        );
        Api::<Namespace>::all(admin.clone())
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
                &serde_json::from_value(json!({"metadata": {"name": "hermes-controller"}, "automountServiceAccountToken": false})).unwrap(),
            )
            .await
            .unwrap();
        let fixture = Self {
            directory,
            admin,
            control,
        };
        for object in admission::rbac(&fixture.render())
            .unwrap()
            .iter()
            .chain(&admission::policies(&fixture.render()).unwrap())
        {
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
                _ => "validatingadmissionpolicybindings",
            };
            let resource = kube::core::ApiResource::from_gvk_with_plural(
                &kube::core::GroupVersionKind::gvk(group, version, object.kind),
                plural,
            );
            let api: Api<kube::core::DynamicObject> = match &object.namespace {
                Some(ns) => Api::namespaced_with(fixture.admin.clone(), ns, &resource),
                None => Api::all_with(fixture.admin.clone(), &resource),
            };
            api.patch(
                &object.name,
                &PatchParams::apply("chart").force(),
                &Patch::Apply(&object.body),
            )
            .await
            .unwrap();
        }
        // The controller's own kubeconfig: a short-lived token of its account.
        let token = accounts
            .create_token_request(
                "hermes-controller",
                &PostParams::default(),
                &TokenRequest {
                    spec: Some(TokenRequestSpec {
                        expiration_seconds: Some(1800),
                        ..TokenRequestSpec::default()
                    }),
                    ..TokenRequest::default()
                },
            )
            .await
            .unwrap()
            .status
            .and_then(|s| s.token)
            .unwrap();
        let mut controller = serde_json::to_value(&kubeconfig).unwrap();
        controller["users"] = json!([{"name": "controller", "user": {"token": token}}]);
        let contexts = controller["contexts"].as_array_mut().unwrap();
        assert_eq!(contexts.len(), 1);
        contexts[0]["name"] = json!(CONTEXT);
        contexts[0]["context"]["user"] = json!("controller");
        std::fs::write(
            fixture.path("kubeconfig"),
            serde_json::to_vec(&controller).unwrap(),
        )
        .unwrap();
        std::fs::write(fixture.path("database-url"), database_url(pool)).unwrap();
        std::fs::write(fixture.path("execution-ca.crt"), CA).unwrap();
        std::fs::write(fixture.path("sealing.key"), [9u8; 32]).unwrap();
        std::fs::write(
            fixture.path("config.json"),
            serde_json::to_vec(&fixture.config()).unwrap(),
        )
        .unwrap();
        tokio::time::sleep(Duration::from_secs(2)).await;
        fixture
    }

    fn path(&self, name: &str) -> PathBuf {
        self.directory.path().join(name)
    }

    fn peers() -> serde_json::Value {
        json!({
            "gateway": {"app.kubernetes.io/component": "hermes-gateway"},
            "controller": {"app.kubernetes.io/component": "hermes-controller"},
            "api": {"app.kubernetes.io/component": "hermes-manager-api"},
        })
    }

    fn render(&self) -> RenderConfig {
        let fixture = settings();
        let peers: Peers = serde_json::from_value(Self::peers()).unwrap();
        RenderConfig {
            installation: installation(),
            sources: fixture.sources.clone(),
            data_identity: fixture.data_identity,
            control_namespace: self.control.clone(),
            controller_service_account: "hermes-controller".into(),
            instance_role: format!("{}-instance", self.control),
            peers,
            execution_url: "https://hermes-manager-api.example.svc:8443/execution".into(),
            execution_port: 8443,
            execution_ca_pem: CA.into(),
            oidc_ca_pem: None,
            native_client_id: "hermes-native".into(),
            public_origin: "https://hermes.example.org/".into(),
            service_egress: Vec::new(),
            public_https: false,
        }
    }

    fn config(&self) -> serde_json::Value {
        let render = self.render();
        let sources: Vec<_> = settings()
            .sources
            .iter()
            .map(|s| serde_json::to_value(s).unwrap())
            .collect();
        json!({
            "installation": {"id": render.installation.id, "issuer": render.installation.issuer},
            "databaseUrlFile": self.path("database-url"),
            "storageSources": sources,
            "controller": {
                "namespace": self.control,
                "serviceAccount": "hermes-controller",
                "instanceRole": render.instance_role,
                "kubeconfig": {"file": self.path("kubeconfig"), "context": CONTEXT},
                "secretSealingPrivateKeyFile": self.path("sealing.key"),
                "peers": Self::peers(),
                "executionUrl": render.execution_url,
                "executionPort": 8443,
                "executionCaFile": self.path("execution-ca.crt"),
                "nativeClientId": "hermes-native",
                "publicOrigin": "https://hermes.example.org/",
                "resyncSeconds": 5,
            },
        })
    }

    fn run(&self, role: &str) -> std::process::Output {
        Command::new(BINARY)
            .args([role, "--config"])
            .arg(self.path("config.json"))
            .output()
            .unwrap()
    }

    fn controller(&self, fault: Option<&str>) -> Child {
        let mut command = Command::new(BINARY);
        command
            .args(["controller", "--config"])
            .arg(self.path("config.json"))
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(point) = fault {
            command.env("HERMES_FAULT_POINT", point);
        }
        command.spawn().unwrap()
    }
}

/// Waits for an aborting controller; the lease it held expires on its own.
fn wait_exit(child: &mut Child, limit: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < limit {
        if child.try_wait().unwrap().is_some() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    let _ = child.wait();
    false
}

async fn material(store: &PgStore, instance: Uuid) -> BTreeMap<String, Vec<u8>> {
    store
        .materials(instance)
        .await
        .unwrap()
        .into_iter()
        .map(|row| (row.purpose.as_str().to_owned(), row.sealed))
        .collect()
}

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
#[ignore = "needs HERMES_TEST_KUBECONFIG from the disposable Kubernetes fixture"]
async fn the_controller_process_commissions_and_survives_aborts_at_every_point(pool: PgPool) {
    let fixture = Fixture::new(&pool).await;
    let initialized = fixture.run("initialize");
    assert!(
        initialized.status.success(),
        "{}",
        String::from_utf8_lossy(&initialized.stderr)
    );
    let store = PgStore::connect(&database_url(&pool)).await.unwrap();
    let receipt = bob_assignment(&store).await;

    let mut sealed = None;
    for point in [
        "controller_after_material_insert",
        "controller_before_apply",
        "controller_after_apply",
        "controller_before_status",
    ] {
        let mut child = fixture.controller(Some(point));
        // Up to one lease duration for the previous term to expire.
        assert!(
            wait_exit(&mut child, Duration::from_secs(60)),
            "{point} never fired"
        );
        assert!(
            !child.wait().unwrap().success(),
            "{point} aborts the process"
        );
        let rows = material(&store, receipt.instance_id).await;
        assert!(!rows.is_empty(), "{point}");
        if rows.len() == 5 {
            assert_eq!(
                sealed.get_or_insert_with(|| rows.clone()),
                &rows,
                "{point}: never regenerated"
            );
        }
    }
    let mut healthy = fixture.controller(None);
    let deadline = Instant::now() + Duration::from_secs(60);
    let desired = loop {
        let desired = store
            .desired_instance(receipt.instance_id)
            .await
            .unwrap()
            .unwrap();
        if desired.namespace_uid.is_some() || Instant::now() > deadline {
            break desired;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    let _ = healthy.kill();
    let _ = healthy.wait();
    assert!(
        desired.namespace_uid.is_some(),
        "the namespace was created and bound"
    );
    let namespace = Api::<Namespace>::all(fixture.admin.clone())
        .get(&desired.namespace)
        .await
        .unwrap();
    assert_eq!(namespace.metadata.uid, desired.namespace_uid);
    assert_eq!(
        Some(material(&store, receipt.instance_id).await),
        sealed,
        "one set of material across every abort"
    );
    let phase: String = sqlx::query_scalar("SELECT phase FROM manager.instances WHERE id = $1")
        .bind(receipt.instance_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        phase, "provisioning",
        "no native Hermes runs in this fixture"
    );

    // Suspended: the controller deletes exactly the Deployment it saw and is
    // aborted right after; the next term finds it gone and retains the data.
    set_access(&store, &receipt, Access::Suspended).await;
    let mut child = fixture.controller(Some("controller_after_delete"));
    assert!(
        wait_exit(&mut child, Duration::from_secs(120)),
        "controller_after_delete never fired"
    );
    assert!(
        !child.wait().unwrap().success(),
        "the delete aborts the process"
    );
    let deployments = Api::<Deployment>::namespaced(fixture.admin.clone(), &desired.namespace);
    assert!(deployments.get_opt("hermes").await.unwrap().is_none());
    let mut healthy = fixture.controller(None);
    let deadline = Instant::now() + Duration::from_secs(90);
    let retained = loop {
        let phase: String = sqlx::query_scalar("SELECT phase FROM manager.instances WHERE id = $1")
            .bind(receipt.instance_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        if phase == "retained" || Instant::now() > deadline {
            break phase;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    let _ = healthy.kill();
    let _ = healthy.wait();
    assert_eq!(retained, "retained");
    assert_eq!(
        Some(material(&store, receipt.instance_id).await),
        sealed,
        "material kept across the stop"
    );

    // A second initialize is refused; the registry keeps its cluster.
    assert!(!fixture.run("initialize").status.success());
    let rebind = fixture.run("rebind-cluster");
    assert!(rebind.status.success());
    assert!(String::from_utf8_lossy(&rebind.stdout).contains("unchanged"));
}

/// A controller started with a sealing key other than the one the stored
/// material was sealed under must stop before it reads or writes anything: a
/// controller that carried on would seal material for new instances that the
/// right key could never open.
#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
#[ignore = "needs HERMES_TEST_KUBECONFIG from the disposable Kubernetes fixture"]
async fn a_controller_with_a_different_sealing_key_exits_before_acting(pool: PgPool) {
    let fixture = Fixture::new(&pool).await;
    let initialized = fixture.run("initialize");
    assert!(
        initialized.status.success(),
        "{}",
        String::from_utf8_lossy(&initialized.stderr)
    );
    let store = PgStore::connect(&database_url(&pool)).await.unwrap();
    let receipt = bob_assignment(&store).await;

    // The right key seals the instance's material.
    let mut right = fixture.controller(None);
    let deadline = Instant::now() + Duration::from_secs(60);
    let sealed = loop {
        let rows = material(&store, receipt.instance_id).await;
        if rows.len() == 5 || Instant::now() > deadline {
            break rows;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    let _ = right.kill();
    let _ = right.wait();
    assert_eq!(sealed.len(), 5, "the controller sealed the material");

    // The same configuration with another key file.
    std::fs::write(fixture.path("sealing.key"), [7u8; 32]).unwrap();
    let mut wrong = Command::new(BINARY)
        .args(["controller", "--config"])
        .arg(fixture.path("config.json"))
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    assert!(
        wait_exit(&mut wrong, Duration::from_secs(30)),
        "a controller with the wrong key kept running"
    );
    let output = wrong.wait_with_output().unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("sealing_key_mismatch"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        material(&store, receipt.instance_id).await,
        sealed,
        "the material is untouched"
    );
}

/// A database restored from another cluster is bound to that cluster. Until an
/// operator rebinds it, the controller refuses to act, and `rebind-cluster`
/// refuses while the cluster still holds a namespace of the installation; once
/// the namespaces are gone it records the new identity, and the controller
/// recreates the instance on the same material.
#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
#[ignore = "needs HERMES_TEST_KUBECONFIG from the disposable Kubernetes fixture"]
async fn a_restored_registry_is_rebound_only_to_a_cluster_without_its_namespaces(pool: PgPool) {
    let fixture = Fixture::new(&pool).await;
    assert!(fixture.run("initialize").status.success());
    let store = PgStore::connect(&database_url(&pool)).await.unwrap();
    let receipt = bob_assignment(&store).await;

    // Commissioned: the instance's namespace exists and is bound.
    let mut controller = fixture.controller(None);
    let deadline = Instant::now() + Duration::from_secs(60);
    let bound = loop {
        let desired = store
            .desired_instance(receipt.instance_id)
            .await
            .unwrap()
            .unwrap();
        if desired.namespace_uid.is_some() || Instant::now() > deadline {
            break desired;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    let _ = controller.kill();
    let _ = controller.wait();
    let uid = bound
        .namespace_uid
        .clone()
        .expect("the namespace was bound");
    let sealed = material(&store, receipt.instance_id).await;
    assert_eq!(sealed.len(), 5);
    let own_cluster = store.cluster_uid().await.unwrap();

    // The registry now claims to come from another cluster.
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SELECT set_config('manager.rebind', 'on', true)")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("UPDATE manager.installation SET cluster_uid = 'restored-from-elsewhere'")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    // The controller refuses to act on a registry bound to another cluster.
    let mut refused = Command::new(BINARY)
        .args(["controller", "--config"])
        .arg(fixture.path("config.json"))
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    assert!(
        wait_exit(&mut refused, Duration::from_secs(30)),
        "it kept running"
    );
    let output = refused.wait_with_output().unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("cluster_changed"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Rebinding is refused while this cluster holds the installation's
    // namespace, and nothing is cleared.
    let blocked = fixture.run("rebind-cluster");
    assert!(!blocked.status.success());
    assert!(
        String::from_utf8_lossy(&blocked.stderr).contains("namespaces_exist"),
        "{}",
        String::from_utf8_lossy(&blocked.stderr)
    );
    assert_eq!(
        store.cluster_uid().await.unwrap(),
        "restored-from-elsewhere"
    );
    let after = store
        .desired_instance(receipt.instance_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.namespace_uid.as_deref(), Some(uid.as_str()));

    // A new cluster has no such namespace. This one is made to look like it by
    // lifting the binding that keeps everyone but the controller out of
    // instance namespaces, as a cluster operator could, and deleting it.
    let bindings: Api<kube::core::DynamicObject> = Api::all_with(
        fixture.admin.clone(),
        &kube::core::ApiResource::from_gvk_with_plural(
            &kube::core::GroupVersionKind::gvk(
                "admissionregistration.k8s.io",
                "v1",
                "ValidatingAdmissionPolicyBinding",
            ),
            "validatingadmissionpolicybindings",
        ),
    );
    for object in admission::policies(&fixture.render()).unwrap() {
        if object.kind == "ValidatingAdmissionPolicyBinding"
            && object.name.ends_with("owned-writes")
        {
            bindings
                .delete(&object.name, &DeleteParams::default())
                .await
                .unwrap();
        }
    }
    tokio::time::sleep(Duration::from_secs(5)).await;
    // Every namespace of the installation goes, including those earlier cases
    // left in this shared cluster.
    let namespaces = Api::<Namespace>::all(fixture.admin.clone());
    let owned = hermes_kube::owned_namespaces(&fixture.admin, installation().id)
        .await
        .unwrap();
    assert!(owned.contains(&bound.namespace));
    for name in &owned {
        namespaces
            .delete(name, &DeleteParams::default())
            .await
            .unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(180);
    while !hermes_kube::owned_namespaces(&fixture.admin, installation().id)
        .await
        .unwrap()
        .is_empty()
    {
        assert!(Instant::now() < deadline, "the namespaces never went");
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    let rebound = fixture.run("rebind-cluster");
    assert!(
        rebound.status.success(),
        "{}",
        String::from_utf8_lossy(&rebound.stderr)
    );
    assert_ne!(
        store.cluster_uid().await.unwrap(),
        "restored-from-elsewhere"
    );
    assert_eq!(store.cluster_uid().await.unwrap(), own_cluster);
    assert!(
        store
            .desired_instance(receipt.instance_id)
            .await
            .unwrap()
            .unwrap()
            .namespace_uid
            .is_none(),
        "the old binding was cleared"
    );

    // The controller recreates the namespace on the material it already had.
    let mut controller = fixture.controller(None);
    let deadline = Instant::now() + Duration::from_secs(90);
    let recreated = loop {
        let desired = store
            .desired_instance(receipt.instance_id)
            .await
            .unwrap()
            .unwrap();
        if desired.namespace_uid.is_some() || Instant::now() > deadline {
            break desired;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    let _ = controller.kill();
    let _ = controller.wait();
    assert!(recreated.namespace_uid.is_some(), "never recreated");
    assert_ne!(recreated.namespace_uid, Some(uid), "a new namespace");
    assert_eq!(
        material(&store, receipt.instance_id).await,
        sealed,
        "the instance keeps its material"
    );
}
