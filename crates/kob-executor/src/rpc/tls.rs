//! Blocking TLS and a minimal HTTP/1.1 GET, for the two places that need them outside the async WebSocket client: the Kaspa
//! resolver's HTTPS API (`rpc::resolver`) and the x402 facilitator's blocking node client (`x402::node`) on a `wss://` node.
//!
//! rustls with the ring provider and the webpki roots compiled in, as the async client (`tokio-tungstenite`) uses: no system
//! OpenSSL, no system certificate store, the same trust on every platform and in a container with nothing installed.

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
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
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

/// The parts of a URL a connection needs: TLS or not, host, port, path with query (`/` when empty).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub tls: bool,
    pub host: String,
    pub port: u16,
    pub path: String,
}

/// Splits `http(s)://` or `ws(s)://` URLs; the default ports are 80 / 443. User information is left out of the target (the
/// WebSocket handshake takes it from the URL); no error text carries it or a query (`redact_url`).
pub fn target(url: &str) -> Result<Target, String> {
    let shown = crate::rpc::redact_url(url);
    let (scheme, rest) = url.split_once("://").ok_or_else(|| format!("{shown}: not a URL"))?;
    let tls = match scheme {
        "https" | "wss" => true,
        "http" | "ws" => false,
        _ => return Err(format!("{shown}: unsupported scheme")),
    };
    let (authority, path) = kob_protocol::redact::url_host(rest);
    if authority.is_empty() {
        return Err(format!("{shown}: no host"));
    }
    let (host, port) = match authority.strip_prefix('[') {
        // [v6]:port
        Some(v6) => {
            let (h, p) = v6.split_once(']').ok_or_else(|| format!("{shown}: bad IPv6 host"))?;
            (h.to_string(), p.strip_prefix(':'))
        }
        None => match authority.rsplit_once(':') {
            Some((h, p)) => (h.to_string(), Some(p)),
            None => (authority.to_string(), None),
        },
    };
    let port = match port {
        Some(p) => p.parse().map_err(|_| format!("{shown}: bad port"))?,
        None if tls => 443,
        None => 80,
    };
    let path = if path.is_empty() {
        "/".to_string()
    } else if path.starts_with('?') {
        format!("/{path}")
    } else {
        path.to_string()
    };
    Ok(Target { tls, host, port, path })
}

/// A TCP connection to `t` with read and write timeouts, wrapped in TLS when `t.tls` (server name: the host).
pub fn connect(t: &Target, timeout: Duration) -> Result<Stream, String> {
    let addr = (t.host.as_str(), t.port)
        .to_socket_addrs()
        .map_err(|e| format!("resolve {}: {e}", t.host))?
        .next()
        .ok_or_else(|| format!("no address for {}", t.host))?;
    let tcp = TcpStream::connect_timeout(&addr, timeout).map_err(|e| format!("connect {}: {e}", t.host))?;
    tcp.set_read_timeout(Some(timeout)).map_err(|e| e.to_string())?;
    tcp.set_write_timeout(Some(timeout)).map_err(|e| e.to_string())?;
    let _ = tcp.set_nodelay(true);
    if !t.tls {
        return Ok(Stream::Plain(tcp));
    }
    let name = rustls::pki_types::ServerName::try_from(t.host.clone()).map_err(|e| format!("{}: {e}", t.host))?;
    let conn = rustls::ClientConnection::new(config(), name).map_err(|e| format!("TLS {}: {e}", t.host))?;
    Ok(Stream::Tls(Box::new(rustls::StreamOwned::new(conn, tcp))))
}

/// The most a GET answer may be (the resolver's is about a hundred bytes).
const MAX_BODY: usize = 64 * 1024;

/// `GET url` over HTTP/1.1 (`Connection: close`): the status code and the body, chunked transfer decoded.
pub fn http_get(url: &str, timeout: Duration) -> Result<(u16, String), String> {
    let t = target(url)?;
    let mut s = connect(&t, timeout)?;
    let host = if (t.tls && t.port == 443) || (!t.tls && t.port == 80) { t.host.clone() } else { format!("{}:{}", t.host, t.port) };
    let req = format!(
        "GET {} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: kob-executor/{}\r\nAccept: application/json\r\nConnection: close\r\n\r\n",
        t.path,
        kob_protocol::VERSION
    );
    s.write_all(req.as_bytes()).map_err(|e| format!("GET {url}: {e}"))?;
    let mut raw = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match s.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                raw.extend_from_slice(&buf[..n]);
                if raw.len() > MAX_BODY {
                    return Err(format!("GET {url}: answer larger than {MAX_BODY} bytes"));
                }
            }
            // a TLS peer that closes without close_notify: what was read is the answer (Connection: close)
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof && !raw.is_empty() => break,
            Err(e) => return Err(format!("GET {url}: {e}")),
        }
    }
    parse_response(&raw).map_err(|e| format!("GET {url}: {e}"))
}

/// An HTTP/1.1 response: status code and body (by `Content-Length`, chunked, or to the end).
pub fn parse_response(raw: &[u8]) -> Result<(u16, String), String> {
    let end = raw.windows(4).position(|w| w == b"\r\n\r\n").ok_or("no end of headers")?;
    let head = std::str::from_utf8(&raw[..end]).map_err(|_| "headers are not text")?;
    let mut lines = head.split("\r\n");
    let status_line = lines.next().ok_or("no status line")?;
    let mut parts = status_line.split_whitespace();
    let version = parts.next().unwrap_or("");
    if !version.starts_with("HTTP/1.") {
        return Err(format!("not HTTP/1.x: {status_line}"));
    }
    let status: u16 = parts.next().and_then(|c| c.parse().ok()).ok_or_else(|| format!("bad status line: {status_line}"))?;
    let (mut chunked, mut length) = (false, None);
    for l in lines {
        let Some((k, v)) = l.split_once(':') else { continue };
        let (k, v) = (k.trim().to_ascii_lowercase(), v.trim());
        if k == "transfer-encoding" && v.to_ascii_lowercase().contains("chunked") {
            chunked = true;
        } else if k == "content-length" {
            length = Some(v.parse::<usize>().map_err(|_| format!("bad Content-Length {v}"))?);
        }
    }
    let rest = &raw[end + 4..];
    let body = if chunked {
        dechunk(rest)?
    } else if let Some(n) = length {
        rest.get(..n).ok_or_else(|| format!("body of {} bytes, Content-Length {n}", rest.len()))?.to_vec()
    } else {
        rest.to_vec()
    };
    Ok((status, String::from_utf8_lossy(&body).into_owned()))
}

fn dechunk(mut rest: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    loop {
        let eol = rest.windows(2).position(|w| w == b"\r\n").ok_or("chunk size without end of line")?;
        let size_text = std::str::from_utf8(&rest[..eol]).map_err(|_| "chunk size is not text")?;
        let size = usize::from_str_radix(size_text.split(';').next().unwrap_or("").trim(), 16)
            .map_err(|_| format!("bad chunk size {size_text}"))?;
        rest = &rest[eol + 2..];
        if size == 0 {
            return Ok(out);
        }
        let chunk = rest.get(..size).ok_or("chunk shorter than its size")?;
        out.extend_from_slice(chunk);
        rest = rest.get(size + 2..).ok_or("chunk without its end of line")?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_split_into_targets() {
        assert_eq!(
            target("https://eric.kaspa.stream/v2/kaspa/mainnet/tls/wrpc/borsh").unwrap(),
            Target { tls: true, host: "eric.kaspa.stream".into(), port: 443, path: "/v2/kaspa/mainnet/tls/wrpc/borsh".into() }
        );
        assert_eq!(
            target("wss://alpha-10.kaspa.stream/kaspa/testnet-10/wrpc/borsh").unwrap(),
            Target { tls: true, host: "alpha-10.kaspa.stream".into(), port: 443, path: "/kaspa/testnet-10/wrpc/borsh".into() }
        );
        assert_eq!(
            target("http://127.0.0.1:8888").unwrap(),
            Target { tls: false, host: "127.0.0.1".into(), port: 8888, path: "/".into() }
        );
        assert_eq!(
            target("ws://[::1]:17210/x?y").unwrap(),
            Target { tls: false, host: "::1".into(), port: 17210, path: "/x?y".into() }
        );
        assert_eq!(target("ws://h?q=1").unwrap().path, "/?q=1");
        // user information is not part of the target; errors never show it
        assert_eq!(
            target("wss://user:pw@h.example:8443/p").unwrap(),
            Target { tls: true, host: "h.example".into(), port: 8443, path: "/p".into() }
        );
        for bad in ["ftp://x", "x", "https://", "http://h:99999/", "ws://user:hunter2@h:bad"] {
            let e = target(bad).unwrap_err();
            assert!(!e.contains("hunter2"), "{e}");
        }
    }

    #[test]
    fn responses_parse_with_length_chunks_or_to_the_end() {
        let r = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 11\r\n\r\n{\"url\":\"x\"}trailing";
        assert_eq!(parse_response(r).unwrap(), (200, "{\"url\":\"x\"}".into()));
        let r = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n{\"a\"\r\n3;ext=1\r\n:1}\r\n0\r\n\r\n";
        assert_eq!(parse_response(r).unwrap(), (200, "{\"a\":1}".into()));
        let r = b"HTTP/1.0 404 Not Found\r\n\r\nNOT FOUND";
        assert_eq!(parse_response(r).unwrap(), (404, "NOT FOUND".into()));
        assert!(parse_response(b"HTTP/1.1 200 OK\r\nContent-Length: 50\r\n\r\nshort").is_err());
        assert!(parse_response(b"SSH-2.0 hello\r\n\r\n").is_err());
        assert!(parse_response(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n").is_err());
        assert!(parse_response(b"HTTP/1.1 200").is_err());
    }

    /// A GET against a local server: plain HTTP end to end (the TLS path is the same request over rustls).
    #[test]
    fn a_get_against_a_local_server() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let srv = std::thread::spawn(move || {
            let (mut c, _) = l.accept().unwrap();
            let mut req = vec![0u8; 1024];
            let n = c.read(&mut req).unwrap();
            let req = String::from_utf8_lossy(&req[..n]).into_owned();
            c.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok").unwrap();
            req
        });
        let (status, body) =
            http_get(&format!("http://127.0.0.1:{port}/v2/kaspa/mainnet/tls/wrpc/borsh"), Duration::from_secs(5)).unwrap();
        assert_eq!((status, body.as_str()), (200, "ok"));
        let req = srv.join().unwrap();
        assert!(req.starts_with("GET /v2/kaspa/mainnet/tls/wrpc/borsh HTTP/1.1\r\n"), "{req}");
        assert!(req.contains(&format!("Host: 127.0.0.1:{port}\r\n")) && req.contains("Connection: close"), "{req}");
    }
}
