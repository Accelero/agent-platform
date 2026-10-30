//! Worker readiness over SSH, in process: log in as `worker` with the
//! instance's client key, accept only the host key the controller
//! provisioned, and check that the shell runs as the backend-declared UID. One
//! deadline bounds the whole check; nothing is retried here.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use hermes_core::domain::execution::{SSH_PORT, SSH_USER, SshCheck};
use russh::ChannelMsg;
use russh::client::{self, Handler};
use russh::keys::{
    PrivateKeyWithHashAlg, PublicKeyOrCertificate, decode_secret_key, parse_public_key_base64,
};

const DEADLINE: Duration = Duration::from_secs(8);
const MAX_OUTPUT: usize = 64;

/// Accepts exactly one server key, and remembers when another was offered.
struct Pinned {
    expected: russh::keys::PublicKey,
    mismatch: Arc<AtomicBool>,
}

impl Handler for Pinned {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        offered: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let matches = matches!(offered, PublicKeyOrCertificate::PublicKey { key, .. } if key.key_data() == self.expected.key_data());
        if !matches {
            self.mismatch.store(true, Ordering::SeqCst);
        }
        Ok(matches)
    }
}

/// `ssh-ed25519 <base64> [comment]` to its key.
fn host_key(line: &str) -> Option<russh::keys::PublicKey> {
    let mut parts = line.split_whitespace();
    (parts.next()? == "ssh-ed25519")
        .then(|| parse_public_key_base64(parts.next()?).ok())
        .flatten()
}

/// Whether the worker at `address` accepts the client key, presents
/// `expected_host_key` and runs the shell as `uid`. A worker that answers but
/// does none of that breaks the contract; one that does not answer yet is
/// checked again later.
pub async fn ready(
    address: &str,
    client_key: &[u8],
    expected_host_key: &str,
    uid: u32,
) -> SshCheck {
    ready_at(address, SSH_PORT, client_key, expected_host_key, uid).await
}

async fn ready_at(
    address: &str,
    port: u16,
    client_key: &[u8],
    expected_host_key: &str,
    uid: u32,
) -> SshCheck {
    tokio::time::timeout(
        DEADLINE,
        check(address, port, client_key, expected_host_key, uid),
    )
    .await
    .unwrap_or(SshCheck::NotYet)
}

async fn check(
    address: &str,
    port: u16,
    client_key: &[u8],
    expected_host_key: &str,
    uid: u32,
) -> SshCheck {
    // Inputs the controller provisioned itself; unusable ones are not the
    // worker's fault, so they are never reported as its failure.
    let Ok(ip) = address.parse::<std::net::IpAddr>() else {
        return SshCheck::NotYet;
    };
    let Some(expected) = host_key(expected_host_key) else {
        return SshCheck::NotYet;
    };
    let Some(key) = std::str::from_utf8(client_key)
        .ok()
        .and_then(|pem| decode_secret_key(pem, None).ok())
    else {
        return SshCheck::NotYet;
    };
    let config = Arc::new(client::Config {
        inactivity_timeout: Some(DEADLINE),
        ..client::Config::default()
    });
    let mismatch = Arc::new(AtomicBool::new(false));
    let pinned = Pinned {
        expected,
        mismatch: mismatch.clone(),
    };
    let mut session = match client::connect(config, (ip, port), pinned).await {
        Ok(session) => session,
        Err(_) if mismatch.load(Ordering::SeqCst) => return SshCheck::ContractFailed,
        Err(_) => return SshCheck::NotYet,
    };
    let authenticated = match session
        .authenticate_publickey(SSH_USER, PrivateKeyWithHashAlg::new(Arc::new(key), None))
        .await
    {
        Ok(result) => result.success(),
        Err(_) => return SshCheck::NotYet,
    };
    if !authenticated {
        return SshCheck::ContractFailed;
    }
    let Ok(mut channel) = session.channel_open_session().await else {
        return SshCheck::NotYet;
    };
    if channel.exec(true, "id -u").await.is_err() {
        return SshCheck::NotYet;
    }
    let (mut output, mut status) = (Vec::new(), None);
    while let Some(message) = channel.wait().await {
        match message {
            ChannelMsg::Data { data } => {
                if output.len() + data.len() > MAX_OUTPUT {
                    return SshCheck::ContractFailed;
                }
                output.extend_from_slice(&data);
            }
            ChannelMsg::ExitStatus { exit_status } => status = Some(exit_status),
            _ => {}
        }
    }
    let _ = session
        .disconnect(russh::Disconnect::ByApplication, "", "")
        .await;
    if status == Some(0) && String::from_utf8_lossy(&output).trim() == uid.to_string() {
        SshCheck::Ready
    } else {
        SshCheck::ContractFailed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use russh::keys::PublicKey;
    use russh::server::{self, Auth, ChannelOpenHandle, Msg, Session};
    use russh::{Channel, ChannelId};
    use tokio::net::TcpListener;

    /// A worker that accepts one client key and answers `id -u`.
    struct Worker {
        authorized: PublicKey,
        uid: &'static str,
    }

    impl server::Handler for Worker {
        type Error = russh::Error;

        async fn auth_publickey(
            &mut self,
            user: &str,
            key: &PublicKey,
        ) -> Result<Auth, Self::Error> {
            Ok(
                if user == "worker" && key.key_data() == self.authorized.key_data() {
                    Auth::Accept
                } else {
                    Auth::reject()
                },
            )
        }

        async fn channel_open_session(
            &mut self,
            _channel: Channel<Msg>,
            reply: ChannelOpenHandle,
            _session: &mut Session,
        ) -> Result<(), Self::Error> {
            reply.accept().await;
            Ok(())
        }

        async fn exec_request(
            &mut self,
            channel: ChannelId,
            data: &[u8],
            session: &mut Session,
        ) -> Result<(), Self::Error> {
            session.channel_success(channel)?;
            if data == b"id -u" {
                session.data(channel, format!("{}\n", self.uid))?;
                session.exit_status_request(channel, 0)?;
            } else {
                session.exit_status_request(channel, 127)?;
            }
            session.eof(channel)?;
            session.close(channel)?;
            Ok(())
        }
    }

    /// A worker on a loopback port; returns the port and its host key line.
    async fn worker(client_public: &str, uid: &'static str) -> (u16, String) {
        let (host_private, host_public) = crate::materials::ed25519_pair().unwrap();
        let config = Arc::new(server::Config {
            keys: vec![decode_secret_key(&host_private, None).unwrap()],
            ..server::Config::default()
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let authorized = host_key(client_public).unwrap();
        tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                let handler = Worker {
                    authorized: authorized.clone(),
                    uid,
                };
                if let Ok(session) = server::run_stream(config.clone(), socket, handler).await {
                    let _ = session.await;
                }
            }
        });
        (port, host_public)
    }

    #[test]
    fn host_keys_must_be_ed25519_lines() {
        assert!(host_key("ssh-rsa AAAAB3NzaC1yc2E").is_none());
        assert!(host_key("ssh-ed25519 not-base64!").is_none());
        assert!(host_key("").is_none());
    }

    #[tokio::test]
    async fn a_worker_is_ready_only_with_its_pinned_host_key_and_the_declared_identity() {
        let (client_private, client_public) = crate::materials::ed25519_pair().unwrap();
        let (port, host_public) = worker(&client_public, "2000").await;
        let key = client_private.as_bytes();
        assert_eq!(
            ready_at("127.0.0.1", port, key, &host_public, 2000).await,
            SshCheck::Ready
        );
        let (_, impostor) = crate::materials::ed25519_pair().unwrap();
        assert_eq!(
            ready_at("127.0.0.1", port, key, &impostor, 2000).await,
            SshCheck::ContractFailed,
            "another host key breaks the contract"
        );
        assert_eq!(
            ready_at("127.0.0.1", port, key, &host_public, 1000).await,
            SshCheck::ContractFailed,
            "the shell must run as the backend declared identity"
        );
        let (stranger, _) = crate::materials::ed25519_pair().unwrap();
        assert_eq!(
            ready_at("127.0.0.1", port, stranger.as_bytes(), &host_public, 2000).await,
            SshCheck::ContractFailed,
            "only the instance's client key"
        );
    }

    #[tokio::test]
    async fn unusable_inputs_and_unreachable_workers_are_not_ready_yet() {
        assert_eq!(
            ready("not-an-address", b"key", "ssh-ed25519 AAAA", 1000).await,
            SshCheck::NotYet
        );
        assert_eq!(
            ready("127.0.0.1", b"not a key", "ssh-ed25519 AAAA", 1000).await,
            SshCheck::NotYet
        );
        let (private, public) = crate::materials::ed25519_pair().unwrap();
        let unused = TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        assert_eq!(
            ready_at("127.0.0.1", unused, private.as_bytes(), &public, 1000).await,
            SshCheck::NotYet,
            "nothing listening yet is retried, not a failure"
        );
    }
}
