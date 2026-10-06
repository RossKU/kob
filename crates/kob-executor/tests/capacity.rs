//! Offline processing capacity: synthetic accepted-chain data at 10 BPS with 300 transactions per block (3,000 tx/s), fed
//! through the indexer's processing pipeline without a network: the JSON of a `getVirtualChainFromBlockV2` response at
//! `High` (each non-KOB transaction about 1,350 bytes with the fields the node sends), parsed as the wRPC client parses it,
//! checked (`into_batch`) and committed (`Ingest::apply_batch`: file database in WAL mode, record log with fsync). The mix
//! is mostly non-KOB transfers (some with covenant outputs of other projects) plus real KOB transactions built with the
//! protocol builders: placements, fills and cancels of asks, and x402-style token payments of an allowlisted token whose
//! transaction ids the facilitator watches.
//!
//! `capacity_smoke` (CI) runs 20 blocks and checks the rows. The full benchmark is ignored:
//! `cargo test --release -p kob-executor --test capacity -- --ignored --nocapture full_capacity`
//!
//! Environment (full benchmark): `KOB_CAP_CHAIN_SECS` seconds of chain per run (default 60), `KOB_CAP_TX_PER_BLOCK`
//! (default 300), `KOB_CAP_MIX` expected KOB transactions per block, e.g. `creates=1,fills=1,cancels=0.5,x402=1,foreign=0.05`
//! (`foreign`: share of the other transactions with a covenant output of another project), `KOB_CAP_VERIFY_THREADS` threads
//! of the pure pass before the database pass (default: cores, at most 4; 1: inline).
//!
//! `the_parallel_verification_pass_changes_nothing` (CI) checks that the parallel pass and the inline one give the same rows;
//! `rows_match_a_reference_file` (ignored, `KOB_CAP_ROWS_REF=<file>`) compares a fixed chain's rows with a file written by
//! another build (a refactoring of the write path must not change a row).

mod common;

use common::*;
use kob_executor::hex::Hash32;
use kob_executor::indexer::ingest::{Cursor, Ingest, IngestConfig, Watcher};
use kob_executor::indexer::recordlog::RecordLog;
use kob_executor::rpc::types::RawVspcResponse;
use kob_executor::testkit::*;
use kob_protocol::build::*;
use kob_protocol::state::*;
use kob_protocol::tx::*;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------------------------
// process CPU time and peak memory (no extra dependencies)

#[cfg(windows)]
mod os {
    use std::time::Duration;
    #[repr(C)]
    #[derive(Default)]
    struct FileTime {
        lo: u32,
        hi: u32,
    }
    #[repr(C)]
    #[derive(Default)]
    struct MemCounters {
        cb: u32,
        page_faults: u32,
        peak_working_set: usize,
        working_set: usize,
        quota_peak_paged: usize,
        quota_paged: usize,
        quota_peak_nonpaged: usize,
        quota_nonpaged: usize,
        pagefile: usize,
        peak_pagefile: usize,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> isize;
        fn GetProcessTimes(h: isize, c: *mut FileTime, e: *mut FileTime, k: *mut FileTime, u: *mut FileTime) -> i32;
        fn K32GetProcessMemoryInfo(h: isize, m: *mut MemCounters, cb: u32) -> i32;
    }
    fn ft(t: &FileTime) -> Duration {
        Duration::from_nanos(((t.hi as u64) << 32 | t.lo as u64) * 100)
    }
    pub fn cpu() -> Duration {
        let (mut c, mut e, mut k, mut u) = Default::default();
        // SAFETY: plain out-parameters of the documented signature
        unsafe { GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u) };
        ft(&k) + ft(&u)
    }
    /// (current, peak) working set in bytes.
    pub fn mem() -> (u64, u64) {
        let mut m = MemCounters { cb: std::mem::size_of::<MemCounters>() as u32, ..Default::default() };
        // SAFETY: `m` is a correctly sized PROCESS_MEMORY_COUNTERS
        unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut m, m.cb) };
        (m.working_set as u64, m.peak_working_set as u64)
    }
}

#[cfg(target_os = "linux")]
mod os {
    use std::time::Duration;
    pub fn cpu() -> Duration {
        let s = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
        let rest = s.rsplit(')').next().unwrap_or("");
        let f: Vec<u64> = rest.split_whitespace().map(|x| x.parse().unwrap_or(0)).collect();
        // fields 14 and 15 (utime, stime) are at 11 and 12 after the command; clock ticks of 100 Hz
        Duration::from_millis((f.get(11).copied().unwrap_or(0) + f.get(12).copied().unwrap_or(0)) * 10)
    }
    pub fn mem() -> (u64, u64) {
        let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
        let kb = |k: &str| {
            s.lines().find(|l| l.starts_with(k)).and_then(|l| l.split_whitespace().nth(1)?.parse::<u64>().ok()).unwrap_or(0) * 1024
        };
        (kb("VmRSS:"), kb("VmHWM:"))
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
mod os {
    pub fn cpu() -> std::time::Duration {
        std::time::Duration::ZERO
    }
    pub fn mem() -> (u64, u64) {
        (0, 0)
    }
}

// ---------------------------------------------------------------------------------------------
// synthetic chain

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
    /// `rate` events on average: its integer part, plus one with the probability of its fraction.
    fn count(&mut self, rate: f64) -> usize {
        rate.floor() as usize + (self.unit() < rate.fract()) as usize
    }
}

#[derive(Clone, Copy, Debug)]
struct Mix {
    tx_per_block: usize,
    creates: f64,
    fills: f64,
    cancels: f64,
    x402: f64,
    foreign: f64,
}

impl Mix {
    fn from_env() -> Mix {
        let mut m = Mix { tx_per_block: 300, creates: 1.0, fills: 1.0, cancels: 0.5, x402: 1.0, foreign: 0.05 };
        if let Ok(v) = std::env::var("KOB_CAP_TX_PER_BLOCK") {
            m.tx_per_block = v.parse().expect("KOB_CAP_TX_PER_BLOCK");
        }
        if let Ok(v) = std::env::var("KOB_CAP_MIX") {
            for kv in v.split(',') {
                let (k, v) = kv.split_once('=').expect("key=value");
                let v: f64 = v.parse().expect("number");
                match k.trim() {
                    "creates" => m.creates = v,
                    "fills" => m.fills = v,
                    "cancels" => m.cancels = v,
                    "x402" => m.x402 = v,
                    "foreign" => m.foreign = v,
                    _ => panic!("unknown mix key {k}"),
                }
            }
        }
        m
    }
}

fn hex_of(tag: &str, n: u64, len: usize) -> String {
    let mut s = String::with_capacity(len * 2);
    let mut i = 0u64;
    while s.len() < len * 2 {
        s.push_str(&h(tag, n * 1_000 + i).to_hex());
        i += 1;
    }
    s.truncate(len * 2);
    s
}

/// A non-KOB transaction exactly as a v2.1.0 node reports one at `High` (field set and order of a TN10 flood transfer
/// captured on 2026-10-02): one input with its spent entry and address, one or two outputs with addresses, the masses and
/// hashes the indexer does not read. Four in five are a P2SH self-transfer with an 8-byte signature script and one
/// output (1,290 bytes, the captured median), one in five a P2PK payment with a 66-byte signature, two outputs and a
/// 32-byte payload (about 1,750 bytes): about 1,380 bytes on average, against 1,354 measured per accepted transaction
/// during the flood. With `foreign`, the first output is bound to a covenant of another project.
fn noise_json(n: u64, foreign: bool, block: &Hash32, daa: u64) -> String {
    let addr = |k: u64| format!("kaspatest:pz{}", &hex_of("addr", k, 32)[..59]);
    let p2sh = |k: u64| format!("0000aa20{}87", &hex_of("sh", k, 32));
    let p2pk = |k: u64| format!("000020{}ac", &hex_of("pk", k, 32));
    let cov = if foreign {
        format!("{{\"authorizingInput\":0,\"covenantId\":\"{}\"}}", h("foreign-cov", n % 97).to_hex())
    } else {
        "null".into()
    };
    let payment = n.is_multiple_of(5);
    let (sig, spk_in, typ) = if payment {
        (format!("41{}01", hex_of("sig", n, 64)), p2pk(n), "PubKey")
    } else {
        (hex_of("redeem", n, 8), p2sh(n), "ScriptHash")
    };
    let output = |value: u64, spk: String, cov: &str, k: u64| {
        format!(
            "{{\"covenant\":{cov},\"scriptPublicKey\":\"{spk}\",\"value\":{value},\"verboseData\":{{\"scriptPublicKeyAddress\":\"{}\",\"scriptPublicKeyType\":\"{typ}\"}}}}",
            addr(k)
        )
    };
    let outputs = if payment {
        format!("{},{}", output(100_000_000 + n, p2pk(n + 1), &cov, n + 1), output(2_900_000_000 - n % 1_000, p2pk(n), "null", n))
    } else {
        output(3_061_837_924 - n % 1_000, spk_in.clone(), &cov, n)
    };
    let payload = if payment { hex_of("payload", n, 32) } else { String::new() };
    format!(
        concat!(
            "{{\"gas\":0,\"inputs\":[{{\"computeBudget\":0,\"previousOutpoint\":{{\"index\":{pi},\"transactionId\":\"{prev}\"}},",
            "\"sequence\":0,\"sigOpCount\":{ops},\"signatureScript\":\"{sig}\",\"verboseData\":{{\"utxoEntry\":{{\"amount\":{amt},",
            "\"blockDaaScore\":null,\"covenantId\":null,\"isCoinbase\":false,\"scriptPublicKey\":\"{spk_in}\",\"verboseData\":{{",
            "\"scriptPublicKeyAddress\":\"{a_in}\",\"scriptPublicKeyType\":\"{typ}\"}}}}}}}}],\"lockTime\":null,\"mass\":0,",
            "\"outputs\":[{outputs}],\"payload\":\"{payload}\",\"storageMass\":0,",
            "\"subnetworkId\":\"0000000000000000000000000000000000000000\",\"verboseData\":{{\"blockHash\":\"{block}\",",
            "\"blockTime\":{time},\"computeMass\":577,\"hash\":\"{hash}\",\"transactionId\":\"{id}\"}},\"version\":null}}"
        ),
        pi = n % 3,
        prev = h("noise-prev", n).to_hex(),
        ops = payment as u8,
        sig = sig,
        amt = 3_062_088_694 - n % 1_000,
        spk_in = spk_in,
        a_in = addr(n),
        typ = typ,
        outputs = outputs,
        payload = payload,
        block = block.to_hex(),
        time = 1_790_000_000_000u64 + daa * 100,
        hash = h("noise-hash", n).to_hex(),
        id = h("noise-id", n).to_hex(),
    )
}

/// The KOB traffic generator: asks placed, filled (4 of 10 whole tokens), the rest cancelled; x402 token payments.
struct Kob {
    w: World,
    open: std::collections::VecDeque<(u64, SignedTx, AskState, Hash32)>,
    filled: std::collections::VecDeque<(u64, SignedTx, AskState, Hash32)>,
    n: i64,
    created: u64,
    fills: u64,
    cancels: u64,
    x402: u64,
}

impl Kob {
    fn new() -> Kob {
        let mut w = World::new();
        w.validate = false; // the builders' output is engine-validated by the flow tests; this one needs volume
        Kob { w, open: Default::default(), filled: Default::default(), n: 0, created: 0, fills: 0, cancels: 0, x402: 0 }
    }

    /// The KOB and x402 transactions of block `i` (DAA `daa`), and the x402 transaction ids.
    fn block(&mut self, i: u64, daa: u64, mix: &Mix, rng: &mut Rng) -> (Vec<SignedTx>, Vec<Hash32>) {
        let mut out = vec![];
        let mut watched = vec![];
        for _ in 0..rng.count(mix.creates) {
            self.n += 1;
            let a = AskState { price: P250 + self.n, ..ask(MAKER_A, P250) };
            let t = self.w.create_tx(AnyState::KobAsk(a.clone()), CARRIER, MAKER_A, 10 * WHOLE);
            self.w.note_daa(&[&t], daa);
            let cov = self.w.cov(&t, 0);
            self.open.push_back((i, t.clone(), a, cov));
            self.created += 1;
            out.push(t);
        }
        for _ in 0..rng.count(mix.fills) {
            if self.open.front().is_none_or(|o| o.0 + 2 > i) {
                break;
            }
            let (_, create, a, cov) = self.open.pop_front().expect("checked");
            let custody = self.w.token_at(&create, 1, Kcc20State::custody(10 * WHOLE, cov.0, EXT));
            let leg = Leg::Ask { order: self.w.order(&create, 0, a.clone()), custody, amount: 4 * WHOLE, t: None };
            let fill = self.w.sign(&Action::Batch(batch(&self.w, daa, vec![leg])));
            self.w.note_daa(&[&fill], daa);
            self.filled.push_back((i, fill.clone(), AskState { amount_left: 6 * WHOLE, ..a }, cov));
            self.fills += 1;
            out.push(fill);
        }
        for _ in 0..rng.count(mix.cancels) {
            if self.filled.front().is_none_or(|o| o.0 + 2 > i) {
                break;
            }
            let (_, fill, a6, cov) = self.filled.pop_front().expect("checked");
            let cancel = self.w.sign(&Action::CancelOrder(CancelOrder {
                order: self.w.order(&fill, find_cov(&fill, &cov).expect("continuation"), AnyState::KobAsk(a6)),
                custody: Some(self.w.token_at(
                    &fill,
                    find_custody(&fill, &cov, 6 * WHOLE).expect("custody"),
                    Kcc20State::custody(6 * WHOLE, cov.0, EXT),
                )),
                prefund: None,
                foreign: vec![],
                strays: vec![],
                tokens: vec![],
                funding: vec![self.w.coin(MAKER_A, 10)],
                change: None,
                replace: None,
                lock_time: 0,
                records: vec![],
                fee: FeeOptions::default(),
            }));
            self.w.note_daa(&[&cancel], daa);
            self.cancels += 1;
            out.push(cancel);
        }
        for _ in 0..rng.count(mix.x402) {
            let pay = self.w.sign(&Action::SendTokens(SendTokens {
                token: TokenRef { covenant_id: TOKEN_COV, program: T3 },
                tokens: vec![self.w.token(TAKER, 50 * WHOLE)],
                recipients: vec![TokenRecipient { pubkey: pk(MAKER_B), amount: 3 * WHOLE, carrier: CARRIER }],
                token_change: Some(pk(TAKER)),
                token_change_carrier: CARRIER,
                funding: vec![self.w.coin(TAKER, 100)],
                change: None,
                records: vec![note_record()],
                fee: FeeOptions::default(),
            }));
            self.w.note_daa(&[&pay], daa);
            watched.push(Hash32(pay.tx.id));
            self.x402 += 1;
            out.push(pay);
        }
        (out, watched)
    }
}

/// One VSPC response of `blocks` (hash, daa, transaction JSONs) as the node's JSON.
fn response_json(blocks: &[(Hash32, u64, Vec<String>)]) -> String {
    let mut s = String::with_capacity(blocks.iter().map(|b| b.2.iter().map(|t| t.len() + 1).sum::<usize>() + 600).sum::<usize>() + 64);
    s.push_str("{\"removedChainBlockHashes\":[],\"addedChainBlockHashes\":[");
    for (i, b) in blocks.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&format!("\"{}\"", b.0.to_hex()));
    }
    s.push_str("],\"chainBlockAcceptedTransactions\":[");
    for (i, (hash, daa, txs)) in blocks.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        // the header fields a v2.1.0 node sends at `High` (parents of 12 levels, as captured)
        let parents: Vec<String> = (0..12u64)
            .map(|l| format!("[{},[\"{}\",\"{}\"]]", 15 + 3 * l, h("par", daa * 100 + l).to_hex(), h("par2", daa * 100 + l).to_hex()))
            .collect();
        s.push_str(&format!(
            concat!(
                "{{\"chainBlockHeader\":{{\"acceptedIdMerkleRoot\":\"{root}\",\"bits\":503873306,\"blueScore\":{daa},",
                "\"blueWork\":\"0000000000000000000000000000000000078d8c12468af0\",\"daaScore\":{daa},\"hash\":\"{hash}\",",
                "\"hashMerkleRoot\":\"{mroot}\",\"nonce\":14779659159087470000,\"parentsByLevel\":[{parents}],\"pruningPoint\":null,",
                "\"timestamp\":{ts},\"utxoCommitment\":null,\"version\":2}},\"acceptedTransactions\":["
            ),
            root = h("aroot", *daa).to_hex(),
            daa = daa,
            hash = hash.to_hex(),
            mroot = h("mroot", *daa).to_hex(),
            parents = parents.join(","),
            ts = 1_790_000_000_000u64 + daa * 100
        ));
        for (j, t) in txs.iter().enumerate() {
            if j > 0 {
                s.push(',');
            }
            s.push_str(t);
        }
        s.push_str("]}");
    }
    s.push_str("]}");
    s
}

#[derive(Default, Debug)]
struct Report {
    blocks: u64,
    txs: u64,
    wire_bytes: u64,
    relevant: u64,
    parse: Duration,
    check: Duration,
    commit: Duration,
    cpu: Duration,
    db_bytes: u64,
    log_bytes: u64,
    peak_rss: u64,
    rss_before: u64,
    commits: u64,
    /// Every derived row, in a stable order (equivalence tests).
    rows: String,
}

impl Report {
    fn wall(&self) -> Duration {
        self.parse + self.check + self.commit
    }
    fn print(&self, name: &str) {
        let wall = self.wall().as_secs_f64();
        let per_k = |d: Duration| d.as_secs_f64() * 1e3 / (self.txs as f64 / 1e3);
        println!("\n== {name} ==");
        println!(
            "{} blocks, {} transactions ({} KOB / x402 relevant), {:.1} MB of JSON ({:.0} B/tx), {} commits",
            self.blocks,
            self.txs,
            self.relevant,
            self.wire_bytes as f64 / 1e6,
            self.wire_bytes as f64 / self.txs as f64,
            self.commits
        );
        println!(
            "processing: {:.0} tx/s ({:.1}x of 3,000 tx/s), {:.1} chain s/s; parse {:.2} ms, check {:.2} ms, commit {:.2} ms per 1k tx",
            self.txs as f64 / wall,
            self.txs as f64 / wall / 3_000.0,
            self.blocks as f64 / 10.0 / wall,
            per_k(self.parse),
            per_k(self.check),
            per_k(self.commit)
        );
        println!(
            "CPU: {:.2} s for {:.2} s of processing ({:.0} % of one core); {:.1} us per tx; at 3,000 tx/s: {:.0} % of one core",
            self.cpu.as_secs_f64(),
            wall,
            100.0 * self.cpu.as_secs_f64() / wall,
            self.cpu.as_secs_f64() * 1e6 / self.txs as f64,
            100.0 * self.cpu.as_secs_f64() / self.txs as f64 * 3_000.0
        );
        println!(
            "writes: database +{:.2} MB, record log +{:.2} MB ({:.1} KB/s of chain); memory: {:.0} MB before, peak {:.0} MB",
            self.db_bytes as f64 / 1e6,
            self.log_bytes as f64 / 1e6,
            (self.db_bytes + self.log_bytes) as f64 / 1e3 / (self.blocks as f64 / 10.0),
            self.rss_before as f64 / 1e6,
            self.peak_rss as f64 / 1e6
        );
    }
}

fn dir_bytes(p: &std::path::Path) -> u64 {
    std::fs::read_dir(p)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| if e.path().is_dir() { dir_bytes(&e.path()) } else { e.metadata().map(|m| m.len()).unwrap_or(0) })
                .sum()
        })
        .unwrap_or(0)
}

/// Generate `blocks` chain blocks of `mix` and push them through parse, check and commit, `per_commit` blocks per VSPC
/// response. Generation is not timed; the three stages are.
fn run(blocks: u64, per_commit: u64, mix: Mix, seed: u64) -> (Report, Kob) {
    let mut cfg = IngestConfig::default();
    if let Some(n) = std::env::var("KOB_CAP_VERIFY_THREADS").ok().and_then(|v| v.parse().ok()) {
        cfg.verify_threads = kob_executor::indexer::ingest::verify_threads(n);
    }
    run_with(blocks, per_commit, mix, seed, cfg)
}

fn run_with(blocks: u64, per_commit: u64, mix: Mix, seed: u64, cfg: IngestConfig) -> (Report, Kob) {
    let dir = tempfile::tempdir().unwrap();
    let db_dir = dir.path().join("db");
    std::fs::create_dir_all(&db_dir).unwrap();
    let log_dir = dir.path().join("records");
    let (log, _) = RecordLog::open(&log_dir, 1 << 26, 0).unwrap();
    let conn = kob_executor::indexer::db::open_writer(&db_dir.join("index.sqlite3"), "testnet-10").unwrap();
    let mut ing = Ingest::new(conn, processor(), Some(log)).with_config(cfg);
    let anchor = h("cap-anchor", seed);
    let base_daa = ANCHOR_DAA;
    ing.init_cursor(&Cursor { hash: anchor, daa: base_daa }).unwrap();
    let db0 = dir_bytes(&db_dir);
    let mut kob = Kob::new();
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut rep = Report { rss_before: os::mem().0, ..Default::default() };
    let mut cursor = anchor;
    let mut noise = seed * 100_000_000;
    let mut watched_at: std::collections::VecDeque<(u64, Hash32)> = Default::default();
    let mut i = 0u64;
    while i < blocks {
        // generate (untimed)
        let mut resp_blocks = vec![];
        let mut watch_now = vec![];
        for _ in 0..per_commit.min(blocks - i) {
            let daa = base_daa + 1 + i;
            let hash = h("cap-block", seed * 10_000_000 + i);
            let (kob_txs, watched) = kob.block(i, daa, &mix, &mut rng);
            let mut txs: Vec<String> = kob_txs.iter().map(|t| serde_json::to_string(&wire_tx(&t.tx)).unwrap()).collect();
            while txs.len() < mix.tx_per_block {
                noise += 1;
                let foreign = rng.unit() < mix.foreign;
                txs.push(noise_json(noise, foreign, &hash, daa));
            }
            // KOB transactions anywhere in the block
            for k in 0..kob_txs.len() {
                let j = (rng.next() % txs.len() as u64) as usize;
                txs.swap(k, j);
            }
            watch_now.extend(watched.into_iter().map(|t| (i, t)));
            resp_blocks.push((hash, daa, txs));
            i += 1;
        }
        let json = response_json(&resp_blocks);
        drop(resp_blocks);
        // the facilitator watches its payments before they are accepted; it drops them about 100 blocks later
        for (b, t) in &watch_now {
            ing.watch_tx_for(Watcher::Facilitator, *t);
            watched_at.push_back((*b, *t));
        }
        while watched_at.front().is_some_and(|(b, _)| b + 100 < i) {
            let (_, t) = watched_at.pop_front().expect("checked");
            ing.unwatch_tx_for(Watcher::Facilitator, &t);
        }

        // process (timed)
        let cpu0 = os::cpu();
        let t0 = Instant::now();
        let mut raw: RawVspcResponse = serde_json::from_str(&json).expect("parse");
        raw.wire_bytes = json.len();
        let t1 = Instant::now();
        let batch = raw.into_batch().expect("consistent");
        let t2 = Instant::now();
        let n_txs: usize = batch.added.iter().map(|b| b.txs.len()).sum();
        let last = batch.added.last().map(|b| b.header.hash).expect("non-empty");
        let a = ing.apply_batch(cursor, &batch).expect("commit");
        drop(batch);
        let t3 = Instant::now();
        rep.cpu += os::cpu() - cpu0;
        rep.parse += t1 - t0;
        rep.check += t2 - t1;
        rep.commit += t3 - t2;
        rep.blocks += a.added as u64;
        rep.txs += n_txs as u64;
        rep.wire_bytes += json.len() as u64;
        rep.relevant += a.relevant_txs;
        rep.commits += 1;
        cursor = last;
        for (_, t) in &watch_now {
            assert!(ing.acceptance_of(t).flatten().is_some(), "a watched payment was seen accepted");
        }
    }
    rep.peak_rss = os::mem().1;
    rep.db_bytes = dir_bytes(&db_dir).saturating_sub(db0);
    rep.log_bytes = dir_bytes(&log_dir);
    // the rows: every placement an order, every fill and cancel an event, every payment a holding
    let q = |sql: &str| ing.conn().query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap() as u64;
    assert_eq!(q("SELECT COUNT(*) FROM orders"), kob.created);
    assert_eq!(q("SELECT COUNT(*) FROM order_events WHERE kind = 'fill'"), kob.fills);
    assert_eq!(q("SELECT COUNT(*) FROM order_events WHERE kind = 'cancel'"), kob.cancels);
    assert_eq!(rep.relevant, kob.created + kob.fills + kob.cancels + kob.x402, "only KOB and payment transactions are relevant");
    assert!(q("SELECT COUNT(*) FROM token_holdings") >= kob.x402, "payments are tracked holdings");
    for sql in [
        "SELECT hex(covenant_id), contract, listed, unlisted_reason, hex(genesis_state) FROM orders ORDER BY covenant_id",
        "SELECT hex(txid), idx, hex(covenant_id), value, hex(state), created_block, spent_block, spent_entry FROM order_utxos ORDER BY txid, idx",
        "SELECT hex(covenant_id), kind, amount, price, payout, closes, detail FROM order_events ORDER BY id",
        "SELECT hex(covenant_id), status, filled_amount, remaining_amount, state_known FROM order_state ORDER BY covenant_id",
        "SELECT hex(txid), idx, hex(owner), amount, role, spent_block FROM token_utxos ORDER BY txid, idx",
        "SELECT hex(txid), idx, hex(owner), amount, role, hex(state), spent_block FROM token_holdings ORDER BY txid, idx",
        "SELECT block_seq, hex(txid), reason FROM rejects ORDER BY id",
    ] {
        let mut st = ing.conn().prepare(sql).unwrap();
        let n = st.column_count();
        let mut rows = st.query([]).unwrap();
        while let Some(r) = rows.next().unwrap() {
            for k in 0..n {
                let v: rusqlite::types::Value = r.get(k).unwrap();
                rep.rows.push_str(&format!("{v:?}|"));
            }
            rep.rows.push('\n');
        }
    }
    (rep, kob)
}

#[test]
fn capacity_smoke() {
    // 20 blocks of 300 transactions with a dense KOB mix, 5 blocks per commit (a follower at the tip)
    let mix = Mix { tx_per_block: 300, creates: 2.0, fills: 1.5, cancels: 1.0, x402: 1.0, foreign: 0.05 };
    let (rep, kob) = run(20, 5, mix, 1);
    rep.print("smoke: 20 blocks x 300 tx, 5 blocks per commit");
    assert_eq!(rep.txs, 20 * 300);
    assert!(kob.fills > 0 && kob.cancels > 0 && kob.x402 > 0, "{} {} {}", kob.fills, kob.cancels, kob.x402);
    let per_tx = rep.wire_bytes as f64 / rep.txs as f64;
    assert!((1_250.0..1_550.0).contains(&per_tx), "realistic JSON per transaction: {per_tx:.0} B");
}

#[test]
#[ignore = "benchmark: run with --release -- --ignored --nocapture full_capacity"]
fn full_capacity_at_3000_tx_per_second() {
    let secs: u64 = std::env::var("KOB_CAP_CHAIN_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(60);
    let mix = Mix::from_env();
    println!("mix: {mix:?}; {} s of chain at 10 BPS", secs);
    let blocks = secs * 10;
    // at the tip: the follower polls every 500 ms, about 5 blocks per commit
    let (tip, _) = run(blocks, 5, mix, 2);
    tip.print("following the tip: 5 blocks per commit");
    // catching up: windows of 100 blocks (10 s of chain, ~40 MB of JSON) per commit
    let (catch, _) = run(blocks, 100, mix, 3);
    catch.print("catching up: 100 blocks per commit");
    let worst = (tip.txs as f64 / tip.wall().as_secs_f64()).min(catch.txs as f64 / catch.wall().as_secs_f64());
    assert!(worst >= 3_000.0, "the indexer must process 3,000 tx/s on this machine: {worst:.0}");
}

#[test]
fn the_parallel_verification_pass_changes_nothing() {
    // the same chain through the single-threaded path and through the parallel pure pass: identical rows
    let mix = Mix { tx_per_block: 300, creates: 4.0, fills: 3.0, cancels: 1.5, x402: 1.0, foreign: 0.05 };
    let one = IngestConfig { verify_threads: 1, ..IngestConfig::default() };
    let four = IngestConfig { verify_threads: 4, ..IngestConfig::default() };
    let (a, _) = run_with(30, 5, mix, 7, one);
    let (b, _) = run_with(30, 5, mix, 7, four);
    assert!(a.rows.len() > 1_000, "the chain carried KOB rows");
    assert_eq!(a.rows, b.rows);
}

#[test]
#[ignore = "development aid: KOB_CAP_ROWS_REF=<file> writes the rows of a fixed chain once, then compares every later run with them"]
fn rows_match_a_reference_file() {
    let path = std::env::var("KOB_CAP_ROWS_REF").expect("KOB_CAP_ROWS_REF");
    let mix = Mix { tx_per_block: 300, creates: 30.0, fills: 30.0, cancels: 15.0, x402: 10.0, foreign: 0.05 };
    let (rep, _) = run_with(40, 5, mix, 11, IngestConfig::default());
    match std::fs::read_to_string(&path) {
        Ok(want) => assert!(want == rep.rows, "rows differ from {path}"),
        Err(_) => std::fs::write(&path, &rep.rows).unwrap(),
    }
}
