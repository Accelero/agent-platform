//! The shared body bound applies even when a route has no body extractor.
use super::*;
use std::{io::Cursor, sync::Arc};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};
use tokio_rustls::{TlsConnector, client::TlsStream};

async fn tls(f: &Fixture, server: &Running, identity: Option<&str>) -> TlsStream<TcpStream> {
    let ca = f.secrets.read("ca.crt").await;
    let mut roots = rustls::RootCertStore::empty();
    for cert in rustls_pemfile::certs(&mut Cursor::new(ca)) {
        roots.add(cert.unwrap()).unwrap();
    }
    let builder = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_root_certificates(roots);
    let config = if let Some(identity) = identity {
        let certificates = f.secrets.read(&format!("{identity}.crt")).await;
        let key = f.secrets.read(&format!("{identity}.key")).await;
        builder
            .with_client_auth_cert(
                rustls_pemfile::certs(&mut Cursor::new(certificates))
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap(),
                rustls_pemfile::private_key(&mut Cursor::new(key))
                    .unwrap()
                    .unwrap(),
            )
            .unwrap()
    } else {
        builder.with_no_client_auth()
    };
    let url = url::Url::parse(&server.url).unwrap();
    let host = url.host_str().unwrap().to_owned();
    tokio::time::timeout(Duration::from_secs(3), async {
        let tcp = TcpStream::connect((host.as_str(), url.port().unwrap()))
            .await
            .unwrap();
        TlsConnector::from(Arc::new(config))
            .connect(rustls::pki_types::ServerName::try_from(host).unwrap(), tcp)
            .await
            .unwrap()
    })
    .await
    .unwrap()
}

async fn response(stream: &mut TlsStream<TcpStream>, status: u16, marker: &str) -> Vec<u8> {
    tokio::time::timeout(Duration::from_secs(3), async {
        let mut data = Vec::new();
        let mut block = [0; 1024];
        while !String::from_utf8_lossy(&data).contains(marker) {
            let size = stream.read(&mut block).await.unwrap();
            assert_ne!(size, 0);
            data.extend_from_slice(&block[..size]);
            assert!(data.len() < 8192);
        }
        assert!(data.starts_with(format!("HTTP/1.1 {status} ").as_bytes()));
        data
    })
    .await
    .unwrap()
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn get_health_fallback_and_chunked_bodies_share_cap_and_read_deadline(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let oversized = "body-content-must-stay-private".repeat(600);
    assert!(oversized.len() > 16 * 1024);
    for path in ["/api/v1/session", "/healthz", "/unknown"] {
        let reply = f
            .client
            .get(format!("{}{path}", f.admin.url))
            .bearer_auth(f.admin_token())
            .body(oversized.clone())
            .send()
            .await
            .unwrap();
        assert_eq!(reply.status(), 422);
        assert_eq!(reply.headers()["cache-control"], "no-store");
        let body = reply.text().await.unwrap();
        assert_eq!(body, "{\"error\":\"invalid_request\"}");
    }
    let reply = f
        .router_client
        .get(format!("{}/healthz", f.resolver.url))
        .body(oversized)
        .send()
        .await
        .unwrap();
    assert_eq!(reply.status(), 422);
    // No Content-Length: the body is split across real HTTP/1.1 chunks.
    for (size, status, marker) in [
        (16 * 1024, 200, "ok"),
        (16 * 1024 + 1, 422, "invalid_request"),
    ] {
        let mut stream = tls(&f, &f.admin, None).await;
        let mut wire=b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n".to_vec();
        for chunk_size in [8192, size - 8192] {
            wire.extend_from_slice(format!("{chunk_size:x}\r\n").as_bytes());
            wire.extend(std::iter::repeat_n(b'x', chunk_size));
            wire.extend_from_slice(b"\r\n");
        }
        wire.extend_from_slice(b"0\r\n\r\n");
        stream.write_all(&wire).await.unwrap();
        response(&mut stream, status, marker).await;
    }
    // The resolver's existing 1.8s overall timeout includes reading a stalled
    // body. Headers alone must not let this GET bypass its response deadline.
    let mut stream = tls(&f, &f.resolver, Some("router")).await;
    stream.write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nContent-Length: 1\r\nConnection: close\r\n\r\n").await.unwrap();
    let elapsed = std::time::Instant::now();
    let result = response(&mut stream, 503, "dependency_unavailable").await;
    assert!(elapsed.elapsed() >= Duration::from_secs(1));
    assert!(String::from_utf8_lossy(&result).contains("no-store"));
}
