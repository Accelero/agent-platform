//! Bounded TLS listeners. Peer identity is derived from a verified certificate,
//! never a forwarded header. Each listener receives only its own Router.
use std::{
    collections::BTreeSet, future::Future, io::BufReader, net::SocketAddr, sync::Arc,
    time::Duration,
};

use axum::Router;
use hermes_application::Error;
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    server::conn::auto::Builder,
    service::TowerToHyperService,
};
use rustls::{
    RootCertStore, ServerConfig,
    pki_types::{CertificateDer, PrivateKeyDer},
    server::WebPkiClientVerifier,
};
use tokio::{net::TcpListener, sync::Semaphore, task::JoinSet};
use tokio_rustls::TlsAcceptor;
use x509_parser::{extensions::GeneralName, prelude::*};

#[derive(Clone)]
pub struct PeerIdentity(pub Option<String>);

pub fn tls_config(
    cert_pem: &[u8],
    key_pem: &[u8],
    client_ca: Option<&[u8]>,
) -> Result<Arc<ServerConfig>, Error> {
    let certificates: Vec<CertificateDer<'static>> =
        rustls_pemfile::certs(&mut BufReader::new(cert_pem))
            .collect::<Result<_, _>>()
            .map_err(|_| Error::Invalid)?;
    let key: PrivateKeyDer<'static> = rustls_pemfile::private_key(&mut BufReader::new(key_pem))
        .map_err(|_| Error::Invalid)?
        .ok_or(Error::Invalid)?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|_| Error::Invalid)?;
    let builder = if let Some(ca) = client_ca {
        let mut roots = RootCertStore::empty();
        for certificate in rustls_pemfile::certs(&mut BufReader::new(ca)) {
            roots
                .add(certificate.map_err(|_| Error::Invalid)?)
                .map_err(|_| Error::Invalid)?;
        }
        let verifier = WebPkiClientVerifier::builder_with_provider(
            Arc::new(roots),
            Arc::new(rustls::crypto::ring::default_provider()),
        )
        .build()
        .map_err(|_| Error::Invalid)?;
        builder.with_client_cert_verifier(verifier)
    } else {
        builder.with_no_client_auth()
    };
    let mut config = builder
        .with_single_cert(certificates, key)
        .map_err(|_| Error::Invalid)?;
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

fn certificate_identity(
    certificate: &CertificateDer<'_>,
    allowed: &BTreeSet<String>,
) -> Option<String> {
    let (_, parsed) = X509Certificate::from_der(certificate.as_ref()).ok()?;
    let sans = parsed.subject_alternative_name().ok()??;
    let matches: BTreeSet<_> = sans
        .value
        .general_names
        .iter()
        .filter_map(|name| match name {
            GeneralName::URI(value) if allowed.contains(*value) => Some((*value).to_string()),
            _ => None,
        })
        .collect();
    if matches.len() == 1 {
        matches.into_iter().next()
    } else {
        None
    }
}

pub async fn serve_until(
    listener: TcpListener,
    config: Arc<ServerConfig>,
    allowed_peers: BTreeSet<String>,
    router: Router,
    shutdown: impl Future<Output = ()> + Send,
) -> Result<(), Error> {
    let acceptor = TlsAcceptor::from(config);
    let permits = Arc::new(Semaphore::new(128));
    let mut connections = JoinSet::new();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            Some(_) = connections.join_next(), if !connections.is_empty() => {},
            accepted = listener.accept() => {
                let (socket, _) = accepted.map_err(|_| Error::Unavailable)?;
                let Ok(permit) = permits.clone().try_acquire_owned() else { drop(socket); continue; };
                let acceptor = acceptor.clone();
                let router = router.clone();
                let allowed = allowed_peers.clone();
                connections.spawn(async move {
                    let _permit = permit;
                    let Ok(Ok(stream)) = tokio::time::timeout(Duration::from_secs(3), acceptor.accept(socket)).await else { return; };
                    let peer = stream.get_ref().1.peer_certificates().and_then(|chain| chain.first()).and_then(|cert| certificate_identity(cert, &allowed));
                    // A CA-signed certificate alone grants no service role.
                    // The handler also requires the exact selected peer role.
                    let service = router.layer(axum::Extension(PeerIdentity(peer)));
                    let mut builder = Builder::new(TokioExecutor::new());
                    builder.http1().max_buf_size(16 * 1024).header_read_timeout(Duration::from_secs(5)).timer(hyper_util::rt::TokioTimer::new());
                    builder.http2().max_header_list_size(16 * 1024).max_concurrent_streams(32);
                    let _ = builder.serve_connection_with_upgrades(TokioIo::new(stream), TowerToHyperService::new(service)).await;
                });
            }
        }
    }
    connections.abort_all();
    while connections.join_next().await.is_some() {}
    Ok(())
}

pub async fn bind(address: &str) -> Result<(TcpListener, SocketAddr), Error> {
    let listener = TcpListener::bind(address)
        .await
        .map_err(|_| Error::Unavailable)?;
    let bound = listener.local_addr().map_err(|_| Error::Unavailable)?;
    Ok((listener, bound))
}
