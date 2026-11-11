//! Blocking TLS for the indexer client (`https://` REST, `wss://` for `kob watch`): rustls with the ring provider and the
//! webpki roots compiled in (the same set `kobd` uses): no system OpenSSL, no system certificate store, the same trust
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

/// Why a socket operation failed, as an English phrase (the operating system's own text comes in the system language and
/// names the host again): the kinds a user meets are named, any other error keeps its own text.
pub fn describe_io(e: &std::io::Error) -> String {
    use std::io::ErrorKind::*;
    match e.kind() {
        ConnectionRefused => "connection refused".into(),
        TimedOut | WouldBlock => "timed out".into(),
        ConnectionReset => "connection reset by the peer".into(),
        ConnectionAborted => "connection closed".into(),
        NotConnected => "not connected".into(),
        HostUnreachable | NetworkUnreachable => "host unreachable".into(),
        NetworkDown => "network down".into(),
        AddrNotAvailable => "address not available".into(),
        NotFound => "not found".into(),
        PermissionDenied => "permission denied".into(),
        UnexpectedEof => "connection closed early".into(),
        _ => e.to_string(),
    }
}

/// A TCP connection to `host:port` with read and write timeouts, wrapped in TLS when `secure` (server name: `host`). The
/// error is the reason only (`connection refused`, `timed out`, `cannot resolve the host name`): the caller names the host.
pub fn connect(secure: bool, host: &str, port: u16, timeout: Duration) -> Result<Stream, String> {
    let addr = (host, port)
        .to_socket_addrs()
        .map_err(|_| "cannot resolve the host name".to_string())?
        .next()
        .ok_or_else(|| "the host name has no address".to_string())?;
    let tcp = TcpStream::connect_timeout(&addr, timeout).map_err(|e| describe_io(&e))?;
    tcp.set_read_timeout(Some(timeout)).and_then(|_| tcp.set_write_timeout(Some(timeout))).map_err(|e| describe_io(&e))?;
    let _ = tcp.set_nodelay(true);
    if !secure {
        return Ok(Stream::Plain(tcp));
    }
    let name = rustls::pki_types::ServerName::try_from(host.to_string()).map_err(|_| "not a valid host name".to_string())?;
    let conn = rustls::ClientConnection::new(config(), name).map_err(|e| format!("TLS: {e}"))?;
    Ok(Stream::Tls(Box::new(rustls::StreamOwned::new(conn, tcp))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_configuration_builds_and_a_refused_connection_is_an_english_reason() {
        let _ = config();
        let e = connect(true, "127.0.0.1", 1, Duration::from_millis(300)).err().unwrap();
        // a closed port is refused (a firewall may time it out instead): never the system language, never the host
        assert!(e == "connection refused" || e == "timed out", "{e}");
        assert!(connect(true, "not a host", 443, Duration::from_millis(300)).is_err());
    }

    #[test]
    fn io_errors_become_phrases() {
        use std::io::{Error, ErrorKind};
        assert_eq!(describe_io(&Error::from(ErrorKind::ConnectionRefused)), "connection refused");
        assert_eq!(describe_io(&Error::from(ErrorKind::TimedOut)), "timed out");
        assert_eq!(describe_io(&Error::from(ErrorKind::WouldBlock)), "timed out");
        assert_eq!(describe_io(&Error::from(ErrorKind::HostUnreachable)), "host unreachable");
        assert_eq!(describe_io(&Error::other("boom")), "boom");
    }
}
