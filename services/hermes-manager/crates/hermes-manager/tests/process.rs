//! The compiled binary's roles, configuration gates and abrupt restarts.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::Duration;

use hermes_core::domain::installation::Installation;
use hermes_store::PgStore;
use hermes_testkit::Secrets;
use hermes_testkit::provider::FakeProvider;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

const BINARY: &str = env!("CARGO_BIN_EXE_hermes-manager");

struct Fixture {
    directory: tempfile::TempDir,
    secrets: Secrets,
    provider: FakeProvider,
    installation: Installation,
    database_url: String,
}

/// The `DATABASE_URL` server, pointed at the test's own database.
fn database_url(pool: &PgPool) -> String {
    let mut url = url::Url::parse(&std::env::var("DATABASE_URL").expect("DATABASE_URL")).unwrap();
    url.set_path(
        pool.connect_options()
            .get_database()
            .expect("database name"),
    );
    url.to_string()
}

impl Fixture {
    async fn new(pool: &PgPool) -> Self {
        let secrets = Secrets::new().await;
        let provider = FakeProvider::start(&secrets, "manager-admin", "manager-api").await;
        let directory = tempfile::tempdir().unwrap();
        let database_url = database_url(pool);
        std::fs::write(directory.path().join("database-url"), &database_url).unwrap();
        std::fs::write(directory.path().join("sealing.pub"), [5u8; 32]).unwrap();
        let installation = Installation {
            id: Uuid::new_v4(),
            issuer: provider.issuer.clone(),
        };
        Self {
            directory,
            secrets,
            provider,
            installation,
            database_url,
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.directory.path().join(name)
    }

    fn config(&self, listen: &str) -> Value {
        json!({
            "installation": {"id": self.installation.id, "issuer": self.installation.issuer},
            "databaseUrlFile": self.path("database-url"),
            "oidcCaFile": self.secrets.path("ca.crt"),
            "storageSources": [{"id": "nvme", "storageClass": {"name": "local-nvme"}}],
            "administration": {
                "listen": listen,
                "tls": {"certificateFile": self.secrets.path("server.crt"), "privateKeyFile": self.secrets.path("server.key")},
                "tokens": {"jwksUrl": self.provider.jwks_url(), "audience": "manager-api", "authorizedClients": ["manager-admin"]},
                "secretSealingPublicKeyFile": self.path("sealing.pub"),
            },
        })
    }

    fn write_config(&self, config: &Value) -> PathBuf {
        let path = self.path("config.json");
        std::fs::write(&path, serde_json::to_vec(config).unwrap()).unwrap();
        path
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(BINARY)
            .args(args)
            .output()
            .expect("binary runs")
    }

    fn identity_file(&self, subject: &str) -> PathBuf {
        let path = self.path("identity.json");
        std::fs::write(
            &path,
            json!({"issuer": self.installation.issuer, "subject": subject}).to_string(),
        )
        .unwrap();
        path
    }

    async fn initialize(&self) {
        let store = PgStore::connect(&self.database_url).await.unwrap();
        store
            .initialize(&self.installation, "fixture-cluster")
            .await
            .unwrap();
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

struct Api(Child);

impl Drop for Api {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start_api(config: &Path) -> Api {
    start_api_with(config, None)
}

/// With `fault`, a `fault-injection` build aborts at that point.
fn start_api_with(config: &Path, fault: Option<&str>) -> Api {
    let mut command = Command::new(BINARY);
    command
        .args(["api", "--config"])
        .arg(config)
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if let Some(point) = fault {
        command.env("HERMES_FAULT_POINT", point);
    }
    Api(command.spawn().expect("api starts"))
}

async fn wait_healthy(client: &reqwest::Client, url: &str) {
    for _ in 0..100 {
        if let Ok(response) = client.get(format!("{url}/healthz")).send().await
            && response.status() == 200
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("api did not become healthy");
}

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn check_requires_an_initialized_registry_of_this_installation(pool: PgPool) {
    let fixture = Fixture::new(&pool).await;
    let config = fixture.write_config(&fixture.config("127.0.0.1:0"));
    let config = config.to_str().unwrap();
    let before = fixture.run(&["check", "--config", config]);
    assert!(!before.status.success());
    assert_eq!(stderr(&before), "hermes-manager: registry_unavailable\n");
    fixture.initialize().await;
    assert!(fixture.run(&["check", "--config", config]).status.success());
    assert!(
        fixture
            .run(&["migrate", "--config", config])
            .status
            .success(),
        "migrate is idempotent"
    );

    let mut foreign = fixture.config("127.0.0.1:0");
    foreign["installation"]["id"] = json!(Uuid::new_v4());
    let foreign = fixture.write_config(&foreign);
    let refused = fixture.run(&["check", "--config", foreign.to_str().unwrap()]);
    assert_eq!(stderr(&refused), "hermes-manager: registry_unavailable\n");
}

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn grant_administrator_bootstraps_once_and_recovers_only_explicitly(pool: PgPool) {
    let fixture = Fixture::new(&pool).await;
    fixture.initialize().await;
    let config = fixture.write_config(&fixture.config("127.0.0.1:0"));
    let (config, alice) = (
        config.to_str().unwrap().to_owned(),
        fixture.identity_file("alice-subject"),
    );
    let alice = alice.to_str().unwrap();
    let first = fixture.run(&[
        "grant-administrator",
        "--config",
        &config,
        "--identity-file",
        alice,
    ]);
    assert!(first.status.success(), "{}", stderr(&first));
    let printed = format!(
        "{}{}",
        String::from_utf8_lossy(&first.stdout),
        stderr(&first)
    );
    assert!(
        !printed.contains("alice-subject"),
        "identity must not be printed"
    );

    let bob = fixture.identity_file("bob-subject");
    let bob = bob.to_str().unwrap();
    let second = fixture.run(&[
        "grant-administrator",
        "--config",
        &config,
        "--identity-file",
        bob,
    ]);
    assert_eq!(stderr(&second), "hermes-manager: administrators_exist\n");
    let recovered = fixture.run(&[
        "grant-administrator",
        "--config",
        &config,
        "--identity-file",
        bob,
        "--recover",
    ]);
    assert!(recovered.status.success(), "{}", stderr(&recovered));
}

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn configuration_errors_are_bounded_codes_without_secrets(pool: PgPool) {
    let fixture = Fixture::new(&pool).await;
    fixture.initialize().await;
    let mut unknown = fixture.config("127.0.0.1:0");
    unknown["administration"]["extra"] = json!(true);
    let mut dashboard_without_browser = fixture.config("127.0.0.1:0");
    dashboard_without_browser["administration"]["dashboardDirectory"] = json!("/nonexistent");
    let mut missing_secret = fixture.config("127.0.0.1:0");
    missing_secret["databaseUrlFile"] = json!("/run/missing/super-secret-path");
    let mut duplicate_sources = fixture.config("127.0.0.1:0");
    duplicate_sources["storageSources"] = json!([
        {"id": "nvme", "storageClass": {"name": "a"}},
        {"id": "nvme", "nfs": {"server": "nas.internal", "root": "/export"}}
    ]);
    std::fs::write(fixture.path("short.pub"), [5u8; 31]).unwrap();
    let mut short_key = fixture.config("127.0.0.1:0");
    short_key["administration"]["secretSealingPublicKeyFile"] = json!(fixture.path("short.pub"));
    let cases = [
        (unknown, "invalid_request"),
        (dashboard_without_browser, "invalid_request"),
        (missing_secret, "invalid_request"),
        (duplicate_sources, "invalid_configuration"),
        (short_key, "invalid_request"),
    ];
    for (config, code) in cases {
        let path = fixture.write_config(&config);
        let output = fixture.run(&["api", "--config", path.to_str().unwrap()]);
        assert!(!output.status.success());
        assert_eq!(stderr(&output), format!("hermes-manager: {code}\n"));
    }
    std::fs::write(
        fixture.path("database-url"),
        "postgres://manager:hunter2@127.0.0.1:1/nowhere",
    )
    .unwrap();
    let path = fixture.write_config(&fixture.config("127.0.0.1:0"));
    let output = fixture.run(&["check", "--config", path.to_str().unwrap()]);
    assert!(!output.status.success());
    assert!(!stderr(&output).contains("hunter2"));
    assert_eq!(stderr(&output), "hermes-manager: dependency_unavailable\n");
}

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn api_serves_from_the_registry_across_an_abrupt_kill(pool: PgPool) {
    let fixture = Fixture::new(&pool).await;
    fixture.initialize().await;
    let port = free_port();
    let execution_port = free_port();
    let resolver_port = free_port();
    let mut settings = fixture.config(&format!("127.0.0.1:{port}"));
    let tls = json!({"certificateFile": fixture.secrets.path("server.crt"), "privateKeyFile": fixture.secrets.path("server.key")});
    settings["execution"] = json!({
        "listen": format!("127.0.0.1:{execution_port}"),
        "tls": tls,
    });
    settings["resolver"] = json!({
        "listen": format!("127.0.0.1:{resolver_port}"),
        "tls": tls,
        "clientCaFile": fixture.secrets.path("ca.crt"),
        "gatewayIdentity": hermes_testkit::GATEWAY_PEER,
    });
    let config = fixture.write_config(&settings);
    let alice = fixture.identity_file("owner-a");
    let granted = fixture.run(&[
        "grant-administrator",
        "--config",
        config.to_str().unwrap(),
        "--identity-file",
        alice.to_str().unwrap(),
    ]);
    assert!(granted.status.success());
    let client = fixture.secrets.client(None);
    let url = format!("https://127.0.0.1:{port}");
    let bearer = format!("Bearer {}", fixture.provider.access_token("owner-a"));
    let write = |value: &'static str| {
        client
            .put(format!("{url}/api/v1/secrets/llm-key"))
            .header("authorization", &bearer)
            .header("idempotency-key", "write-llm-key")
            .json(&json!({"kind": "env", "value": value}))
            .send()
    };
    let mut receipts = Vec::new();
    for _ in 0..2 {
        let api = start_api(&config);
        wait_healthy(&client, &url).await;
        let session = client
            .get(format!("{url}/api/v1/session"))
            .header("authorization", &bearer)
            .send()
            .await
            .unwrap();
        assert_eq!(session.status(), 200);
        // The execution listener runs in the same process and knows no
        // administrator credential.
        let execution = client
            .get(format!(
                "https://127.0.0.1:{execution_port}/execution/v1/environments"
            ))
            .header("authorization", &bearer)
            .send()
            .await
            .unwrap();
        assert_eq!(execution.status(), 401);
        // So does the resolver, which answers only the gateway certificate.
        let resolve = |peer: &'static str| {
            fixture
                .secrets
                .client(Some(peer))
                .post(format!(
                    "https://127.0.0.1:{resolver_port}/internal/v1/routes/resolve"
                ))
                .json(&json!({"identity": {"issuer": fixture.installation.issuer, "subject": "owner-a"}}))
                .send()
        };
        let foreign = resolve("foreign").await.unwrap();
        assert_eq!(foreign.status(), 403);
        let gateway = resolve("gateway").await.unwrap();
        assert_eq!(gateway.status(), 403, "an administrator has no route");
        assert_eq!(
            gateway.json::<Value>().await.unwrap(),
            json!({"status": "denied", "reason": "access_denied"})
        );
        let written = write("sk-fixture").await.unwrap();
        assert_eq!(written.status(), 200);
        receipts.push(written.json::<Value>().await.unwrap());
        drop(api); // SIGKILL: no shutdown hooks run.
    }
    assert_eq!(receipts[0], json!({"name": "llm-key", "version": 1}));
    assert_eq!(
        receipts[1], receipts[0],
        "the acknowledged write survived and replays"
    );
}

/// The API process aborted right before and right after a command commits:
/// before, nothing persists; after, the retry replays the recorded receipt.
#[cfg(feature = "fault-injection")]
#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn api_aborts_around_a_commit_persist_nothing_or_replay(pool: PgPool) {
    let fixture = Fixture::new(&pool).await;
    fixture.initialize().await;
    let port = free_port();
    let config = fixture.write_config(&fixture.config(&format!("127.0.0.1:{port}")));
    let alice = fixture.identity_file("owner-a");
    let granted = fixture.run(&[
        "grant-administrator",
        "--config",
        config.to_str().unwrap(),
        "--identity-file",
        alice.to_str().unwrap(),
    ]);
    assert!(granted.status.success());
    let client = fixture.secrets.client(None);
    let url = format!("https://127.0.0.1:{port}");
    let bearer = format!("Bearer {}", fixture.provider.access_token("owner-a"));
    let write = |key: &'static str| {
        client
            .put(format!("{url}/api/v1/secrets/llm-key"))
            .header("authorization", &bearer)
            .header("idempotency-key", key)
            .json(&json!({"kind": "env", "value": "sk-fixture"}))
            .send()
    };
    let versions = || {
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM manager.admin_secrets").fetch_one(&pool)
    };
    // A first session outside the faults, so only the write runs a command.
    {
        let _api = start_api(&config);
        wait_healthy(&client, &url).await;
        let session = client
            .get(format!("{url}/api/v1/session"))
            .header("authorization", &bearer)
            .send()
            .await
            .unwrap();
        assert_eq!(session.status(), 200);
    }

    for (point, persisted) in [("envelope_before_commit", 0), ("envelope_after_commit", 1)] {
        let api = start_api_with(&config, Some(point));
        wait_healthy(&client, &url).await;
        assert!(write(point).await.is_err(), "{point}: the process aborted");
        drop(api);
        assert_eq!(versions().await.unwrap(), persisted, "{point}");
    }
    let _api = start_api(&config);
    wait_healthy(&client, &url).await;
    // The committed write replays; the aborted one applies once on retry.
    let replayed = write("envelope_after_commit").await.unwrap();
    assert_eq!(replayed.status(), 200);
    assert_eq!(
        replayed.json::<Value>().await.unwrap(),
        json!({"name": "llm-key", "version": 1})
    );
    let retried = write("envelope_before_commit").await.unwrap();
    assert_eq!(
        retried.json::<Value>().await.unwrap(),
        json!({"name": "llm-key", "version": 2})
    );
    assert_eq!(versions().await.unwrap(), 2);
}

#[test]
fn the_binary_writes_sealing_keys_once() {
    let directory = tempfile::tempdir().unwrap();
    let (private, public) = (directory.path().join("k"), directory.path().join("p"));
    let run = || {
        Command::new(BINARY)
            .args(["generate-sealing-key", "--private-key-file"])
            .arg(&private)
            .arg("--public-key-file")
            .arg(&public)
            .output()
            .unwrap()
    };
    assert!(run().status.success());
    assert_eq!(std::fs::read(&private).unwrap().len(), 32);
    assert_eq!(std::fs::read(&public).unwrap().len(), 32);
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&private).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let again = run();
    assert!(!again.status.success(), "never overwrites a key");
}
