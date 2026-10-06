//! The mining loop against a mock wRPC JSON node: getBlockTemplate -> search -> kaspa_pow verify -> submitBlock, and intermittent
//! mining driven by the mock's UTXO set (getBlockDagInfo / getUtxosByAddresses).
//!
//! The backend follows the build: CPU without `--features gpu`; with it, `auto` (the GPU where an OpenCL device exists, else CPU).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use kaspa_addresses::{Address, Prefix, Version};
use kaspa_consensus_core::header::Header;
use kaspa_pow::State;
use kaspa_rpc_core::RpcRawHeader;
use serde_json::{Value, json};
use tn10_miner::job::synthetic_header;
use tn10_miner::run::{self, RunConfig, Summary};
use tn10_miner::settings::{self, BackendKind, FileSettings};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

const KAS: u64 = 100_000_000;
/// ~1 in 4096 nonces pass
const BITS: u32 = 0x1f0f_ffff;
const VIRTUAL_DAA: u64 = 1_000_000;

#[derive(Default)]
struct Mock {
    templates: Vec<(Instant, Header)>,
    accepted: Vec<(Instant, u64)>,
    bad: Vec<String>,
    /// KAS credited per accepted block, as a coinbase UTXO `coinbase_age` DAA old at query time
    reward: u64,
    coinbase_age: u64,
    /// an extra immature coinbase that must never count
    immature: u64,
    /// a plain UTXO present from the start
    plain: u64,
    listings: usize,
    totals: usize,
}

fn reply(m: &Mutex<Mock>, method: &str, params: &Value) -> Value {
    let mut m = m.lock().unwrap();
    match method {
        "getBlockTemplate" => {
            let seed = m.templates.len() as u8;
            let h = synthetic_header(seed, BITS);
            m.templates.push((Instant::now(), h.clone()));
            json!({ "block": { "header": serde_json::to_value(RpcRawHeader::from(&h)).unwrap(), "transactions": [] }, "isSynced": true })
        }
        "submitBlock" => {
            let raw: RpcRawHeader = serde_json::from_value(params["block"]["header"].clone()).unwrap();
            let got = Header::try_from(&raw).unwrap();
            // the submitted header is a template header with only the nonce changed
            let tpl = m.templates.iter().rev().map(|(_, h)| h).find(|h| {
                let mut t = (*h).clone();
                t.nonce = got.nonce;
                t.finalize();
                t.hash == got.hash
            });
            let ok = tpl.is_some() && State::new(&got).check_pow(got.nonce).0 && params["block"]["transactions"] == json!([]);
            if ok {
                m.accepted.push((Instant::now(), got.nonce));
                json!({ "report": { "type": "success" } })
            } else {
                m.bad.push(format!("rejected nonce {:#x}", got.nonce));
                json!({ "report": { "type": "reject", "reason": "BlockInvalid" } })
            }
        }
        "getBlockDagInfo" => json!({ "virtualDaaScore": VIRTUAL_DAA }),
        "getUtxosByAddresses" => {
            m.listings += 1;
            let entry = |amount: u64, daa: u64, cb: bool| {
                json!({ "address": params["addresses"][0], "outpoint": { "transactionId": "00".repeat(32), "index": 0 },
                        "utxoEntry": { "amount": amount, "scriptPublicKey": "", "blockDaaScore": daa, "isCoinbase": cb, "covenantId": null } })
            };
            let mut entries = vec![entry(m.plain, 5, false), entry(m.immature, VIRTUAL_DAA - 10, true)];
            for _ in &m.accepted {
                entries.push(entry(m.reward, VIRTUAL_DAA - m.coinbase_age, true));
            }
            json!({ "entries": entries })
        }
        "getBalanceByAddress" => {
            m.totals += 1;
            json!({ "balance": m.plain + m.immature + m.reward * m.accepted.len() as u64 })
        }
        o => {
            m.bad.push(format!("unexpected method {o}"));
            Value::Null
        }
    }
}

async fn serve(mock: Arc<Mutex<Mock>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let mock = mock.clone();
            tokio::spawn(async move {
                let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else { return };
                while let Some(Ok(msg)) = ws.next().await {
                    let Message::Text(text) = msg else { continue };
                    let req: Value = serde_json::from_str(&text).unwrap();
                    let params = reply(&mock, req["method"].as_str().unwrap_or(""), &req["params"]);
                    let resp = json!({ "id": req["id"], "params": params });
                    if ws.send(Message::Text(resp.to_string())).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    url
}

fn pay_address() -> String {
    Address::new(Prefix::Testnet, Version::PubKey, &[7u8; 32]).to_string()
}

fn settings_from(json_text: &str) -> settings::Settings {
    let file: FileSettings = serde_json::from_str(json_text).unwrap();
    settings::resolve(&|_| None, &file, None).unwrap()
}

async fn mine(url: String, s: settings::Settings, max_blocks: u64, max_runtime: Duration) -> Summary {
    run::run(RunConfig {
        node: url,
        wallets: vec![pay_address()],
        max_blocks,
        settings: s,
        settings_file: None,
        env: Arc::new(|_| None),
        cli_backend: None,
        report_every: Duration::from_secs(30),
        max_runtime: Some(max_runtime),
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn template_found_verify_submit_loop() {
    let mock = Arc::new(Mutex::new(Mock::default()));
    let url = serve(mock.clone()).await;
    let s = settings_from(r#"{"threads":2,"roundMs":2000}"#);
    let sum = mine(url, s, 5, Duration::from_secs(120)).await;
    let m = mock.lock().unwrap();
    assert!(m.bad.is_empty(), "{:?}", m.bad);
    assert_eq!(sum.accepted, 5);
    assert_eq!(m.accepted.len(), 5);
    assert_eq!(sum.rejected, 0);
    assert_eq!(sum.verify_failures, 0);
    assert!(sum.templates >= 5);
    eprintln!("backend {} templates {} hashes {}", sum.backend, sum.templates, sum.hashes);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn intermittent_bursts_follow_the_spendable_balance() {
    // start: 50 KAS plain + 10000 KAS immature coinbase (must not count) -> below low (100): burst. Every accepted block adds a
    // mature 30 KAS coinbase; at >= 200 KAS (6 blocks) the burst must end and the miner must stop asking for templates.
    let mock = Arc::new(Mutex::new(Mock {
        reward: 30 * KAS,
        coinbase_age: 5000,
        immature: 10_000 * KAS,
        plain: 50 * KAS,
        ..Default::default()
    }));
    let url = serve(mock.clone()).await;
    let s = settings_from(r#"{"threads":2,"roundMs":300,"lowKas":100,"highKas":200,"pollSec":1}"#);
    let t0 = Instant::now();
    let sum = mine(url, s, 0, Duration::from_secs(8)).await;
    let m = mock.lock().unwrap();
    assert!(m.bad.is_empty(), "{:?}", m.bad);
    assert!(sum.accepted >= 5, "the burst mined up to the high watermark: {}", sum.accepted);
    assert_eq!(sum.bursts, 1);
    // the moment the balance reached 200 KAS: the 5th accepted block (50 + 5 * 30)
    let reached = m.accepted[4].0;
    let last_template = m.templates.last().unwrap().0;
    assert!(
        last_template.duration_since(reached) < Duration::from_millis(1000 + 300 + 500),
        "templates must stop within one poll + one round after the high watermark: {:?}",
        last_template.duration_since(reached)
    );
    // and it stayed idle for the rest of the run
    assert!(t0.elapsed() >= Duration::from_secs(8));
    assert!(Instant::now().duration_since(last_template) > Duration::from_secs(3));
    assert!(sum.mining < Duration::from_secs(6), "mined {:?} of 8 s", sum.mining);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_while_the_balance_is_high() {
    let mock = Arc::new(Mutex::new(Mock { plain: 1_000 * KAS, ..Default::default() }));
    let url = serve(mock.clone()).await;
    let s = settings_from(r#"{"threads":2,"lowKas":100,"highKas":200,"pollSec":1}"#);
    let sum = mine(url, s, 0, Duration::from_secs(3)).await;
    assert_eq!(mock.lock().unwrap().templates.len(), 0);
    assert_eq!((sum.accepted, sum.templates, sum.bursts), (0, 0, 0));
    // one full UTXO listing at start, then only the cheap total (no coinbase of ours can be immature)
    let m = mock.lock().unwrap();
    assert_eq!(m.listings, 1);
    assert!(m.totals >= 1, "totals {}", m.totals);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn duty_cycle_caps_continuous_mining() {
    let mock = Arc::new(Mutex::new(Mock::default()));
    let url = serve(mock.clone()).await;
    // continuous, but at most 30 % of a 4 s window
    let s = settings_from(r#"{"threads":1,"roundMs":200,"maxDuty":0.3,"dutyWindowSec":4}"#);
    let sum = mine(url, s, 0, Duration::from_secs(7)).await;
    // the duty counts the whole mining state (templates, search, submits): templates come from t=0 to 1.2 s, then none until the
    // 0.3 s resume budget is back at t=4.3 (the first burst slides out of the window), then again
    // (t=0 is the first template: opening a GPU backend compiles the kernel first)
    let t0 = mock.lock().unwrap().templates[0].0;
    let at: Vec<f64> = mock.lock().unwrap().templates.iter().map(|(t, _)| t.duration_since(t0).as_secs_f64()).collect();
    assert!(at.iter().any(|&x| x < 1.2), "{at:?}");
    assert!(!at.iter().any(|&x| x > 1.2 + 0.25 && x < 4.0), "templates during the duty pause: {at:?}");
    assert!(at.iter().any(|&x| x > 4.2), "{at:?}");
    assert!(sum.accepted > 0);
    let _ = BackendKind::Cpu;
}
