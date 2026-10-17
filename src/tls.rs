//! Built-in HTTPS (Cargo feature `tls`): rustls with the `ring` provider,
//! TLS 1.2 + 1.3, HTTP/1.1 only. A reverse proxy is the recommended setup.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use rustls::ServerConfig;
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::server::TlsStream;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Finished handshakes waiting for `accept()`.
const QUEUE_DEPTH: usize = 32;

/// The cert chain and key from PEM files, checked against each other.
pub fn server_config(cert: &Path, key: &Path) -> Result<Arc<ServerConfig>, String> {
    let read = |path: &Path| std::fs::read(path).map_err(|e| format!("tls: cannot read {}: {e}", path.display()));
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(&read(cert)?)
        .collect::<Result<_, _>>()
        .map_err(|e| format!("tls: {}: {e}", cert.display()))?;
    if certs.is_empty() {
        return Err(format!("tls: no certificate in {}", cert.display()));
    }
    let key_der = PrivateKeyDer::from_pem_slice(&read(key)?).map_err(|e| match e {
        rustls_pki_types::pem::Error::NoItemsFound => format!("tls: no private key in {}", key.display()),
        e => format!("tls: {}: {e}", key.display()),
    })?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
        .and_then(|b| b.with_no_client_auth().with_single_cert(certs, key_der))
        .map_err(|e| match e {
            rustls::Error::InconsistentKeys(_) => {
                format!("tls: the key in {} does not match the certificate in {}", key.display(), cert.display())
            }
            e => format!("tls: {e}"),
        })?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

/// TCP connections that finished their TLS handshake, for `axum::serve`.
pub struct TlsListener {
    ready: mpsc::Receiver<(TlsStream<TcpStream>, SocketAddr)>,
    addr: SocketAddr,
}

impl TlsListener {
    /// Accepts on `tcp` in a task; each handshake runs in a task of its own.
    pub fn new(tcp: TcpListener, config: Arc<ServerConfig>) -> std::io::Result<TlsListener> {
        let addr = tcp.local_addr()?;
        let (tx, ready) = mpsc::channel(QUEUE_DEPTH);
        let acceptor = TlsAcceptor::from(config);
        tokio::spawn(async move {
            loop {
                let (stream, peer) = match tcp.accept().await {
                    Ok(v) => v,
                    Err(e) => {
                        // Out of file descriptors and the like: wait, as axum does.
                        eprintln!("rulite: accept: {e}");
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        continue;
                    }
                };
                if tx.is_closed() {
                    return;
                }
                let (acceptor, tx) = (acceptor.clone(), tx.clone());
                tokio::spawn(async move {
                    match tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(stream)).await {
                        Ok(Ok(tls)) => {
                            let _ = tx.send((tls, peer)).await;
                        }
                        Ok(Err(e)) => eprintln!("rulite: tls handshake with {peer}: {e}"),
                        Err(_) => eprintln!("rulite: tls handshake with {peer}: timed out"),
                    }
                });
            }
        });
        Ok(TlsListener { ready, addr })
    }
}

impl axum::serve::Listener for TlsListener {
    type Io = TlsStream<TcpStream>;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        match self.ready.recv().await {
            Some(v) => v,
            // The accept task is gone; nothing will come any more.
            None => std::future::pending().await,
        }
    }

    fn local_addr(&self) -> std::io::Result<SocketAddr> {
        Ok(self.addr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tls_dir() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/tls")
    }

    #[test]
    fn loads_the_test_cert() {
        let dir = tls_dir();
        let config = server_config(&dir.join("cert.pem"), &dir.join("key.pem")).unwrap();
        assert_eq!(config.alpn_protocols, [b"http/1.1".to_vec()]);
    }

    #[test]
    fn pem_errors() {
        let dir = tls_dir();
        let (cert, key) = (dir.join("cert.pem"), dir.join("key.pem"));
        let err = |c: &Path, k: &Path| server_config(c, k).unwrap_err();

        assert!(err(&dir.join("missing.pem"), &key).starts_with("tls: cannot read"));
        assert!(err(&cert, &dir.join("missing.pem")).starts_with("tls: cannot read"));
        assert!(err(&key, &key).starts_with("tls: no certificate in"));
        assert!(err(&cert, &cert).starts_with("tls: no private key in"));
        assert!(err(&cert, &dir.join("other-key.pem")).contains("does not match the certificate"));
    }
}
