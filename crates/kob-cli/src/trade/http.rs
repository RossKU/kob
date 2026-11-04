//! A minimal blocking HTTP/1.1 GET client for the indexer API (`docs/ops/executor.md` Part B): `http://` and `https://` (rustls,
//! [`crate::tls`]), JSON bodies, `Content-Length` or chunked responses. URLs are shown redacted (no credentials, no API keys) in
//! every message.

use std::io::{Read, Write};
use std::time::Duration;

use serde_json::Value;

/// Largest indexer response read (64 MiB).
pub const MAX_RESPONSE_BYTES: u64 = 64 << 20;

/// The indexer endpoint when `--indexer` is not given.
pub const INDEXER_ENV: &str = "KOB_INDEXER_URL";

/// `--indexer` if given, else [`INDEXER_ENV`], else the indexer the deployment record of `network` names
/// ([`kob_protocol::artifacts::deployment_indexer_url`], in every build; none while the record leaves it unset).
pub fn indexer_arg(flag: Option<String>, network: &str) -> Option<String> {
    // the unit tests of this crate never reach for the public default (the record of testnet-10 names a live server): the tests that
    // check what a command does without an indexer rely on there being none
    let deployment = if cfg!(test) { None } else { kob_protocol::artifacts::deployment_indexer_url(network) };
    indexer_from(flag, std::env::var(INDEXER_ENV).ok(), deployment)
}

/// [`indexer_arg`] with the environment variable and the deployment default given: the flag wins over the variable, the variable
/// over the default; blank counts as unset.
pub fn indexer_from(flag: Option<String>, env: Option<String>, deployment: Option<String>) -> Option<String> {
    let clean = |v: Option<String>| v.map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    clean(flag).or_else(|| clean(env)).or_else(|| clean(deployment))
}

/// The indexer as messages show it.
pub fn shown(url: &str) -> String {
    kob_protocol::redact::redact_url(url)
}

struct Target {
    secure: bool,
    host: String,
    port: u16,
    base: String,
    auth: Option<String>,
}

pub(crate) fn b64(s: &[u8]) -> String {
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
    let (secure, rest) = if let Some(r) = url.strip_prefix("http://") {
        (false, r)
    } else if let Some(r) = url.strip_prefix("https://") {
        (true, r)
    } else {
        return Err(format!("indexer {}: the indexer URL must be http:// or https://", shown(url)));
    };
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
        _ => (hostport.to_string(), if secure { 443 } else { 80 }),
    };
    if host.is_empty() {
        return Err(format!("indexer {}: no host", shown(url)));
    }
    Ok(Target { secure, host, port, base: path.trim_end_matches('/').to_string(), auth })
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

/// An answer with its headers (names in lower case).
#[derive(Debug, Clone)]
pub struct Response {
    /// Status code.
    pub status: u16,
    /// Headers, names in lower case.
    pub headers: Vec<(String, String)>,
    /// The JSON body (`null` for an empty one).
    pub body: Value,
}

impl Response {
    /// The first header `name` (lower case).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
}

/// `GET <indexer><path>`: the status code and the JSON body (`null` for an empty one).
pub fn get(indexer: &str, path: &str, timeout: Duration) -> Result<(u16, Value), String> {
    get_with(indexer, path, &[], timeout).map(|r| (r.status, r.body))
}

/// [`get`] with extra request headers (`If-None-Match`), and the answer's headers.
pub fn get_with(indexer: &str, path: &str, headers: &[(&str, &str)], timeout: Duration) -> Result<Response, String> {
    let t = parse(indexer)?;
    let who = shown(indexer);
    let mut s = crate::tls::connect(t.secure, &t.host, t.port, timeout).map_err(|e| format!("indexer {who}: {e}"))?;
    let mut req = format!(
        "GET {}{} HTTP/1.1\r\nHost: {}\r\nAccept: application/json\r\nConnection: close\r\nUser-Agent: kob-cli\r\n",
        t.base, path, t.host
    );
    if let Some(a) = &t.auth {
        req.push_str(&format!("Authorization: Basic {a}\r\n"));
    }
    for (k, v) in headers {
        // a header is one line: a value that came from a server cannot add a line to the request
        if k.contains(['\r', '\n', ':']) || v.contains(['\r', '\n']) {
            return Err(format!("indexer {who}: bad request header {k}"));
        }
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    s.write_all(req.as_bytes()).map_err(|e| format!("indexer {who}: {e}"))?;
    let mut raw = vec![];
    // a response is bounded: an endless one is an error, not memory exhaustion
    match (&mut s).take(MAX_RESPONSE_BYTES + 1).read_to_end(&mut raw) {
        Ok(_) => {}
        // a TLS peer that closes without close_notify: what was read is the answer (Connection: close)
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof && !raw.is_empty() => {}
        Err(e) => return Err(format!("indexer {who}: {e}")),
    }
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
    let headers: Vec<(String, String)> =
        lines.filter_map(|l| l.split_once(':').map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))).collect();
    let chunked = headers.iter().any(|(k, v)| k == "transfer-encoding" && v.to_ascii_lowercase().contains("chunked"));
    if chunked {
        body = dechunk(&body).map_err(|e| format!("indexer {who}: {e}"))?;
    }
    let v = if body.iter().all(|b| b.is_ascii_whitespace()) {
        Value::Null
    } else {
        serde_json::from_slice(&body).map_err(|e| format!("indexer {who}{path}: not JSON ({e})"))?
    };
    Ok(Response { status, headers, body: v })
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
        /// path -> extra response headers (an `ETag` makes the route answer `If-None-Match` with `304`).
        pub headers: BTreeMap<String, Vec<(String, String)>>,
        /// Every request's headers (names in lower case), in the order of `seen`.
        pub request_headers: Vec<Vec<(String, String)>>,
    }

    /// Start a mock indexer; returns its URL.
    pub fn start(routes: BTreeMap<String, Value>) -> (String, Arc<Mutex<MockIndexer>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let st = Arc::new(Mutex::new(MockIndexer { routes, seen: vec![], headers: BTreeMap::new(), request_headers: vec![] }));
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
                    let sent: Vec<(String, String)> = String::from_utf8_lossy(&got)
                        .lines()
                        .skip(1)
                        .filter_map(|l| l.split_once(':').map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string())))
                        .collect();
                    let (body, extra) = {
                        let mut s = st.lock().unwrap();
                        s.seen.push(path.clone());
                        s.request_headers.push(sent.clone());
                        let extra = s.headers.get(&path).cloned().unwrap_or_default();
                        (s.routes.get(&path).or_else(|| s.routes.get(path.split('?').next().unwrap_or(""))).cloned(), extra)
                    };
                    let extra_text: String = extra.iter().map(|(k, v)| format!("{k}: {v}\r\n")).collect();
                    // a route with an ETag answers a request that has it with 304
                    let etag = extra.iter().find(|(k, _)| k.eq_ignore_ascii_case("etag")).map(|(_, v)| v.clone());
                    if etag.is_some() && sent.iter().any(|(k, v)| k == "if-none-match" && Some(v) == etag.as_ref()) && body.is_some() {
                        let resp = format!("HTTP/1.1 304 Not Modified\r\n{extra_text}Connection: close\r\n\r\n");
                        let _ = stream.write_all(resp.as_bytes());
                        return;
                    }
                    let (status, body) = match body {
                        Some(b) => ("200 OK", b.to_string()),
                        None => ("404 Not Found", r#"{"error":{"code":"not_found","message":"unknown"}}"#.to_string()),
                    };
                    // chunked, to exercise the decoder
                    let resp = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n{extra_text}Transfer-Encoding: chunked\r\n\r\n{:x}\r\n{body}\r\n0\r\n\r\n",
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
        assert!(get_ok("ftp://x", "/", Duration::from_millis(300)).unwrap_err().contains("must be http:// or https://"));
        // https:// is spoken too (TLS to a port nothing listens on: refused, the host named, no credentials)
        let e = get_ok("https://bob:pw@127.0.0.1:1", "/", Duration::from_millis(300)).unwrap_err();
        assert!(e.contains("127.0.0.1") && !e.contains("pw"), "{e}");
        assert_eq!(parse("https://h").unwrap().port, 443);
        assert_eq!(parse("http://h").unwrap().port, 80);
        assert_eq!(b64(b"bob:pw"), "Ym9iOnB3");
    }

    #[test]
    fn the_indexer_comes_from_the_flag_then_the_variable_then_the_deployment_default() {
        let s = |x: &str| Some(x.to_string());
        let (f, e, d) = (s("http://flag"), s("http://env"), s("https://deployment"));
        assert_eq!(indexer_from(f.clone(), e.clone(), d.clone()), f);
        assert_eq!(indexer_from(None, e.clone(), d.clone()), e);
        assert_eq!(indexer_from(None, None, d.clone()), d);
        // blank is unset, at every level
        assert_eq!(indexer_from(s("  "), s(""), d.clone()), d);
        assert_eq!(indexer_from(s(" http://flag "), e, d), f);
        // a build whose record names none (and no flag or variable) has none: the commands say what to pass
        assert_eq!(indexer_from(None, None, None), None);
    }

    #[test]
    fn chunked_bodies() {
        assert_eq!(dechunk(b"3\r\nabc\r\n2;x=1\r\nde\r\n0\r\n\r\n").unwrap(), b"abcde");
        assert!(dechunk(b"5\r\nab").is_err());
        // a chunk size near usize::MAX from a hostile indexer is an error, not an overflow
        assert!(dechunk(b"ffffffffffffffff\r\nab").is_err());
    }
}
