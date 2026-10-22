use axum::Router;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hermes_adapters::server::{bind, serve_until, tls_config};
use jsonwebtoken::EncodingKey;
use serde_json::{Value, json};
use std::{collections::BTreeSet, path::Path, time::Duration};
use tempfile::TempDir;
use tokio::{process::Command, sync::oneshot, task::JoinHandle};
pub const FORWARD: &str = "spiffe://agents.example.org/router-forward-auth";
pub const ROUTER: &str = "spiffe://agents.example.org/router-plugin";

async fn openssl(directory: &Path, args: &[&str]) -> Vec<u8> {
    let mut command = Command::new("openssl");
    command.current_dir(directory).args(args).kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(10), command.output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        output.status.success(),
        "fixture cryptography command failed"
    );
    output.stdout
}

pub struct Secrets {
    directory: TempDir,
    pub jwk: Value,
    pub signer: EncodingKey,
    pub foreign_signer: EncodingKey,
}
impl Secrets {
    pub async fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path();
        openssl(
            path,
            &[
                "req",
                "-x509",
                "-newkey",
                "rsa:2048",
                "-nodes",
                "-keyout",
                "ca.key",
                "-out",
                "ca.crt",
                "-days",
                "1",
                "-subj",
                "/CN=hermes-fixture-ca",
                "-addext",
                "basicConstraints=critical,CA:TRUE",
            ],
        )
        .await;
        for (name, san, usage, serial) in [
            ("server", "IP:127.0.0.1,DNS:localhost", "serverAuth", "2"),
            (
                "router",
                "URI:spiffe://agents.example.org/router-plugin",
                "clientAuth",
                "3",
            ),
            (
                "forward",
                "URI:spiffe://agents.example.org/router-forward-auth",
                "clientAuth",
                "5",
            ),
            (
                "foreign",
                "URI:spiffe://agents.example.org/unrelated",
                "clientAuth",
                "4",
            ),
        ] {
            let key = format!("{name}.key");
            let csr = format!("{name}.csr");
            let cert = format!("{name}.crt");
            let extensions = format!("{name}.ext");
            tokio::fs::write(path.join(&extensions),format!("basicConstraints=critical,CA:FALSE\nsubjectAltName={san}\nextendedKeyUsage={usage}\nkeyUsage=digitalSignature,keyEncipherment\n")).await.unwrap();
            openssl(
                path,
                &[
                    "req",
                    "-new",
                    "-newkey",
                    "rsa:2048",
                    "-nodes",
                    "-keyout",
                    &key,
                    "-out",
                    &csr,
                    "-subj",
                    "/CN=hermes-fixture",
                ],
            )
            .await;
            openssl(
                path,
                &[
                    "x509",
                    "-req",
                    "-in",
                    &csr,
                    "-CA",
                    "ca.crt",
                    "-CAkey",
                    "ca.key",
                    "-set_serial",
                    serial,
                    "-out",
                    &cert,
                    "-days",
                    "1",
                    "-extfile",
                    &extensions,
                ],
            )
            .await;
        }
        let modulus =
            String::from_utf8(openssl(path, &["rsa", "-in", "ca.key", "-noout", "-modulus"]).await)
                .unwrap();
        let modulus = modulus.trim().strip_prefix("Modulus=").unwrap();
        let bytes: Vec<_> = (0..modulus.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&modulus[i..i + 2], 16).unwrap())
            .collect();
        let signer =
            EncodingKey::from_rsa_pem(&tokio::fs::read(path.join("ca.key")).await.unwrap())
                .unwrap();
        let foreign_signer =
            EncodingKey::from_rsa_pem(&tokio::fs::read(path.join("foreign.key")).await.unwrap())
                .unwrap();
        Self {
            directory,
            jwk: json!({"kty":"RSA","kid":"fixture","use":"sig","alg":"RS256","n":URL_SAFE_NO_PAD.encode(bytes),"e":"AQAB"}),
            signer,
            foreign_signer,
        }
    }
    pub async fn read(&self, name: &str) -> Vec<u8> {
        tokio::fs::read(self.directory.path().join(name))
            .await
            .unwrap()
    }
    pub async fn client(&self, identity: Option<&str>) -> reqwest::Client {
        let mut builder = reqwest::Client::builder()
            .https_only(true)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(3))
            .add_root_certificate(
                reqwest::Certificate::from_pem(&self.read("ca.crt").await).unwrap(),
            );
        if let Some(name) = identity {
            let mut pem = self.read(&format!("{name}.key")).await;
            pem.extend(self.read(&format!("{name}.crt")).await);
            builder = builder.identity(reqwest::Identity::from_pem(&pem).unwrap());
        }
        builder.build().unwrap()
    }
}

pub struct Running {
    pub url: String,
    pub task: JoinHandle<()>,
    _stop: oneshot::Sender<()>,
}
impl Drop for Running {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub async fn start(secrets: &Secrets, router: Router, mtls: bool) -> Running {
    start_with_peers(secrets, router, mtls, BTreeSet::from([ROUTER.into()])).await
}
pub async fn start_with_peers(
    secrets: &Secrets,
    router: Router,
    mtls: bool,
    peers: BTreeSet<String>,
) -> Running {
    let (listener, _) = bind("127.0.0.1:0").await.unwrap();
    start_on_listener(secrets, router, mtls, peers, listener).await
}
pub async fn start_on_listener(
    secrets: &Secrets,
    router: Router,
    mtls: bool,
    peers: BTreeSet<String>,
    listener: tokio::net::TcpListener,
) -> Running {
    let cert = secrets.read("server.crt").await;
    let key = secrets.read("server.key").await;
    let ca = secrets.read("ca.crt").await;
    let tls = tls_config(&cert, &key, mtls.then_some(ca.as_slice())).unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, stopped) = oneshot::channel();
    let task = tokio::spawn(async move {
        serve_until(
            listener,
            tls,
            if mtls { peers } else { BTreeSet::new() },
            router,
            async {
                let _ = stopped.await;
            },
        )
        .await
        .unwrap();
    });
    Running {
        url: format!("https://{address}"),
        task,
        _stop: stop,
    }
}
