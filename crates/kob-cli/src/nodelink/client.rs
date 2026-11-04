//! A blocking Borsh wRPC connection: the calls of the JSON client (`wrpc::Conn`), carried over the node's Borsh encoding
//! ([`super::bridge`]) on a `ws://` or `wss://` socket. The public nodes the resolvers name serve this encoding only.

use std::time::Duration;

use serde_json::Value;
use tungstenite::client::IntoClientRequest;
use tungstenite::http::HeaderValue;
use tungstenite::{Message, WebSocket};

use super::bridge;
use super::frame::{parse_reply, Reply};
use crate::tls::Stream;
use crate::trade::http::b64;
use crate::wrpc::{shown, WrpcError};

/// The parts of a node URL a connection needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// `wss://`.
    pub secure: bool,
    /// Host without brackets.
    pub host: String,
    /// Port.
    pub port: u16,
    /// `Authorization: Basic` value of credentials in the URL.
    pub auth: Option<String>,
    /// The URL without credentials (what the handshake requests).
    pub url: String,
}

/// Split a `ws://` / `wss://` URL (default ports 80 / 443; an IPv6 host in brackets).
pub fn target(url: &str) -> Result<Target, String> {
    let bad = |what: &str| format!("node {}: {what}", shown(url));
    let (secure, rest) = if let Some(r) = url.strip_prefix("wss://") {
        (true, r)
    } else if let Some(r) = url.strip_prefix("ws://") {
        (false, r)
    } else {
        return Err(bad("not a ws:// or wss:// URL"));
    };
    let (authority, path) = match rest.find(['/', '?', '#']) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let (auth, hostport) = match authority.rsplit_once('@') {
        Some((cred, hp)) => (Some(b64(cred.as_bytes())), hp),
        None => (None, authority),
    };
    let default = if secure { 443 } else { 80 };
    let (host, port) = if let Some(inner) = hostport.strip_prefix('[') {
        let (h, after) = inner.split_once(']').ok_or_else(|| bad("bad host"))?;
        match after.strip_prefix(':') {
            Some(p) => (h, p.parse().map_err(|_| bad("bad port"))?),
            None if after.is_empty() => (h, default),
            None => return Err(bad("bad host")),
        }
    } else {
        match hostport.rsplit_once(':') {
            Some(("", _)) => ("", default),
            Some((h, p)) => (h, p.parse().map_err(|_| bad("bad port"))?),
            None => (hostport, default),
        }
    };
    if host.is_empty() {
        return Err(bad("no host"));
    }
    let shown_host = if host.contains(':') { format!("[{host}]") } else { host.to_string() };
    let path = if path.starts_with('/') {
        path.to_string()
    } else if path.is_empty() {
        "/".to_string()
    } else {
        format!("/{path}")
    };
    let url = format!("{}://{shown_host}:{port}{path}", if secure { "wss" } else { "ws" });
    Ok(Target { secure, host: host.to_string(), port, auth, url })
}

/// One Borsh wRPC connection; requests are sent one at a time.
pub struct BorshConn {
    ws: WebSocket<Stream>,
    next_id: u64,
}

impl BorshConn {
    /// Connect to a Borsh endpoint (`wss://host/kaspa/mainnet/wrpc/borsh`, or an own node's `ws://host:17110`).
    pub fn connect(url: &str, timeout: Duration) -> Result<BorshConn, WrpcError> {
        let t = target(url).map_err(WrpcError::Socket)?;
        let socket = |e: String| WrpcError::Socket(format!("node {}: {e}", shown(url)));
        let stream = crate::tls::connect(t.secure, &t.host, t.port, timeout).map_err(socket)?;
        let mut req = t.url.as_str().into_client_request().map_err(|e| socket(e.to_string()))?;
        if let Some(a) = &t.auth {
            req.headers_mut()
                .insert("Authorization", HeaderValue::from_str(&format!("Basic {a}")).map_err(|e| socket(e.to_string()))?);
        }
        let config = tungstenite::protocol::WebSocketConfig {
            max_message_size: Some(16 << 20),
            max_frame_size: Some(16 << 20),
            ..Default::default()
        };
        match tungstenite::client::client_with_config(req, stream, Some(config)) {
            Ok((ws, _)) => Ok(BorshConn { ws, next_id: 1 }),
            Err(tungstenite::HandshakeError::Failure(tungstenite::Error::Http(r))) => {
                Err(socket(format!("the upgrade was refused: HTTP {}", r.status().as_u16())))
            }
            Err(tungstenite::HandshakeError::Failure(e)) => Err(socket(kob_protocol::redact::scrub_url(&e.to_string(), url))),
            Err(tungstenite::HandshakeError::Interrupted(_)) => Err(socket("the handshake timed out".into())),
        }
    }

    /// Send `method` with the JSON `params` of the JSON wRPC and return the answer as the JSON `params` of that wRPC.
    pub fn call(&mut self, method: &str, params: Value) -> Result<Value, WrpcError> {
        let enc = bridge::encode(method, &params).map_err(WrpcError::Protocol)?;
        let id = self.next_id;
        self.next_id += 1;
        self.ws.send(Message::binary(enc.frame(id))).map_err(|e| WrpcError::Socket(e.to_string()))?;
        loop {
            match self.ws.read().map_err(|e| WrpcError::Socket(e.to_string()))? {
                Message::Binary(bytes) => match parse_reply(&bytes[..]).map_err(WrpcError::Protocol)? {
                    Reply::Ok { id: Some(i), body } if i == id => return bridge::decode(method, body).map_err(WrpcError::Protocol),
                    Reply::Err { id: Some(i), message } if i == id => return Err(WrpcError::Node(message)),
                    // a notification or another request's answer
                    _ => continue,
                },
                Message::Close(_) => return Err(WrpcError::Socket("connection closed before the response".into())),
                _ => continue,
            }
        }
    }

    /// Close the connection (best effort).
    pub fn close(mut self) {
        let _ = self.ws.close(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets() {
        let t = target("wss://iris.kaspa.blue/kaspa/mainnet/wrpc/borsh").unwrap();
        assert_eq!((t.secure, t.host.as_str(), t.port), (true, "iris.kaspa.blue", 443));
        assert_eq!(t.url, "wss://iris.kaspa.blue:443/kaspa/mainnet/wrpc/borsh");
        let t = target("ws://127.0.0.1:17210").unwrap();
        assert_eq!((t.secure, t.port, t.url.as_str()), (false, 17210, "ws://127.0.0.1:17210/"));
        let t = target("ws://[::1]:17110/x?y=1").unwrap();
        assert_eq!((t.host.as_str(), t.url.as_str()), ("::1", "ws://[::1]:17110/x?y=1"));
        let t = target("ws://bob:pw@h:1/a").unwrap();
        assert_eq!((t.auth.as_deref(), t.url.as_str()), (Some("Ym9iOnB3"), "ws://h:1/a"));
        for bad in ["", "h:1", "http://h", "ws://", "ws://:1", "ws://h:x", "ws://[::1"] {
            let e = target(bad).unwrap_err();
            assert!(!e.contains("pw"), "{e}");
        }
        assert!(!target("ws://bob:pw@:1").unwrap_err().contains("pw"));
    }
}
