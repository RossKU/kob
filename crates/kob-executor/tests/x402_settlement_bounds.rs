//! Bounds of x402 settlement and of the HTTP transports, each on the real code path: ledger replay and the facilitator's
//! reconcile, the x402 HTTP router, the production `ProtocolVerifier`, the read API's `WsRegistry` and `api::conn::serve`.
//!
//! Run: KOB_SKIP_NETWORK_TESTS=1 cargo test -p kob-executor --test x402_settlement_bounds -- --nocapture --test-threads=8

use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use kaspa_consensus_core::subnets::SUBNETWORK_ID_NATIVE;
use kaspa_consensus_core::tx::{ScriptPublicKey, Transaction, TransactionInput, TransactionOutpoint, TransactionOutput};
use kaspa_consensus_core::Hash;
use kob_executor::x402::config::{api_key_hash, X402Config};
use kob_executor::x402::facilitator::{Facilitator, FacilitatorConfig, InvoiceRuntime, InvoiceStore};
use kob_executor::x402::http::{router, AppState};
use kob_executor::x402::ledger::{Ledger, LedgerError, NewEntry, State, Watched};
use kob_executor::x402::testutil::*;
use kob_x402::chain::{ChainError, ChainUtxo, ChainView, FixedClock, Outpoint, OutputStatus, SubmitError, Txid};
use kob_x402::policy::Policy;
use kob_x402::safe_tx::spk_to_hex;
use kob_x402::testkit::{p2pk_spk, pubkey, MockChain};
use kob_x402::wire::{hex, Finality, Network};
use serde_json::{Map, Value};
use tower::ServiceExt;

const KAS: u64 = 100_000_000;
const KEY1: &str = "secret-key-1";

// ======================================================================================= intent entries

/// A covenant output holding the payer's KAS: the intent output of an unexecuted creation (the merchant got nothing).
fn intent_output(chain: &MockChain) -> (Outpoint, ScriptPublicKey) {
    let spk = ScriptPublicKey::new(0, {
        let mut s = vec![0xaa, 0x20];
        s.extend_from_slice(&[0x5a; 32]);
        s.push(0x87);
        s.into()
    });
    (chain.add_utxo(5 * KAS, spk.clone(), Some([0x42; 32])), spk)
}

fn intent_entry(op: &Outpoint, spk: &ScriptPublicKey) -> NewEntry {
    NewEntry {
        txid: op.txid,
        request_hash: [7; 32],
        requirements_hash: [8; 32],
        payment_id: Some("payment-id-intent-0001".into()),
        profile: "standard-native".into(),
        kind: "intent-to-kas".into(),
        merchant: "shop".into(),
        network: "testnet-10".into(),
        payer: Some(address_of_key(PAYER_KEY)),
        amount: "100000000".into(),
        finality: "accepted".into(),
        consumed: vec![Outpoint::new([0x99; 32], 0)],
        order_inputs: vec![],
        // what an intent settlement records first: the watched output IS the intent output of the creation
        watched: Watched { txid: hex(&op.txid), index: op.index, spk: spk_to_hex(spk), amount: 5 * KAS },
        extension: Map::new(),
        now_ms: START_MS,
        intent: None,
        invoice: None,
    }
}

/// An intent payment recorded for an earlier router template (or in any intent format this build does not read): the ledger
/// refuses to open, naming the line and telling the operator to archive the ledger, wherever the record is. It is never
/// replayed as a direct payment.
#[test]
fn an_intent_record_of_another_format_stops_the_ledger_instead_of_replaying() {
    for start in [State::Broadcast, State::Pending] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.jsonl");
        let chain = Arc::new(MockChain::new());
        let (op, spk) = intent_output(&chain);
        {
            let l = Ledger::open(&path).unwrap();
            l.claim(intent_entry(&op, &spk)).unwrap();
            if start != State::Pending {
                l.transition(&hex(&op.txid), start, None, START_MS).unwrap();
            }
        }
        for old in [
            serde_json::json!({"facts": {"intent": {"kind": "TokenToKas", "maxSell": "200000000"}}, "executions": [], "lost": []}),
            serde_json::json!({"format": "a later one", "facts": 1}),
        ] {
            let raw = std::fs::read_to_string(&path).unwrap();
            let mut out = String::new();
            for line in raw.lines().filter(|l| !l.trim().is_empty()) {
                let mut v: Value = serde_json::from_str(line).unwrap();
                v["entry"]["intent"] = old.clone();
                out.push_str(&v.to_string());
                out.push('\n');
            }
            std::fs::write(&path, &out).unwrap();
            let e = Ledger::open(&path).err().expect("an intent record of another format does not replay");
            println!("{start:?}: {e}");
            assert!(matches!(e, LedgerError::Unsupported { line: 1, .. }), "{e}");
            assert!(e.to_string().contains("archive"), "{e}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), out, "the ledger is left untouched");
        }
    }
}

/// An entry recorded as an intent payment without a readable intent record (here put in a ledger directly) is never
/// finalized as a direct payment: its watched output is the creation's own intent output, which is on chain while the
/// merchant was not paid.
#[test]
fn an_intent_entry_is_never_finalized_as_a_direct_payment() {
    for start in [State::Broadcast, State::Pending, State::Ambiguous] {
        let chain = Arc::new(MockChain::new());
        let clock = Arc::new(FixedClock::new(START_MS));
        let (op, spk) = intent_output(&chain);
        let ledger = Arc::new(Ledger::in_memory());
        ledger.claim(intent_entry(&op, &spk)).unwrap();
        let txid = hex(&op.txid);
        if start != State::Pending {
            ledger.transition(&txid, start, None, START_MS).unwrap();
        }
        let f = Fixture::with_ledger(ledger.clone(), chain.clone(), clock.clone());
        for _ in 0..3 {
            let rep = f.fac.reconcile();
            let e = ledger.get(&txid).unwrap();
            assert_ne!(e.state, State::Accepted, "{start:?}: an intent entry was finalized from its own creation output ({rep:?})");
            assert!(e.response.is_none());
        }
    }
}

// ======================================================================================= invoice pays

fn cfg_json(extra: &str) -> String {
    format!(
        r#"{{"merchants":[
            {{"id":"shop","apiKeySha256":"{}","allowedPayTo":["{}"],"allowedAssets":["KAS"]}}
        ]{extra}}}"#,
        hex(&api_key_hash(KEY1)),
        address_of_key(MERCHANT_KEY),
    )
}

async fn send(app: &Router, mut req: Request<Body>, ip: &str) -> (StatusCode, Vec<u8>) {
    let addr: SocketAddr = if ip.contains(':') { format!("[{ip}]:5000") } else { format!("{ip}:5000") }.parse().unwrap();
    req.extensions_mut().insert(ConnectInfo(addr));
    let resp = app.clone().oneshot(req).await.unwrap();
    let (parts, body) = resp.into_parts();
    (parts.status, body.collect().await.unwrap().to_bytes().to_vec())
}

fn post(path: &str, key: Option<&str>, body: Vec<u8>) -> Request<Body> {
    let mut b = Request::builder().method("POST").uri(path).header(header::CONTENT_TYPE, "application/json");
    if let Some(k) = key {
        b = b.header(header::AUTHORIZATION, format!("Bearer {k}"));
    }
    b.body(Body::from(body)).unwrap()
}

fn get(path: &str) -> Request<Body> {
    Request::builder().method("GET").uri(path).body(Body::empty()).unwrap()
}

/// `POST /invoices/{id}/pay` is anonymous. While one payment of an invoice is settling (it holds the invoice's lock for its
/// observation, up to the settle wait), further requests for that invoice are answered `invoice_pending` at once instead of
/// waiting, and they run in a slot pool of their own: a merchant's `/settle` is served meanwhile.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn anonymous_invoice_pays_do_not_hold_merchant_settle_slots() {
    const SLOTS: usize = 4;
    let fx = Fixture::with_invoices();
    let built = X402Config::from_json(&cfg_json(&format!(r#","maxConcurrentSettles":{SLOTS},"maxSettlesPerMerchant":2"#)))
        .unwrap()
        .build()
        .unwrap();
    let state = Arc::new(AppState::new(fx.fac.clone(), &built));
    let app = router(state);

    // the merchant registers an invoice; its id is public (the payer's link)
    let reqs = kas_requirements(100_000_000, Finality::Accepted, 60);
    let inv = kob_x402::invoice::Invoice::new(Network::Testnet10, "order-7", START_MS + 600_000, None, vec![reqs.clone()]);
    let (st, b) = send(&app, post("/invoices", Some(KEY1), serde_json::to_vec(&inv).unwrap()), "10.0.0.1").await;
    assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&b));
    let id = serde_json::from_slice::<Value>(&b).unwrap()["id"].as_str().unwrap().to_string();

    // (1) a genuine payment of the invoice is settling: broadcast, not yet final (the mock chain does not mine), so its
    // pay holds the invoice lock for the facilitator's settle wait (5 s in the fixture)
    let input = fx.fund(300_000_000);
    let (tx, entries) = build_kas_payment(&fx.chain, PAYER_KEY, &[input], &p2pk_spk(&pubkey(MERCHANT_KEY)), 100_000_000);
    let freq = request_for(&tx, &entries, &reqs, kob_x402::wire::parse_hash32(&id).unwrap(), "payment-id-invoice-00001");
    let pay_body = serde_json::to_vec(&freq.payment_payload).unwrap();
    let holder = {
        let (app, path, body) = (app.clone(), format!("/invoices/{id}/pay"), pay_body.clone());
        tokio::spawn(async move { send(&app, post(&path, None, body), "10.0.0.3").await })
    };
    let t = Instant::now();
    while fx.chain.submit_count() == 0 && t.elapsed() < Duration::from_secs(5) {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(fx.chain.submit_count(), 1, "the genuine invoice payment was broadcast and is being observed");

    // (2) anonymous requests for the same invoice: a payload that is not even a transaction, whose `accepted` copies the
    // public invoice
    let mut other = freq.payment_payload.clone();
    other.payload.transaction = "not a transaction".into();
    let other_body = serde_json::to_vec(&other).unwrap();
    let mut parked = vec![];
    for k in 0..SLOTS - 1 {
        let (app, path, body) = (app.clone(), format!("/invoices/{id}/pay"), other_body.clone());
        let ip = format!("10.9.{k}.1");
        parked.push(tokio::spawn(async move { send(&app, post(&path, None, body), &ip).await }));
    }
    tokio::time::sleep(Duration::from_millis(300)).await;

    // (3) a merchant's ordinary /settle of an unrelated direct payment
    let other = fx.fund(500_000_000);
    let (req2, _) = fx.payment(&[other], 120_000_000, "payment-id-direct-000002", 9);
    let chain = fx.chain.clone();
    let miner = std::thread::spawn(move || {
        let base = chain.submit_count();
        let t = Instant::now();
        while chain.submit_count() <= base && t.elapsed() < Duration::from_secs(3) {
            std::thread::sleep(Duration::from_millis(2));
        }
        std::thread::sleep(Duration::from_millis(10));
        chain.mine(0);
    });
    let (st, b) = send(&app, post("/settle", Some(KEY1), serde_json::to_vec(&req2).unwrap()), "10.0.0.1").await;
    miner.join().unwrap();
    println!("merchant /settle while {} anonymous pays target a settling invoice -> {st} {}", SLOTS - 1, String::from_utf8_lossy(&b));
    for p in parked {
        let _ = p.await;
    }
    let _ = holder.await;
    assert_ne!(st, StatusCode::SERVICE_UNAVAILABLE, "anonymous invoice pays held the settle pool; the merchant's /settle got 503");
}

// ===================================================================================== input lookups

/// Records every script the facilitator asks the chain to resolve (a node resolves each through
/// `getUtxosByAddresses(<address of the script>)`).
struct CountingChain {
    inner: Arc<MockChain>,
    looked_up: Mutex<Vec<ScriptPublicKey>>,
}

impl ChainView for CountingChain {
    fn utxos(&self, w: &[(Outpoint, ScriptPublicKey)]) -> Result<Vec<Option<ChainUtxo>>, ChainError> {
        self.looked_up.lock().unwrap().extend(w.iter().map(|(_, s)| s.clone()));
        self.inner.utxos(w)
    }
    fn utxos_of(&self, s: &ScriptPublicKey) -> Result<Vec<(Outpoint, ChainUtxo)>, ChainError> {
        self.looked_up.lock().unwrap().push(s.clone());
        self.inner.utxos_of(s)
    }
    fn virtual_daa_score(&self) -> Result<u64, ChainError> {
        self.inner.virtual_daa_score()
    }
    fn output_status(&self, o: &Outpoint, s: &ScriptPublicKey) -> Result<OutputStatus, ChainError> {
        self.inner.output_status(o, s)
    }
    fn in_mempool(&self, t: &Txid) -> Result<bool, ChainError> {
        self.inner.in_mempool(t)
    }
    fn submit(&self, tx: &Transaction) -> Result<Txid, SubmitError> {
        self.inner.submit(tx)
    }
}

/// The production verifier checks the request authorization against the key of the embedded utxo of
/// `authorization.inputIndex` BEFORE the chain lookup: a payload whose authorization does not verify makes the chain
/// resolve no script at all, however many inputs (up to `max_inputs`) with distinct scripts it names. Reached anonymously
/// through `/invoices/{id}/pay`.
#[test]
fn an_unauthorized_invoice_pay_resolves_no_input_script() {
    let mock = Arc::new(MockChain::new());
    let counting = Arc::new(CountingChain { inner: mock.clone(), looked_up: Mutex::new(vec![]) });
    let clock = Arc::new(FixedClock::new(START_MS));
    let policy = Policy::new(Network::Testnet10);
    let n_inputs = policy.limits.max_inputs;
    let fac = Facilitator::new(
        policy,
        counting.clone(),
        clock,
        Arc::new(Ledger::in_memory()),
        FacilitatorConfig { settle_wait: Duration::from_millis(50), poll_interval: Duration::from_millis(5), ..Default::default() },
    )
    // NO with_verifier: the production ProtocolVerifier runs
    .with_invoices(InvoiceRuntime {
        store: Arc::new(InvoiceStore::in_memory()),
        max_lifetime_ms: 86_400_000,
        public_url: None,
        max_open_per_merchant: 100,
        max_extra_payments: 16,
    });
    let reqs = kas_requirements(100_000_000, Finality::Accepted, 60);
    let inv = kob_x402::invoice::Invoice::new(Network::Testnet10, "order-lookups", START_MS + 600_000, None, vec![reqs.clone()]);
    let id = fac.register_invoice("shop", inv, &|_| true).unwrap()["id"].as_str().unwrap().to_string();

    // a transaction of max_inputs inputs, each naming a DIFFERENT script in its embedded utxo, signatures that verify
    // nothing; one output paying the invoice
    let entries: Vec<_> = (0..n_inputs)
        .map(|i| {
            ChainUtxo {
                amount: 10 * KAS,
                script_public_key: p2pk_spk(&pubkey(40 + i as u8)),
                block_daa_score: 900,
                is_coinbase: false,
                covenant_id: None,
            }
            .to_entry()
        })
        .collect();
    let ins: Vec<TransactionInput> = (0..n_inputs)
        .map(|i| {
            let mut sig = vec![0x41];
            sig.extend_from_slice(&[0x33; 64]);
            sig.push(0x01);
            TransactionInput::new(TransactionOutpoint::new(Hash::from_bytes([0x60 + i as u8; 32]), 0), sig, 0, 1)
        })
        .collect();
    let mut tx = Transaction::new(
        0,
        ins,
        vec![TransactionOutput::new(100_000_000, p2pk_spk(&pubkey(MERCHANT_KEY)))],
        0,
        SUBNETWORK_ID_NATIVE,
        0,
        vec![],
    );
    tx.finalize();
    let freq = request_for(&tx, &entries, &reqs, kob_x402::wire::parse_hash32(&id).unwrap(), "payment-id-lookups-0001");
    let r = fac.pay_invoice(&id, freq.payment_payload);
    assert!(!r.success);
    let looked: BTreeSet<Vec<u8>> = counting.looked_up.lock().unwrap().iter().map(|s| s.script().to_vec()).collect();
    println!("unauthorized pay -> {:?}; the facilitator resolved {} distinct scripts", r.error_reason, looked.len());
    assert_eq!(looked.len(), 0, "{} scripts were resolved on the chain before the authorization was checked", looked.len());
}

// ======================================================================================== IPv6 sites

fn v6(site: u16, subnet: u16, host: u16) -> Ipv6Addr {
    Ipv6Addr::new(0x2001, 0x0db8, site, subnet, 0, 0, 0, host)
}

/// The read API's WebSocket registry counts the /64s of one IPv6 site together (`max_ws_per_site`), so one /48 cannot take
/// every WebSocket slot: a client from another network still gets one.
#[test]
fn one_ipv6_site_does_not_take_every_websocket_slot() {
    use kob_executor::api::ws::{WsLimits, WsRegistry, WsReject};
    let cfg = kob_executor::config::ApiConfig::default();
    let limits = WsLimits::of(&cfg);
    let total = limits.max_total;
    let reg = WsRegistry::default();
    let mut held = vec![];
    let mut subnet = 0u16;
    while reg.total() < total {
        match reg.try_acquire(IpAddr::V6(v6(1, subnet, (held.len() % 7) as u16 + 1)), &limits) {
            Ok(g) => held.push(g),
            Err(WsReject::PerIp) => subnet += 1,
            Err(WsReject::Total | WsReject::PerSite) => break,
        }
    }
    let outsider = reg.try_acquire(IpAddr::V6(v6(0x99, 0, 1)), &limits);
    println!(
        "one /48 (2001:db8:1::/48) holds {} of {total} WS slots from {} /64s; outsider -> {:?}",
        held.len(),
        subnet + 1,
        outsider.as_ref().err()
    );
    assert!(outsider.is_ok(), "one IPv6 /48 holds all {total} WebSocket slots ({} /64s)", subnet + 1);
}

/// The x402 request limiter takes a token of the client's IPv6 site (`rateLimit.perSite`) as well as of its /64: under a
/// frozen clock (no refill) one /64 is cut off after its burst, and one /48 rotating through its /64s after the site's.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_x402_request_limiter_aggregates_an_ipv6_site() {
    let fx = Fixture::with_invoices();
    let built = X402Config::from_json(&cfg_json("")).unwrap().build().unwrap();
    let app = router(Arc::new(AppState::new(fx.fac.clone(), &built)));
    let path = format!("/invoices/{}/status", "00".repeat(32));
    // control: one /64 is limited after its burst
    let mut limited_one = 0;
    for _ in 0..40 {
        let (st, _) = send(&app, get(&path), &v6(2, 5, 1).to_string()).await;
        limited_one += usize::from(st == StatusCode::TOO_MANY_REQUESTS);
    }
    assert!(limited_one > 0, "control: a single /64 is rate limited after its burst");
    let mut limited = 0;
    for k in 0..600u16 {
        let (st, _) = send(&app, get(&path), &v6(3, k, 1).to_string()).await;
        limited += usize::from(st == StatusCode::TOO_MANY_REQUESTS);
    }
    println!("one /64 x 40 -> {limited_one} x 429; one /48 x 600 (distinct /64s) -> {limited} x 429");
    assert!(limited > 0, "600 requests from one IPv6 /48 were never rate limited by the x402 limiter");
}

/// The transport connection cap (`api::conn::serve`, shared by the read API and x402) counts the /64s of one IPv6 site
/// together (`max_per_site`). Real TCP over loopback aliases of one /48 (2001:db8:7::/48, `ip -6 addr add ... dev lo`): 4 /64s
/// x `max_per_ip` reach only the site's share, and a client from another network (::1) is served. Skipped (printed) when the
/// aliases are absent.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_ipv6_site_does_not_take_every_connection() {
    use kob_executor::api::client_ip::TrustedProxies;
    use kob_executor::api::conn::{serve, ConnLimits};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpSocket};

    let srcs: Vec<Ipv6Addr> = (0..4).map(|s| v6(7, s, 1)).collect();
    // probe the aliases
    for s in &srcs {
        if std::net::TcpListener::bind(SocketAddr::new(IpAddr::V6(*s), 0)).is_err() {
            println!("SKIPPED: loopback alias {s} not configured");
            return;
        }
    }
    let listener = match TcpListener::bind("[::]:0").await {
        Ok(l) => l,
        Err(e) => {
            println!("SKIPPED: no IPv6 listener ({e})");
            return;
        }
    };
    let port = listener.local_addr().unwrap().port();
    let limits = ConnLimits {
        header_timeout: Duration::from_secs(20),
        write_timeout: Duration::from_secs(20),
        max_connections: 8,
        max_per_ip: 2,
        ipv6_prefix_bits: 64,
        max_per_site: 4,
        ipv6_site_prefix_bits: 48,
        exempt: TrustedProxies::default(),
    };
    let app = Router::new().route("/", axum::routing::get(|| async { "ok" }));
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(serve(listener, app, limits, "site-cap", async move {
        let _ = stop_rx.await;
    }));
    async fn ask(port: u16) -> String {
        let mut c = tokio::net::TcpStream::connect(SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), port)).await.unwrap();
        // an over-cap connection is answered at accept, before any request: read first
        let mut buf = vec![0u8; 256];
        if let Ok(Ok(n)) = tokio::time::timeout(Duration::from_millis(500), c.read(&mut buf)).await {
            if n > 0 {
                return String::from_utf8_lossy(&buf[..n]).lines().next().unwrap_or("").to_string();
            }
        }
        let _ = c.write_all(b"GET / HTTP/1.1\r\nhost: x\r\nconnection: close\r\n\r\n").await;
        let mut out = vec![];
        let _ = tokio::time::timeout(Duration::from_secs(3), c.read_to_end(&mut out)).await;
        String::from_utf8_lossy(&out).lines().next().unwrap_or("").to_string()
    }
    let control = ask(port).await;
    assert!(control.contains("200"), "control: the outsider is served on an idle server ({control:?})");
    let mut held = vec![];
    for s in &srcs {
        for _ in 0..2 {
            let sock = TcpSocket::new_v6().unwrap();
            sock.bind(SocketAddr::new(IpAddr::V6(*s), 0)).unwrap();
            held.push(sock.connect(SocketAddr::new(IpAddr::V6(srcs[0]), port)).await.unwrap());
        }
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    let head = ask(port).await;
    println!("8 connections held from 4 /64s of one /48; outsider (::1) -> {head:?}");
    drop(held);
    let _ = stop_tx.send(());
    let _ = tokio::time::timeout(Duration::from_secs(5), server).await;
    assert!(head.contains("200"), "one IPv6 /48 holds every connection; outsider got {head:?}");
}
