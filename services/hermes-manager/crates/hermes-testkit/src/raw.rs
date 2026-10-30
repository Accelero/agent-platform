//! A byte-level HTTP/1.1 client over TLS for wire cases a friendly client would
//! normalize away: chunked, truncated or cancelled bodies, repeated headers and
//! header values that are not text. Every read has a deadline.

use std::sync::Arc;
use std::time::Duration;

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, RootCertStore};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use url::Url;

use crate::Secrets;

const DEADLINE: Duration = Duration::from_secs(10);

/// One response, as the server wrote it.
#[derive(Debug)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, Vec<u8>)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&[u8]> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_slice())
    }

    /// The JSON body; panics when it is not JSON.
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).expect("JSON response body")
    }
}

/// One request line and header block ending the headers.
pub fn head(method: &str, path: &str, headers: &[(&str, &[u8])]) -> Vec<u8> {
    let mut bytes = format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\n").into_bytes();
    for (name, value) in headers {
        bytes.extend_from_slice(name.as_bytes());
        bytes.extend_from_slice(b": ");
        bytes.extend_from_slice(value);
        bytes.extend_from_slice(b"\r\n");
    }
    bytes.extend_from_slice(b"Connection: close\r\n\r\n");
    bytes
}

/// `body` as chunks of at most `size` bytes, without a length.
pub fn chunked(body: &[u8], size: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    for chunk in body.chunks(size) {
        bytes.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
        bytes.extend_from_slice(chunk);
        bytes.extend_from_slice(b"\r\n");
    }
    bytes.extend_from_slice(b"0\r\n\r\n");
    bytes
}

/// A connection that has completed the TLS handshake.
pub struct Raw {
    stream: TlsStream<TcpStream>,
}

impl Secrets {
    /// Connects to the listener at `url`, trusting the fixture CA and offering
    /// HTTP/1.1 only.
    pub async fn raw(&self, url: &str) -> Raw {
        self.raw_as(url, None).await
    }

    /// As [`Secrets::raw`], presenting the `gateway` or `foreign` client
    /// certificate when `identity` names one.
    pub async fn raw_as(&self, url: &str, identity: Option<&str>) -> Raw {
        let url = Url::parse(url).expect("listener URL");
        let address = (
            url.host_str().expect("host").to_owned(),
            url.port().expect("port"),
        );
        let mut roots = RootCertStore::empty();
        for certificate in rustls_pemfile::certs(&mut self.read("ca.crt").as_slice()) {
            roots.add(certificate.expect("CA")).expect("root");
        }
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let builder = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("protocol versions")
            .with_root_certificates(roots);
        let mut config = match identity {
            Some(name) => {
                let certificates: Vec<_> =
                    rustls_pemfile::certs(&mut self.read(&format!("{name}.crt")).as_slice())
                        .collect::<Result<_, _>>()
                        .expect("client certificate");
                let key =
                    rustls_pemfile::private_key(&mut self.read(&format!("{name}.key")).as_slice())
                        .expect("client key")
                        .expect("one key");
                builder
                    .with_client_auth_cert(certificates, key)
                    .expect("client identity")
            }
            None => builder.with_no_client_auth(),
        };
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        let socket = timeout(DEADLINE, TcpStream::connect(address))
            .await
            .expect("connect deadline")
            .expect("connect");
        let stream = timeout(
            DEADLINE,
            TlsConnector::from(Arc::new(config))
                .connect(ServerName::try_from("localhost").expect("name"), socket),
        )
        .await
        .expect("handshake deadline")
        .expect("handshake");
        Raw { stream }
    }
}

impl Raw {
    /// Writes `bytes`; false once the server has closed the connection, as it
    /// may after refusing a request early.
    pub async fn send(&mut self, bytes: &[u8]) -> bool {
        self.stream.write_all(bytes).await.is_ok() && self.stream.flush().await.is_ok()
    }

    /// Ends the request side of the connection but keeps reading, as a client
    /// does when its body ends before the declared length.
    pub async fn close_write(&mut self) {
        let _ = self.stream.shutdown().await;
    }

    /// Drops the connection without reading a response, as a client that
    /// cancels mid-request does.
    pub async fn abandon(mut self) {
        let _ = self.stream.shutdown().await;
    }

    /// Reads one response; `None` when the server closed without writing one.
    pub async fn try_response(mut self) -> Option<Response> {
        let mut buffer = Vec::new();
        let end = loop {
            if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                break position + 4;
            }
            if !self.fill(&mut buffer).await {
                return None;
            }
        };
        let head = String::from_utf8_lossy(&buffer[..end - 4]).into_owned();
        let mut lines = head.split("\r\n");
        let status = lines
            .next()?
            .split(' ')
            .nth(1)
            .and_then(|code| code.parse().ok())?;
        let headers: Vec<(String, Vec<u8>)> = lines
            .filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().as_bytes().to_vec()))
            .collect();
        let mut body = buffer.split_off(end);
        let find = |name: &str| {
            headers
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| String::from_utf8_lossy(value).into_owned())
        };
        if let Some(length) = find("content-length").and_then(|value| value.parse::<usize>().ok()) {
            while body.len() < length && self.fill(&mut body).await {}
            body.truncate(length);
        } else if find("transfer-encoding").is_some_and(|value| value.contains("chunked")) {
            while !body.ends_with(b"0\r\n\r\n") && self.fill(&mut body).await {}
            body = dechunk(&body);
        } else {
            while self.fill(&mut body).await {}
        }
        Some(Response {
            status,
            headers,
            body,
        })
    }

    pub async fn response(self) -> Response {
        self.try_response().await.expect("a response")
    }

    /// Appends more bytes; false at end of stream, error or deadline.
    async fn fill(&mut self, buffer: &mut Vec<u8>) -> bool {
        let mut chunk = [0_u8; 16 * 1024];
        match timeout(DEADLINE, self.stream.read(&mut chunk)).await {
            Ok(Ok(count)) if count > 0 => {
                buffer.extend_from_slice(&chunk[..count]);
                true
            }
            _ => false,
        }
    }
}

fn dechunk(mut bytes: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    while let Some(position) = bytes.windows(2).position(|window| window == b"\r\n") {
        let Some(size) = std::str::from_utf8(&bytes[..position])
            .ok()
            .and_then(|size| usize::from_str_radix(size.trim(), 16).ok())
        else {
            break;
        };
        bytes = &bytes[position + 2..];
        if size == 0 || bytes.len() < size {
            break;
        }
        body.extend_from_slice(&bytes[..size]);
        bytes = bytes.get(size + 2..).unwrap_or_default();
    }
    body
}
