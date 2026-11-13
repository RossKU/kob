//! The watch loop.
//!
//! It connects to the WebSocket of the executor, subscribes and prints what arrives. It also
//! refetches the REST resources that `book` and `order` notices point to, debounced, and keeps
//! the connection alive with pings. When the connection drops, it reconnects with a growing
//! pause and starts from fresh state, because the server replays nothing, see
//! `docs/api-reference.md`, section 4.4.
//!
//! The loop is blocking and runs in one thread. The socket is polled with a short read timeout,
//! so a pending refetch, a ping and Ctrl-C are served between frames.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use kob_protocol::json::to_hex;
use serde_json::{json, Value};
use tungstenite::client::IntoClientRequest;
use tungstenite::http::HeaderValue;
use tungstenite::{Message, WebSocket};

use super::frames;
use super::names::Names;
use super::out::Out;
use super::url::WsTarget;
use crate::tls::Stream;
use crate::trade::data;
use crate::trade::http;
use crate::trade::opts::{Common, Parsed};
use crate::trade::views;

/// What a watch listens to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// Every fill, or the fills of one token.
    Fills(Option<[u8; 32]>),
    /// The book of a token, or of a pair with both tokens, which is reprinted on a notice.
    Book([u8; 32], Option<[u8; 32]>),
    /// Orders by covenant id, whose state is printed on a notice.
    Orders(Vec<[u8; 32]>),
    /// The progress and health of the indexer.
    Health,
}

/// A REST resource that a notice asks to be read again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Key {
    Book,
    Order([u8; 32]),
}

impl Kind {
    /// The channels to subscribe to: always `reorg`, plus those of the kind.
    pub fn channels(&self) -> Vec<String> {
        let mut v = vec!["reorg".to_string()];
        match self {
            Kind::Fills(None) => v.push("fills".into()),
            Kind::Fills(Some(t)) => v.push(format!("fills:{}", to_hex(t))),
            Kind::Book(a, b) => {
                v.push(format!("book:{}", to_hex(a)));
                if let Some(b) = b {
                    v.push(format!("book:{}", to_hex(b)));
                }
            }
            Kind::Orders(ids) => v.extend(ids.iter().map(|i| format!("order:{}", to_hex(i)))),
            Kind::Health => v.push("health".into()),
        }
        v
    }

    /// What the watch is called in its first line.
    pub fn name(&self) -> String {
        match self {
            Kind::Fills(None) => "fills".into(),
            Kind::Fills(Some(t)) => format!("fills of {}", frames::short(&to_hex(t))),
            Kind::Book(a, None) => format!("the book of {}", frames::short(&to_hex(a))),
            Kind::Book(a, Some(b)) => format!("the book of {} / {}", frames::short(&to_hex(a)), frames::short(&to_hex(b))),
            Kind::Orders(ids) => format!("{} order(s)", ids.len()),
            Kind::Health => "health".into(),
        }
    }

    fn tracked(&self) -> Vec<Key> {
        match self {
            Kind::Book(..) => vec![Key::Book],
            Kind::Orders(ids) => ids.iter().map(|i| Key::Order(*i)).collect(),
            _ => vec![],
        }
    }
}

/// The timings of a watch. Tests shorten them.
#[derive(Debug, Clone)]
pub struct Timing {
    /// A ping every this long. By default the server closes a session that is silent for 60 s.
    pub ping_every: Duration,
    /// How long a notice waits for more before its resource is read again.
    pub debounce: Duration,
    /// The least time between two reads of a resource.
    pub min_gap: Duration,
    /// The read timeout of the socket.
    ///
    /// It is how often the loop looks at the clock and at Ctrl-C.
    pub poll: Duration,
    /// The first pause before a reconnect, doubled up to `backoff_max`.
    pub backoff_start: Duration,
    /// The longest pause before a reconnect.
    pub backoff_max: Duration,
    /// The timeout of connect, handshake and subscribe.
    pub connect: Duration,
}

impl Default for Timing {
    fn default() -> Timing {
        Timing {
            ping_every: Duration::from_secs(25),
            debounce: Duration::from_millis(500),
            min_gap: Duration::from_secs(2),
            poll: Duration::from_millis(250),
            backoff_start: Duration::from_secs(1),
            backoff_max: Duration::from_secs(30),
            connect: Duration::from_secs(10),
        }
    }
}

/// The longest `Retry-After` that is honoured.
const RETRY_AFTER_MAX: Duration = Duration::from_secs(60);
/// Channels per subscribe message.
///
/// A message is at most 4096 bytes, and a channel name with an id is about 70.
const CHANNELS_PER_MESSAGE: usize = 40;

/// A watch.
pub struct Watch {
    /// The WebSocket address.
    pub target: WsTarget,
    /// The indexer, for REST.
    pub indexer: String,
    /// What to listen to.
    pub kind: Kind,
    /// The shared options, such as network and registry.
    pub common: Common,
    /// The arguments that `kob book` reads, for a book watch.
    pub parsed: Parsed,
    /// The token lists.
    pub names: Names,
    /// The timings.
    pub timing: Timing,
}

struct ConnectError {
    msg: String,
    fatal: bool,
    retry_after: Option<Duration>,
}

enum End {
    Stop,
    Dropped { why: String, subscribed: bool },
    Fatal(String),
}

#[derive(Default)]
struct Pending {
    due: BTreeMap<Key, Instant>,
    last: BTreeMap<Key, Instant>,
}

impl Pending {
    /// Handles a notice: the resource is read after the debounce, and no sooner than `min_gap`
    /// after its last read.
    fn schedule(&mut self, key: Key, now: Instant, t: &Timing, debounce: bool) {
        if self.due.contains_key(&key) {
            return;
        }
        let earliest = if debounce { now + t.debounce } else { now };
        let due = match self.last.get(&key) {
            Some(l) => earliest.max(*l + t.min_gap),
            None => earliest,
        };
        self.due.insert(key, due);
    }

    fn take_due(&mut self, now: Instant) -> Vec<Key> {
        let keys: Vec<Key> = self.due.iter().filter(|(_, d)| **d <= now).map(|(k, _)| *k).collect();
        for k in &keys {
            self.due.remove(k);
            self.last.insert(*k, now);
        }
        keys
    }
}

fn sleep_unless_stopped(d: Duration, stop: &AtomicBool) {
    let end = Instant::now() + d;
    while !stop.load(Ordering::Relaxed) {
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        std::thread::sleep(left.min(Duration::from_millis(100)));
    }
}

fn is_timeout(e: &std::io::Error) -> bool {
    matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut)
}

/// Reads one frame, or returns `None` when nothing arrived within the read timeout or a control
/// frame was handled.
fn poll(ws: &mut WebSocket<Stream>) -> Result<Option<Value>, String> {
    match ws.read() {
        Ok(Message::Text(t)) => {
            Ok(Some(serde_json::from_str(t.as_str()).unwrap_or_else(|_| json!({"type": "unparsed", "data": {"text": t.as_str()}}))))
        }
        Ok(Message::Close(f)) => Err(match f {
            Some(f) if f.reason.is_empty() => format!("the server closed the connection (code {})", u16::from(f.code)),
            Some(f) => format!("the server closed the connection (code {}: {})", u16::from(f.code), f.reason),
            None => "the server closed the connection".into(),
        }),
        // Tungstenite queued the pong, so send it now.
        Ok(Message::Ping(_)) => ws.flush().map(|_| None).or_else(|e| {
            if matches!(&e, tungstenite::Error::Io(i) if is_timeout(i)) {
                Ok(None)
            } else {
                Err(e.to_string())
            }
        }),
        Ok(_) => Ok(None),
        Err(tungstenite::Error::Io(e)) if is_timeout(&e) => Ok(None),
        Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => Err("the connection closed".into()),
        Err(tungstenite::Error::Io(e)) => Err(crate::tls::describe_io(&e)),
        Err(e) => Err(e.to_string()),
    }
}

impl Watch {
    /// Runs until Ctrl-C (`stop`), a fatal error or a closed output.
    ///
    /// The exit code is 0 after a stop or a closed output, and 1 after a fatal error.
    pub fn run(&mut self, stop: &AtomicBool, out: &mut Out) -> i32 {
        let mut backoff = self.timing.backoff_start;
        let mut was_subscribed = false;
        // The first line names the network and the indexer that it watches.
        let channels = self.kind.channels();
        out.event(
            &format!("watching {} on network {} (indexer {})", self.kind.name(), self.common.network, self.target.shown()),
            &json!({"type": "start", "data": {"network": self.common.network, "indexer": self.target.shown(), "channels": channels}}),
        );
        loop {
            if stop.load(Ordering::Relaxed) || out.broken {
                return 0;
            }
            let pause = match self.connect() {
                Err(e) if e.fatal => {
                    out.error(&e.msg);
                    return 1;
                }
                Err(e) => {
                    let pause = e.retry_after.unwrap_or(backoff);
                    out.note(&format!("cannot connect: {}; retrying in {}s", e.msg, pause.as_secs().max(1)));
                    pause
                }
                Ok(ws) => match self.session(ws, stop, out, was_subscribed) {
                    End::Stop => return 0,
                    End::Fatal(m) => {
                        out.error(&m);
                        return 1;
                    }
                    End::Dropped { why, subscribed } => {
                        was_subscribed |= subscribed;
                        if subscribed {
                            backoff = self.timing.backoff_start;
                        }
                        out.note(&format!("connection lost: {why}; reconnecting in {}s", backoff.as_secs().max(1)));
                        backoff
                    }
                },
            };
            sleep_unless_stopped(pause, stop);
            backoff = (backoff * 2).min(self.timing.backoff_max);
        }
    }

    fn connect(&self) -> Result<WebSocket<Stream>, ConnectError> {
        let t = &self.target;
        let fail = |msg: String| ConnectError { msg, fatal: false, retry_after: None };
        let stream = crate::tls::connect(t.secure, &t.host, t.port, self.timing.connect)
            .map_err(|e| fail(format!("cannot connect to the indexer {} ({e})", t.shown())))?;
        let mut req = t.url().into_client_request().map_err(|e| fail(e.to_string()))?;
        if let Some(a) = &t.auth {
            req.headers_mut().insert("Authorization", HeaderValue::from_str(&format!("Basic {a}")).map_err(|e| fail(e.to_string()))?);
        }
        req.headers_mut().insert("User-Agent", HeaderValue::from_static("kob-cli"));
        let config = tungstenite::protocol::WebSocketConfig {
            max_message_size: Some(1 << 20),
            max_frame_size: Some(1 << 20),
            ..Default::default()
        };
        match tungstenite::client::client_with_config(req, stream, Some(config)) {
            Ok((ws, _)) => {
                ws.get_ref().tcp().set_read_timeout(Some(self.timing.poll)).map_err(|e| fail(e.to_string()))?;
                Ok(ws)
            }
            Err(tungstenite::HandshakeError::Failure(tungstenite::Error::Http(resp))) => {
                let status = resp.status().as_u16();
                let retry_after = resp
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.trim().parse::<u64>().ok())
                    .map(|s| Duration::from_secs(s).min(RETRY_AFTER_MAX));
                let body = resp.body().as_deref().map(String::from_utf8_lossy).unwrap_or_default();
                let code = serde_json::from_str::<Value>(&body)
                    .ok()
                    .and_then(|v| v.pointer("/error/message").and_then(Value::as_str).map(str::to_owned))
                    .unwrap_or_default();
                let msg = format!("{}: the upgrade was refused: HTTP {status} {code}", t.shown());
                let fatal = matches!(status, 400 | 401 | 403 | 404 | 405);
                let msg = if status == 404 {
                    format!("{msg} (this endpoint has no /v1/ws route: it is not a KOB executor, or an older one)")
                } else {
                    msg
                };
                Err(ConnectError { msg: msg.trim().to_string(), fatal, retry_after })
            }
            Err(tungstenite::HandshakeError::Failure(e)) => Err(fail(format!("{}: {e}", t.shown()))),
            Err(tungstenite::HandshakeError::Interrupted(_)) => Err(fail(format!("{}: the handshake timed out", t.shown()))),
        }
    }

    /// One connection: subscribes, then serves frames, pings and refetches until it ends.
    fn session(&mut self, mut ws: WebSocket<Stream>, stop: &AtomicBool, out: &mut Out, reconnect: bool) -> End {
        let timing = self.timing.clone();
        let mut pending = Pending::default();
        // 1. Subscribe, before any refetch, so that a change in between is not lost.
        let wanted = self.kind.channels();
        let mut held: Vec<String> = vec![];
        let mut errors: Vec<String> = vec![];
        for chunk in wanted.chunks(CHANNELS_PER_MESSAGE) {
            let msg = json!({"op": "subscribe", "channels": chunk}).to_string();
            if let Err(e) = ws.send(Message::text(msg)) {
                return End::Dropped { why: e.to_string(), subscribed: false };
            }
            let deadline = Instant::now() + timing.connect;
            loop {
                if stop.load(Ordering::Relaxed) {
                    let _ = ws.close(None);
                    return End::Stop;
                }
                if Instant::now() > deadline {
                    return End::Dropped { why: "no reply to the subscription".into(), subscribed: false };
                }
                match poll(&mut ws) {
                    Err(why) => return End::Dropped { why, subscribed: false },
                    Ok(None) => {}
                    Ok(Some(f)) if f["type"] == "subscribed" => {
                        let (chans, errs) = frames::subscribed(&f);
                        held = chans;
                        errors.extend(errs);
                        break;
                    }
                    Ok(Some(f)) => self.on_frame(&f, out, &mut pending),
                }
            }
        }
        for e in &errors {
            out.error(&format!("subscription refused by the server: {e}"));
        }
        if !held.iter().any(|c| c != "reorg") {
            return End::Fatal("the server accepted none of the channels asked for".into());
        }
        if reconnect {
            out.note("reconnected; everything shown is read again (events during the gap are not replayed)");
            if let Err(e) = self.names.reload() {
                out.error(&format!("token lists: {e}"));
            }
        }
        // 2. What is shown is read once now.
        let now = Instant::now();
        for k in self.kind.tracked() {
            pending.schedule(k, now, &timing, false);
        }
        // 3. Serve.
        let mut next_ping = Instant::now() + timing.ping_every;
        loop {
            if stop.load(Ordering::Relaxed) || out.broken {
                let _ = ws.close(None);
                let _ = ws.flush();
                return End::Stop;
            }
            let now = Instant::now();
            if now >= next_ping {
                next_ping = now + timing.ping_every;
                if let Err(e) = ws.send(Message::text(r#"{"op":"ping"}"#)) {
                    return End::Dropped { why: e.to_string(), subscribed: true };
                }
            }
            for key in pending.take_due(now) {
                self.refetch(key, out);
                if stop.load(Ordering::Relaxed) {
                    break;
                }
            }
            match poll(&mut ws) {
                Err(why) => return End::Dropped { why, subscribed: true },
                Ok(None) => {}
                Ok(Some(f)) => self.on_frame(&f, out, &mut pending),
            }
        }
    }

    /// Handles one server frame.
    fn on_frame(&mut self, f: &Value, out: &mut Out, pending: &mut Pending) {
        let data = f.get("data").cloned().unwrap_or(Value::Null);
        let now = Instant::now();
        match f["type"].as_str().unwrap_or("") {
            "pong" | "subscribed" | "unsubscribed" => {}
            "fill" => {
                let info = data["token"].as_str().and_then(kob_protocol::registry::parse_hex32).and_then(|c| self.names.info(&c));
                out.event(&frames::fill_line(&data, info.as_ref()), f);
            }
            "book" => {
                out.json_only(f);
                pending.schedule(Key::Book, now, &self.timing, true);
            }
            "order" => {
                out.json_only(f);
                if let Some(id) = data["covenant_id"].as_str().and_then(kob_protocol::registry::parse_hex32) {
                    if self.kind.tracked().contains(&Key::Order(id)) {
                        pending.schedule(Key::Order(id), now, &self.timing, true);
                    }
                }
            }
            "cursor" => out.event(&frames::cursor_line(&data), f),
            "health" => out.event(&frames::health_line(&data), f),
            "reorg" => {
                out.frame_note(&frames::reorg_line(&data), f);
                for k in self.kind.tracked() {
                    pending.schedule(k, now, &self.timing, false);
                }
            }
            "resync" => {
                out.frame_note(&frames::resync_line(&data), f);
                for k in self.kind.tracked() {
                    pending.schedule(k, now, &self.timing, false);
                }
            }
            "error" => {
                out.json_only(f);
                out.error(&format!("server: {}", data["message"].as_str().unwrap_or("(no message)")));
            }
            _ => out.event(&frames::generic_line(f), f),
        }
    }

    /// Reads a resource again and prints it.
    fn refetch(&mut self, key: Key, out: &mut Out) {
        match key {
            Key::Book => match views::book_view(&self.common, &self.parsed) {
                Ok((v, lines)) => {
                    let channel = v["token"].as_str().map(|t| format!("book:{t}"));
                    out.block(&lines, &json!({"channel": channel, "type": "snapshot", "data": v}));
                }
                Err(e) => out.error(&format!("book: {e}")),
            },
            Key::Order(id) => {
                let hex = to_hex(&id);
                let channel = format!("order:{hex}");
                match http::get(&self.indexer, &format!("/v1/orders/{hex}"), data::INDEXER_TIMEOUT) {
                    Ok((200, v)) => {
                        let row = views::order_row(&v, &self.names.reg, &self.names.idx);
                        out.event(
                            &format!("order  {}", views::order_row_line(&row)),
                            &json!({"channel": channel, "type": "snapshot", "data": row}),
                        );
                    }
                    Ok((404, _)) => out.event(
                        &format!("order  {} is not known to the indexer (never seen, or reverted)", frames::short(&hex)),
                        &json!({"channel": channel, "type": "snapshot", "data": null}),
                    ),
                    Ok((status, _)) => out.error(&format!("order {}: indexer answered HTTP {status}", frames::short(&hex))),
                    Err(e) => out.error(&format!("order {}: {e}", frames::short(&hex))),
                }
            }
        }
    }
}
