//! The executor's WebSocket address, derived from the indexer URL (`docs/api-reference.md` section 4: `ws://<host>/v1/ws`).

use crate::trade::http::{b64, shown};

/// The WebSocket route under the indexer's base path.
pub const WS_PATH: &str = "/v1/ws";

/// Where `kob watch` connects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WsTarget {
    /// `wss://` (the indexer was `https://`).
    pub secure: bool,
    /// Host without brackets.
    pub host: String,
    /// Port.
    pub port: u16,
    /// The base path of the indexer plus [`WS_PATH`].
    pub path: String,
    /// `Authorization: Basic` value of credentials in the URL (`http://user:pw@host`).
    pub auth: Option<String>,
    shown: String,
}

impl WsTarget {
    /// `ws://host:port/v1/ws` without credentials (the port is omitted when it is the scheme's default).
    pub fn url(&self) -> String {
        let default = if self.secure { 443 } else { 80 };
        let host = if self.host.contains(':') { format!("[{}]", self.host) } else { self.host.clone() };
        let port = if self.port == default { String::new() } else { format!(":{}", self.port) };
        format!("{}://{host}{port}{}", if self.secure { "wss" } else { "ws" }, self.path)
    }

    /// The indexer as messages show it (no credentials).
    pub fn shown(&self) -> &str {
        &self.shown
    }
}

/// The WebSocket target of an indexer URL: `http` becomes `ws`, `https` becomes `wss`, the path is the indexer's base path
/// followed by `/v1/ws`.
pub fn ws_target(indexer: &str) -> Result<WsTarget, String> {
    let who = shown(indexer);
    let (secure, rest) = if let Some(r) = indexer.strip_prefix("http://") {
        (false, r)
    } else if let Some(r) = indexer.strip_prefix("https://") {
        (true, r)
    } else {
        return Err(format!("indexer {who}: the WebSocket address is derived from an http:// or https:// URL"));
    };
    let (authority, base) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let (auth, hostport) = match authority.rsplit_once('@') {
        Some((cred, hp)) => (Some(b64(cred.as_bytes())), hp),
        None => (None, authority),
    };
    let default = if secure { 443 } else { 80 };
    let bad_port = || format!("indexer {who}: bad port");
    let (host, port) = if let Some(inner) = hostport.strip_prefix('[') {
        // an IPv6 literal: [::1] or [::1]:8091
        let (h, after) = inner.split_once(']').ok_or_else(|| format!("indexer {who}: bad host"))?;
        match after.strip_prefix(':') {
            Some(p) => (h, p.parse().map_err(|_| bad_port())?),
            None if after.is_empty() => (h, default),
            None => return Err(format!("indexer {who}: bad host")),
        }
    } else {
        match hostport.rsplit_once(':') {
            Some(("", _)) => ("", default),
            Some((h, p)) => (h, p.parse().map_err(|_| bad_port())?),
            None => (hostport, default),
        }
    };
    if host.is_empty() {
        return Err(format!("indexer {who}: no host"));
    }
    let base = base.split(['?', '#']).next().unwrap_or("").trim_end_matches('/');
    Ok(WsTarget { secure, host: host.to_string(), port, path: format!("{base}{WS_PATH}"), auth, shown: who })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(i: &str) -> String {
        ws_target(i).unwrap().url()
    }

    #[test]
    fn http_becomes_ws_and_https_wss() {
        assert_eq!(url("http://127.0.0.1:8091"), "ws://127.0.0.1:8091/v1/ws");
        assert_eq!(url("http://127.0.0.1:8091/"), "ws://127.0.0.1:8091/v1/ws");
        assert_eq!(url("http://example.org"), "ws://example.org/v1/ws");
        assert_eq!(url("https://example.org"), "wss://example.org/v1/ws");
        assert_eq!(url("https://example.org:443/"), "wss://example.org/v1/ws");
        assert_eq!(url("https://example.org:8443"), "wss://example.org:8443/v1/ws");
        // the indexer's base path is kept
        assert_eq!(url("http://h:1/kob/api/"), "ws://h:1/kob/api/v1/ws");
        assert_eq!(url("http://[::1]:8091"), "ws://[::1]:8091/v1/ws");
        assert_eq!(url("https://[::1]"), "wss://[::1]/v1/ws");
    }

    #[test]
    fn credentials_go_to_the_header_never_to_the_url_or_the_messages() {
        let t = ws_target("http://bob:pw@h:2/api").unwrap();
        assert_eq!(t.url(), "ws://h:2/api/v1/ws");
        assert_eq!(t.auth.as_deref(), Some("Ym9iOnB3"));
        assert!(!t.shown().contains("pw") && !t.shown().contains("bob"), "{}", t.shown());
        let e = ws_target("ftp://bob:pw@h").unwrap_err();
        assert!(!e.contains("pw") && e.contains("http://"), "{e}");
    }

    #[test]
    fn bad_urls_are_refused() {
        for bad in ["", "h:1", "ws://h", "http://", "http://:80", "http://h:x", "http://[::1", "http://[::1]x"] {
            assert!(ws_target(bad).is_err(), "{bad}");
        }
    }
}
