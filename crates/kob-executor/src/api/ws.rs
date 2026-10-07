//! WebSocket feed at `/v1/ws`.
//!
//! Client frames (JSON text): `{"op":"subscribe","channels":[..]}`, `{"op":"unsubscribe","channels":[..]}`,
//! `{"op":"ping"}`. Channels: `health`, `reorg`, `fills`, `fills:<token>`, `book:<token>`,
//! `order:<covenant id>` (ids are 64 hex characters).
//!
//! Server frames: `{"channel":..,"type":..,"data":..}` for feed messages and `{"type":..,"data":..}`
//! for control replies (`subscribed`, `unsubscribed`, `pong`, `error`, `resync`). Book and order
//! frames are notices: clients refetch the REST resource. `resync` means the client fell behind the
//! broadcast and missed events, so it should refetch everything it displays.

use super::{ApiError, App, ClientIp};
use crate::hex::Hash32;
use crate::indexer::status::IndexEvent;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};
use std::net::IpAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::broadcast::error::RecvError;

pub const MAX_MESSAGE_BYTES: usize = 4096;
/// Server Ping (and health frame) cadence, at most: a session idle limit below four times this ticks faster.
const PING_EVERY: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Channel {
    Health,
    Reorg,
    /// All fills, or those of one token.
    Fills(Option<Hash32>),
    Book(Hash32),
    Order(Hash32),
}

impl Channel {
    pub fn parse(s: &str) -> Result<Channel, String> {
        let h = |x: &str| Hash32::parse(x).map_err(|_| format!("invalid channel `{s}`: id must be 64 hex characters"));
        match s.split_once(':') {
            None => match s {
                "health" => Ok(Channel::Health),
                "reorg" => Ok(Channel::Reorg),
                "fills" => Ok(Channel::Fills(None)),
                _ => Err(format!("unknown channel `{s}`")),
            },
            Some(("fills", t)) => Ok(Channel::Fills(Some(h(t)?))),
            Some(("book", t)) => Ok(Channel::Book(h(t)?)),
            Some(("order", c)) => Ok(Channel::Order(h(c)?)),
            Some(_) => Err(format!("unknown channel `{s}`")),
        }
    }

    pub fn name(&self) -> String {
        match self {
            Channel::Health => "health".into(),
            Channel::Reorg => "reorg".into(),
            Channel::Fills(None) => "fills".into(),
            Channel::Fills(Some(t)) => format!("fills:{t}"),
            Channel::Book(t) => format!("book:{t}"),
            Channel::Order(c) => format!("order:{c}"),
        }
    }
}

fn frame(channel: &Channel, ty: &str, data: Value) -> Value {
    json!({"channel": channel.name(), "type": ty, "data": data})
}

/// The frames one committed batch produces for a set of subscriptions (deterministic order).
pub fn frames_for(ev: &IndexEvent, subs: &BTreeSet<Channel>) -> Vec<Value> {
    let mut out = Vec::new();
    for ch in subs {
        match ch {
            Channel::Health => out.push(frame(
                ch,
                "cursor",
                json!({
                    "cursor_hash": ev.cursor_hash,
                    "cursor_daa": ev.cursor_daa,
                    "added_blocks": ev.added_blocks,
                    "reverted_blocks": ev.reverted_blocks,
                }),
            )),
            Channel::Reorg => {
                if ev.reverted_blocks > 0 {
                    out.push(frame(
                        ch,
                        "reorg",
                        json!({"reverted_blocks": ev.reverted_blocks, "added_blocks": ev.added_blocks, "cursor_daa": ev.cursor_daa}),
                    ));
                }
            }
            Channel::Fills(t) => {
                for f in &ev.fills {
                    if t.is_none() || f.token == *t {
                        out.push(frame(ch, "fill", serde_json::to_value(f).unwrap_or(Value::Null)));
                    }
                }
            }
            Channel::Book(t) => {
                if ev.tokens.contains(t) {
                    out.push(frame(ch, "book", json!({"token": t, "cursor_daa": ev.cursor_daa})));
                }
            }
            Channel::Order(c) => {
                if ev.orders.contains(c) {
                    out.push(frame(ch, "order", json!({"covenant_id": c, "cursor_daa": ev.cursor_daa})));
                }
            }
        }
    }
    out
}

/// Apply one client frame to the subscription set and return the reply.
pub fn handle_op(text: &str, subs: &mut BTreeSet<Channel>, max_subs: usize) -> Value {
    let msg: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => return json!({"type": "error", "data": {"message": "invalid json"}}),
    };
    let names = |m: &Value| -> Vec<String> {
        m.get("channels")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
            .unwrap_or_default()
    };
    match msg.get("op").and_then(Value::as_str) {
        Some("ping") => json!({"type": "pong"}),
        Some("subscribe") => {
            let mut errors = Vec::new();
            for n in names(&msg) {
                match Channel::parse(&n) {
                    Err(e) => errors.push(e),
                    Ok(c) => {
                        if !subs.contains(&c) && subs.len() >= max_subs {
                            errors.push(format!("subscription limit of {max_subs} reached"));
                            break;
                        }
                        subs.insert(c);
                    }
                }
            }
            let chans: Vec<String> = subs.iter().map(Channel::name).collect();
            json!({"type": "subscribed", "data": {"channels": chans, "errors": errors}})
        }
        Some("unsubscribe") => {
            for n in names(&msg) {
                if let Ok(c) = Channel::parse(&n) {
                    subs.remove(&c);
                }
            }
            let chans: Vec<String> = subs.iter().map(Channel::name).collect();
            json!({"type": "unsubscribed", "data": {"channels": chans}})
        }
        _ => json!({"type": "error", "data": {"message": "unknown op"}}),
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum WsReject {
    Total,
    PerIp,
    /// The IPv6 site (all /64s of one /48) holds its share already.
    PerSite,
}

/// The caps of [`WsRegistry::try_acquire`].
#[derive(Clone, Copy, Debug)]
pub struct WsLimits {
    pub max_total: usize,
    pub max_per_ip: usize,
    /// IPv6 clients are counted per this prefix (64).
    pub prefix_bits: u8,
    /// Connections of one IPv6 site together; 0 = no site cap.
    pub max_per_site: usize,
    /// Leading bits of an IPv6 site (48); 0 = no site cap.
    pub site_prefix_bits: u8,
}

impl WsLimits {
    pub fn of(cfg: &crate::config::ApiConfig) -> WsLimits {
        WsLimits {
            max_total: cfg.max_ws_connections,
            max_per_ip: cfg.max_ws_per_ip,
            prefix_bits: cfg.rate_limit.ipv6_prefix_bits,
            max_per_site: cfg.max_ws_per_site,
            site_prefix_bits: cfg.rate_limit.ipv6_site_prefix_bits,
        }
    }
}

#[derive(Default)]
struct RegistryInner {
    total: AtomicUsize,
    per_ip: Mutex<HashMap<IpAddr, usize>>,
    /// Site keys in their own table (a /48 key equals the key of its first /64).
    per_site: Mutex<HashMap<IpAddr, usize>>,
}

/// Counts open WebSocket connections, in total and per client address.
#[derive(Clone, Default)]
pub struct WsRegistry(Arc<RegistryInner>);

/// Releases its slot on drop.
pub struct WsGuard {
    reg: Arc<RegistryInner>,
    ip: IpAddr,
    site: Option<IpAddr>,
}

fn release(map: &mut HashMap<IpAddr, usize>, key: &IpAddr) {
    if let Some(n) = map.get_mut(key) {
        *n = n.saturating_sub(1);
        if *n == 0 {
            map.remove(key);
        }
    }
}

impl WsRegistry {
    pub fn try_acquire(&self, ip: IpAddr, l: &WsLimits) -> Result<WsGuard, WsReject> {
        let key = super::client_ip::rate_key(ip, l.prefix_bits);
        let site = (l.max_per_site > 0 && l.site_prefix_bits > 0 && super::client_ip::unmap(ip).is_ipv6())
            .then(|| super::client_ip::rate_key(ip, l.site_prefix_bits));
        let mut per = self.0.per_ip.lock().unwrap_or_else(|e| e.into_inner());
        let mut sites = self.0.per_site.lock().unwrap_or_else(|e| e.into_inner());
        if self.0.total.load(Ordering::SeqCst) >= l.max_total {
            return Err(WsReject::Total);
        }
        if per.get(&key).copied().unwrap_or(0) >= l.max_per_ip {
            return Err(WsReject::PerIp);
        }
        if let Some(s) = site {
            if sites.get(&s).copied().unwrap_or(0) >= l.max_per_site {
                return Err(WsReject::PerSite);
            }
            *sites.entry(s).or_insert(0) += 1;
        }
        *per.entry(key).or_insert(0) += 1;
        self.0.total.fetch_add(1, Ordering::SeqCst);
        Ok(WsGuard { reg: self.0.clone(), ip: key, site })
    }

    pub fn total(&self) -> usize {
        self.0.total.load(Ordering::SeqCst)
    }
}

impl Drop for WsGuard {
    fn drop(&mut self) {
        let mut per = self.reg.per_ip.lock().unwrap_or_else(|e| e.into_inner());
        let mut sites = self.reg.per_site.lock().unwrap_or_else(|e| e.into_inner());
        release(&mut per, &self.ip);
        if let Some(s) = &self.site {
            release(&mut sites, s);
        }
        self.reg.total.fetch_sub(1, Ordering::SeqCst);
    }
}

pub async fn handler(State(app): State<Arc<App>>, Extension(ClientIp(ip)): Extension<ClientIp>, ws: WebSocketUpgrade) -> Response {
    let guard = match app.ws.try_acquire(ip, &WsLimits::of(&app.state.cfg)) {
        Ok(g) => g,
        Err(WsReject::Total) => {
            let mut e = ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "ws_capacity", "too many WebSocket connections");
            e.retry_after = Some(5);
            return e.into_response();
        }
        Err(WsReject::PerIp) => {
            let mut e =
                ApiError::new(StatusCode::TOO_MANY_REQUESTS, "ws_per_ip_limit", "too many WebSocket connections from this address");
            e.retry_after = Some(5);
            return e.into_response();
        }
        Err(WsReject::PerSite) => {
            let mut e =
                ApiError::new(StatusCode::TOO_MANY_REQUESTS, "ws_per_site_limit", "too many WebSocket connections from this network");
            e.retry_after = Some(5);
            return e.into_response();
        }
    };
    ws.max_message_size(MAX_MESSAGE_BYTES).max_frame_size(MAX_MESSAGE_BYTES).on_upgrade(move |socket| session(socket, app, guard))
}

/// Longest a single frame may take to leave: a client that never reads would otherwise park its session inside
/// `send` past every idle timeout.
const SEND_TIMEOUT: Duration = Duration::from_secs(10);

async fn send(socket: &mut WebSocket, v: &Value) -> bool {
    matches!(tokio::time::timeout(SEND_TIMEOUT, socket.send(Message::Text(v.to_string().into()))).await, Ok(Ok(())))
}

async fn session(mut socket: WebSocket, app: Arc<App>, _guard: WsGuard) {
    let mut rx = app.state.events.subscribe();
    let mut shutdown = app.shutdown.subscribe();
    let max_subs = app.state.cfg.max_ws_subscriptions;
    let max_msgs = app.state.cfg.ws_client_msgs_per_sec.max(1);
    let mut subs: BTreeSet<Channel> = BTreeSet::new();
    // idle = no APPLICATION message (a subscription change or `{"op":"ping"}`) for this long; protocol Ping / Pong frames
    // do not count, or a client that only auto-answers our Pings would hold its slot forever
    let idle_timeout = Duration::from_millis(app.state.cfg.ws_idle_timeout_ms.max(1));
    let mut tick = tokio::time::interval(PING_EVERY.min(idle_timeout / 4).max(Duration::from_millis(10)));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tick.tick().await;
    let mut last_rx = Instant::now();
    let mut window = (Instant::now(), 0u32);
    if *shutdown.borrow() {
        return;
    }
    loop {
        tokio::select! {
            _ = shutdown.changed() => {
                let _ = socket.send(Message::Close(Some(CloseFrame { code: 1001, reason: "server shutting down".into() }))).await;
                break;
            }
            msg = socket.recv() => {
                let Some(Ok(msg)) = msg else { break };
                if matches!(msg, Message::Text(_)) {
                    last_rx = Instant::now();
                }
                if window.0.elapsed() >= Duration::from_secs(1) {
                    window = (Instant::now(), 0);
                }
                window.1 += 1;
                if window.1 > max_msgs {
                    let _ = socket.send(Message::Close(Some(CloseFrame { code: 1008, reason: "message rate exceeded".into() }))).await;
                    break;
                }
                match msg {
                    Message::Text(t) => {
                        let reply = handle_op(t.as_str(), &mut subs, max_subs);
                        if !send(&mut socket, &reply).await {
                            break;
                        }
                    }
                    Message::Close(_) => break,
                    Message::Binary(_) => {
                        let _ = send(&mut socket, &json!({"type": "error", "data": {"message": "binary frames are not supported"}})).await;
                    }
                    Message::Ping(_) | Message::Pong(_) => {}
                }
            }
            ev = rx.recv() => {
                match ev {
                    Ok(e) => {
                        for f in frames_for(&e, &subs) {
                            if !send(&mut socket, &f).await {
                                return;
                            }
                        }
                    }
                    Err(RecvError::Lagged(n)) => {
                        if !send(&mut socket, &json!({"type": "resync", "data": {"missed_events": n}})).await {
                            break;
                        }
                    }
                    Err(RecvError::Closed) => break,
                }
            }
            _ = tick.tick() => {
                if last_rx.elapsed() > idle_timeout {
                    let close = Message::Close(Some(CloseFrame { code: 1000, reason: "idle".into() }));
                    let _ = tokio::time::timeout(SEND_TIMEOUT, socket.send(close)).await;
                    break;
                }
                if !matches!(tokio::time::timeout(SEND_TIMEOUT, socket.send(Message::Ping(Vec::new().into()))).await, Ok(Ok(()))) {
                    break;
                }
                if subs.contains(&Channel::Health) {
                    let s = app.state.health.snapshot();
                    let f = frame(&Channel::Health, "health", json!({
                        "state": s.state, "cursor_daa": s.cursor_daa, "node_daa": s.node_daa, "lag_daa": s.lag_daa(),
                    }));
                    if !send(&mut socket, &f).await {
                        break;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer::status::FillNotice;

    fn h(b: u8) -> Hash32 {
        Hash32([b; 32])
    }

    fn event() -> IndexEvent {
        IndexEvent {
            cursor_hash: Some(h(9)),
            cursor_daa: 500,
            reverted_blocks: 0,
            added_blocks: 3,
            orders: vec![h(1)],
            tokens: vec![h(2)],
            fills: vec![
                FillNotice {
                    order: h(1),
                    token: Some(h(2)),
                    side: 1,
                    price: Some(5),
                    amount: 2_000,
                    payout: None,
                    txid: h(7),
                    block: h(8),
                    daa: 499,
                },
                FillNotice {
                    order: h(3),
                    token: Some(h(4)),
                    side: 2,
                    price: Some(6),
                    amount: 1_000,
                    payout: None,
                    txid: h(7),
                    block: h(8),
                    daa: 499,
                },
            ],
        }
    }

    #[test]
    fn channel_parse_roundtrip() {
        for c in [
            Channel::Health,
            Channel::Reorg,
            Channel::Fills(None),
            Channel::Fills(Some(h(1))),
            Channel::Book(h(2)),
            Channel::Order(h(3)),
        ] {
            assert_eq!(Channel::parse(&c.name()).unwrap(), c);
        }
        assert!(Channel::parse("book:zz").is_err());
        assert!(Channel::parse("nope").is_err());
        assert!(Channel::parse("book").is_err());
        assert!(Channel::parse("other:00").is_err());
    }

    #[test]
    fn frames_match_subscriptions() {
        let ev = event();
        let mut subs = BTreeSet::new();
        assert!(frames_for(&ev, &subs).is_empty());
        subs.insert(Channel::Fills(Some(h(2))));
        subs.insert(Channel::Book(h(2)));
        subs.insert(Channel::Book(h(5)));
        subs.insert(Channel::Order(h(1)));
        subs.insert(Channel::Order(h(6)));
        subs.insert(Channel::Reorg);
        let frames = frames_for(&ev, &subs);
        let kinds: Vec<(String, String)> =
            frames.iter().map(|f| (f["channel"].as_str().unwrap().to_string(), f["type"].as_str().unwrap().to_string())).collect();
        assert_eq!(kinds.len(), 3, "{kinds:?}");
        assert!(kinds.contains(&(format!("fills:{}", h(2)), "fill".into())));
        assert!(kinds.contains(&(format!("book:{}", h(2)), "book".into())));
        assert!(kinds.contains(&(format!("order:{}", h(1)), "order".into())));
        // reorg only fires on reverts
        assert!(!kinds.iter().any(|(c, _)| c == "reorg"));
        let mut ev2 = ev.clone();
        ev2.reverted_blocks = 2;
        let frames = frames_for(&ev2, &BTreeSet::from([Channel::Reorg]));
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0]["data"]["reverted_blocks"], 2);
        // all-fills channel gets both
        let frames = frames_for(&ev, &BTreeSet::from([Channel::Fills(None)]));
        assert_eq!(frames.len(), 2);
        // health gets the cursor on every event
        let frames = frames_for(&ev, &BTreeSet::from([Channel::Health]));
        assert_eq!(frames[0]["data"]["cursor_daa"], 500);
    }

    #[test]
    fn subscription_limit_and_errors() {
        let mut subs = BTreeSet::new();
        let r = handle_op(&format!(r#"{{"op":"subscribe","channels":["bogus","health","fills","book:{}"]}}"#, h(1)), &mut subs, 2);
        assert_eq!(subs.len(), 2);
        assert_eq!(r["type"], "subscribed");
        assert_eq!(r["data"]["errors"].as_array().unwrap().len(), 2);
        // re-subscribing an existing channel at the limit is fine
        let r = handle_op(r#"{"op":"subscribe","channels":["health"]}"#, &mut subs, 2);
        assert!(r["data"]["errors"].as_array().unwrap().is_empty());
        let r = handle_op(r#"{"op":"unsubscribe","channels":["health"]}"#, &mut subs, 2);
        assert_eq!(r["type"], "unsubscribed");
        assert_eq!(subs.len(), 1);
        assert_eq!(handle_op(r#"{"op":"ping"}"#, &mut subs, 2)["type"], "pong");
        assert_eq!(handle_op("not json", &mut subs, 2)["type"], "error");
        assert_eq!(handle_op(r#"{"op":"dance"}"#, &mut subs, 2)["type"], "error");
    }

    #[test]
    fn registry_counts_and_releases() {
        let reg = WsRegistry::default();
        let a: IpAddr = "1.1.1.1".parse().unwrap();
        let b: IpAddr = "2.2.2.2".parse().unwrap();
        let l = WsLimits { max_total: 3, max_per_ip: 2, prefix_bits: 64, max_per_site: 2, site_prefix_bits: 48 };
        let g1 = reg.try_acquire(a, &l).unwrap();
        let g2 = reg.try_acquire(a, &l).unwrap();
        assert_eq!(reg.try_acquire(a, &l).err(), Some(WsReject::PerIp));
        let g3 = reg.try_acquire(b, &l).unwrap();
        assert_eq!(reg.try_acquire(b, &l).err(), Some(WsReject::Total));
        assert_eq!(reg.total(), 3);
        drop(g1);
        assert!(reg.try_acquire(a, &l).is_ok());
        drop((g2, g3));
        assert_eq!(reg.total(), 0);
        assert!(reg.0.per_ip.lock().unwrap().is_empty());
    }

    #[test]
    fn the_64s_of_one_ipv6_site_share_the_site_cap() {
        let reg = WsRegistry::default();
        let ip = |s: &str| -> IpAddr { s.parse().unwrap() };
        let l = WsLimits { max_total: 100, max_per_ip: 2, prefix_bits: 64, max_per_site: 3, site_prefix_bits: 48 };
        let g1 = reg.try_acquire(ip("2001:db8:7:1::1"), &l).unwrap();
        let g2 = reg.try_acquire(ip("2001:db8:7:2::1"), &l).unwrap();
        let g3 = reg.try_acquire(ip("2001:db8:7::1"), &l).unwrap();
        assert_eq!(reg.try_acquire(ip("2001:db8:7:3::1"), &l).err(), Some(WsReject::PerSite));
        // another site and IPv4 are untouched
        let g4 = reg.try_acquire(ip("2001:db8:8:1::1"), &l).unwrap();
        let g5 = reg.try_acquire(ip("1.1.1.1"), &l).unwrap();
        drop(g1);
        let g6 = reg.try_acquire(ip("2001:db8:7:3::1"), &l).unwrap();
        // 0 = no site cap
        let open = WsLimits { max_per_site: 0, ..l };
        let g7 = reg.try_acquire(ip("2001:db8:7:4::1"), &open).unwrap();
        drop((g2, g3, g4, g5, g6, g7));
        assert_eq!(reg.total(), 0);
        assert!(reg.0.per_site.lock().unwrap().is_empty() && reg.0.per_ip.lock().unwrap().is_empty());
    }
}
