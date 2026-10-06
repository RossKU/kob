//! The facilitator with the real `kob_x402` verifier and real payments built by `pay_native`
//! (KAS `standard-native`), over the deterministic mock chain. No network.

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use kaspa_addresses::{Address, Version};
use kob_executor::x402::facilitator::{Facilitator, FacilitatorConfig};
use kob_executor::x402::ledger::{Ledger, State};
use kob_protocol::tx::{KeyUtxo, Utxo};
use kob_x402::canonical::http_request_hash;
use kob_x402::chain::{ChainView, FixedClock, SubmitError};
use kob_x402::client::native::{native_requirements, pay_native, PayOptions};
use kob_x402::common::requirements_hash_hex;
use kob_x402::policy::Policy;
use kob_x402::testkit::{pubkey, secret, MockChain};
use kob_x402::wire::{hex, FacilitatorRequest, Finality, Network, PaymentRequirements, SettlementResponse};

const PAYER: u8 = 7;
const MERCHANT: u8 = 8;
const NOW_MS: u64 = 1_800_000_000_000;
const AMOUNT: u64 = 20_000_000;

fn addr(n: u8) -> String {
    Address::new(Network::Testnet10.prefix(), Version::PubKey, &pubkey(n)).to_string()
}

struct Env {
    chain: Arc<MockChain>,
    clock: Arc<FixedClock>,
    ledger: Arc<Ledger>,
    fac: Arc<Facilitator>,
}

impl Env {
    fn new() -> Env {
        let chain = Arc::new(MockChain::new());
        let clock = Arc::new(FixedClock::new(NOW_MS));
        let ledger = Arc::new(Ledger::in_memory());
        let fac = Arc::new(Facilitator::new(
            Policy::new(Network::Testnet10),
            chain.clone(),
            clock.clone(),
            ledger.clone(),
            FacilitatorConfig { settle_wait: Duration::from_secs(5), poll_interval: Duration::from_millis(5), ..Default::default() },
        ));
        Env { chain, clock, ledger, fac }
    }

    fn coin(&self, amount: u64) -> KeyUtxo {
        let op = self.chain.add_p2pk(&pubkey(PAYER), amount);
        let u = self.chain.utxo(&op).unwrap();
        KeyUtxo {
            utxo: Utxo { transaction_id: op.txid, index: op.index, amount, block_daa_score: u.block_daa_score, covenant_id: None },
            pubkey: pubkey(PAYER),
        }
    }

    fn offer(&self, amount: u64, finality: Finality) -> PaymentRequirements {
        native_requirements(Network::Testnet10, amount, &addr(MERCHANT), 60, finality).unwrap()
    }

    fn request(&self, offer: &PaymentRequirements, coins: &[KeyUtxo], resource: &str, payment_id: Option<&str>) -> FacilitatorRequest {
        let rh = requirements_hash_hex(offer).unwrap();
        let request_hash = hex(&http_request_hash("GET", resource, None, &rh).unwrap());
        let mut opts = PayOptions::new(u64::MAX);
        opts.payment_id = payment_id.map(str::to_string);
        let payload = pay_native(offer, &request_hash, &secret(PAYER), coins, NOW_MS, &opts).unwrap();
        FacilitatorRequest {
            x402_version: 2,
            payment_payload: payload,
            payment_requirements: offer.clone(),
            request_hash: Some(request_hash),
            resource: None,
        }
    }

    fn mine_after_submit(&self, advance: u64) -> thread::JoinHandle<()> {
        let c = self.chain.clone();
        let base = c.submit_count();
        thread::spawn(move || {
            let t = std::time::Instant::now();
            while c.submit_count() <= base && t.elapsed() < Duration::from_secs(10) {
                thread::sleep(Duration::from_millis(1));
            }
            thread::sleep(Duration::from_millis(10));
            c.mine(advance);
        })
    }
}

fn diag(r: &SettlementResponse) -> String {
    r.extensions.as_ref().and_then(|e| e["kaspa"]["diagnostic"].as_str()).unwrap_or("").to_string()
}

#[test]
fn a_real_native_payment_verifies_settles_and_replays_idempotently() {
    let e = Env::new();
    let coin = e.coin(500_000_000);
    let offer = e.offer(AMOUNT, Finality::Accepted);
    let req = e.request(&offer, &[coin], "https://api.example.test/file", None);

    let v = e.fac.verify(&req);
    assert!(v.is_valid, "{v:?}");
    assert!(e.ledger.is_empty());

    let miner = e.mine_after_submit(0);
    let s = e.fac.settle("shop", &req);
    miner.join().unwrap();
    assert!(s.success, "{s:?}");
    assert_eq!(s.network.as_deref(), Some("kaspa:testnet-10"));
    assert_eq!(s.amount.as_deref(), Some("20000000"));
    let kaspa = &s.extensions.as_ref().unwrap()["kaspa"];
    assert_eq!(kaspa["binding"], "kaspa-exact-v2");
    assert_eq!(kaspa["profile"], "standard-native");
    assert_eq!(kaspa["finality"], "accepted");
    assert_eq!(kaspa["paymentOutputIndex"], 0);
    assert_eq!(e.chain.submit_count(), 1);
    assert_eq!(e.ledger.get(&s.transaction).unwrap().state, State::Accepted);

    // identical retry: the cached outcome, no second broadcast; the spent input no longer verifies but the ledger answers first
    assert_eq!(e.fac.settle("shop", &req), s);
    assert_eq!(e.chain.submit_count(), 1);
    // a fresh /verify of the settled payment reports the consumed input honestly
    assert!(!e.fac.verify(&req).is_valid);
}

#[test]
fn attacks_against_the_facilitator_with_real_payments() {
    let e = Env::new();
    let offer = e.offer(AMOUNT, Finality::Accepted);
    let coin = e.coin(500_000_000);
    let req = e.request(&offer, std::slice::from_ref(&coin), "https://api.example.test/a", None);

    // tampered price in the payer's copy of the offer
    let mut cheap = req.clone();
    cheap.payment_payload.accepted.amount = "1".into();
    assert_eq!(diag(&e.fac.settle("shop", &cheap)), "invalid_kaspa_x402_accepted");
    // the resource server's request hash differs from the one the payer signed
    let mut other_rh = req.clone();
    other_rh.request_hash = Some(hex(&[9; 32]));
    assert_eq!(diag(&e.fac.settle("shop", &other_rh)), "invalid_kaspa_x402_payload");
    // request hash absent
    let mut none = req.clone();
    none.request_hash = None;
    assert!(!e.fac.settle("shop", &none).success);
    // expired authorization (the verifier's clock is past the payer's expiry)
    e.clock.set(NOW_MS + 61_000);
    assert_eq!(diag(&e.fac.settle("shop", &req)), "expired_authorization");
    e.clock.set(NOW_MS);
    assert!(e.ledger.is_empty(), "none of these created state");
    assert_eq!(e.chain.submit_count(), 0);

    // an ambiguous first attempt keeps the coin consumed: a second payment of the same coin is refused
    let second = e.request(&e.offer(AMOUNT + 1, Finality::Accepted), std::slice::from_ref(&coin), "https://api.example.test/b", None);
    e.chain.set_submit_failure(Some(SubmitError::Unavailable("reset".into())));
    let s1 = e.fac.settle("shop", &req);
    assert_eq!(diag(&s1), "node_unavailable");
    e.chain.set_submit_failure(None);
    let s2 = e.fac.settle("shop", &second);
    assert_eq!(diag(&s2), "replay");
    assert_eq!(e.chain.submit_count(), 1);
    // the first payment is still recoverable by its identical retry
    let miner = e.mine_after_submit(0);
    let s1b = e.fac.settle("shop", &req);
    miner.join().unwrap();
    assert!(s1b.success, "{s1b:?}");
}

#[test]
fn payment_identifier_binds_the_request_and_the_merchant() {
    let e = Env::new();
    let offer = e.offer(AMOUNT, Finality::Accepted);
    let id = "explicit-payment-id-0001";
    let c1 = e.coin(500_000_000);
    let c2 = e.coin(500_000_000);
    let a = e.request(&offer, &[c1], "https://api.example.test/a", Some(id));
    let b = e.request(&offer, &[c2], "https://api.example.test/b", Some(id)); // same id, another resource
    let miner = e.mine_after_submit(0);
    let sa = e.fac.settle("shop", &a);
    miner.join().unwrap();
    assert!(sa.success);
    let sb = e.fac.settle("shop", &b);
    assert!(!sb.success);
    assert_eq!(diag(&sb), "kaspa_payment_identifier_conflict");
    assert_eq!(e.chain.submit_count(), 1);
}

#[test]
fn confirmed_finality_with_a_real_payment() {
    let e = Env::new();
    let offer = e.offer(AMOUNT, Finality::Confirmed);
    let req = e.request(&offer, &[e.coin(500_000_000)], "https://api.example.test/c", None);
    e.fac.set_settle_wait(Duration::from_millis(80));
    let miner = e.mine_after_submit(0);
    let s = e.fac.settle("shop", &req);
    miner.join().unwrap();
    assert!(!s.success);
    assert_eq!(diag(&s), "settlement_pending");
    e.chain.advance_daa(100);
    e.fac.set_settle_wait(Duration::from_secs(5));
    let s = e.fac.settle("shop", &req);
    assert!(s.success, "{s:?}");
    assert_eq!(s.extensions.as_ref().unwrap()["kaspa"]["finality"], "confirmed");
    assert_eq!(e.chain.submit_count(), 1);
}

#[test]
fn already_broadcast_by_the_payer_is_benign_and_mempool_alone_is_not_success() {
    let e = Env::new();
    let offer = e.offer(AMOUNT, Finality::Accepted);
    let req = e.request(&offer, &[e.coin(500_000_000)], "https://api.example.test/d", None);
    e.fac.set_settle_wait(Duration::from_millis(60));
    // the payer submits the transaction themselves
    let parsed = kob_x402::safe_tx::SafeTx::parse(&req.payment_payload.payload.transaction, 1 << 20).unwrap().to_consensus().unwrap();
    e.chain.submit(&parsed.tx).unwrap();
    let s = e.fac.settle("shop", &req);
    assert!(!s.success, "in the mempool only");
    assert_eq!(diag(&s), "settlement_pending");
    e.chain.mine(0);
    let s = e.fac.settle("shop", &req);
    assert!(s.success, "{s:?}");
}

#[test]
fn full_http_stack_with_a_real_payment() {
    use axum::body::Body;
    use axum::extract::ConnectInfo;
    use axum::http::{header, Request, StatusCode};
    use http_body_util::BodyExt;
    use kob_executor::x402::config::{api_key_hash, X402Config};
    use kob_executor::x402::http::{router, AppState};
    use tower::ServiceExt;

    let e = Env::new();
    let offer = e.offer(AMOUNT, Finality::Accepted);
    let req = e.request(&offer, &[e.coin(500_000_000)], "https://api.example.test/e", None);
    let cfg = format!(
        r#"{{"merchants":[{{"id":"shop","apiKeySha256":"{}","allowedPayTo":["{}"],"allowedAssets":["KAS"]}}]}}"#,
        hex(&api_key_hash("k-secret")),
        addr(MERCHANT)
    );
    let built = X402Config::from_json(&cfg).unwrap().build().unwrap();
    let app = router(Arc::new(AppState::new(e.fac.clone(), &built)));
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    let body = serde_json::to_vec(&req).unwrap();
    let miner = e.mine_after_submit(0);
    let (status, json) = rt.block_on(async {
        let mut r = Request::builder()
            .method("POST")
            .uri("/settle")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::AUTHORIZATION, "Bearer k-secret")
            .body(Body::from(body))
            .unwrap();
        r.extensions_mut().insert(ConnectInfo::<std::net::SocketAddr>("10.0.0.1:1".parse().unwrap()));
        let resp = app.oneshot(r).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice::<serde_json::Value>(&bytes).unwrap())
    });
    miner.join().unwrap();
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["success"], true, "{json}");
    assert_eq!(json["amount"], "20000000");
}
