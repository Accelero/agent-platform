//! Kill the compiled controller after a real Secret create, before UID commit.
use base64::{Engine, engine::general_purpose::STANDARD};
use hermes_adapters::{postgres::PgStore, workloads::WorkloadConfig};
use hermes_application::{
    AdminPolicy, AssignmentCommand, Assignments, InstallationScope, VerifiedPrincipal,
};
use hermes_domain::{Access, Identity, TemplateVersion};
use k8s_openapi::api::{apps::v1::Deployment, core::v1::Secret};
use kube::{Api, Client, Config};
use serde_json::json;
use sqlx::{ConnectOptions, PgPool};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write,
    os::unix::{fs::OpenOptionsExt, process::ExitStatusExt},
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::process::{Child, Command};
use uuid::Uuid;

fn write_private(path: &Path, bytes: &[u8]) {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    file.write_all(bytes).unwrap();
}

fn start(binary: &Path, config: &Path) -> Child {
    Command::new(binary)
        .arg("controller")
        .arg("--config")
        .arg(config)
        // The fixture holds credentials only in private files. Even a failed
        // child cannot accidentally publish a library's source error in output.
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap()
}

pub(super) async fn verify(
    pool: &PgPool,
    scope: &InstallationScope,
    config: Config,
    workload: &WorkloadConfig,
    cluster: &str,
) {
    assert_eq!(config.cluster_url.host(), Some("127.0.0.1"));
    let binary = PathBuf::from(
        std::env::var_os("HERMES_TEST_MANAGER_BINARY")
            .expect("compiled manager from disposable fixture runner required"),
    );
    assert!(binary.is_absolute() && binary.is_file());
    let client = Client::try_from(config.clone()).unwrap();
    let store = PgStore::connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    let policy = AdminPolicy {
        issuer: scope.issuer.clone(),
        audience: "manager-api".into(),
        management_client: "manager-admin".into(),
        administrator_role: "administrator".into(),
    };
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
    let accepted = Assignments::new(store, scope.clone(), policy)
        .unwrap()
        .execute(
            &actor,
            Uuid::new_v4(),
            "process-crash-commission",
            AssignmentCommand::Create {
                identity: Identity {
                    issuer: scope.issuer.clone(),
                    subject: "owner-controller-process-fixture".into(),
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
    let namespace = hermes_domain::instance_namespace(scope.id, accepted.instance_id);
    let secrets: Api<Secret> = Api::namespaced(client.clone(), &namespace);
    let deployments: Api<Deployment> = Api::namespaced(client, &namespace);

    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("database-url");
    write_private(
        &database,
        pool.connect_options().to_url_lossy().as_str().as_bytes(),
    );
    let material_key = directory.path().join("material-key");
    let ownership_key = directory.path().join("ownership-key");
    write_private(&material_key, &[42; 32]);
    write_private(&ownership_key, &[41; 32]);
    let material_ring = directory.path().join("material-keyring.json");
    let ownership_ring = directory.path().join("ownership-keyring.json");
    for (ring, key) in [
        (&material_ring, &material_key),
        (&ownership_ring, &ownership_key),
    ] {
        write_private(
            ring,
            &serde_json::to_vec(&json!({
                "activeKeyId":"fixture","keyFiles":{"fixture":key}
            }))
            .unwrap(),
        );
    }
    let mut ca_pem = String::new();
    for der in config.root_cert.as_ref().unwrap() {
        ca_pem.push_str(&format!(
            "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n",
            STANDARD.encode(der)
        ));
    }
    let kubeconfig = directory.path().join("kubeconfig.json");
    write_private(&kubeconfig, &serde_json::to_vec(&json!({
        "apiVersion":"v1","kind":"Config","current-context":"scoped-controller",
        "clusters":[{"name":"fixture","cluster":{
            "server":config.cluster_url.to_string(),
            "certificate-authority-data":STANDARD.encode(ca_pem.as_bytes())
        }}],
        "contexts":[{"name":"scoped-controller","context":{"cluster":"fixture","user":"controller"}}],
        "users":[{"name":"controller","user":serde_json::to_value(&config.auth_info).unwrap()}]
    })).unwrap());
    let path = directory.path().join("controller.json");
    let mut process_config = json!({
        "installation":{"id":scope.id,"epoch":scope.epoch,"issuer":scope.issuer},
        "databaseUrlFile":database,
        "materialEncryptionKeyringFile":material_ring,
        "ownershipKeyringFile":ownership_ring,
        "kubernetes":{"source":"kubeconfig","file":kubeconfig,"context":"scoped-controller"},
        "clusterIdentity":cluster,"workload":workload,
        "executionUrl":"https://manager.example.org/execution"
    });
    write_private(&path, &serde_json::to_vec(&process_config).unwrap());

    // A fixture-only BEFORE trigger creates an exact crash boundary. It pauses
    // only this assignment's first Secret binding, after the API create returns.
    // Production timeout/fence checks remain enabled in the actual controller.
    let lock = (Uuid::new_v4().as_u128() as u64 & i64::MAX as u64) as i64;
    // Both interpolated values are generated typed UUID/i64 values; neither
    // accepts SQL syntax or user-provided strings.
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "CREATE FUNCTION manager.fixture_pause_secret_binding() RETURNS trigger LANGUAGE plpgsql AS $pause$ BEGIN IF OLD.kind='Secret' AND OLD.state='Reserved' AND NEW.state='Bound' AND NEW.instance_id='{}'::uuid THEN PERFORM pg_advisory_xact_lock({lock}); END IF; RETURN NEW; END $pause$; CREATE TRIGGER fixture_pause_secret_binding BEFORE UPDATE ON manager.owned_resources FOR EACH ROW EXECUTE FUNCTION manager.fixture_pause_secret_binding();",
        accepted.instance_id
    ))).execute(pool).await.unwrap();
    let mut pause = pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(lock)
        .execute(&mut *pause)
        .await
        .unwrap();
    let mut first = start(&binary, &path);
    let deadline = Instant::now() + Duration::from_secs(30);
    let original = loop {
        assert!(
            first.try_wait().unwrap().is_none(),
            "controller stopped before crash boundary"
        );
        let blocked: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_locks WHERE locktype='advisory' AND NOT granted AND classid::bigint=$1 AND objid::bigint=$2)")
            .bind(lock >> 32).bind(lock & 0xffff_ffff).fetch_one(pool).await.unwrap();
        if blocked {
            let secret = secrets.get("native-bootstrap").await.unwrap();
            let state: (String, Option<String>) = sqlx::query_as("SELECT state,uid FROM manager.owned_resources WHERE instance_id=$1 AND kind='Secret'")
                .bind(accepted.instance_id).fetch_one(pool).await.unwrap();
            assert_eq!(state, ("Reserved".into(), None));
            break secret;
        }
        assert!(
            Instant::now() < deadline,
            "controller did not reach Secret UID commit boundary"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    };
    first.start_kill().unwrap();
    let killed = tokio::time::timeout(Duration::from_secs(3), first.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(killed.signal(), Some(9));
    pause.rollback().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), sqlx::raw_sql(
        "DROP TRIGGER fixture_pause_secret_binding ON manager.owned_resources; DROP FUNCTION manager.fixture_pause_secret_binding()"
    ).execute(pool)).await.unwrap().unwrap();
    let uid: Option<String> = sqlx::query_scalar(
        "SELECT uid FROM manager.owned_resources WHERE instance_id=$1 AND kind='Secret'",
    )
    .bind(accepted.instance_id)
    .fetch_one(pool)
    .await
    .unwrap();
    assert!(
        uid.is_none(),
        "dead process committed an unacknowledged UID"
    );
    sqlx::query("UPDATE manager.operations SET lease_until=clock_timestamp()-interval '1 second',next_attempt_at=clock_timestamp()-interval '1 second' WHERE instance_id=$1")
        .bind(accepted.instance_id).execute(pool).await.unwrap();
    // The first process froze the entire set before its first child call. A new
    // public origin cannot rewrite either the recovered Secret or later Pod env.
    process_config["workload"]["publicOrigin"] = json!("https://changed-process.example.org");
    std::fs::write(&path, serde_json::to_vec(&process_config).unwrap()).unwrap();
    let mut restarted = start(&binary, &path);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        assert!(
            restarted.try_wait().unwrap().is_none(),
            "reopened controller stopped"
        );
        let state: String =
            sqlx::query_scalar("SELECT state FROM manager.operations WHERE instance_id=$1")
                .bind(accepted.instance_id)
                .fetch_one(pool)
                .await
                .unwrap();
        if state == "Waiting" {
            break;
        }
        assert!(
            Instant::now() < deadline && state != "Blocked",
            "reopened commissioning failed: {state}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let recovered = secrets.get("native-bootstrap").await.unwrap();
    assert_eq!(recovered.metadata.uid, original.metadata.uid);
    assert!(
        recovered.data == original.data,
        "process restart changed owner credentials"
    );
    let bound: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM manager.owned_resources WHERE instance_id=$1 AND state='Bound'",
    )
    .bind(accepted.instance_id)
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(bound, 14);
    let routes: i64 =
        sqlx::query_scalar("SELECT count(*) FROM manager.instance_routes WHERE instance_id=$1")
            .bind(accepted.instance_id)
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(routes, 0);
    let deployment = deployments.get("hermes").await.unwrap();
    let environment = deployment
        .spec
        .unwrap()
        .template
        .spec
        .unwrap()
        .containers
        .remove(0)
        .env
        .unwrap();
    assert_eq!(
        environment
            .iter()
            .find(|e| e.name == "HERMES_DASHBOARD_PUBLIC_URL")
            .unwrap()
            .value
            .as_deref(),
        Some(workload.public_origin.as_str())
    );

    let signal = tokio::time::timeout(
        Duration::from_secs(3),
        Command::new("/bin/kill")
            .arg("-TERM")
            .arg(restarted.id().unwrap().to_string())
            .status(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(signal.success());
    let stopped = tokio::time::timeout(Duration::from_secs(3), restarted.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(
        stopped.success(),
        "controller did not shut down cleanly after SIGTERM"
    );
}
