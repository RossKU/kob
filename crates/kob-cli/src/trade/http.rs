//! A minimal blocking HTTP/1.1 GET client for the indexer API (`docs/ops/executor.md` Part B): plain `http://` only (put a
//! TLS-terminating proxy in front of a remote indexer), JSON bodies, `Content-Length` or chunked responses. URLs are shown
//! redacted (no credentials, no API keys) in every message.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use serde_json::Value;

/// Largest indexer response read (64 MiB).
pub const MAX_RESPONSE_BYTES: u64 = 64 << 20;

/// The indexer endpoint when `--indexer` is not given.
pub const INDEXER_ENV: &str = "KOB_INDEXER_URL";

/// `--indexer` if given, else [`INDEXER_ENV`].
pub fn indexer_arg(flag: Option<String>) -> Option<String> {
    flag.filter(|v| !v.trim().is_empty())
        .or_else(|| std::env::var(INDEXER_ENV).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty()))
}

/// The indexer as messages show it.
pub fn shown(url: &str) -> String {
    kob_protocol::redact::redact_url(url)
}

struct Target {
    host: String,
    port: u16,
    base: String,
    auth: Option<String>,
}

fn b64(s: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in s.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn parse(url: &str) -> Result<Target, String> {
    let rest = url.strip_prefix("http://").ok_or_else(|| {
        format!("indexer {}: only http:// is supported (put a TLS-terminating proxy in front of a remote indexer)", shown(url))
    })?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let (auth, hostport) = match authority.rsplit_once('@') {
        Some((cred, hp)) => (Some(b64(cred.as_bytes())), hp),
        None => (None, authority),
    };
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) if !h.is_empty() => (h.to_string(), p.parse().map_err(|_| format!("indexer {}: bad port", shown(url)))?),
        _ => (hostport.to_string(), 80),
    };
    if host.is_empty() {
        return Err(format!("indexer {}: no host", shown(url)));
    }
    Ok(Target { host, port, base: path.trim_end_matches('/').to_string(), auth })
}

fn dechunk(mut body: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = vec![];
    loop {
        let at = body.windows(2).position(|w| w == b"\r\n").ok_or("truncated chunked body")?;
        let size_line = std::str::from_utf8(&body[..at]).map_err(|_| "bad chunk size")?;
        let size = usize::from_str_radix(size_line.split(';').next().unwrap_or("").trim(), 16).map_err(|_| "bad chunk size")?;
        body = &body[at + 2..];
        if size == 0 {
            return Ok(out);
        }
        if size.checked_add(2).is_none_or(|n| body.len() < n) {
            return Err("truncated chunk".into());
        }
        out.extend_from_slice(&body[..size]);
        body = &body[size + 2..];
    }
}

/// `GET <indexer><path>`: the status code and the JSON body (`null` for an empty one).
pub fn get(indexer: &str, path: &str, timeout: Duration) -> Result<(u16, Value), String> {
    let t = parse(indexer)?;
    let who = shown(indexer);
    let addr = (t.host.as_str(), t.port)
        .to_socket_addrs()
        .map_err(|e| format!("indexer {who}: {e}"))?
        .next()
        .ok_or_else(|| format!("indexer {who}: no address"))?;
    let mut s = TcpStream::connect_timeout(&addr, timeout).map_err(|e| format!("indexer {who}: {e}"))?;
    s.set_read_timeout(Some(timeout)).and_then(|_| s.set_write_timeout(Some(timeout))).map_err(|e| e.to_string())?;
    let mut req = format!(
        "GET {}{} HTTP/1.1\r\nHost: {}\r\nAccept: application/json\r\nConnection: close\r\nUser-Agent: kob-cli\r\n",
        t.base, path, t.host
    );
    if let Some(a) = &t.auth {
        req.push_str(&format!("Authorization: Basic {a}\r\n"));
    }
    req.push_str("\r\n");
    s.write_all(req.as_bytes()).map_err(|e| format!("indexer {who}: {e}"))?;
    let mut raw = vec![];
    // a response is bounded: an endless one is an error, not memory exhaustion
    (&mut s).take(MAX_RESPONSE_BYTES + 1).read_to_end(&mut raw).map_err(|e| format!("indexer {who}: {e}"))?;
    if raw.len() as u64 > MAX_RESPONSE_BYTES {
        return Err(format!("indexer {who}: response larger than {MAX_RESPONSE_BYTES} bytes"));
    }
    let end = raw.windows(4).position(|w| w == b"\r\n\r\n").ok_or_else(|| format!("indexer {who}: malformed response"))?;
    let head = String::from_utf8_lossy(&raw[..end]).to_string();
    let mut body = raw[end + 4..].to_vec();
    let mut lines = head.lines();
    let status: u16 = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse().ok())
        .ok_or_else(|| format!("indexer {who}: malformed status line"))?;
    let chunked =
        lines.any(|l| l.to_ascii_lowercase().starts_with("transfer-encoding:") && l.to_ascii_lowercase().contains("chunked"));
    if chunked {
        body = dechunk(&body).map_err(|e| format!("indexer {who}: {e}"))?;
    }
    let v = if body.iter().all(|b| b.is_ascii_whitespace()) {
        Value::Null
    } else {
        serde_json::from_slice(&body).map_err(|e| format!("indexer {who}{path}: not JSON ({e})"))?
    };
    Ok((status, v))
}

/// [`get`] that wants a 200.
pub fn get_ok(indexer: &str, path: &str, timeout: Duration) -> Result<Value, String> {
    let (status, v) = get(indexer, path, timeout)?;
    if status != 200 {
        let msg = v.pointer("/error/message").and_then(Value::as_str).unwrap_or("");
        return Err(format!("indexer {}{}: HTTP {status} {msg}", shown(indexer), path.split('?').next().unwrap_or("")));
    }
    Ok(v)
}

#[cfg(test)]
pub mod mock {
    //! A mock indexer for tests: answers GET paths from a fixed map (the query string included, else the bare path).

    use std::collections::BTreeMap;
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::thread;

    use super::*;

    /// Shared state.
    #[derive(Default)]
    pub struct MockIndexer {
        /// path -> JSON body (status 200); a missing path answers 404.
        pub routes: BTreeMap<String, Value>,
        /// Every path requested.
        pub seen: Vec<String>,
    }

    /// Start a mock indexer; returns its URL.
    pub fn start(routes: BTreeMap<String, Value>) -> (String, Arc<Mutex<MockIndexer>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let st = Arc::new(Mutex::new(MockIndexer { routes, seen: vec![] }));
        let s2 = st.clone();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let st = s2.clone();
                thread::spawn(move || {
                    let mut buf = vec![0u8; 8192];
                    let mut got = vec![];
                    while !got.windows(4).any(|w| w == b"\r\n\r\n") {
                        let n = stream.read(&mut buf).unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        got.extend_from_slice(&buf[..n]);
                    }
                    let line = String::from_utf8_lossy(&got).lines().next().unwrap_or("").to_string();
                    let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
                    let body = {
                        let mut s = st.lock().unwrap();
                        s.seen.push(path.clone());
                        s.routes.get(&path).or_else(|| s.routes.get(path.split('?').next().unwrap_or(""))).cloned()
                    };
                    let (status, body) = match body {
                        Some(b) => ("200 OK", b.to_string()),
                        None => ("404 Not Found", r#"{"error":{"code":"not_found","message":"unknown"}}"#.to_string()),
                    };
                    // chunked, to exercise the decoder
                    let resp = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{body}\r\n0\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(resp.as_bytes());
                });
            }
        });
        (url, st)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn get_against_a_mock_indexer() {
        let routes = [("/v1/tokens".to_string(), json!({"tokens": []}))].into_iter().collect();
        let (url, st) = mock::start(routes);
        assert_eq!(get_ok(&url, "/v1/tokens", Duration::from_secs(5)).unwrap(), json!({"tokens": []}));
        let e = get_ok(&url, "/v1/orders/zz", Duration::from_secs(5)).unwrap_err();
        assert!(e.contains("HTTP 404"), "{e}");
        assert_eq!(st.lock().unwrap().seen.len(), 2);
        // a base path and credentials: the request carries them, messages never show them
        let e = get_ok("http://bob:pw@127.0.0.1:1/api", "/v1/tokens", Duration::from_millis(300)).unwrap_err();
        assert!(!e.contains("pw") && !e.contains("bob"), "{e}");
        assert!(get_ok("https://x", "/", Duration::from_millis(300)).unwrap_err().contains("only http://"));
        assert_eq!(b64(b"bob:pw"), "Ym9iOnB3");
    }

    #[test]
    fn chunked_bodies() {
        assert_eq!(dechunk(b"3\r\nabc\r\n2;x=1\r\nde\r\n0\r\n\r\n").unwrap(), b"abcde");
        assert!(dechunk(b"5\r\nab").is_err());
        // a chunk size near usize::MAX from a hostile indexer is an error, not an overflow
        assert!(dechunk(b"ffffffffffffffff\r\nab").is_err());
    }
}
