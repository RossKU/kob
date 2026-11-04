//! `kob watch` against a local server that speaks the executor's WebSocket protocol (`docs/api.md` section 4) and a few REST
//! routes on the same port: the frames, the refetch of notices (debounced), the reconnect, the errors.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kob_protocol::artifacts::{token_template, TemplateId};
use kob_protocol::json::to_hex;
use serde_json::{json, Value};
use tungstenite::{Message, WebSocket};

use super::out::Out;
use super::session::{Timing, Watch};
use super::setup;

const TOKEN: [u8; 32] = [0x70; 32];
const ORDER: [u8; 32] = [0xa1; 32];
const GONE: [u8; 32] = [0xa2; 32];

#[derive(Clone, Default)]
struct Cap(Arc<Mutex<Vec<u8>>>);

impl Write for Cap {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Cap {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).to_string()
    }
}

/// What a connection's script does when it returns.
#[derive(PartialEq)]
enum After {
    /// Drop the TCP connection without a closing handshake.
    Drop,
    /// Keep serving pings until the client goes.
    Hold,
}

type Script = Arc<dyn Fn(usize, &mut WebSocket<TcpStream>) -> After + Send + Sync>;

struct Server {
    url: String,
    seen: Arc<Mutex<Vec<String>>>,
    subs: Arc<Mutex<Vec<Value>>>,
    pings: Arc<AtomicUsize>,
    done: Arc<AtomicBool>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.done.store(true, Ordering::Relaxed);
    }
}

impl Server {
    fn hits(&self, path: &str) -> usize {
        self.seen.lock().unwrap().iter().filter(|p| p.as_str() == path).count()
    }
}

fn recv(ws: &mut WebSocket<TcpStream>) -> Option<String> {
    loop {
        match ws.read() {
            Ok(Message::Text(t)) => return Some(t.as_str().to_string()),
            Ok(_) => {}
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                return None
            }
            Err(_) => return None,
        }
    }
}

fn send(ws: &mut WebSocket<TcpStream>, v: Value) {
    ws.send(Message::text(v.to_string())).unwrap();
}

/// A server on one port: `GET /v1/ws` upgrades and runs the script of that connection (numbered from 0); every other GET is
/// answered from `routes` as JSON, 404 for the rest. `refuse_first`: the first upgrade gets `503` with `Retry-After: 1`.
fn start(routes: BTreeMap<String, Value>, refuse_first: bool, script: Script) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(vec![]));
    let subs = Arc::new(Mutex::new(vec![]));
    let pings = Arc::new(AtomicUsize::new(0));
    let done = Arc::new(AtomicBool::new(false));
    let conns = Arc::new(AtomicUsize::new(0));
    let server = Server { url, seen: seen.clone(), subs: subs.clone(), pings: pings.clone(), done: done.clone() };
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let (seen, subs, pings, done, conns, script, routes) =
                (seen.clone(), subs.clone(), pings.clone(), done.clone(), conns.clone(), script.clone(), routes.clone());
            std::thread::spawn(move || {
                let mut head = [0u8; 1024];
                let n = stream.peek(&mut head).unwrap_or(0);
                let line = String::from_utf8_lossy(&head[..n]).lines().next().unwrap_or("").to_string();
                if line.starts_with("GET /v1/ws") {
                    let idx = conns.fetch_add(1, Ordering::SeqCst);
                    if refuse_first && idx == 0 {
                        let mut s = stream;
                        let mut buf = [0u8; 2048];
                        let _ = s.read(&mut buf);
                        let _ = s.write_all(
                            b"HTTP/1.1 503 Service Unavailable\r\nRetry-After: 1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        );
                        return;
                    }
                    let Ok(mut ws) = tungstenite::accept(stream) else { return };
                    let _ = ws.get_ref().set_read_timeout(Some(Duration::from_millis(100)));
                    // the subscription every watch opens with
                    let Some(first) = recv_wait(&mut ws, &done) else { return };
                    let msg: Value = serde_json::from_str(&first).unwrap();
                    assert_eq!(msg["op"], "subscribe");
                    subs.lock().unwrap().push(msg["channels"].clone());
                    if script(idx, &mut ws) == After::Drop {
                        return;
                    }
                    while !done.load(Ordering::Relaxed) {
                        match ws.read() {
                            Ok(Message::Text(t)) if t.as_str().contains("ping") => {
                                pings.fetch_add(1, Ordering::SeqCst);
                                send(&mut ws, json!({"type": "pong"}));
                            }
                            Ok(Message::Close(_)) | Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                                return
                            }
                            Ok(_) => {}
                            Err(tungstenite::Error::Io(e))
                                if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
                            Err(_) => return,
                        }
                    }
                } else {
                    let mut s = stream;
                    let mut buf = [0u8; 4096];
                    let n = s.read(&mut buf).unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]).to_string();
                    let path = req.lines().next().unwrap_or("").split_whitespace().nth(1).unwrap_or("/").to_string();
                    seen.lock().unwrap().push(path.clone());
                    let body = routes.get(&path).or_else(|| routes.get(path.split('?').next().unwrap_or("")));
                    let (status, body) = match body {
                        Some(b) => ("200 OK", b.to_string()),
                        None => ("404 Not Found", r#"{"error":{"code":"not_found","message":"unknown"}}"#.to_string()),
                    };
                    let _ = s.write_all(
                        format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
                            .as_bytes(),
                    );
                }
            });
        }
    });
    server
}

fn recv_wait(ws: &mut WebSocket<TcpStream>, done: &AtomicBool) -> Option<String> {
    let end = Instant::now() + Duration::from_secs(10);
    while !done.load(Ordering::Relaxed) && Instant::now() < end {
        if let Some(t) = recv(ws) {
            return Some(t);
        }
    }
    None
}

fn routes() -> BTreeMap<String, Value> {
    let cov = to_hex(&TOKEN);
    let mut r = BTreeMap::new();
    r.insert(
        "/v1/tokens".to_string(),
        json!({"tokens": [{"ticker": "WATCHT", "covenant_id": cov, "template_hash": to_hex(&token_template(TemplateId::Kcc20Ref).hash),
                           "extension_commitment": to_hex(&[0xee; 32]), "decimals": 3, "open_asks": 1, "open_bids": 1}]}),
    );
    r.insert(
        format!("/v1/books/{cov}?depth=20"),
        json!({"token": cov, "aggregated": true,
               "asks": [{"price": "250000000", "amount": "5000", "amount_estimated": false, "orders": 2, "scale": 1000}],
               "bids": [{"price": "245000000", "amount": "3000", "amount_estimated": true, "orders": 1, "scale": 1000}]}),
    );
    r.insert(format!("/v1/trades/{cov}?limit=1"), json!({"items": [], "price_basis": "1000"}));
    r.insert(
        format!("/v1/orders/{}", to_hex(&ORDER)),
        json!({"covenant_id": to_hex(&ORDER), "contract": "KobAsk", "status": "partial", "state_known": false, "side": 1, "token": cov}),
    );
    r
}

fn fill(amount: &str) -> Value {
    json!({"channel": "fills", "type": "fill", "data": {"order": to_hex(&ORDER), "token": to_hex(&TOKEN), "side": 1, "price": "250000000",
           "amount": amount, "payout": "625000000", "txid": to_hex(&[0x62; 32]), "block": to_hex(&[0x01; 32]), "daa": 995}})
}

fn subscribed(channels: &[String]) -> Value {
    json!({"type": "subscribed", "data": {"channels": channels, "errors": []}})
}

fn fixed_now() -> u64 {
    1_793_761_724
}

fn fast() -> Timing {
    Timing {
        ping_every: Duration::from_millis(150),
        debounce: Duration::from_millis(300),
        min_gap: Duration::from_millis(100),
        poll: Duration::from_millis(50),
        backoff_start: Duration::from_millis(100),
        backoff_max: Duration::from_millis(400),
        connect: Duration::from_secs(5),
    }
}

/// The channels a subscribe message asked for, as the server's reply lists them (sorted, `reorg` first).
fn reply_channels(msg: &Value) -> Vec<String> {
    msg.as_array().unwrap().iter().filter_map(|c| c.as_str().map(str::to_owned)).collect()
}

struct Run {
    code: i32,
    out: String,
    err: String,
}

/// Run `kob watch <args>` against `server` until `until(stdout, stderr)` holds (20 s at most), then stop it.
fn watch(server: &Server, args: &[&str], json: bool, until: impl Fn(&str, &str) -> bool) -> Run {
    let mut a: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    a.extend(["--network".into(), "testnet-10".into(), "--indexer".into(), server.url.clone()]);
    if json {
        a.push("--json".into());
    }
    let (mut w, j): (Watch, bool) = setup(&a).unwrap_or_else(|e| panic!("{e:?}"));
    assert_eq!(j, json);
    w.timing = fast();
    let (out, err) = (Cap::default(), Cap::default());
    let mut o = Out::with(json, Box::new(out.clone()), Box::new(err.clone()), fixed_now);
    let stop = Arc::new(AtomicBool::new(false));
    let s2 = stop.clone();
    let h = std::thread::spawn(move || w.run(&s2, &mut o));
    let end = Instant::now() + Duration::from_secs(20);
    while Instant::now() < end && !until(&out.text(), &err.text()) && !h.is_finished() {
        std::thread::sleep(Duration::from_millis(20));
    }
    // let a late duplicate show up before the verdict
    std::thread::sleep(Duration::from_millis(400));
    stop.store(true, Ordering::Relaxed);
    let code = h.join().unwrap();
    Run { code, out: out.text(), err: err.text() }
}

#[test]
fn fills_frames_reconnect_and_server_notices() {
    let script: Script = Arc::new(|idx, ws| {
        let chans = vec!["reorg".to_string(), "fills".to_string()];
        send(ws, subscribed(&chans));
        if idx == 0 {
            send(ws, fill("2500"));
            send(
                ws,
                json!({"channel": "reorg", "type": "reorg", "data": {"reverted_blocks": 1, "added_blocks": 2, "cursor_daa": 996}}),
            );
            send(ws, json!({"type": "resync", "data": {"missed_events": 12}}));
            send(ws, json!({"type": "error", "data": {"message": "unknown op"}}));
            send(ws, json!({"channel": "fills", "type": "future-frame", "data": {"a": 1}}));
            After::Drop
        } else {
            send(ws, fill("1"));
            After::Hold
        }
    });
    let server = start(routes(), false, script);
    let r = watch(&server, &["fills"], false, |o, _| o.matches("fill  ask").count() >= 2);
    assert_eq!(r.code, 0, "{}", r.err);
    // time (UTC), kind, amount and price in the token's decimals, short order id, full txid
    let want = format!(
        "03:08:44  fill  ask  2.5 WATCHT @ 2.5 KAS per WATCHT  paid 6.25 KAS  order a1a1a1a1..a1a1a1a1  tx {}  daa 995",
        "62".repeat(32)
    );
    assert!(r.out.contains(&want), "{}", r.out);
    assert!(r.out.contains("fill  ask  0.001 WATCHT @"), "{}", r.out);
    assert!(r.out.contains("03:08:44  watching fills on network testnet-10 (indexer http://127.0.0.1:"), "{}", r.out);
    assert!(r.out.contains("future-frame [fills]  {\"a\":1}"), "{}", r.out);
    // notes and errors go to stderr
    assert!(!r.out.contains("note:") && !r.out.contains("error:"), "{}", r.out);
    assert!(r.err.contains("note: reorg: 1 block(s) reverted, 2 added (daa 996)"), "{}", r.err);
    assert!(r.err.contains("note: resync: this client missed 12 events"), "{}", r.err);
    assert!(r.err.contains("error: server: unknown op"), "{}", r.err);
    assert!(r.err.contains("note: connection lost"), "{}", r.err);
    assert!(r.err.contains("note: reconnected"), "{}", r.err);
    // both connections subscribed to reorg and fills; pings kept the second alive
    let subs = server.subs.lock().unwrap().clone();
    assert!(subs.len() >= 2, "{subs:?}");
    assert!(subs.iter().all(|s| reply_channels(s) == ["reorg", "fills"]), "{subs:?}");
    assert!(server.pings.load(Ordering::SeqCst) >= 1);
}

#[test]
fn fills_of_one_token_by_ticker_and_json_lines() {
    let script: Script = Arc::new(|_, ws| {
        send(ws, subscribed(&["reorg".to_string(), format!("fills:{}", to_hex(&TOKEN))]));
        send(ws, fill("2500"));
        After::Hold
    });
    let server = start(routes(), false, script);
    let r = watch(&server, &["fills", "--token", "watcht"], true, |o, _| o.contains("\"fill\""));
    assert_eq!(r.code, 0, "{}", r.err);
    assert_eq!(server.subs.lock().unwrap()[0], json!(["reorg", format!("fills:{}", to_hex(&TOKEN))]));
    // NDJSON: the frame as the server sent it, one per line
    let lines: Vec<Value> = r.out.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines.len(), 2, "{}", r.out);
    assert_eq!(lines[0]["type"], "start");
    assert_eq!(lines[0]["data"]["network"], "testnet-10");
    assert_eq!(lines[1], fill("2500"));
    assert_eq!(r.err, "");
}

#[test]
fn a_book_notice_reads_the_book_again_once_per_burst() {
    let cov = to_hex(&TOKEN);
    let path = format!("/v1/books/{cov}?depth=20");
    let script: Script = Arc::new(move |idx, ws| {
        send(ws, subscribed(&["reorg".to_string(), format!("book:{cov}")]));
        if idx == 0 {
            std::thread::sleep(Duration::from_millis(200));
            for daa in 1..=5 {
                send(ws, json!({"channel": format!("book:{cov}"), "type": "book", "data": {"token": cov, "cursor_daa": daa}}));
            }
            std::thread::sleep(Duration::from_millis(900));
            After::Drop
        } else {
            After::Hold
        }
    });
    let server = start(routes(), false, script);
    let r = watch(&server, &["book", "WATCHT"], false, |o, e| {
        o.matches("last fill").count() + o.matches("-- spread").count() >= 3 && e.contains("reconnected")
    });
    assert_eq!(r.code, 0, "{}", r.err);
    // the first read, one read for the five notices, one after the reconnect
    assert_eq!(server.hits(&path), 3, "{}", r.out);
    assert!(r.out.contains("03:08:44  book WATCHT"), "{}", r.out);
    assert!(r.out.contains("-- spread 0.05 KAS per WATCHT (best ask 2.5 KAS per WATCHT, best bid 2.45 KAS per WATCHT)"), "{}", r.out);
    assert!(r.out.contains("2.5 WATCHT") || r.out.contains("5 WATCHT"), "{}", r.out);
}

#[test]
fn an_order_notice_prints_the_orders_state() {
    let (a, b) = (to_hex(&ORDER), to_hex(&GONE));
    let (a2, b2) = (a.clone(), b.clone());
    let script: Script = Arc::new(move |_, ws| {
        let mut chans = vec![format!("order:{a2}"), format!("order:{b2}")];
        chans.insert(0, "reorg".into());
        send(ws, subscribed(&chans));
        std::thread::sleep(Duration::from_millis(300));
        send(ws, json!({"channel": format!("order:{a2}"), "type": "order", "data": {"covenant_id": a2, "cursor_daa": 9}}));
        After::Hold
    });
    let server = start(routes(), false, script);
    let r = watch(&server, &["order", &a, &b], true, |o, _| o.matches("\"snapshot\"").count() >= 3);
    assert_eq!(r.code, 0, "{}", r.err);
    let lines: Vec<Value> = r.out.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    // the notice frame itself, then snapshots: the order twice (first read, after the notice), the unknown one once as null
    assert!(lines.iter().any(|l| l["type"] == "order"), "{}", r.out);
    let snaps: Vec<&Value> = lines.iter().filter(|l| l["type"] == "snapshot").collect();
    assert_eq!(snaps.iter().filter(|s| s["channel"] == format!("order:{a}") && s["data"]["status"] == "partial").count(), 2);
    assert!(snaps.iter().any(|s| s["channel"] == format!("order:{b}") && s["data"].is_null()));
    assert_eq!(server.hits(&format!("/v1/orders/{a}")), 2);
    // human form
    let server = start(routes(), false, {
        let a = a.clone();
        Arc::new(move |_, ws| {
            send(ws, subscribed(&["reorg".to_string(), format!("order:{a}")]));
            After::Hold
        })
    });
    let r = watch(&server, &["order", &a], false, |o, _| o.contains("KobAsk"));
    assert!(
        r.out.contains(&format!("03:08:44  order  {a} KobAsk WATCHT: sell (state not proven by the indexer) [partial]")),
        "{}",
        r.out
    );
}

#[test]
fn health_lines_and_a_refused_upgrade_that_asks_to_wait() {
    let script: Script = Arc::new(|_, ws| {
        send(ws, subscribed(&["health".to_string(), "reorg".to_string()]));
        send(
            ws,
            json!({"channel": "health", "type": "cursor", "data": {"cursor_hash": to_hex(&[1; 32]), "cursor_daa": 995, "added_blocks": 2, "reverted_blocks": 0}}),
        );
        send(
            ws,
            json!({"channel": "health", "type": "health", "data": {"state": "following", "cursor_daa": 995, "node_daa": 997, "lag_daa": 2}}),
        );
        After::Hold
    });
    let server = start(routes(), true, script);
    let start_at = Instant::now();
    let r = watch(&server, &["health"], false, |o, _| o.contains("health  following"));
    assert_eq!(r.code, 0, "{}", r.err);
    // 503 Retry-After: 1 is honoured over the short backoff
    assert!(r.err.contains("HTTP 503") && r.err.contains("retrying in 1s"), "{}", r.err);
    assert!(start_at.elapsed() >= Duration::from_millis(900));
    assert!(r.out.contains("03:08:44  cursor  daa 995  +2 -0 blocks"), "{}", r.out);
    assert!(r.out.contains("03:08:44  health  following  daa 995  node 997  lag 2"), "{}", r.out);
}

#[test]
fn a_subscription_the_server_refuses_ends_the_watch_with_the_reason() {
    let script: Script = Arc::new(|_, ws| {
        send(
            ws,
            json!({"type": "subscribed", "data": {"channels": ["reorg"], "errors": ["invalid channel `order:zz`: id must be 64 hex characters"]}}),
        );
        After::Hold
    });
    let server = start(routes(), false, script);
    let r = watch(&server, &["order", &to_hex(&ORDER)], false, |_, e| e.contains("accepted none"));
    assert_eq!(r.code, 1);
    assert!(r.err.contains("subscription refused by the server: invalid channel `order:zz`"), "{}", r.err);
    assert!(r.err.contains("the server accepted none of the channels"), "{}", r.err);
}

#[test]
fn an_endpoint_without_the_route_is_refused_clearly() {
    // the HTTP side answers /v1/ws with 404: fatal, with the hint
    let (url, _state) = crate::trade::http::mock::start(routes());
    let mut a: Vec<String> = ["health", "--network", "testnet-10", "--indexer"].iter().map(|s| s.to_string()).collect();
    a.push(url);
    let (mut w, _) = setup(&a).unwrap();
    w.timing = fast();
    let (out, err) = (Cap::default(), Cap::default());
    let mut o = Out::with(false, Box::new(out), Box::new(err.clone()), fixed_now);
    assert_eq!(w.run(&AtomicBool::new(false), &mut o), 1);
    assert!(err.text().contains("HTTP 404") && err.text().contains("no /v1/ws route"), "{}", err.text());
}

#[test]
fn arguments() {
    let (url, _s) = crate::trade::http::mock::start(routes());
    let a = |s: &str| -> Vec<String> {
        let mut v: Vec<String> = s.split_whitespace().map(str::to_string).collect();
        v.extend(["--network".into(), "testnet-10".into(), "--indexer".into(), url.clone()]);
        v
    };
    let err = |s: &str| setup(&a(s)).map(|_| ()).unwrap_err();
    assert!(err("").0.contains("fills, book, order or health"));
    assert!(err("candles").0.contains("unknown watch `candles`"));
    assert_eq!(err("book").1, 2);
    assert!(err("order").0.contains("at least one order"));
    assert!(err("order zz").0.contains("not an order covenant id"));
    assert!(err("fills --token nosuch").0.contains("unknown token `nosuch`"));
    assert!(err("health extra").0.contains("unexpected argument"));
    assert!(err("fills --yes").0.contains("signs nothing"));
    assert!(err("fills --key-file k").0.contains("signs nothing"));
    assert!(err("book WATCHT --quote WATCHT").0.contains("names the token itself"));
    let many = (0..61).map(|i| format!("{:064x}", i + 1)).collect::<Vec<_>>().join(" ");
    assert!(err(&format!("order {many}")).0.contains("at most 60"));
    // no indexer
    let e = setup(&["health".into(), "--network".into(), "testnet-10".into()]);
    if std::env::var(crate::trade::http::INDEXER_ENV).is_err() {
        assert!(e.map(|_| ()).unwrap_err().0.contains("--indexer"));
    }
    // channels
    let (w, _) = setup(&a("book WATCHT")).unwrap();
    assert_eq!(w.kind.channels(), vec!["reorg".to_string(), format!("book:{}", to_hex(&TOKEN))]);
    let (w, _) = setup(&a(&format!("order {} {}", to_hex(&ORDER), to_hex(&ORDER)))).unwrap();
    assert_eq!(w.kind.channels().len(), 2, "a repeated id is one channel");
}
