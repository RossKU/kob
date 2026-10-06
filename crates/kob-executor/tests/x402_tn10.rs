//! Live testnet-10 end-to-end test of x402: KAS exact, KCC-20 exact and
//! swap-and-pay settled through the real facilitator (HTTP in-process, `NodeChain` over wRPC) against
//! a TN10 node of your own, with fresh random keys funded by the self-mining tool.
//!
//! ONE sequential test, step by step:
//!
//! * (a) KAS `standard-native` through `POST /verify` and `POST /settle`, replay / conflict / tamper checks;
//! * (b) two KCC-20 tokens (A, B) issued with the reference 8/8 program and allowlisted in the policy;
//! * (c) KCC-20 `exact` payment of token A;
//! * (d) swap-and-pay on chain: SW1 (A -> KAS), SW2 (KAS -> B), SW3 (A -> KAS -> B via `SwapRoute`);
//! * (e) live `order_conflict`: a third party fills the bid the payer signed against; re-quote, re-sign, settle;
//! * (f) `confirmed` finality;
//! * (g) fee-floor probe (interop finding for the rc.1 binding);
//! * (h) intent-based swap-and-pay through an invoice: the merchant registers an invoice (`POST /invoices`), the
//!   payer fetches it (`GET /invoices/{id}`, the id is checked), signs ONE intent creation and pays it anonymously
//!   (`POST /invoices/{id}/pay`); the facilitator broadcasts the creation and executes the intent against a live bid
//!   of the indexer's book; the invoice status is `paid` with the execution; a second payment is a refused duplicate.
//!
//! Skipped when `KOB_SKIP_NETWORK_TESTS=1`. Env: `KOB_TN10_MINER` (REQUIRED: the path of the built `tn10-miner` binary, see
//! `tools/wallet-gate/miner/README.md`), `KOB_TN10_WRPC` (the testnet-10 node, default `ws://127.0.0.1:18210`; `KOB_TN10_NODE` is still read as an alias). Writes `x402-tn10-report.json`
//! into the cargo target directory. The whole test has a 25 minute budget.
#![allow(clippy::too_many_arguments)]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Display;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use kaspa_addresses::{Address, Prefix, Version};
use kaspa_consensus_core::subnets::SUBNETWORK_ID_NATIVE;
use kaspa_consensus_core::tx::{ScriptPublicKey, Transaction, TransactionInput, TransactionOutput, UtxoEntry};
use kob_executor::config::{IndexerConfig, StartMode};
use kob_executor::index_cli::node_client;
use kob_executor::indexer::book::snapshot as book_snapshot;
use kob_executor::indexer::ingest::Ingest;
use kob_executor::indexer::Indexer;
use kob_executor::tokens::ListingRules;
use kob_executor::x402::config::{api_key_hash, X402Config};
use kob_executor::x402::facilitator::{Facilitator, FacilitatorConfig, IntentRuntime, InvoiceRuntime, InvoiceStore};
use kob_executor::x402::http::{router, serve, AppState};
use kob_executor::x402::indexed::IndexedChain;
use kob_executor::x402::ledger::{Ledger, State};
use kob_executor::x402::node::{NodeChain, Rpc, WrpcClient};
use kob_executor::x402::quote::{book_view, quote as book_quote, QuoteRequest, Take};
use kob_protocol::artifacts::{template, TemplateId};
use kob_protocol::build::{build_batch, build_create_order, Batch, CreateOrder, Leg};
use kob_protocol::defaults::tips;
use kob_protocol::issue::{build_genesis, FundingUtxo, Holder, IssueSpec, SCHEME_P2PK_SCHNORR};
use kob_protocol::script::{p2pk_spk, push_data};
use kob_protocol::state::{AnyState, AskState, BidState, Kcc20State, OrderState};
use kob_protocol::tx::{
    finalize, masses, min_fee, pubkey_of, sighash, sign_digest, sign_locally, BuiltTx, FeeOptions, FinalizeOptions, KeyUtxo,
    OrderUtxo, TokenUtxo, Utxo, MIN_FEE_RATE,
};
use kob_x402::canonical::http_request_hash;
use kob_x402::chain::{ChainUtxo, ChainView, Clock, Outpoint, OutputStatus, SubmitError, SystemClock, Tracked, Txid};
use kob_x402::client::intent::{intent_requirements, pay_intent, IntentOptions};
use kob_x402::client::native::{native_requirements, pay_native, PayOptions};
use kob_x402::client::swap::{
    conflicting_orders, is_retryable_conflict, pay_swap, preflight_swap, swap_requirements, MerchantGain, OrderRef, PayAssetSpec,
    PayerFunds, Quote, SwapOfferParams, SwapOptions, SwapPayment,
};
use kob_x402::client::token::{kcc20_requirements, pay_kcc20_with, Kcc20Options};
use kob_x402::common::requirements_hash_hex;
use kob_x402::error::X402Error;
use kob_x402::intent::KeeperParams;
use kob_x402::policy::{AllowedToken, Custody};
use kob_x402::safe_tx::spk_from_hex;
use kob_x402::verify::VerifyCtx;
use kob_x402::wire::{
    hex, parse_hash32, FacilitatorRequest, Finality, Network, PaymentPayload, PaymentRequirements, SettlementResponse, VerifyResponse,
};
use serde_json::{json, Value};

const DEFAULT_NODE: &str = "ws://127.0.0.1:18210";
const KAS: u64 = 100_000_000;
/// Coinbase maturity on TN10 in DAA (about 100 s at 10 DAA/s) plus a safety margin.
const COINBASE_MATURITY: u64 = 1_000 + 40;
const TEST_BUDGET: Duration = Duration::from_secs(25 * 60);
const API_KEY: &str = "tn10-e2e-merchant-key";
const MERCHANT_ID: &str = "shop";
/// Order parameters (the protocol fixtures' scale): one whole token = 1000 base units, prices in sompi per whole token.
const SCALE: i64 = 1_000;
/// One whole token in base units (order amounts are whole tokens times this).
const WHOLE: i64 = SCALE;
const TIP: i64 = 100_000;
const P_BID: i64 = 245_000_000;
const P_ASK: i64 = 250_000_000;
const DELIVERY_CARRIER: i64 = 10 * KAS as i64;
const ORDER_CARRIER: u64 = 10 * KAS;
const TOKEN_CARRIER: u64 = 10 * KAS;
const PAY_CARRIER: u64 = 2 * KAS;
const PROGRAM: TemplateId = TemplateId::Kcc20Ref8x8;

// ------------------------------------------------------------------------------------------ logging

static START: OnceLock<Instant> = OnceLock::new();

fn elapsed() -> Duration {
    START.get_or_init(Instant::now).elapsed()
}

macro_rules! log {
    ($($a:tt)*) => { println!("[{:>7.1}s] {}", elapsed().as_secs_f64(), format!($($a)*)) };
}

fn check_budget() {
    assert!(elapsed() < TEST_BUDGET, "whole-test budget of {TEST_BUDGET:?} exhausted");
}

fn now_ms() -> u64 {
    SystemClock.now_ms()
}

/// Retries a transient failure with exponential backoff (a busy node), then fails the test.
fn retry<T, E: Display>(what: &str, mut f: impl FnMut() -> Result<T, E>) -> T {
    let mut delay = Duration::from_millis(400);
    for attempt in 1..=8 {
        match f() {
            Ok(v) => return v,
            Err(e) => {
                log!("transient error in {what} (attempt {attempt}/8): {e}");
                check_budget();
                std::thread::sleep(delay);
                delay = (delay * 2).min(Duration::from_secs(8));
            }
        }
    }
    panic!("{what}: giving up after 8 attempts");
}

fn poll<T>(what: &str, timeout: Duration, every: Duration, mut f: impl FnMut() -> Option<T>) -> T {
    let t = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        check_budget();
        assert!(t.elapsed() < timeout, "timed out after {timeout:?} waiting for {what}");
        std::thread::sleep(every);
    }
}

// ------------------------------------------------------------------------------------------ report

struct Report {
    path: PathBuf,
    root: serde_json::Map<String, Value>,
    steps: Vec<Value>,
    step_started: Instant,
    fees: u64,
}

impl Report {
    fn new(path: PathBuf) -> Report {
        let mut root = serde_json::Map::new();
        root.insert("test".into(), json!("x402_tn10"));
        root.insert("startedAtUnixMs".into(), json!(now_ms()));
        root.insert("result".into(), json!("running"));
        Report { path, root, steps: vec![], step_started: Instant::now(), fees: 0 }
    }
    fn set(&mut self, k: &str, v: Value) {
        self.root.insert(k.into(), v);
    }
    /// Starts timing a step.
    fn begin(&mut self, name: &str) {
        log!("=== step {name}");
        self.step_started = Instant::now();
    }
    fn step(&mut self, name: &str, detail: Value) {
        let ms = self.step_started.elapsed().as_millis() as u64;
        log!("=== step {name} done in {:.1}s", ms as f64 / 1000.0);
        self.steps.push(json!({ "step": name, "ms": ms, "detail": detail }));
        self.save();
    }
    fn save(&self) {
        let mut root = self.root.clone();
        root.insert("steps".into(), Value::Array(self.steps.clone()));
        root.insert("elapsedSeconds".into(), json!(elapsed().as_secs_f64()));
        root.insert("feesSompi".into(), json!(self.fees));
        if let Ok(text) = serde_json::to_string_pretty(&Value::Object(root)) {
            let _ = std::fs::write(&self.path, text);
        }
    }
}

impl Drop for Report {
    fn drop(&mut self) {
        if self.root.get("result") == Some(&json!("running")) {
            self.root.insert("result".into(), json!("failed (see the test output)"));
        }
        self.save();
    }
}

// ------------------------------------------------------------------------------------------ keys

#[derive(Clone)]
struct Key {
    name: &'static str,
    sk: [u8; 32],
    pk: [u8; 32],
}

impl Key {
    fn random(name: &'static str) -> Key {
        use rand::RngCore;
        loop {
            let mut sk = [0u8; 32];
            rand::thread_rng().fill_bytes(&mut sk);
            if let Ok(pk) = pubkey_of(&sk) {
                return Key { name, sk, pk };
            }
        }
    }
    fn addr(&self) -> String {
        Address::new(Prefix::Testnet, Version::PubKey, &self.pk).to_string()
    }
    fn spk(&self) -> ScriptPublicKey {
        p2pk_spk(&self.pk)
    }
    fn json(&self) -> Value {
        json!({ "address": self.addr(), "publicKey": hex(&self.pk), "secretKey": hex(&self.sk) })
    }
}

// ------------------------------------------------------------------------------------------ node access

struct Net {
    node: String,
    chain: Arc<NodeChain>,
    rpc: WrpcClient,
}

fn key_utxo(op: &Outpoint, u: &ChainUtxo, pk: [u8; 32]) -> KeyUtxo {
    KeyUtxo {
        utxo: Utxo {
            transaction_id: op.txid,
            index: op.index,
            amount: u.amount,
            block_daa_score: u.block_daa_score,
            covenant_id: None,
        },
        pubkey: pk,
    }
}

impl Net {
    fn connect(node: &str) -> Net {
        Net {
            node: node.to_string(),
            chain: Arc::new(NodeChain::connect(node, Network::Testnet10, Duration::from_secs(20))),
            rpc: WrpcClient::new(node, Duration::from_secs(20)),
        }
    }
    fn vdaa(&self) -> u64 {
        retry("virtual DAA score", || self.chain.virtual_daa_score())
    }
    fn utxos_of(&self, spk: &ScriptPublicKey) -> Vec<(Outpoint, ChainUtxo)> {
        retry("getUtxosByAddresses", || self.chain.utxos_of(spk))
    }
    /// Spendable KAS UTXOs of a key: mature, not covenant-bound, largest first.
    fn kas_utxos(&self, k: &Key) -> Vec<KeyUtxo> {
        let vdaa = self.vdaa();
        let mut v: Vec<KeyUtxo> = self
            .utxos_of(&k.spk())
            .iter()
            .filter(|(_, u)| u.covenant_id.is_none() && (!u.is_coinbase || vdaa >= u.block_daa_score + COINBASE_MATURITY))
            .map(|(o, u)| key_utxo(o, u, k.pk))
            .collect();
        v.sort_by(|a, b| {
            b.utxo.amount.cmp(&a.utxo.amount).then((a.utxo.transaction_id, a.utxo.index).cmp(&(b.utxo.transaction_id, b.utxo.index)))
        });
        v
    }
    fn balance(&self, k: &Key) -> u64 {
        self.utxos_of(&k.spk()).iter().map(|(_, u)| u.amount).sum()
    }
    /// Polls until the output is in the virtual UTXO set.
    fn wait_utxo(&self, what: &str, op: &Outpoint, spk: &ScriptPublicKey) -> ChainUtxo {
        poll(&format!("{what} to be accepted ({op})"), Duration::from_secs(180), Duration::from_millis(700), || {
            self.chain.utxos(&[(*op, spk.clone())]).ok().and_then(|mut v| v.remove(0))
        })
    }
    fn wait_gone(&self, what: &str, op: &Outpoint, spk: &ScriptPublicKey) {
        poll(&format!("{what} to be spent ({op})"), Duration::from_secs(180), Duration::from_millis(700), || {
            match self.chain.utxos(&[(*op, spk.clone())]) {
                Ok(v) if v[0].is_none() => Some(()),
                _ => None,
            }
        })
    }
    /// Submits a transaction, retrying only outcomes that are not decided by the node.
    fn try_submit(&self, tx: &Transaction) -> Result<Txid, SubmitError> {
        let mut delay = Duration::from_millis(500);
        for _ in 0..6 {
            match self.chain.submit(tx) {
                Err(SubmitError::Unavailable(m)) => {
                    log!("submit outcome unknown ({m}); the identical transaction is resent");
                    std::thread::sleep(delay);
                    delay *= 2;
                }
                Err(SubmitError::AlreadyKnown) => return Ok(tx.id().as_bytes()),
                other => return other,
            }
        }
        Err(SubmitError::Unavailable("node unavailable after retries".into()))
    }
    fn submit(&self, what: &str, tx: &Transaction) -> Txid {
        match self.try_submit(tx) {
            Ok(id) => {
                log!("submitted {what}: {}", hex(&id));
                id
            }
            Err(e) => panic!("submit of {what} failed: {e:?}"),
        }
    }
    /// Entries of the address mempool (sending + receiving) for `addrs`.
    fn mempool_entries(&self, addrs: &[String]) -> usize {
        let r = retry("getMempoolEntriesByAddresses", || {
            self.rpc.call(
                "getMempoolEntriesByAddresses",
                json!({ "addresses": addrs, "includeOrphanPool": true, "filterTransactionPool": false }),
            )
        });
        r["entries"]
            .as_array()
            .map(|a| {
                a.iter().map(|e| e["sending"].as_array().map_or(0, Vec::len) + e["receiving"].as_array().map_or(0, Vec::len)).sum()
            })
            .unwrap_or(0)
    }
    fn in_mempool(&self, txid: &Txid) -> bool {
        retry("getMempoolEntry", || self.chain.in_mempool(txid))
    }
}

// ------------------------------------------------------------------------------------------ miner

fn target_dir() -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).parent().expect("target dir").to_path_buf()
}

/// The miner binary: `KOB_TN10_MINER` is required (build it with `cargo build --release --manifest-path tools/wallet-gate/miner/Cargo.toml`).
fn locate_miner() -> PathBuf {
    let p = std::env::var("KOB_TN10_MINER")
        .expect("KOB_TN10_MINER must point at the built tn10-miner binary (tools/wallet-gate/miner), or set KOB_SKIP_NETWORK_TESTS=1");
    assert!(Path::new(&p).exists(), "KOB_TN10_MINER={p} does not exist");
    p.into()
}

/// Mines `blocks` blocks, paying the coinbase round robin to `wallets` (an address may repeat to weight it).
fn run_miner(node: &str, wallets: &[String], blocks: usize) {
    let exe = locate_miner();
    assert_eq!(blocks % wallets.len(), 0, "MAX_BLOCKS must be a multiple of the wallet list so the split is exact");
    let log_path = target_dir().join("x402-tn10-miner.log");
    let logf = std::fs::File::create(&log_path).expect("miner log");
    let mut child = Command::new(&exe)
        .env("WALLET", wallets.join(","))
        .env("MAX_BLOCKS", blocks.to_string())
        .env("NODE", node)
        .stdout(Stdio::from(logf.try_clone().unwrap()))
        .stderr(Stdio::from(logf))
        .spawn()
        .unwrap_or_else(|e| panic!("start the miner {}: {e}", exe.display()));
    log!("miner {} started for {blocks} blocks (log: {})", exe.display(), log_path.display());
    let t = Instant::now();
    loop {
        match child.try_wait().expect("miner status") {
            Some(st) => {
                assert!(st.success(), "the miner failed: {st:?} (see {})", log_path.display());
                break;
            }
            None if t.elapsed() > Duration::from_secs(12 * 60) => {
                let _ = child.kill();
                panic!("the miner did not finish {blocks} blocks within 12 minutes (see {})", log_path.display());
            }
            None => std::thread::sleep(Duration::from_secs(2)),
        }
    }
    log!("miner finished in {:.0}s", t.elapsed().as_secs_f64());
}

// ------------------------------------------------------------------------------------------ transactions

/// A version-0 P2PK spend of `inputs` (all owned by `sk`): `pays` plus a change output to `change_spk`;
/// fee = `fee` or the exact relay floor. Signed, storage mass committed.
fn plain_kas_tx(
    inputs: &[KeyUtxo],
    pays: &[(u64, ScriptPublicKey)],
    change_spk: &ScriptPublicKey,
    fee: Option<u64>,
    sk: &[u8; 32],
) -> (Transaction, Vec<UtxoEntry>, u64) {
    let total: u64 = inputs.iter().map(|k| k.utxo.amount).sum();
    let pay_sum: u64 = pays.iter().map(|p| p.0).sum();
    let entries: Vec<UtxoEntry> =
        inputs.iter().map(|k| UtxoEntry::new(k.utxo.amount, p2pk_spk(&k.pubkey), k.utxo.block_daa_score, false, None)).collect();
    let build = |fee: u64, sign: bool| -> Transaction {
        let ins = inputs.iter().map(|k| TransactionInput::new(k.utxo.outpoint(), push_data(&[0u8; 65]), u64::MAX, 1)).collect();
        let mut outs: Vec<TransactionOutput> = pays.iter().map(|(v, s)| TransactionOutput::new(*v, s.clone())).collect();
        outs.push(TransactionOutput::new(total - pay_sum - fee, change_spk.clone()));
        let mut tx = Transaction::new(0, ins, outs, 0, SUBNETWORK_ID_NATIVE, 0, vec![]);
        tx.set_storage_mass(masses(&tx, &entries).storage);
        if sign {
            for i in 0..tx.inputs.len() {
                let sig = sign_digest(sk, &sighash(&tx, &entries, i)).expect("sign");
                tx.inputs[i].signature_script = push_data(&sig);
            }
            tx.finalize();
        }
        tx
    };
    let fee = fee.unwrap_or_else(|| {
        let mut f = 0u64;
        for _ in 0..8 {
            let need = min_fee(&masses(&build(f, false), &entries), MIN_FEE_RATE);
            if need <= f {
                break;
            }
            f = need;
        }
        f
    });
    (build(fee, true), entries, fee)
}

/// Greedy selection of the largest UTXOs covering `need`.
fn pick(utxos: &[KeyUtxo], need: u64, max: usize) -> Vec<KeyUtxo> {
    let mut sum = 0;
    let mut out = vec![];
    for u in utxos {
        if sum >= need || out.len() == max {
            break;
        }
        sum += u.utxo.amount;
        out.push(u.clone());
    }
    assert!(sum >= need, "not enough spendable KAS: need {need} sompi, picked {sum}");
    out
}

fn secrets(keys: &[&Key]) -> BTreeMap<[u8; 32], [u8; 32]> {
    keys.iter().map(|k| (k.pk, k.sk)).collect()
}

/// Signs a builder transaction locally, commits the exact budgets, runs the engine and returns it.
fn sign_built(built: &BuiltTx, keys: &BTreeMap<[u8; 32], [u8; 32]>) -> (Transaction, Vec<UtxoEntry>, u64) {
    let sigs = sign_locally(built, keys).expect("sign locally");
    let signed = finalize(built, &sigs, FinalizeOptions { tighten_budgets: true }).expect("finalize");
    let (tx, entries) = signed.tx.to_tx().expect("signed tx");
    kob_protocol::verify::validate(&tx, &entries).expect("engine validation of our own transaction");
    (tx, entries, signed.fee.fee)
}

fn op_of(txid: &Txid, index: u32) -> Outpoint {
    Outpoint::new(*txid, index)
}

// ------------------------------------------------------------------------------------------ tokens

#[derive(Clone)]
struct Issued {
    ticker: &'static str,
    cov: [u8; 32],
    ext: [u8; 32],
    txid: Txid,
    /// Token UTXOs per holder in output order.
    utxos: Vec<TokenUtxo>,
    program_hash: [u8; 32],
}

impl Issued {
    fn cfg_json(&self) -> Value {
        json!({
            "covenantId": hex(&self.cov),
            "programTemplateHash": hex(&self.program_hash),
            "extensionCommitment": hex(&self.ext),
            "custody": "unconditional",
            "ticker": self.ticker,
            "decimals": 3,
        })
    }
}

fn token_spk(state: &Kcc20State) -> ScriptPublicKey {
    state.spk_with(template(PROGRAM))
}

/// Registers a token UTXO from the chain (amount, DAA) after verifying the script and covenant id.
fn token_utxo_on_chain(net: &Net, txid: &Txid, index: u32, cov: [u8; 32], state: Kcc20State) -> TokenUtxo {
    let op = op_of(txid, index);
    let u = net.wait_utxo("token utxo", &op, &token_spk(&state));
    assert_eq!(u.covenant_id, Some(cov), "token output must carry the token's covenant id");
    TokenUtxo {
        utxo: Utxo { transaction_id: *txid, index, amount: u.amount, block_daa_score: u.block_daa_score, covenant_id: Some(cov) },
        state: state.into(),
    }
}

fn issue_token(
    net: &Net,
    rep: &mut Report,
    issuer: &Key,
    name: &'static str,
    ticker: &'static str,
    holders: &[([u8; 32], u64)],
) -> Issued {
    let supply: u64 = holders.iter().map(|h| h.1).sum();
    let hs: Vec<Holder> = holders.iter().map(|(pk, a)| Holder::new(*pk, SCHEME_P2PK_SCHNORR, *a)).collect();
    let coins = net.kas_utxos(issuer);
    let funding = pick(&coins, TOKEN_CARRIER * holders.len() as u64 + KAS, 12);
    let fund: Vec<FundingUtxo> =
        funding.iter().map(|k| FundingUtxo { outpoint: k.utxo.outpoint(), amount: k.utxo.amount, owner_pubkey: k.pubkey }).collect();
    let mut spec = IssueSpec::new(name, ticker, 3, supply, hs, fund);
    spec.carrier = TOKEN_CARRIER;
    let mut plan = build_genesis(&spec).expect("genesis plan");
    plan.sign(&secp256k1::SecretKey::from_slice(&issuer.sk).expect("issuer key")).expect("sign genesis");
    plan.verify().expect("genesis verifies (engine, covenant context, mass)");
    // the issue program and the x402 verifiers' pinned program must be the same script
    let program_hash = plan.program.template_hash;
    assert_eq!(program_hash, template(PROGRAM).hash, "issued program hash differs from the pinned 8/8 template");
    for s in &plan.states {
        let st = Kcc20State::p2pk(s.amount as i64, s.owner, s.extension_commitment);
        assert_eq!(plan.program.spk(s), token_spk(&st), "issue::Program and the x402 template disagree on the token script");
    }
    let txid = net.submit(&format!("{ticker} issuance"), &plan.tx);
    rep.fees += plan.fee;
    let cov = plan.covenant_id.as_bytes();
    let mut utxos = vec![];
    for (i, s) in plan.states.iter().enumerate() {
        utxos.push(token_utxo_on_chain(net, &txid, i as u32, cov, Kcc20State::p2pk(s.amount as i64, s.owner, s.extension_commitment)));
    }
    log!("token {name} ({ticker}) issued: covenant {} in {}", hex(&cov), hex(&txid));
    Issued { ticker, cov, ext: spec.extension_commitment, txid, utxos, program_hash }
}

// ------------------------------------------------------------------------------------------ orders

fn bid_state(maker: &Key, t: &Issued, vdaa: u64) -> BidState {
    let tp = template(PROGRAM);
    BidState {
        maker: maker.pk,
        token_cov_id: t.cov,
        token_tpl_hash: tp.hash,
        tpl_prefix_len: tp.prefix.len() as i64,
        tpl_suffix_len: tp.suffix.len() as i64,
        extension_commitment: t.ext,
        scale: SCALE,
        min_fill: WHOLE,
        price: P_BID,
        tip: TIP,
        tif: 0,
        active_from: 0,
        expiry_daa: vdaa as i64 + 3_000_000,
        refund_tip: tips(PROGRAM).expect("tips").refund_tip as i64,
        reserve: 0,
        delivery_carrier: DELIVERY_CARRIER,
        interval: 0,
        max_fill: 0,
        slope: 0,
        price_end: 0,
        decay_step: 1_000,
    }
}

fn ask_state(maker: &Key, t: &Issued, n: i64, vdaa: u64) -> AskState {
    let tp = template(PROGRAM);
    AskState {
        maker: maker.pk,
        token_cov_id: t.cov,
        token_tpl_hash: tp.hash,
        tpl_prefix_len: tp.prefix.len() as i64,
        tpl_suffix_len: tp.suffix.len() as i64,
        scale: SCALE,
        min_fill: WHOLE,
        price: P_ASK,
        tip: TIP,
        tif: 0,
        active_from: 0,
        expiry_daa: vdaa as i64 + 3_000_000,
        refund_tip: tips(PROGRAM).expect("tips").refund_tip as i64,
        interval: 0,
        max_fill: 0,
        slope: 0,
        price_end: 0,
        decay_step: 1_000,
        amount_left: n * WHOLE,
    }
}

struct LiveBid {
    order: OrderUtxo<BidState>,
    txid: Txid,
}

struct LiveAsk {
    order: OrderUtxo<AskState>,
    txid: Txid,
}

/// A bid funded for `n` whole tokens in at most `fills` fills.
fn create_bid(net: &Net, rep: &mut Report, maker: &Key, t: &Issued, n: i64, fills: i64) -> LiveBid {
    let vdaa = net.vdaa();
    let state = bid_state(maker, t, vdaa);
    let value = state.escrow(n * WHOLE, fills).expect("escrow") as u64;
    let coins = net.kas_utxos(maker);
    let req = CreateOrder {
        order: AnyState::KobBid(state.clone()),
        value,
        tokens: vec![],
        token_carrier: 0,
        funding: pick(&coins, value + KAS, 12),
        change: None,
        lock_time: 0,
        deadline: None,
        records: vec![],
        fee: FeeOptions::default(),
    };
    let built = build_create_order(&req, &kob_protocol::budget::lookup).expect("create bid");
    let (tx, _, fee) = sign_built(&built, &secrets(&[maker]));
    let txid = net.submit(&format!("bid of {} ({n} whole tokens)", t.ticker), &tx);
    rep.fees += fee;
    let cov = built.covenants[0].covenant_id;
    let u = net.wait_utxo("bid order", &op_of(&txid, 0), &state.spk());
    assert_eq!(u.covenant_id, Some(cov));
    assert_eq!(u.amount, value);
    LiveBid {
        order: OrderUtxo {
            utxo: Utxo { transaction_id: txid, index: 0, amount: value, block_daa_score: u.block_daa_score, covenant_id: Some(cov) },
            state,
        },
        txid,
    }
}

/// An ask of `n` whole tokens.
fn create_ask(net: &Net, rep: &mut Report, maker: &Key, t: &Issued, holding: &TokenUtxo, n: i64) -> LiveAsk {
    let vdaa = net.vdaa();
    let state = ask_state(maker, t, n, vdaa);
    let coins = net.kas_utxos(maker);
    let req = CreateOrder {
        order: AnyState::KobAsk(state.clone()),
        value: ORDER_CARRIER,
        tokens: vec![holding.clone()],
        token_carrier: TOKEN_CARRIER,
        funding: pick(&coins, ORDER_CARRIER + KAS, 12),
        change: None,
        lock_time: 0,
        deadline: None,
        records: vec![],
        fee: FeeOptions::default(),
    };
    let built = build_create_order(&req, &kob_protocol::budget::lookup).expect("create ask");
    let (tx, _, fee) = sign_built(&built, &secrets(&[maker]));
    let txid = net.submit(&format!("ask of {} ({n} whole tokens)", t.ticker), &tx);
    rep.fees += fee;
    let cov = built.covenants[0].covenant_id;
    let u = net.wait_utxo("ask order", &op_of(&txid, 0), &state.spk());
    assert_eq!(u.covenant_id, Some(cov));
    let custody_state = Kcc20State::custody(n * WHOLE, cov, t.ext);
    let custody = token_utxo_on_chain(net, &txid, 1, t.cov, custody_state);
    assert_eq!(custody.utxo.amount, TOKEN_CARRIER);
    LiveAsk {
        order: OrderUtxo {
            utxo: Utxo {
                transaction_id: txid,
                index: 0,
                amount: ORDER_CARRIER,
                block_daa_score: u.block_daa_score,
                covenant_id: Some(cov),
            },
            state,
        },
        txid,
    }
}

// ------------------------------------------------------------------------------------------ facilitator service

/// The in-process indexer (as in `kob-executor run`): follows TN10 from the sink the test starts at; the
/// facilitator follows acceptance through it, and swap quotes come from its book.
struct Idx {
    ingest: Arc<Mutex<Ingest>>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Idx {
    fn drop(&mut self) {
        if let Some(s) = self.shutdown.take() {
            let _ = s.send(());
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn start_indexer(node: &str) -> Idx {
    let data_dir = target_dir().join(format!("x402-tn10-index-{}", now_ms()));
    let mut cfg = IndexerConfig {
        network: "testnet-10".into(),
        rpc_url: node.to_string(),
        data_dir,
        start: StartMode::Sink,
        poll_interval_ms: 200,
        // the test issues its own tokens: no allowlist, small orders, test expiries
        rules: ListingRules {
            require_allowlist: false,
            min_order_value_sompi: 1,
            max_expiry_span_daa: 1 << 40,
            ..ListingRules::default()
        },
        ..IndexerConfig::default()
    };
    cfg.api.enabled = false;
    let indexer = Indexer::open(cfg.clone()).expect("open the indexer");
    let ingest = indexer.ingest.clone();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let thread = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().expect("runtime");
        rt.block_on(async move {
            let running = indexer.spawn(node_client(&cfg)).expect("start the follower");
            let _ = rx.await;
            running.stop().await;
        });
    });
    log!("indexer following {node} from its sink");
    Idx { ingest, shutdown: Some(tx), thread: Some(thread) }
}

impl Idx {
    /// A swap quote from the indexer's book once it lists every order of `listed` and none of `gone`.
    fn quote(&self, what: &str, reqs: &[QuoteRequest], listed: &[Outpoint], gone: &[Outpoint], lock_time: u64) -> Quote {
        let book = poll(&format!("{what}: the indexer's book"), Duration::from_secs(180), Duration::from_millis(500), || {
            let b = book_snapshot(self.ingest.lock().unwrap().conn(), None).ok()?;
            let ops: BTreeSet<Outpoint> =
                b.orders.iter().map(|o| Outpoint::new(o.order.utxo.transaction_id, o.order.utxo.index)).collect();
            (listed.iter().all(|o| ops.contains(o)) && gone.iter().all(|o| !ops.contains(o))).then_some(b)
        });
        let q =
            book_quote(&book, reqs, lock_time, now_ms() / 1000).unwrap_or_else(|| panic!("{what}: the book cannot fill the quote"));
        log!(
            "{what}: quoted from the indexer's book: {:?}",
            q.orders.iter().map(|o| (o.outpoint().to_string(), o.amount())).collect::<Vec<_>>()
        );
        q
    }
}

/// The script of a quoted order (to watch it being spent).
fn quoted_spk(o: &OrderRef) -> ScriptPublicKey {
    match &o.leg {
        Leg::Bid { order, .. } => order.state.spk(),
        Leg::Ask { order, .. } => order.state.spk(),
        _ => panic!("quotes hold plain bids and asks"),
    }
}

struct Svc {
    fac: Arc<Facilitator>,
    view: Arc<IndexedChain<Arc<NodeChain>>>,
    addr: SocketAddr,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Svc {
    fn drop(&mut self) {
        if let Some(s) = self.shutdown.take() {
            let _ = s.send(());
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Starts the facilitator as `kob-executor run` does (real `NodeChain` for UTXO facts and submission, the
/// in-process indexer's acceptance tracking for finality, real verifiers) behind its HTTP service on an
/// ephemeral port.
fn start_service(net: &Net, idx: &Idx, ledger: Arc<Ledger>, merchant: &Key, assets: &[String], tokens: &[Value]) -> Svc {
    start_service_with(net, idx, ledger, merchant, assets, tokens, None)
}

/// [`start_service`], with intent-based swap-and-pay (the indexer's book, `keeper` receiving the keeper's share) and
/// invoices when `keeper` is set.
fn start_service_with(
    net: &Net,
    idx: &Idx,
    ledger: Arc<Ledger>,
    merchant: &Key,
    assets: &[String],
    tokens: &[Value],
    keeper: Option<[u8; 32]>,
) -> Svc {
    let mut allowed = vec!["KAS".to_string()];
    allowed.extend(assets.iter().cloned());
    let cfg = json!({
        "network": "kaspa:testnet-10",
        "node": net.node,
        "confirmationsDaa": 20,
        "tokens": tokens,
        "merchants": [{
            "id": MERCHANT_ID,
            "apiKeySha256": hex(&api_key_hash(API_KEY)),
            "allowedPayTo": [merchant.addr()],
            "allowedAssets": allowed,
        }],
        "settleWaitMs": 60_000,
        "pollIntervalMs": 300,
        "nodeTimeoutMs": 20_000,
    });
    let built = X402Config::from_json(&cfg.to_string()).expect("config").build().expect("config builds");
    assert_eq!(built.policy.tokens.iter().count(), tokens.len());
    let view = Arc::new(IndexedChain::new(net.chain.clone(), idx.ingest.clone()));
    let mut fac = Facilitator::new(
        built.policy.clone(),
        view.clone(),
        Arc::new(SystemClock),
        ledger,
        FacilitatorConfig {
            settle_wait: Duration::from_millis(built.cfg.settle_wait_ms),
            poll_interval: Duration::from_millis(built.cfg.poll_interval_ms),
            reorg_watch_daa: built.cfg.reorg_watch_daa,
            kill_switch_file: None,
            ..Default::default()
        },
    );
    if let Some(k) = keeper {
        let ingest = idx.ingest.clone();
        fac = fac
            .with_intents(IntentRuntime {
                book: Arc::new(move || {
                    let b = book_snapshot(ingest.lock().unwrap().conn(), None).map_err(|e| e.to_string())?;
                    Ok(book_view(&b))
                }),
                keeper: KeeperParams { keeper: k, ..KeeperParams::default() },
                max_attempts: 10,
                lock_margin_daa: 50,
            })
            .with_invoices(InvoiceRuntime {
                store: Arc::new(InvoiceStore::in_memory()),
                max_lifetime_ms: 3_600_000,
                public_url: None,
                max_open_per_merchant: 100,
            });
    }
    let fac = Arc::new(fac);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let state = Arc::new(AppState::new(fac.clone(), &built));
    let header_timeout = Duration::from_millis(built.cfg.header_timeout_ms);
    let max_conn = built.cfg.max_connections;
    let thread = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().expect("runtime");
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::from_std(listener).expect("listener");
            let app = router(state);
            let _ = serve(listener, app, header_timeout, max_conn, async {
                let _ = rx.await;
            })
            .await;
        });
    });
    log!("facilitator up on http://{addr} ({} tokens allowlisted)", tokens.len());
    Svc { fac, view, addr, shutdown: Some(tx), thread: Some(thread) }
}

/// Minimal HTTP/1.1 client (`Connection: close`): returns the status and the body.
fn http(addr: SocketAddr, method: &str, path: &str, key: Option<&str>, body: &[u8]) -> (u16, Vec<u8>) {
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(10)).expect("connect to the facilitator");
    s.set_read_timeout(Some(Duration::from_secs(180))).unwrap();
    let mut head = format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nContent-Type: application/json\r\n");
    if let Some(k) = key {
        head.push_str(&format!("Authorization: Bearer {k}\r\n"));
    }
    head.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    s.write_all(head.as_bytes()).unwrap();
    s.write_all(body).unwrap();
    let mut raw = vec![];
    s.read_to_end(&mut raw).expect("read the response");
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").expect("http response head") + 4;
    let head = String::from_utf8_lossy(&raw[..split]).to_string();
    let status: u16 = head.split_whitespace().nth(1).and_then(|s| s.parse().ok()).expect("status");
    let mut body = raw[split..].to_vec();
    if head.to_ascii_lowercase().contains("transfer-encoding: chunked") {
        let (mut out, mut rest) = (vec![], &body[..]);
        loop {
            let nl = rest.windows(2).position(|w| w == b"\r\n").expect("chunk size");
            let n = usize::from_str_radix(std::str::from_utf8(&rest[..nl]).unwrap().trim(), 16).expect("chunk size hex");
            if n == 0 {
                break;
            }
            out.extend_from_slice(&rest[nl + 2..nl + 2 + n]);
            rest = &rest[nl + 2 + n + 2..];
        }
        body = out;
    }
    (status, body)
}

impl Svc {
    fn verify(&self, req: &FacilitatorRequest) -> VerifyResponse {
        let (st, body) = http(self.addr, "POST", "/verify", Some(API_KEY), &serde_json::to_vec(req).unwrap());
        assert_eq!(st, 200, "/verify: {}", String::from_utf8_lossy(&body));
        serde_json::from_slice(&body).expect("verify response")
    }
    fn settle(&self, req: &FacilitatorRequest) -> SettlementResponse {
        let (st, body) = http(self.addr, "POST", "/settle", Some(API_KEY), &serde_json::to_vec(req).unwrap());
        assert_eq!(st, 200, "/settle: {}", String::from_utf8_lossy(&body));
        serde_json::from_slice(&body).expect("settle response")
    }
    fn broadcasts(&self) -> u64 {
        self.fac.metrics.broadcasts.load(std::sync::atomic::Ordering::Relaxed)
    }
}

fn diag_of(r: &SettlementResponse) -> String {
    r.extensions.as_ref().and_then(|e| e["kaspa"]["diagnostic"].as_str()).unwrap_or("").to_string()
}

fn vdiag_of(r: &VerifyResponse) -> String {
    r.extensions.as_ref().and_then(|e| e["kaspa"]["diagnostic"].as_str()).unwrap_or("").to_string()
}

// ------------------------------------------------------------------------------------------ requests

/// The merchant's request hash for `resource` and the selected offer (binding: `httpRequestHash`).
fn request_hash_for(resource: &str, offer: &PaymentRequirements) -> String {
    let rh = requirements_hash_hex(offer).expect("requirements hash");
    hex(&http_request_hash("GET", resource, None, &rh).expect("request hash"))
}

fn facilitator_request(payload: PaymentPayload, offer: &PaymentRequirements, request_hash: &str) -> FacilitatorRequest {
    FacilitatorRequest {
        x402_version: 2,
        payment_payload: payload,
        payment_requirements: offer.clone(),
        request_hash: Some(request_hash.to_string()),
        resource: None,
    }
}

fn payment_output_index(r: &SettlementResponse) -> u32 {
    r.extensions.as_ref().and_then(|e| e["kaspa"]["paymentOutputIndex"].as_u64()).expect("paymentOutputIndex") as u32
}

/// What must not move when a payment is rejected before broadcast.
#[derive(Debug, PartialEq, Eq)]
struct Snapshot {
    payer_utxos: BTreeSet<Outpoint>,
    merchant_balance: u64,
    mempool_entries: usize,
    ledger_len: usize,
    broadcasts: u64,
}

fn snapshot(net: &Net, svc: &Svc, payer: &Key, merchant: &Key) -> Snapshot {
    Snapshot {
        payer_utxos: net.utxos_of(&payer.spk()).into_iter().map(|(o, _)| o).collect(),
        merchant_balance: net.balance(merchant),
        mempool_entries: net.mempool_entries(&[payer.addr(), merchant.addr()]),
        ledger_len: svc.fac.ledger.len(),
        broadcasts: svc.broadcasts(),
    }
}

fn ok_settle(what: &str, r: &SettlementResponse) -> Txid {
    assert!(r.success, "{what}: settlement failed: {r:?}");
    let txid = parse_hash32(&r.transaction).expect("settled txid");
    log!("{what}: settled, txid {}", r.transaction);
    txid
}

fn assert_accepted(net: &Net, svc: &Svc, txid: &Txid, merchant_out: &Outpoint, merchant_spk: &ScriptPublicKey) {
    match retry("output status", || net.chain.output_status(merchant_out, merchant_spk)) {
        OutputStatus::Accepted { .. } => {}
        other => panic!("merchant output {merchant_out} is not in the UTXO set: {other:?}"),
    }
    assert!(!net.in_mempool(txid), "an accepted transaction must have left the mempool");
    let e = svc.fac.ledger.get(&hex(txid)).expect("ledger entry");
    assert_eq!(e.state, State::Accepted, "ledger state");
    // the indexer's acceptance tracking (the facilitator of `kob-executor run`) sees it too; the ledger may have settled first
    // from the UTXO set, so give the indexer's virtual-chain feed time to catch up on a busy node
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match svc.view.tracked(txid) {
            Tracked::Accepted { block_daa_score } => {
                if let Some(d) = e.accepted_daa {
                    assert_eq!(d, block_daa_score, "accepted DAA of the ledger");
                }
                break;
            }
            other if Instant::now() >= deadline => panic!("the indexer did not see {} accepted: {other:?}", hex(txid)),
            _ => std::thread::sleep(Duration::from_millis(500)),
        }
    }
}

// ------------------------------------------------------------------------------------------ the test

#[test]
fn x402_on_testnet_10() {
    if std::env::var("KOB_SKIP_NETWORK_TESTS").as_deref() == Ok("1") {
        eprintln!("KOB_SKIP_NETWORK_TESTS=1: skipping the live TN10 x402 end-to-end test");
        return;
    }
    let _ = elapsed();
    let node = std::env::var("KOB_TN10_WRPC").or_else(|_| std::env::var("KOB_TN10_NODE")).unwrap_or_else(|_| DEFAULT_NODE.to_string());
    let net = Net::connect(&node);
    let indexer = start_indexer(&node);
    let mut rep = Report::new(target_dir().join("x402-tn10-report.json"));
    rep.set("node", json!(node));
    rep.set("network", json!("kaspa:testnet-10"));

    // ---- keys and funding
    rep.begin("keys-and-funding");
    let issuer = Key::random("issuer");
    let payer = Key::random("payer");
    let maker1 = Key::random("maker1");
    let maker2 = Key::random("maker2");
    let filler = Key::random("filler");
    let merchant = Key::random("merchant");
    let all: Vec<&Key> = vec![&issuer, &payer, &maker1, &maker2, &filler, &merchant];
    let mut keys_json = serde_json::Map::new();
    for k in &all {
        keys_json.insert(k.name.into(), k.json());
    }
    rep.set("keys", Value::Object(keys_json));
    rep.save();
    let all_secrets = secrets(&all);

    net.chain.check_network().unwrap_or_else(|e| panic!("the node at {node} is not a synced TN10 node with a UTXO index: {e:?}"));
    let d0 = net.vdaa();
    log!("node {node}: virtual DAA {d0}");

    // weights: issuer 2, payer 2, maker1 3, maker2 2, filler 1 -> 10 slots; 14 rounds = 140 blocks (about 400+ KAS)
    let weights: [(&Key, usize); 5] = [(&issuer, 2), (&payer, 2), (&maker1, 3), (&maker2, 2), (&filler, 1)];
    let mut wallets: Vec<String> = vec![];
    let mut w = weights.to_vec();
    while w.iter().any(|(_, n)| *n > 0) {
        for (k, n) in w.iter_mut() {
            if *n > 0 {
                wallets.push(k.addr());
                *n -= 1;
            }
        }
    }
    let blocks = wallets.len() * 14;
    run_miner(&node, &wallets, blocks);
    let mined: u64 = weights.iter().map(|(k, _)| net.balance(k)).sum();
    log!("mined {} KAS across the funded keys", mined as f64 / KAS as f64);
    // wait for coinbase maturity through NodeChain
    poll("all coinbase outputs to mature", Duration::from_secs(400), Duration::from_secs(5), || {
        let vdaa = net.vdaa();
        let mut worst = 0u64;
        for (k, _) in &weights {
            for (_, u) in net.utxos_of(&k.spk()) {
                if u.is_coinbase {
                    worst = worst.max((u.block_daa_score + COINBASE_MATURITY).saturating_sub(vdaa));
                }
            }
        }
        if worst == 0 {
            Some(())
        } else {
            log!("waiting for coinbase maturity: {worst} DAA (about {} s) to go", worst / 10);
            None
        }
    });
    let spendable: BTreeMap<&str, u64> =
        weights.iter().map(|(k, _)| (k.name, net.kas_utxos(k).iter().map(|u| u.utxo.amount).sum::<u64>())).collect();
    log!("spendable KAS (sompi): {spendable:?}");
    rep.set("minedSompi", json!(mined));
    rep.step("keys-and-funding", json!({ "blocks": blocks, "minedSompi": mined, "spendable": spendable, "startDaa": d0 }));

    let ledger_path = target_dir().join(format!("x402-tn10-ledger-{}.jsonl", now_ms()));
    let ledger = Arc::new(Ledger::open(&ledger_path).expect("open the replay ledger"));
    let merchant_spk = merchant.spk();
    let clock = SystemClock;

    // =============================================================================== (a) KAS exact
    rep.begin("a-kas-exact");
    let svc1 = start_service(&net, &indexer, ledger.clone(), &merchant, &[], &[]);
    {
        let (st, body) = http(svc1.addr, "GET", "/supported", None, b"");
        assert_eq!(st, 200);
        let sup: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(sup["kinds"][0]["network"], "kaspa:testnet-10");
        assert_eq!(sup["kinds"][0]["extra"]["binding"], "kaspa-exact-v2");
        let (st, _) = http(svc1.addr, "POST", "/settle", None, b"{}");
        assert_eq!(st, 401, "no API key, no ledger access");
    }
    let amount_a: u64 = 50_000_000;
    let offer_a = native_requirements(Network::Testnet10, amount_a, &merchant.addr(), 120, Finality::Accepted).expect("offer");
    let coins = net.kas_utxos(&payer);
    let res_a = "https://merchant.example/tn10/kas";
    let rh_a = request_hash_for(res_a, &offer_a);
    let mk_native = |offer: &PaymentRequirements, rh: &str, at_ms: u64, coins: &[KeyUtxo], id: Option<&str>| {
        let mut opts = PayOptions::new(u64::MAX);
        opts.payment_id = id.map(str::to_string);
        pay_native(offer, rh, &payer.sk, coins, at_ms, &opts).expect("pay_native")
    };
    let pay_a = mk_native(&offer_a, &rh_a, now_ms(), &coins, None);
    let req_a = facilitator_request(pay_a.clone(), &offer_a, &rh_a);
    // a competing payment that reuses the same funding input (another amount / resource)
    let offer_a2 = native_requirements(Network::Testnet10, amount_a + 1_000, &merchant.addr(), 120, Finality::Accepted).unwrap();
    let rh_a2 = request_hash_for("https://merchant.example/tn10/kas-other", &offer_a2);
    let req_a2 = facilitator_request(mk_native(&offer_a2, &rh_a2, now_ms(), &coins, None), &offer_a2, &rh_a2);
    // preflight on the payer's side
    let vctx = VerifyCtx { chain: &*net.chain, clock: &clock, policy: &svc1.fac.policy };
    kob_x402::client::native::preflight_native(&vctx, &offer_a, &pay_a, &rh_a).expect("payer preflight");

    let v = svc1.verify(&req_a);
    assert!(v.is_valid, "/verify of a good payment: {v:?}");

    // --- attacks: everything must be refused BEFORE anything is created or broadcast
    let before = snapshot(&net, &svc1, &payer, &merchant);
    let mut tampered = vec![];
    {
        let mut r = req_a.clone();
        r.payment_payload.accepted.amount = "1".into();
        tampered.push(("tampered amount in accepted", r));
        let mut r = req_a.clone();
        r.payment_requirements.amount = (amount_a + 1).to_string();
        tampered.push(("tampered amount in the server's requirements", r));
        let mut r = req_a.clone();
        r.request_hash = Some(hex(&[0x77; 32]));
        tampered.push(("tampered requestHash", r));
        let mut r = req_a.clone();
        r.payment_payload.payload.request_hash = hex(&[0x66; 32]);
        tampered.push(("tampered payload.requestHash", r));
        let expired = mk_native(&offer_a, &rh_a, now_ms() - 400_000, &coins, None);
        tampered.push(("expired authorization", facilitator_request(expired, &offer_a, &rh_a)));
    }
    let mut attack_log = vec![];
    for (name, r) in &tampered {
        let s = svc1.settle(r);
        assert!(!s.success, "{name}: must be refused");
        log!("attack refused: {name}: {} / {}", s.error_reason.clone().unwrap_or_default(), diag_of(&s));
        assert_eq!(
            snapshot(&net, &svc1, &payer, &merchant),
            before,
            "{name}: nothing may change (mempool, UTXOs, ledger, broadcasts)"
        );
        let v = svc1.verify(r);
        assert!(!v.is_valid, "{name}: /verify must refuse too");
        attack_log.push(json!({ "attack": name, "reason": s.error_reason, "diagnostic": diag_of(&s) }));
    }
    assert!(svc1.fac.ledger.is_empty(), "no attack may create ledger state");

    // --- the real settlement
    let bal0 = net.balance(&merchant);
    let t0 = Instant::now();
    let s = svc1.settle(&req_a);
    let txid_a = ok_settle("KAS payment", &s);
    let settle_ms = t0.elapsed().as_millis() as u64;
    assert_eq!(s.amount.as_deref(), Some(amount_a.to_string().as_str()));
    let idx = payment_output_index(&s);
    let mspk = merchant_spk.clone();
    assert_accepted(&net, &svc1, &txid_a, &op_of(&txid_a, idx), &mspk);
    assert_eq!(net.balance(&merchant), bal0 + amount_a, "the merchant balance rose by exactly the amount");
    let pay_out = net.wait_utxo("merchant KAS output", &op_of(&txid_a, idx), &mspk);
    assert_eq!(pay_out.amount, amount_a);
    let fee_a = svc1.fac.ledger.get(&hex(&txid_a)).map(|_| ()).is_some();
    assert!(fee_a);
    // --- replay of the identical /settle: the cached response, no second broadcast
    let b0 = svc1.broadcasts();
    let s_again = svc1.settle(&req_a);
    assert_eq!(s_again, s, "an identical retry returns the cached settlement");
    assert_eq!(svc1.broadcasts(), b0, "no second broadcast");
    // --- a different request spending the same input is refused
    let s2 = svc1.settle(&req_a2);
    assert!(!s2.success, "a conflicting spend of the same input must be refused");
    log!("competing payment refused: {} / {}", s2.error_reason.clone().unwrap_or_default(), diag_of(&s2));
    assert_eq!(svc1.broadcasts(), b0, "the refused competitor was not broadcast");
    let v_after = svc1.verify(&req_a);
    assert!(!v_after.is_valid, "a settled payment does not verify again (its input is spent)");
    rep.step(
        "a-kas-exact",
        json!({
            "txid": hex(&txid_a), "amountSompi": amount_a, "settleMs": settle_ms, "attacks": attack_log,
            "competitorReason": s2.error_reason, "competitorDiagnostic": diag_of(&s2),
            "replayServedFromLedger": true, "afterSettleVerify": vdiag_of(&v_after),
        }),
    );

    // =============================================================================== (b) issue two KCC-20 tokens
    rep.begin("b-issue-tokens");
    let token_a = issue_token(&net, &mut rep, &issuer, "KOB x402 test A", "KXA", &[(payer.pk, 100_000), (filler.pk, 10_000)]);
    let token_b = issue_token(
        &net,
        &mut rep,
        &issuer,
        "KOB x402 test B",
        "KXB",
        &[(maker2.pk, 5 * SCALE as u64), (maker2.pk, 5 * SCALE as u64)],
    );
    let asset_a = hex(&token_a.cov);
    let asset_b = hex(&token_b.cov);
    drop(svc1);
    let svc = start_service(
        &net,
        &indexer,
        ledger.clone(),
        &merchant,
        &[asset_a.clone(), asset_b.clone()],
        &[token_a.cfg_json(), token_b.cfg_json()],
    );
    for t in [&token_a, &token_b] {
        let allowed: &AllowedToken = svc.fac.policy.tokens.find(&t.cov).expect("registered");
        assert_eq!(
            (allowed.program, allowed.extension_commitment, allowed.custody),
            (PROGRAM, t.ext, Custody::Unconditional),
            "{} registered as issued",
            t.ticker
        );
    }
    let allowed_a: AllowedToken = svc.fac.policy.tokens.find(&token_a.cov).unwrap().clone();
    let allowed_b: AllowedToken = svc.fac.policy.tokens.find(&token_b.cov).unwrap().clone();
    {
        let (_, body) = http(svc.addr, "GET", "/supported", None, b"");
        let sup: Value = serde_json::from_slice(&body).unwrap();
        assert!(sup["kinds"][0]["extra"]["profiles"].as_array().unwrap().contains(&json!("kcc20")));
        assert_eq!(sup["kinds"][0]["extra"]["tokens"].as_array().unwrap().len(), 2);
    }
    rep.set(
        "tokens",
        json!({
            "A": { "covenantId": asset_a, "issueTxid": hex(&token_a.txid), "ticker": "KXA", "program": "kcc20-ref-8x8" },
            "B": { "covenantId": asset_b, "issueTxid": hex(&token_b.txid), "ticker": "KXB", "program": "kcc20-ref-8x8" },
        }),
    );
    rep.step(
        "b-issue-tokens",
        json!({ "A": { "txid": hex(&token_a.txid), "covenant": asset_a }, "B": { "txid": hex(&token_b.txid), "covenant": asset_b } }),
    );

    // the payer's spendable token A UTXO (tracked locally: a P2SH token script depends on its amount)
    let mut payer_a: Vec<TokenUtxo> = vec![token_a.utxos[0].clone()];
    let payer_secrets = secrets(&[&payer]);

    // =============================================================================== (c) KCC-20 exact
    rep.begin("c-kcc20-exact");
    let amount_c: u64 = 500;
    let offer_c = kcc20_requirements(Network::Testnet10, &allowed_a, amount_c, &merchant.addr(), PAY_CARRIER, 120, Finality::Accepted)
        .expect("kcc20 offer");
    let rh_c = request_hash_for("https://merchant.example/tn10/kcc20", &offer_c);
    let opts_c = Kcc20Options { payment_id: Some("kcc20-tn10-payment-0001".into()), ..Default::default() };
    let payload_c =
        pay_kcc20_with(&offer_c, &rh_c, &payer_secrets, payer_a.clone(), net.kas_utxos(&payer), now_ms(), &opts_c).expect("pay_kcc20");
    let req_c = facilitator_request(payload_c, &offer_c, &rh_c);
    let v = svc.verify(&req_c);
    assert!(v.is_valid, "/verify kcc20: {v:?}");
    let s = svc.settle(&req_c);
    let txid_c = ok_settle("KCC-20 payment", &s);
    let idx_c = payment_output_index(&s);
    let token_spk_c = spk_from_hex(offer_c.extra["token"]["tokenScriptPublicKey"].as_str().unwrap()).expect("token spk");
    let merchant_token = net.wait_utxo("merchant token output", &op_of(&txid_c, idx_c), &token_spk_c);
    assert_eq!(merchant_token.covenant_id, Some(token_a.cov));
    assert_eq!(merchant_token.amount, PAY_CARRIER, "the merchant token output carries exactly the offered carrier");
    assert_accepted(&net, &svc, &txid_c, &op_of(&txid_c, idx_c), &token_spk_c);
    // the payer's change: token A minus the payment, same owner
    let change_state = Kcc20State::p2pk(100_000 - amount_c as i64, payer.pk, token_a.ext);
    let (tx_c, _) = kob_x402::safe_tx::SafeTx::parse(&req_c.payment_payload.payload.transaction, 1 << 20)
        .unwrap()
        .to_consensus()
        .map(|p| (p.tx, ()))
        .unwrap();
    let change_idx =
        tx_c.outputs.iter().position(|o| o.script_public_key == token_spk(&change_state)).expect("payer token change output") as u32;
    let change_utxo = token_utxo_on_chain(&net, &txid_c, change_idx, token_a.cov, change_state);
    payer_a = vec![change_utxo];
    rep.step("c-kcc20-exact", json!({ "txid": hex(&txid_c), "amountUnits": amount_c, "merchantTokenOutput": format!("{}:{idx_c}", hex(&txid_c)), "payerChangeUnits": payer_a[0].state.amount() }));

    // =============================================================================== (d) swap-and-pay
    rep.begin("d-orders");
    let bid_sw1 = create_bid(&net, &mut rep, &maker1, &token_a, 3, 1);
    let ask_sw2 = create_ask(&net, &mut rep, &maker2, &token_b, &token_b.utxos[0], 5);
    let bid_sw3 = create_bid(&net, &mut rep, &maker1, &token_a, 3, 1);
    let ask_sw3 = create_ask(&net, &mut rep, &maker2, &token_b, &token_b.utxos[1], 5);
    rep.step(
        "d-orders",
        json!({
            "bidSw1": hex(&bid_sw1.txid), "askSw2": hex(&ask_sw2.txid), "bidSw3": hex(&bid_sw3.txid), "askSw3": hex(&ask_sw3.txid),
        }),
    );

    let swap_opts = SwapOptions::default();
    let lock_time = |n: &Net| n.vdaa().saturating_sub(50);
    let sign_swap = |offer: &PaymentRequirements, rh: &str, quote: &Quote, funds: &PayerFunds, max_pay: u64| -> SwapPayment {
        let opts = SwapOptions { max_pay: Some(max_pay), ..swap_opts.clone() };
        pay_swap(&svc.fac.policy, offer, quote, rh, &payer_secrets, funds, now_ms(), &opts).expect("pay_swap")
    };
    let settle_swap =
        |what: &str, offer: &PaymentRequirements, rh: &str, p: &SwapPayment, max_pay: u64| -> (SettlementResponse, Txid) {
            let vctx = VerifyCtx { chain: &*net.chain, clock: &clock, policy: &svc.fac.policy };
            let (_, facts) =
                preflight_swap(&vctx, offer, &p.payload, rh, Some(max_pay)).unwrap_or_else(|e| panic!("{what}: payer preflight: {e}"));
            log!("{what}: payer spends {} units of the pay asset, fee {} sompi", facts.payer_spent, p.fee);
            let req = facilitator_request(p.payload.clone(), offer, rh);
            let v = svc.verify(&req);
            assert!(v.is_valid, "{what}: /verify: {v:?}");
            let s = svc.settle(&req);
            let txid = ok_settle(what, &s);
            assert_eq!(txid, p.txid);
            (s, txid)
        };

    // ---- SW1: payer pays token A, the merchant receives KAS through the bid
    rep.begin("d-sw1");
    let amount_sw1: u64 = KAS;
    let offer_sw1 = swap_requirements(&SwapOfferParams {
        network: Network::Testnet10,
        amount: amount_sw1,
        pay_to: &merchant.addr(),
        max_timeout_seconds: 120,
        finality: Finality::Accepted,
        gain: MerchantGain::Kas,
        pay_assets: vec![PayAssetSpec::Token(&allowed_a)],
    })
    .expect("sw1 offer");
    let rh_sw1 = request_hash_for("https://merchant.example/tn10/sw1", &offer_sw1);
    let funds = |tokens: Vec<TokenUtxo>, kas: usize| PayerFunds {
        tokens,
        funding: net.kas_utxos(&payer).into_iter().take(kas).collect(),
        change: payer.pk,
    };
    let all_orders = [&bid_sw1.order.utxo, &ask_sw2.order.utxo, &bid_sw3.order.utxo, &ask_sw3.order.utxo].map(|u| u.outpoint_of());
    let sell_a = |n: i64, exclude: Vec<Outpoint>| QuoteRequest {
        token: token_a.cov,
        take: Take::SellToBids,
        amount: n * WHOLE,
        limit_price: None,
        exclude,
    };
    let buy_b =
        |n: i64| QuoteRequest { token: token_b.cov, take: Take::BuyFromAsks, amount: n * WHOLE, limit_price: None, exclude: vec![] };
    let quote_sw1 = indexer.quote("SW1", &[sell_a(3, vec![])], &all_orders, &[], lock_time(&net));
    assert_eq!(quote_sw1.orders.len(), 1, "one bid of 3 whole tokens fills SW1");
    let sw1_bid = quote_sw1.orders[0].clone();
    let p_sw1 = sign_swap(&offer_sw1, &rh_sw1, &quote_sw1, &funds(payer_a.clone(), 0), 3 * SCALE as u64);
    let bal_sw1 = net.balance(&merchant);
    let (s_sw1, txid_sw1) = settle_swap("SW1", &offer_sw1, &rh_sw1, &p_sw1, 3 * SCALE as u64);
    let idx = payment_output_index(&s_sw1);
    assert_accepted(&net, &svc, &txid_sw1, &op_of(&txid_sw1, idx), &merchant_spk);
    assert_eq!(net.balance(&merchant), bal_sw1 + amount_sw1, "SW1: the merchant received exactly the KAS amount");
    net.wait_gone("SW1 bid", &sw1_bid.outpoint(), &quoted_spk(&sw1_bid));
    // the payer's token change (same leader output layout: find the P2PK token change by its recomputed script)
    let change_state = Kcc20State::p2pk(payer_a[0].state.amount() - 3 * SCALE, payer.pk, token_a.ext);
    payer_a = vec![find_token_change(&net, &p_sw1.tx, &txid_sw1, token_a.cov, change_state)];
    rep.step("d-sw1", json!({ "txid": hex(&txid_sw1), "merchantKasSompi": amount_sw1, "payerSpentUnits": p_sw1.payer_spent, "fee": p_sw1.fee, "orders": [sw1_bid.outpoint().to_string()], "quotedFrom": "indexer book" }));

    // ---- SW2: payer pays KAS, the merchant receives token B through the ask
    rep.begin("d-sw2");
    let amount_sw2: u64 = 2 * SCALE as u64;
    let offer_sw2 = swap_requirements(&SwapOfferParams {
        network: Network::Testnet10,
        amount: amount_sw2,
        pay_to: &merchant.addr(),
        max_timeout_seconds: 120,
        finality: Finality::Accepted,
        gain: MerchantGain::Token { token: &allowed_b, carrier: PAY_CARRIER },
        pay_assets: vec![PayAssetSpec::Kas],
    })
    .expect("sw2 offer");
    let rh_sw2 = request_hash_for("https://merchant.example/tn10/sw2", &offer_sw2);
    let quote_sw2 = indexer.quote("SW2", &[buy_b(2)], &[], &[sw1_bid.outpoint()], lock_time(&net));
    assert_eq!(quote_sw2.orders.len(), 1, "one ask of 5 whole tokens fills SW2");
    let sw2_ask = quote_sw2.orders[0].clone();
    let max_kas = 8 * KAS;
    let p_sw2 = sign_swap(&offer_sw2, &rh_sw2, &quote_sw2, &funds(vec![], 3), max_kas);
    let (s_sw2, txid_sw2) = settle_swap("SW2", &offer_sw2, &rh_sw2, &p_sw2, max_kas);
    let idx = payment_output_index(&s_sw2);
    let tspk_b = spk_from_hex(offer_sw2.extra["token"]["tokenScriptPublicKey"].as_str().unwrap()).unwrap();
    let u = net.wait_utxo("SW2 merchant token B", &op_of(&txid_sw2, idx), &tspk_b);
    assert_eq!((u.covenant_id, u.amount), (Some(token_b.cov), PAY_CARRIER));
    assert_accepted(&net, &svc, &txid_sw2, &op_of(&txid_sw2, idx), &tspk_b);
    net.wait_gone("SW2 ask", &sw2_ask.outpoint(), &quoted_spk(&sw2_ask));
    rep.step(
        "d-sw2",
        json!({ "txid": hex(&txid_sw2), "merchantTokenB": amount_sw2, "payerSpentSompi": p_sw2.payer_spent, "fee": p_sw2.fee }),
    );

    // ---- SW3: payer pays token A, the merchant receives token B (A -> KAS -> B)
    rep.begin("d-sw3");
    let offer_sw3 = swap_requirements(&SwapOfferParams {
        network: Network::Testnet10,
        amount: amount_sw2,
        pay_to: &merchant.addr(),
        max_timeout_seconds: 120,
        finality: Finality::Accepted,
        gain: MerchantGain::Token { token: &allowed_b, carrier: PAY_CARRIER },
        pay_assets: vec![PayAssetSpec::Token(&allowed_a)],
    })
    .expect("sw3 offer");
    let rh_sw3 = request_hash_for("https://merchant.example/tn10/sw3", &offer_sw3);
    let quote_sw3 =
        indexer.quote("SW3", &[sell_a(3, vec![]), buy_b(2)], &[], &[sw1_bid.outpoint(), sw2_ask.outpoint()], lock_time(&net));
    let p_sw3 = sign_swap(&offer_sw3, &rh_sw3, &quote_sw3, &funds(payer_a.clone(), 2), 3 * SCALE as u64);
    let (s_sw3, txid_sw3) = settle_swap("SW3", &offer_sw3, &rh_sw3, &p_sw3, 3 * SCALE as u64);
    let idx = payment_output_index(&s_sw3);
    let u = net.wait_utxo("SW3 merchant token B", &op_of(&txid_sw3, idx), &tspk_b);
    assert_eq!((u.covenant_id, u.amount), (Some(token_b.cov), PAY_CARRIER));
    assert_accepted(&net, &svc, &txid_sw3, &op_of(&txid_sw3, idx), &tspk_b);
    let change_state = Kcc20State::p2pk(payer_a[0].state.amount() - 3 * SCALE, payer.pk, token_a.ext);
    payer_a = vec![find_token_change(&net, &p_sw3.tx, &txid_sw3, token_a.cov, change_state)];
    rep.step(
        "d-sw3",
        json!({ "txid": hex(&txid_sw3), "merchantTokenB": amount_sw2, "payerSpentUnits": p_sw3.payer_spent, "fee": p_sw3.fee }),
    );

    // =============================================================================== (e) order conflict, live
    rep.begin("e-order-conflict");
    let bid_e1 = create_bid(&net, &mut rep, &maker1, &token_a, 5, 2);
    let bid_e2 = create_bid(&net, &mut rep, &maker1, &token_a, 3, 1);
    let offer_e = swap_requirements(&SwapOfferParams {
        network: Network::Testnet10,
        amount: amount_sw1,
        pay_to: &merchant.addr(),
        max_timeout_seconds: 120,
        finality: Finality::Accepted,
        gain: MerchantGain::Kas,
        pay_assets: vec![PayAssetSpec::Token(&allowed_a)],
    })
    .expect("conflict offer");
    let rh_e = request_hash_for("https://merchant.example/tn10/conflict", &offer_e);
    let quote_e1 = Quote { lock_time: lock_time(&net), orders: vec![OrderRef::bid(bid_e1.order.clone(), 3 * WHOLE)] };
    let p_e1 = sign_swap(&offer_e, &rh_e, &quote_e1, &funds(payer_a.clone(), 0), 3 * SCALE as u64);
    let req_e1 = facilitator_request(p_e1.payload.clone(), &offer_e, &rh_e);
    assert!(svc.verify(&req_e1).is_valid, "the payment is valid until the order moves");
    // a third party fills bid 1 first (a plain taker transaction)
    let filler_tok = token_a.utxos[1].clone();
    let taker_batch = Batch {
        lock_time: lock_time(&net),
        legs: vec![Leg::Bid { order: bid_e1.order.clone(), amount: 2 * WHOLE, t: None }],
        updates: vec![],
        taker_tokens: vec![filler_tok],
        taker: Some(filler.pk),
        taker_token_carrier: PAY_CARRIER,
        receivers: vec![],
        payments: vec![],
        funding: pick(&net.kas_utxos(&filler), KAS, 4),
        change: Some(filler.pk),
        records: vec![],
        fee: FeeOptions::default(),
    };
    let built = build_batch(&taker_batch, &kob_protocol::budget::lookup).expect("taker batch");
    let (tx_fill, _, fee_fill) = sign_built(&built, &secrets(&[&filler]));
    let txid_fill = net.submit("third-party fill of bid 1", &tx_fill);
    rep.fees += fee_fill;
    net.wait_gone("filled bid 1", &bid_e1.order.utxo.outpoint_of(), &bid_e1.order.state.spk());
    log!("bid 1 was filled first: {}", bid_e1.order.utxo.outpoint_of());
    // the payer's payment signed against the old bid: retryable order_conflict
    let ledger_len = svc.fac.ledger.len();
    let bcasts = svc.broadcasts();
    let s_e1 = svc.settle(&req_e1);
    assert!(!s_e1.success);
    assert_eq!(s_e1.error_reason.as_deref(), Some("invalid_transaction_state"), "{s_e1:?}");
    assert_eq!(diag_of(&s_e1), "order_conflict", "{s_e1:?}");
    let ext = &s_e1.extensions.as_ref().unwrap()["kaspa"];
    assert_eq!(ext["retryable"], json!(true));
    let named: Vec<String> = ext["details"]["orders"]
        .as_array()
        .expect("details.orders")
        .iter()
        .map(|o| format!("{}:{}", o["txid"].as_str().unwrap(), o["index"]))
        .collect();
    assert_eq!(named, vec![bid_e1.order.utxo.outpoint_of().to_string()], "details.orders names the spent outpoint");
    assert_eq!(svc.fac.ledger.len(), ledger_len, "the failed attempt left no ledger entry");
    assert_eq!(svc.broadcasts(), bcasts, "the failed attempt was not broadcast");
    for i in &p_e1.tx.inputs {
        assert!(
            !svc.fac.ledger.is_consumed(&Outpoint::new(i.previous_outpoint.transaction_id.as_bytes(), i.previous_outpoint.index)),
            "the payer's inputs must stay reusable"
        );
    }
    // the SDK classifies it as a retryable conflict naming the same order
    let e = X402Error::state(kob_x402::error::Diag::OrderConflict, "x").retryable().with_details(ext["details"].clone());
    assert!(is_retryable_conflict(&e));
    assert_eq!(conflicting_orders(&e), vec![bid_e1.order.utxo.outpoint_of()]);
    // re-quote from the indexer's book without the lost order, RE-SIGN, settle
    let lost = conflicting_orders(&e);
    let quote_e2 = indexer.quote(
        "order-conflict re-quote",
        &[sell_a(3, lost.clone())],
        &[bid_e2.order.utxo.outpoint_of()],
        &lost,
        lock_time(&net),
    );
    assert!(quote_e2.orders.iter().all(|o| !lost.contains(&o.outpoint())));
    let p_e2 = sign_swap(&offer_e, &rh_e, &quote_e2, &funds(payer_a.clone(), 0), 3 * SCALE as u64);
    assert_ne!(p_e2.txid, p_e1.txid, "a re-signed payment is a new transaction");
    assert_eq!(
        p_e2.tx
            .inputs
            .iter()
            .map(|i| i.previous_outpoint)
            .filter(|o| p_e1.tx.inputs.iter().any(|x| x.previous_outpoint == *o))
            .count(),
        1,
        "the retry re-uses the payer's token input of the failed attempt"
    );
    let bal_e = net.balance(&merchant);
    let (s_e2, txid_e2) = settle_swap("order-conflict retry", &offer_e, &rh_e, &p_e2, 3 * SCALE as u64);
    let idx = payment_output_index(&s_e2);
    assert_accepted(&net, &svc, &txid_e2, &op_of(&txid_e2, idx), &merchant_spk);
    assert_eq!(net.balance(&merchant), bal_e + amount_sw1);
    let change_state = Kcc20State::p2pk(payer_a[0].state.amount() - 3 * SCALE, payer.pk, token_a.ext);
    payer_a = vec![find_token_change(&net, &p_e2.tx, &txid_e2, token_a.cov, change_state)];
    rep.step(
        "e-order-conflict",
        json!({
            "bid1": bid_e1.order.utxo.outpoint_of().to_string(), "bid2": bid_e2.order.utxo.outpoint_of().to_string(),
            "thirdPartyFillTxid": hex(&txid_fill), "failedAttemptTxid": hex(&p_e1.txid), "conflictReason": s_e1.error_reason,
            "conflictDiagnostic": diag_of(&s_e1), "conflictDetails": ext["details"], "retryTxid": hex(&txid_e2),
        }),
    );

    // =============================================================================== (f) confirmed finality
    rep.begin("f-confirmed");
    let amount_f: u64 = 20_000_000;
    let offer_f = native_requirements(Network::Testnet10, amount_f, &merchant.addr(), 120, Finality::Confirmed).unwrap();
    let rh_f = request_hash_for("https://merchant.example/tn10/confirmed", &offer_f);
    let coins = net.kas_utxos(&payer);
    let req_f = facilitator_request(mk_native(&offer_f, &rh_f, now_ms(), &coins, None), &offer_f, &rh_f);
    let bal_f = net.balance(&merchant);
    let t0 = Instant::now();
    let s_f = svc.settle(&req_f);
    let txid_f = ok_settle("confirmed KAS payment", &s_f);
    let confirm_ms = t0.elapsed().as_millis() as u64;
    assert_eq!(s_f.extensions.as_ref().unwrap()["kaspa"]["finality"], "confirmed");
    let entry = svc.fac.ledger.get(&hex(&txid_f)).expect("ledger entry");
    assert_eq!(entry.state, State::Accepted);
    let idx = payment_output_index(&s_f);
    let out = net.wait_utxo("confirmed output", &op_of(&txid_f, idx), &merchant_spk);
    let depth = net.vdaa().saturating_sub(out.block_daa_score);
    assert!(
        depth >= svc.fac.policy.confirmations_daa,
        "confirmed means at least {} DAA deep (was {depth})",
        svc.fac.policy.confirmations_daa
    );
    assert_eq!(net.balance(&merchant), bal_f + amount_f);
    rep.step("f-confirmed", json!({ "txid": hex(&txid_f), "settleMs": confirm_ms, "confirmationsDaa": svc.fac.policy.confirmations_daa, "depthAtCheck": depth }));

    // =============================================================================== (g) fee-floor probe
    rep.begin("g-fee-floor-probe");
    let probe = fee_floor_probe(&net, &mut rep, &payer, &merchant);
    rep.step("g-fee-floor-probe", probe.clone());
    rep.set("feeFloorProbe", probe);

    // =============================================================================== (h) intent + invoice
    rep.begin("h-intent-invoice");
    let keeper = Key::random("keeper");
    drop(svc);
    let svc_h = start_service_with(
        &net,
        &indexer,
        ledger.clone(),
        &merchant,
        &[asset_a.clone(), asset_b.clone()],
        &[token_a.cfg_json(), token_b.cfg_json()],
        Some(keeper.pk),
    );
    {
        let (_, body) = http(svc_h.addr, "GET", "/supported", None, b"");
        let sup: Value = serde_json::from_slice(&body).unwrap();
        assert!(sup["kinds"][0]["extra"]["routeBindings"].as_array().unwrap().contains(&json!("kob-intent-v1")));
        assert_eq!(sup["kinds"][0]["extra"]["invoices"], "kob-invoice-v1");
    }
    // a resting bid of token A the keeper can sell the intent's tokens into
    let bid_h = create_bid(&net, &mut rep, &maker1, &token_a, 4, 2);
    let bid_h_op = bid_h.order.utxo.outpoint_of();
    poll("the indexer lists the intent's bid", Duration::from_secs(180), Duration::from_millis(500), || {
        let b = book_snapshot(indexer.ingest.lock().unwrap().conn(), None).ok()?;
        b.orders.iter().any(|o| Outpoint::new(o.order.utxo.transaction_id, o.order.utxo.index) == bid_h_op).then_some(())
    });
    // the merchant registers an invoice: KAS, paid with token A through a router intent
    let amount_h: u64 = 2 * KAS;
    let offer_h = intent_requirements(&SwapOfferParams {
        network: Network::Testnet10,
        amount: amount_h,
        pay_to: &merchant.addr(),
        max_timeout_seconds: 600,
        finality: Finality::Accepted,
        gain: MerchantGain::Kas,
        pay_assets: vec![PayAssetSpec::Token(&allowed_a)],
    })
    .expect("intent offer");
    let inv = kob_x402::invoice::Invoice::new(
        Network::Testnet10,
        "tn10-order-1",
        now_ms() + 900_000,
        Some("live test".into()),
        vec![offer_h],
    );
    let (st, body) = http(svc_h.addr, "POST", "/invoices", Some(API_KEY), &serde_json::to_vec(&inv).unwrap());
    assert_eq!(st, 200, "POST /invoices: {}", String::from_utf8_lossy(&body));
    let id = serde_json::from_slice::<Value>(&body).unwrap()["id"].as_str().unwrap().to_string();
    // the payer fetches it (what a QR code points at) and checks the id
    let (st, body) = http(svc_h.addr, "GET", &format!("/invoices/{id}"), None, b"");
    assert_eq!(st, 200);
    let fetched: kob_x402::invoice::Invoice = serde_json::from_slice(&body).unwrap();
    kob_x402::invoice::check_id(&fetched, &id).expect("the invoice hashes to its id");
    let offer_h = fetched.accepts[0].clone();
    // ONE signature: the intent creation, locking 2 whole tokens of A at most
    let opts_h = IntentOptions { max_sell: Some(2 * SCALE), ..IntentOptions::default() };
    let p_h = pay_intent(&svc_h.fac.policy, &offer_h, &asset_a, &id, &payer_secrets, &funds(payer_a.clone(), 1), now_ms(), &opts_h)
        .expect("pay_intent");
    let bal_h = net.balance(&merchant);
    let (st, body) = http(svc_h.addr, "POST", &format!("/invoices/{id}/pay"), None, &serde_json::to_vec(&p_h.payload).unwrap());
    assert_eq!(st, 200, "POST /invoices/{{id}}/pay: {}", String::from_utf8_lossy(&body));
    let s_h: SettlementResponse = serde_json::from_slice(&body).unwrap();
    let txid_h = ok_settle("intent via invoice", &s_h);
    assert_ne!(txid_h, p_h.txid, "the settlement reports the keeper's execution, not the creation");
    let kob = &s_h.extensions.as_ref().unwrap()["kob"]["intent"];
    assert_eq!(kob["creation"], hex(&p_h.txid));
    let idx = payment_output_index(&s_h);
    let out = net.wait_utxo("merchant output of the execution", &op_of(&txid_h, idx), &merchant_spk);
    assert_eq!(out.amount, amount_h);
    assert_eq!(net.balance(&merchant), bal_h + amount_h, "the merchant received exactly the invoice amount");
    let (_, body) = http(svc_h.addr, "GET", &format!("/invoices/{id}/status"), None, b"");
    let status: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(status["status"], "paid", "{status}");
    assert_eq!(status["payment"]["transaction"], hex(&txid_h));
    // a second payment of the paid invoice is refused before broadcast and kept as a duplicate
    let before = svc_h.broadcasts();
    // the creation's token change (the payer's) pays the duplicate
    let change_h = find_token_change(
        &net,
        &p_h.tx,
        &p_h.txid,
        token_a.cov,
        Kcc20State::p2pk(payer_a[0].state.amount() - 2 * SCALE, payer.pk, token_a.ext),
    );
    let p_dup = pay_intent(&svc_h.fac.policy, &offer_h, &asset_a, &id, &payer_secrets, &funds(vec![change_h], 1), now_ms(), &opts_h)
        .expect("the duplicate intent");
    let (_, body) = http(svc_h.addr, "POST", &format!("/invoices/{id}/pay"), None, &serde_json::to_vec(&p_dup.payload).unwrap());
    let r: SettlementResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(diag_of(&r), "invoice_paid", "{r:?}");
    let dup = true;
    assert_eq!(svc_h.broadcasts(), before, "no second broadcast");
    rep.step(
        "h-intent-invoice",
        json!({
            "invoice": id, "creation": hex(&p_h.txid), "execution": hex(&txid_h), "merchantKasSompi": amount_h,
            "bid": bid_h_op.to_string(), "duplicateRefused": dup,
        }),
    );

    // =============================================================================== summary
    let fin: u64 = all.iter().map(|k| net.balance(k)).sum();
    rep.set("finalKasSompiOfAllKeys", json!(fin));
    rep.set("kasNetSpentSompi", json!(mined.saturating_sub(fin)));
    rep.set("result", json!("passed"));
    rep.save();
    print_summary(&rep);
    let _ = all_secrets;
}

/// The change output of a token payment, verified on chain.
fn find_token_change(net: &Net, tx: &Transaction, txid: &Txid, cov: [u8; 32], state: Kcc20State) -> TokenUtxo {
    let spk = token_spk(&state);
    let idx = tx
        .outputs
        .iter()
        .position(|o| o.script_public_key == spk)
        .unwrap_or_else(|| panic!("no token change output of {} units", state.amount));
    token_utxo_on_chain(net, txid, idx as u32, cov, state)
}

trait OutpointOf {
    fn outpoint_of(&self) -> Outpoint;
}
impl OutpointOf for Utxo {
    fn outpoint_of(&self) -> Outpoint {
        Outpoint::new(self.transaction_id, self.index)
    }
}

// ------------------------------------------------------------------------------------------ fee probe

/// Interop finding for the binding, and the check of our fee rule against the node itself: a rusty-kaspa
/// v2.1.0 node's relay floor is 100 sompi per gram of max(compute mass, normalized transient mass = 2 x size);
/// storage mass is not part of it (`mining/src/mempool/check_transaction_standard.rs`). A standard-native
/// transaction shaped like the rc.1 vector (1 P2PK input, 0.2 KAS to the merchant plus change) is submitted at
/// the rc.1 fee (200000 sompi) and at the floor minus one sompi, both refused, and at the floor, accepted; then
/// the payer SDK's default fee for the same shape is exactly that floor and is accepted too.
fn fee_floor_probe(net: &Net, rep: &mut Report, payer: &Key, merchant: &Key) -> Value {
    let coins = net.kas_utxos(payer);
    let big = pick(&coins, 3 * KAS, 4);
    // two 0.5 KAS probe coins: one for the attempts (a refused submission spends nothing), one for our builder
    let (split, _, split_fee) =
        plain_kas_tx(&big, &[(50_000_000, payer.spk()), (50_000_000, payer.spk())], &payer.spk(), None, &payer.sk);
    let txid_split = net.submit("probe split", &split);
    rep.fees += split_fee;
    let c0 = net.wait_utxo("probe coin 0", &op_of(&txid_split, 0), &payer.spk());
    let c1 = net.wait_utxo("probe coin 1", &op_of(&txid_split, 1), &payer.spk());
    let coin = |i: u32, c: &ChainUtxo| key_utxo(&op_of(&txid_split, i), c, payer.pk);
    let (k0, k1) = (coin(0, &c0), coin(1, &c1));
    let fee_estimate = retry("getFeeEstimate", || net.rpc.call("getFeeEstimate", json!({})));
    log!("node getFeeEstimate: {fee_estimate}");

    let shape =
        |fee: u64| plain_kas_tx(std::slice::from_ref(&k0), &[(20_000_000, merchant.spk())], &payer.spk(), Some(fee), &payer.sk);
    // the masses of the shape do not depend on the fee (fixed-width values)
    let floor = {
        let (tx, entries, _) = shape(200_000);
        let m = masses(&tx, &entries);
        assert!(m.storage > m.fee_mass, "the storage mass would dominate a storage-inclusive fee: {m:?}");
        min_fee(&m, MIN_FEE_RATE)
    };
    assert_eq!(floor, 203_600, "the rc.1 shape's node floor (100 x compute 2036)");
    let mut attempts = vec![];
    for (label, fee, relayed) in
        [("rc1-vector-fee-200000", 200_000, false), ("node-floor-minus-1", floor - 1, false), ("node-floor", floor, true)]
    {
        let (tx, entries, fee) = shape(fee);
        let mass = masses(&tx, &entries);
        let answer = net.try_submit(&tx);
        let (accepted, message) = match &answer {
            Ok(id) => (true, format!("accepted: {}", hex(id))),
            Err(e) => (false, format!("{e:?}")),
        };
        log!(
            "probe {label}: fee {fee} sompi, compute {}, 2 x size {}, storage {} -> {message}",
            mass.compute,
            mass.transient_normalized,
            mass.storage
        );
        attempts.push(json!({
            "attempt": label, "feeSompi": fee, "storageMass": mass.storage, "computeMass": mass.compute, "feeMass": mass.fee_mass,
            "nodeFloorSompi": floor, "accepted": accepted, "nodeAnswer": message,
        }));
        assert_eq!(accepted, relayed, "{label}: the node's answer contradicts the relay floor {floor}: {message}");
        if accepted {
            rep.fees += fee;
            net.wait_utxo("probe transaction", &op_of(&tx.id().as_bytes(), 0), &merchant.spk());
        }
    }
    // the payer SDK's default: exactly the node's floor, accepted
    let offer = native_requirements(Network::Testnet10, 20_000_000, &merchant.addr(), 60, Finality::Accepted).unwrap();
    let rh = request_hash_for("https://merchant.example/tn10/fee-floor", &offer);
    let paid =
        pay_native(&offer, &rh, &payer.sk, std::slice::from_ref(&k1), now_ms(), &PayOptions::new(20_000_000)).expect("pay_native");
    let parsed = kob_x402::safe_tx::SafeTx::parse(&paid.payload.transaction, 1 << 20).unwrap().to_consensus().unwrap();
    let tx = parsed.tx;
    assert_eq!(tx.inputs.len(), 1);
    let entries = vec![UtxoEntry::new(k1.utxo.amount, p2pk_spk(&k1.pubkey), k1.utxo.block_daa_score, false, None)];
    let mass = masses(&tx, &entries);
    let fee = entries.iter().map(|e| e.amount).sum::<u64>() - tx.outputs.iter().map(|o| o.value).sum::<u64>();
    assert_eq!(fee, min_fee(&mass, MIN_FEE_RATE), "the SDK's default fee is exactly the node's relay floor");
    let txid_floor = net.submit("SDK default-fee probe", &tx);
    rep.fees += fee;
    net.wait_utxo("SDK probe output", &op_of(&txid_floor, 0), &merchant.spk());
    log!("the SDK's default fee ({fee} sompi = {MIN_FEE_RATE} sompi/gram x {}) is accepted on TN10", mass.fee_mass);
    json!({
        "nodeFeeEstimate": fee_estimate,
        "nodeFloorSompi": floor,
        "attempts": attempts,
        "sdkDefaultFee": { "feeSompi": fee, "feeMass": mass.fee_mass, "storageMass": mass.storage, "txid": hex(&txid_floor), "accepted": true },
    })
}

fn print_summary(rep: &Report) {
    println!();
    println!("{:<22} {:>9}  txids", "step", "seconds");
    println!("{}", "-".repeat(110));
    for s in &rep.steps {
        let d = &s["detail"];
        let mut ids: Vec<String> = vec![];
        collect_txids(d, &mut ids);
        println!(
            "{:<22} {:>9.1}  {}",
            s["step"].as_str().unwrap_or("?"),
            s["ms"].as_u64().unwrap_or(0) as f64 / 1000.0,
            ids.join(" ")
        );
    }
    println!("{}", "-".repeat(110));
    println!("total {:.1}s, fees {:.4} KAS, report: {}", elapsed().as_secs_f64(), rep.fees as f64 / KAS as f64, rep.path.display());
}

fn collect_txids(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::String(s) if s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()) => out.push(s[..12].to_string()),
        Value::Array(a) => a.iter().for_each(|x| collect_txids(x, out)),
        Value::Object(m) => m
            .iter()
            .filter(|(k, _)| k.to_ascii_lowercase().contains("txid") || k.as_str() == "txid")
            .for_each(|(_, x)| collect_txids(x, out)),
        _ => {}
    }
}
