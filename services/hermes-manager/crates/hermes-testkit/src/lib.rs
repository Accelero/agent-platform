//! Shared test fixtures: generated TLS material, a minimal TLS server, a fake
//! OIDC provider and an access-token minter. Used only as a dev-dependency.

pub mod contract;
pub mod provider;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder;
use hyper_util::service::TowerToHyperService;
use jsonwebtoken::EncodingKey;
use rustls::ServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::process::Command;
use tokio::task::JoinHandle;

/// mTLS URI identity of the gateway's client certificate.
pub const GATEWAY_PEER: &str = "spiffe://agents.example.org/hermes-gateway";
/// A certificate from the same CA with an unrelated identity.
pub const FOREIGN_PEER: &str = "spiffe://agents.example.org/unrelated";

async fn openssl(directory: &Path, args: &[&str]) -> Vec<u8> {
    let mut command = Command::new("openssl");
    command.current_dir(directory).args(args).kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(20), command.output())
        .await
        .expect("openssl finishes")
        .expect("openssl runs");
    assert!(
        output.status.success(),
        "fixture cryptography command failed"
    );
    output.stdout
}

/// A throwaway CA, server and client certificates, and an RSA signing key
/// whose public half is published as the fixture JWK (`kid = fixture`).
pub struct Secrets {
    directory: TempDir,
    pub jwk: Value,
    pub signer: EncodingKey,
    /// Signs with a key the JWKS does not contain.
    pub foreign_signer: EncodingKey,
}

impl Secrets {
    pub async fn new() -> Self {
        let directory = tempfile::tempdir().expect("temporary directory");
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
        let leaves = [
            (
                "server",
                "IP:127.0.0.1,DNS:localhost".to_owned(),
                "serverAuth",
                "2",
            ),
            ("gateway", format!("URI:{GATEWAY_PEER}"), "clientAuth", "3"),
            ("foreign", format!("URI:{FOREIGN_PEER}"), "clientAuth", "4"),
        ];
        for (name, san, usage, serial) in leaves {
            let extensions = format!(
                "basicConstraints=critical,CA:FALSE\nsubjectAltName={san}\n\
                 extendedKeyUsage={usage}\nkeyUsage=digitalSignature,keyEncipherment\n"
            );
            tokio::fs::write(path.join(format!("{name}.ext")), extensions)
                .await
                .expect("write extensions");
            let (key, csr, cert, ext) = (
                format!("{name}.key"),
                format!("{name}.csr"),
                format!("{name}.crt"),
                format!("{name}.ext"),
            );
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
                    &ext,
                ],
            )
            .await;
        }
        let modulus =
            String::from_utf8(openssl(path, &["rsa", "-in", "ca.key", "-noout", "-modulus"]).await)
                .expect("modulus text");
        let modulus = modulus
            .trim()
            .strip_prefix("Modulus=")
            .expect("modulus prefix");
        let bytes: Vec<u8> = (0..modulus.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&modulus[i..i + 2], 16).expect("hex"))
            .collect();
        let read = |name: &str| std::fs::read(path.join(name)).expect("fixture file");
        Self {
            jwk: json!({"kty": "RSA", "kid": "fixture", "use": "sig", "alg": "RS256",
                        "n": URL_SAFE_NO_PAD.encode(bytes), "e": "AQAB"}),
            signer: EncodingKey::from_rsa_pem(&read("ca.key")).expect("signer"),
            foreign_signer: EncodingKey::from_rsa_pem(&read("foreign.key"))
                .expect("foreign signer"),
            directory,
        }
    }

    pub fn path(&self, name: &str) -> std::path::PathBuf {
        self.directory.path().join(name)
    }

    pub fn read(&self, name: &str) -> Vec<u8> {
        std::fs::read(self.path(name)).expect("fixture file")
    }

    /// HTTPS client trusting the fixture CA, optionally presenting a client
    /// certificate (`gateway` or `foreign`). No redirects, no proxies.
    pub fn client(&self, identity: Option<&str>) -> reqwest::Client {
        let mut builder = reqwest::Client::builder()
            .https_only(true)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(5))
            .add_root_certificate(
                reqwest::Certificate::from_pem(&self.read("ca.crt")).expect("ca"),
            );
        if let Some(name) = identity {
            let mut pem = self.read(&format!("{name}.key"));
            pem.extend(self.read(&format!("{name}.crt")));
            builder = builder.identity(reqwest::Identity::from_pem(&pem).expect("client identity"));
        }
        builder.build().expect("client")
    }

    /// Server TLS configuration with the fixture server certificate.
    pub fn server_config(&self) -> Arc<ServerConfig> {
        let certificates: Vec<CertificateDer<'static>> =
            rustls_pemfile::certs(&mut self.read("server.crt").as_slice())
                .collect::<Result<_, _>>()
                .expect("server certificate");
        let key: PrivateKeyDer<'static> =
            rustls_pemfile::private_key(&mut self.read("server.key").as_slice())
                .expect("server key")
                .expect("one key");
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut config = ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("protocol versions")
            .with_no_client_auth()
            .with_single_cert(certificates, key)
            .expect("server config");
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Arc::new(config)
    }
}

/// A running fixture server, stopped on drop.
pub struct Running {
    pub url: String,
    task: JoinHandle<()>,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Serves `router` over TLS on an ephemeral loopback port.
pub async fn serve_tls(secrets: &Secrets, router: Router) -> Running {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let url = format!("https://{}", listener.local_addr().expect("address"));
    let acceptor = tokio_rustls::TlsAcceptor::from(secrets.server_config());
    let task = tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                return;
            };
            let acceptor = acceptor.clone();
            let router = router.clone();
            tokio::spawn(async move {
                let Ok(stream) = acceptor.accept(socket).await else {
                    return;
                };
                let _ = Builder::new(TokioExecutor::new())
                    .serve_connection(TokioIo::new(stream), TowerToHyperService::new(router))
                    .await;
            });
        }
    });
    Running { url, task }
}
