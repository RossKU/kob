//! Blocking TLS for the indexer client (`https://` REST, `wss://` for `kob watch`): rustls with the ring provider and the
//! webpki roots compiled in (the same set `kob-executor` uses): no system OpenSSL, no system certificate store, the same trust
//! on every platform. The node's wRPC stays plain `ws://` (a local node).

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// The client configuration: ring, the safe default protocol versions, the webpki roots.
fn config() -> Arc<rustls::ClientConfig> {
    static CFG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    CFG.get_or_init(|| {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .expect("ring supports the default protocol versions")
            .with_root_certificates(roots)
            .with_no_client_auth();
        Arc::new(cfg)
    })
    .clone()
}

/// A plain or TLS stream over TCP.
pub enum Stream {
    /// `http://`, `ws://`.
    Plain(TcpStream),
    /// `https://`, `wss://`.
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}

impl Stream {
    /// The TCP socket underneath (timeouts are set on it).
    pub fn tcp(&self) -> &TcpStream {
        match self {
            Stream::Plain(s) => s,
            Stream::Tls(s) => &s.sock,
        }
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Stream::Plain(s) => s.read(buf),
            Stream::Tls(s) => s.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Stream::Plain(s) => s.write(buf),
            Stream::Tls(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Stream::Plain(s) => s.flush(),
            Stream::Tls(s) => s.flush(),
        }
    }
}

/// A TCP connection to `host:port` with read and write timeouts, wrapped in TLS when `secure` (server name: `host`). The
/// errors name the host only.
pub fn connect(secure: bool, host: &str, port: u16, timeout: Duration) -> Result<Stream, String> {
    let addr =
        (host, port).to_socket_addrs().map_err(|e| format!("{host}: {e}"))?.next().ok_or_else(|| format!("{host}: no address"))?;
    let tcp = TcpStream::connect_timeout(&addr, timeout).map_err(|e| format!("{host}: {e}"))?;
    tcp.set_read_timeout(Some(timeout)).and_then(|_| tcp.set_write_timeout(Some(timeout))).map_err(|e| e.to_string())?;
    let _ = tcp.set_nodelay(true);
    if !secure {
        return Ok(Stream::Plain(tcp));
    }
    let name = rustls::pki_types::ServerName::try_from(host.to_string()).map_err(|e| format!("{host}: {e}"))?;
    let conn = rustls::ClientConnection::new(config(), name).map_err(|e| format!("TLS {host}: {e}"))?;
    Ok(Stream::Tls(Box::new(rustls::StreamOwned::new(conn, tcp))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_configuration_builds_and_a_refused_connection_names_the_host_only() {
        let _ = config();
        let e = connect(true, "127.0.0.1", 1, Duration::from_millis(300)).err().unwrap();
        assert!(e.starts_with("127.0.0.1:"), "{e}");
        assert!(connect(true, "not a host", 443, Duration::from_millis(300)).is_err());
    }
}
