//! Several nodes (`rpc::multi`): the primary is the chain authority, windows of transaction bodies come from every node and
//! are checked against the primary before they are applied. Mock nodes on one shared chain: one slow, one that does not know
//! the newest blocks, and liars of several kinds. The database must equal a clean replay of the chain, a liar must be found
//! and dropped, and nothing it altered may reach the database.

mod common;

use common::*;
use kob_protocol::build::*;
use kob_protocol::state::*;
use kob_protocol::tx::*;
use kobd::config::StartMode;
use kobd::hex::Hash32;
use kobd::indexer::db::open_memory;
use kobd::indexer::follower::{Follower, FollowerConfig, StepOutcome};
use kobd::indexer::ingest::Ingest;
use kobd::indexer::status::{FollowerState, HealthState};
use kobd::rpc::multi::{MultiNode, NodeSpec, BENCH_FIRST, BENCH_STRIKES};
use kobd::rpc::types::*;
use kobd::rpc::{ChainSource, RpcError, WindowRequest};
use kobd::testkit::*;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// What a test node does to the answers of the shared mock chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Honest,
    /// Every window answer arrives this much later.
    Slow(Duration),
    /// Knows no chain block with a DAA score above this (a node behind the others).
    Behind(u64),
    /// Changes an output value of the first transaction of a window (the transaction no longer hashes to its id).
    AltersOutput,
    /// Drops the last accepted transaction of a chain block that has more than one (the primary reports it).
    DropsTx,
    /// Rewrites the DAA score of the first chain block (its header no longer hashes to its block hash).
    AltersHeader,
    /// Strips the covenant id from the spent outputs of every input (outside every hash: only the comparison with the
    /// primary's copy finds it, where the transaction matters to KOB).
    StripsSpent,
    /// Hides the last accepted transaction of every chain block from this DAA score on, in every answer (a primary that
    /// lies; only a cross-checking node can contradict it).
    HidesTxFrom(u64),
    /// Reports a blue score far above the real one for every block it knows. The end of a window from it would bound the
    /// primary's acceptance data unless the primary used its own.
    InflatesBlue,
    /// Its acceptance data unbounded by `minConfirmationCount` times out, as a request for a whole backlog would.
    IdsTimeOutUnbounded,
    /// Every window it serves arrives as a frame header claiming 3 GiB, beyond the client's absolute limit (1 GiB).
    Oversize,
}

/// What a node forges about the cursor once `forge` is set: that it does not know it, or that it is below its retention
/// root (the error texts of the node).
const FORGES_UNKNOWN: u8 = 1;
const FORGES_RETENTION: u8 = 2;

/// What a node's windows do once `down` is set: hang until they time out (180 s, the client's request timeout), or lose the
/// connection at once.
const HANGS: u8 = 1;
const RESETS: u8 = 2;

struct Peer {
    node: Arc<MockNode>,
    mode: Mode,
    windows: AtomicUsize,
    /// `FORGES_UNKNOWN` or `FORGES_RETENTION`: every chain request fails with that error, and no block is known.
    forge: AtomicU8,
    /// `HANGS` or `RESETS`: every window it is asked for fails that way. Its other answers stay fine.
    down: AtomicU8,
    /// Requests for its acceptance data, as the cross-checking node asks for them.
    ids: AtomicUsize,
}

impl Peer {
    fn new(node: &Arc<MockNode>, mode: Mode) -> Arc<Peer> {
        Arc::new(Peer {
            node: node.clone(),
            mode,
            windows: AtomicUsize::new(0),
            forge: AtomicU8::new(0),
            down: AtomicU8::new(0),
            ids: AtomicUsize::new(0),
        })
    }

    fn forged(&self, start: Hash32) -> Result<(), RpcError> {
        match self.forge.load(Ordering::Relaxed) {
            FORGES_UNKNOWN => Err(RpcError::Node(format!("cannot find header {start}"))),
            FORGES_RETENTION => Err(RpcError::Node("the queried hash does not have retention root on its chain".into())),
            _ => Ok(()),
        }
    }
}

impl ChainSource for Peer {
    async fn vspc_v2(&self, req: VspcRequest) -> Result<RawVspcResponse, RpcError> {
        let windowed = req.data_verbosity_level == Verbosity::Full;
        if windowed {
            self.windows.fetch_add(1, Ordering::Relaxed);
        }
        self.forged(req.start_hash)?;
        if windowed {
            match self.down.load(Ordering::Relaxed) {
                HANGS => {
                    tokio::time::sleep(Duration::from_secs(180)).await;
                    return Err(RpcError::Timeout(Duration::from_secs(180)));
                }
                RESETS => return Err(RpcError::Transport("connection reset by peer".into())),
                _ => {}
            }
        }
        if let Mode::Slow(d) = self.mode {
            tokio::time::sleep(d).await;
        }
        if windowed && self.mode == Mode::Oversize {
            return Err(RpcError::TooLarge { size: 3 << 30, limit: req.max_bytes.unwrap_or(1 << 30) });
        }
        let mut r = self.node.vspc_v2(req).await?;
        if let Mode::HidesTxFrom(daa) = self.mode {
            for b in r.chain_block_accepted_transactions.iter_mut().filter(|b| b.chain_block_header.daa_score >= daa) {
                b.accepted_transactions.pop();
            }
        }
        if !windowed {
            return Ok(r);
        }
        let blocks = &mut r.chain_block_accepted_transactions;
        match self.mode {
            Mode::AltersOutput => {
                if let Some(t) = blocks.iter_mut().flat_map(|b| b.accepted_transactions.iter_mut()).find(|t| !t.outputs.is_empty()) {
                    t.outputs[0].value += 1;
                }
            }
            Mode::DropsTx => {
                if let Some(b) = blocks.iter_mut().find(|b| b.accepted_transactions.len() > 1) {
                    b.accepted_transactions.pop();
                }
            }
            Mode::AltersHeader => {
                if let Some(b) = blocks.first_mut() {
                    b.chain_block_header.daa_score += 1;
                }
            }
            Mode::StripsSpent => {
                for t in blocks.iter_mut().flat_map(|b| b.accepted_transactions.iter_mut()) {
                    for i in &mut t.inputs {
                        if let Some(u) = i.verbose_data.as_mut().and_then(|v| v.utxo_entry.as_mut()) {
                            u.covenant_id = None;
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(r)
    }
    async fn dag_info(&self) -> Result<DagInfo, RpcError> {
        self.node.dag_info().await
    }
    async fn server_info(&self) -> Result<ServerInfo, RpcError> {
        self.node.server_info().await
    }
    async fn utxos_by_addresses(&self, a: &[String]) -> Result<Vec<AddressUtxo>, RpcError> {
        self.node.utxos_by_addresses(a).await
    }
    async fn block_exists(&self, hash: Hash32) -> Result<bool, RpcError> {
        if self.forge.load(Ordering::Relaxed) != 0 {
            return Ok(false);
        }
        self.node.block_exists(hash).await
    }
    async fn sink_blue_score(&self) -> Result<u64, RpcError> {
        let s = self.node.sink_blue_score().await?;
        Ok(match self.mode {
            Mode::Behind(max) => s.min(max),
            _ => s,
        })
    }
    async fn block_blue_score(&self, hash: Hash32) -> Result<Option<u64>, RpcError> {
        let b = self.node.block_blue_score(hash).await?;
        Ok(match self.mode {
            Mode::Behind(max) => b.filter(|b| *b <= max),
            Mode::InflatesBlue => b.map(|b| b + (1 << 40)),
            _ => b,
        })
    }
    async fn chain_hashes(&self, start: Hash32) -> Result<ChainHashes, RpcError> {
        self.forged(start)?;
        ChainSource::chain_hashes(&*self.node, start).await
    }
    async fn chain_with_ids(&self, start: Hash32, m: Option<u64>) -> Result<ChainIds, RpcError> {
        self.ids.fetch_add(1, Ordering::Relaxed);
        self.forged(start)?;
        if self.mode == Mode::IdsTimeOutUnbounded && m.is_none() {
            return Err(RpcError::Timeout(Duration::from_secs(180)));
        }
        self.node.chain_with_ids(start, m).await
    }
}

/// A node that vouches for the primary's answers (`NodeSpec::cross_check`: the second node of a resolver pair) and serves
/// no windows of its own.
fn cross_check(name: &str) -> NodeSpec {
    NodeSpec { url: name.into(), fetch: false, submit: true, connections: 1, cross_check: true, may_lead: false }
}

fn spec(name: &str, fetch: bool, connections: usize) -> NodeSpec {
    NodeSpec { url: name.into(), fetch, submit: true, connections, cross_check: false, may_lead: fetch }
}

/// A follower over `nodes` (the first one the primary, also the verifier) on a fresh database.
struct Multi {
    follower: Follower<MultiNode<Peer>>,
    ingest: Arc<Mutex<Ingest>>,
    health: Arc<HealthState>,
}

fn multi(anchor: Hash32, nodes: Vec<(NodeSpec, Arc<Peer>)>) -> Multi {
    let verifier = nodes[0].1.clone();
    let source = Arc::new(MultiNode::new(nodes, verifier));
    let ingest = Arc::new(Mutex::new(Ingest::new(open_memory("testnet-10").unwrap(), processor(), None).with_config(wide_window())));
    let health = Arc::new(HealthState::new("testnet-10"));
    let (events, _) = tokio::sync::broadcast::channel(64);
    let cfg = FollowerConfig {
        network: "testnet-10".into(),
        start: StartMode::Hash(anchor),
        poll_interval: Duration::from_millis(1),
        backoff: Duration::from_millis(1),
        max_backoff: Duration::from_millis(5),
        min_confirmations: None,
        max_walkback_blocks: 1_000,
        status_refresh: Duration::from_millis(0),
        caught_up_daa: 100,
        batch_initial_blue: 8,
        batch_target: Duration::from_secs(30),
        fetch_parallel: source.fetch_connections(),
        prefetch_max_bytes: 256 << 20,
        prefetch_min_lag_blue: 0,
        prefetch_initial_blocks: 6,
        authority_failures: kobd::indexer::follower::AUTHORITY_FAILURES,
        authority_stall: kobd::indexer::follower::AUTHORITY_STALL,
    };
    let follower = Follower::new(source.clone(), ingest.clone(), cfg, health.clone(), events);
    Multi { follower, ingest, health }
}

impl Multi {
    async fn sync(&self, max_steps: usize) {
        for _ in 0..max_steps {
            let o = self.follower.step().await;
            assert!(!matches!(o, StepOutcome::Gap(_)), "gap: {o:?}");
            if matches!(o, StepOutcome::Idle) {
                return;
            }
        }
        panic!("the follower did not reach the sink in {max_steps} steps");
    }

    fn snapshot(&self) -> String {
        snapshot(self.ingest.lock().unwrap().conn())
    }
}

/// Small asks every tenth block among noise (a third of it with covenant outputs of another project), and from block 50 on,
/// cancels of the earlier asks: transactions that spend tracked order outputs and carry no payload. Returns the asks.
fn chain(c: &Ctx, blocks: u64) -> Vec<Hash32> {
    let mut asks: Vec<(SignedTx, AskState)> = vec![];
    let mut covs = vec![];
    for i in 0..blocks {
        let noise: Vec<Tx> = (0..6).map(|j| noise_tx(i * 1_000 + j, j % 3 == 0)).collect();
        if i % 10 == 0 {
            let a = AskState { amount_left: 2 * WHOLE, ..ask(MAKER_A, P250 + i as i64) };
            let create = c.w.create_tx(AnyState::KobAsk(a.clone()), CARRIER, MAKER_A, 2 * WHOLE);
            c.w.include_with(&c.hs.node, &[&create], noise);
            covs.push(c.w.cov(&create, 0));
            asks.push((create, a));
        } else if i % 10 == 5 && i >= 50 && !asks.is_empty() {
            let (create, a) = asks.remove(0);
            let cov = c.w.cov(&create, 0);
            let cancel = c.w.sign(&Action::CancelOrder(CancelOrder {
                retired: None,
                prefund: None,
                order: c.w.order(&create, 0, AnyState::KobAsk(a)),
                custody: Some(c.w.token_at(&create, 1, Kcc20State::custody(2 * WHOLE, cov.0, EXT))),
                foreign: vec![],
                strays: vec![],
                tokens: vec![],
                funding: vec![c.w.coin(MAKER_A, 10)],
                change: None,
                replace: None,
                lock_time: 0,
                records: vec![],
                fee: FeeOptions::default(),
            }));
            assert!(cancel.tx.payload.is_empty(), "a cancel without a payload: only the tracked outpoint makes it matter");
            c.w.include_with(&c.hs.node, &[&cancel], noise);
        } else {
            c.hs.node.push_block(noise);
        }
    }
    covs
}

fn statuses(m: &Multi, covs: &[Hash32]) -> Vec<String> {
    let g = m.ingest.lock().unwrap();
    covs.iter()
        .map(|c| {
            g.conn()
                .query_row(
                    "SELECT status FROM order_state WHERE order_id = (SELECT id FROM orders WHERE covenant_id = ?1)",
                    [&c.0[..]],
                    |r| r.get(0),
                )
                .unwrap()
        })
        .collect()
}

/// Honest, slow and behind nodes: the database equals a clean replay, every node but the one behind served windows, and
/// the one behind was set aside after it failed one.
#[tokio::test]
async fn several_nodes_one_slow_one_behind_equal_a_clean_replay() {
    let c = Ctx::new();
    let covs = chain(&c, 160);
    let node = c.hs.node.clone();
    let behind_at = node.chain()[60].daa;
    let primary = Peer::new(&node, Mode::Honest);
    let fast = Peer::new(&node, Mode::Honest);
    let slow = Peer::new(&node, Mode::Slow(Duration::from_millis(40)));
    let behind = Peer::new(&node, Mode::Behind(behind_at));
    let m = multi(
        node.anchor(),
        vec![
            (spec("primary", true, 2), primary.clone()),
            (spec("fast", true, 2), fast.clone()),
            (spec("slow", true, 2), slow.clone()),
            (spec("behind", true, 2), behind.clone()),
        ],
    );
    m.sync(2_000).await;
    let h = m.health.snapshot();
    assert_eq!(h.cursor_hash, Some(node.tip()));
    assert_eq!(h.state, FollowerState::Following);
    assert_eq!(m.snapshot(), fresh_snapshot(&node.chain()), "the same rows as a clean replay");
    let st = statuses(&m, &covs);
    assert!(st.iter().filter(|s| *s == "cancelled").count() >= 5 && st.iter().any(|s| s == "open"), "{st:?}");
    // windows from the other nodes, checked: the cancels' chain blocks compared with the primary's copy
    assert!(fast.windows.load(Ordering::Relaxed) > 0, "the fast node served windows");
    assert!(h.untrusted_windows_total > 0 && h.untrusted_blocks_total > 0, "{h:?}");
    assert!(h.confirmed_blocks_total > 0 && h.confirmed_blocks_total < h.untrusted_blocks_total, "{h:?}");
    assert_eq!(h.lies_total, 0);
    let nodes = &h.nodes;
    assert_eq!(nodes.len(), 4, "{nodes:?}");
    assert!(nodes.iter().all(|n| n.lies == 0 && n.dropped_reason.is_none()), "{nodes:?}");
    assert!(nodes[3].out_of_sync > 0, "the node behind failed a window it did not know: {:?}", nodes[3]);
    assert!(nodes[1].windows_ok > 0 && nodes[1].bytes > 0, "{:?}", nodes[1]);
    println!(
        "windows: primary {} fast {} slow {} behind {} (failed {}); blocks checked {} of which compared with the primary {}",
        nodes[0].windows_ok,
        nodes[1].windows_ok,
        nodes[2].windows_ok,
        nodes[3].windows_ok,
        nodes[3].windows_failed,
        h.untrusted_blocks_total,
        h.confirmed_blocks_total
    );
}

/// A node that alters what a hash covers (an output, a header, the accepted set) is found at the check of its first
/// window, dropped, and its windows go to the primary: the database equals a clean replay.
#[tokio::test]
async fn a_node_contradicting_a_hash_or_the_acceptance_data_is_dropped_at_its_first_window() {
    for mode in [Mode::AltersOutput, Mode::DropsTx, Mode::AltersHeader] {
        let c = Ctx::new();
        let _ = chain(&c, 80);
        let node = c.hs.node.clone();
        let primary = Peer::new(&node, Mode::Honest);
        let liar = Peer::new(&node, mode);
        // the primary serves windows only when no other node can: every window is offered to the liar first
        let m = multi(node.anchor(), vec![(spec("primary", false, 2), primary.clone()), (spec("liar", true, 2), liar.clone())]);
        m.sync(2_000).await;
        let h = m.health.snapshot();
        assert_eq!(h.cursor_hash, Some(node.tip()), "{mode:?}");
        assert_eq!(m.snapshot(), fresh_snapshot(&node.chain()), "{mode:?}: nothing the liar altered was applied");
        let l = &h.nodes[1];
        assert_eq!(l.state, "dropped", "{mode:?}: {l:?}");
        assert!(l.lies >= 1 && l.dropped_reason.is_some(), "{mode:?}: {l:?}");
        assert_eq!(l.windows_ok, 0, "{mode:?}: no window of the liar was used");
        let asked = liar.windows.load(Ordering::Relaxed);
        assert!((1..=2).contains(&asked), "{mode:?}: dropped at its first window(s), asked {asked}");
        assert!(h.nodes[0].windows_ok > 0, "{mode:?}: the primary took over");
        println!("{mode:?}: dropped ({})", l.dropped_reason.as_deref().unwrap_or(""));
    }
}

/// A node that alters what no hash covers (the spent outputs of the inputs) passes the hash checks; the chain blocks with
/// transactions that matter to KOB (here the cancels, which spend tracked orders) are compared with the primary's copy
/// before applying, the difference drops the node, and the database equals a clean replay (the cancels applied).
#[tokio::test]
async fn a_node_altering_unhashed_data_of_a_kob_transaction_is_caught_by_the_primary_copy() {
    let c = Ctx::new();
    let covs = chain(&c, 120);
    let node = c.hs.node.clone();
    let primary = Peer::new(&node, Mode::Honest);
    let liar = Peer::new(&node, Mode::StripsSpent);
    let m = multi(node.anchor(), vec![(spec("primary", false, 2), primary.clone()), (spec("liar", true, 3), liar.clone())]);
    m.sync(2_000).await;
    let h = m.health.snapshot();
    assert_eq!(h.cursor_hash, Some(node.tip()));
    assert_eq!(m.snapshot(), fresh_snapshot(&node.chain()), "the cancels were applied from the primary's data");
    assert!(statuses(&m, &covs).iter().filter(|s| *s == "cancelled").count() >= 5);
    let l = &h.nodes[1];
    assert_eq!(l.state, "dropped", "{l:?}");
    assert!(h.lies_total >= 1, "{h:?}");
    assert!(l.dropped_reason.as_deref().is_some_and(|r| r.contains("spent output") || r.contains("inputs")), "{l:?}");
    // its windows before the first cancel were used (nothing there matters to KOB beyond hashed data)
    assert!(l.windows_ok >= 1, "{l:?}");
    println!("dropped after {} windows: {}", l.windows_ok, l.dropped_reason.as_deref().unwrap_or(""));
}

/// A pair that cross-checks (the resolver default): with two honest nodes the database equals a clean replay and nothing is
/// contradicted; a primary that hides transactions from one chain block on is contradicted by the other node there, and the
/// follower applies the chain up to the block before it and nothing after: never data only one node vouches for while the
/// other contradicts it.
#[tokio::test]
async fn a_cross_checked_pair_indexes_only_what_both_nodes_report() {
    let c = Ctx::new();
    let covs = chain(&c, 70);
    let node = c.hs.node.clone();
    let m = multi(
        node.anchor(),
        vec![(spec("primary", true, 2), Peer::new(&node, Mode::Honest)), (cross_check("second"), Peer::new(&node, Mode::Honest))],
    );
    m.sync(2_000).await;
    let h = m.health.snapshot();
    assert_eq!(h.cursor_hash, Some(node.tip()));
    assert_eq!(m.snapshot(), fresh_snapshot(&node.chain()), "honest pair: a clean replay");
    assert_eq!(h.nodes[1].disagreements, 0, "{:?}", h.nodes[1]);
    assert!(statuses(&m, &covs).iter().any(|s| s == "open"));

    let c = Ctx::new();
    let _ = chain(&c, 70);
    let node = c.hs.node.clone();
    let blocks = node.chain();
    let k = 41;
    let liar = Peer::new(&node, Mode::HidesTxFrom(blocks[k].daa));
    let m = multi(node.anchor(), vec![(spec("primary", true, 2), liar), (cross_check("second"), Peer::new(&node, Mode::Honest))]);
    for _ in 0..400 {
        let o = m.follower.step().await;
        assert!(!matches!(o, StepOutcome::Gap(_)), "gap: {o:?}");
    }
    let h = m.health.snapshot();
    assert_eq!(h.cursor_hash, Some(blocks[k - 1].hash), "applied up to the block before the contradicted one");
    assert_eq!(m.snapshot(), fresh_snapshot(&blocks[..k]), "only what both nodes report");
    assert!(h.nodes[1].disagreements > 0, "{:?}", h.nodes[1]);
    assert!(h.nodes[1].last_error.as_deref().is_some_and(|e| e.contains("accepted")), "{:?}", h.nodes[1]);
    println!("contradicted {} times; cursor held at block {}", h.nodes[1].disagreements, k - 1);
}

/// The primary's acceptance data for a window from another node is bounded by the primary's own blue score of the window
/// end, never by the other node's claim. A node that reports a huge one would otherwise make the primary's request unbounded
/// (a whole backlog), and its timeout counts against the primary: two of them and a stall hand the chain authority over
/// (`indexer::follower`, *A failing chain authority*), possibly to that very node.
#[tokio::test]
async fn a_node_inflating_the_window_end_cannot_make_the_primary_fail() {
    let c = Ctx::new();
    let _ = chain(&c, 80);
    let node = c.hs.node.clone();
    let primary = Peer::new(&node, Mode::IdsTimeOutUnbounded);
    let liar = Peer::new(&node, Mode::InflatesBlue);
    // the primary serves windows only when no other node can: every window is offered to the liar first
    let source =
        MultiNode::new(vec![(spec("primary", false, 2), primary.clone()), (spec("inflates", true, 2), liar.clone())], primary);
    let blocks = node.chain();
    for k in [10usize, 25, 40] {
        let w = WindowRequest::new(blocks[k].hash, blocks[k + 10].hash);
        let f = source.fetch_window(w).await.expect("the primary's request is bounded by its own blue score");
        assert_eq!(f.origin.node, 1, "the window came from the other node, checked");
        assert_eq!(f.raw.added_chain_block_hashes, blocks[k + 1..=k + 10].iter().map(|b| b.hash).collect::<Vec<_>>());
    }
    let st = source.board().snapshot();
    assert_eq!((st[0].windows_failed, st[0].timeouts, st[0].last_error.clone()), (0, 0, None), "{:?}", st[0]);
    assert_eq!(st[1].windows_ok, 3, "{:?}", st[1]);
}

/// A node whose answers claim a frame beyond the client's absolute limit (1 GiB) fails the window itself: it backs off
/// and the window goes to the next node. Before, the claim was taken for the window's size: the window was split down to
/// one chain block, asked for without a limit and offered to the same node again, forever, inside one step.
#[tokio::test]
async fn a_node_claiming_an_oversized_frame_is_charged_and_its_windows_go_elsewhere() {
    let c = Ctx::new();
    let _ = chain(&c, 80);
    let node = c.hs.node.clone();
    let primary = Peer::new(&node, Mode::Honest);
    let liar = Peer::new(&node, Mode::Oversize);
    let m = multi(node.anchor(), vec![(spec("primary", false, 2), primary.clone()), (spec("oversize", true, 2), liar.clone())]);
    tokio::time::timeout(Duration::from_secs(120), m.sync(2_000)).await.expect("the follower reaches the sink");
    let h = m.health.snapshot();
    assert_eq!(h.cursor_hash, Some(node.tip()));
    assert_eq!(m.snapshot(), fresh_snapshot(&node.chain()), "the same rows as a clean replay");
    let l = &h.nodes[1];
    assert!(l.windows_failed >= 1 && l.windows_ok == 0, "{l:?}");
    assert!(l.last_error.as_deref().is_some_and(|e| e.contains("exceeds")), "{l:?}");
    assert!(h.nodes[0].windows_ok > 0, "the primary took the windows: {:?}", h.nodes[0]);
}

/// With a cross-checking node (a resolver pair), the authority's word alone never rewinds or halts the store. An authority
/// that says it does not know the cursor, or that the cursor is below its retention root, while the other node knows the
/// cursor and its chain, is not believed: nothing is rewound, no gap, the disagreement is counted. Once the other node says
/// the same, the follower acts as before (a walk back that finds no stored block known, or the retention gap).
#[tokio::test]
async fn an_authority_that_disowns_the_cursor_is_not_believed_without_the_cross_checking_node() {
    for forge in [FORGES_UNKNOWN, FORGES_RETENTION] {
        let c = Ctx::new();
        let _ = chain(&c, 40);
        let node = c.hs.node.clone();
        let primary = Peer::new(&node, Mode::Honest);
        let second = Peer::new(&node, Mode::Honest);
        let m = multi(node.anchor(), vec![(spec("primary", true, 2), primary.clone()), (cross_check("second"), second.clone())]);
        m.sync(2_000).await;
        let before = m.snapshot();
        primary.forge.store(forge, Ordering::Relaxed);
        for _ in 0..20 {
            let o = m.follower.step().await;
            assert!(matches!(o, StepOutcome::Retry(_)), "forge {forge}: {o:?}");
        }
        let h = m.health.snapshot();
        assert_eq!(h.cursor_hash, Some(node.tip()), "forge {forge}: the cursor stays");
        assert_eq!((h.reorgs_total, h.reverted_blocks_total, h.gap_reason.clone()), (0, 0, None), "forge {forge}");
        assert_eq!(m.snapshot(), before, "forge {forge}: nothing rewound");
        assert!(h.nodes[1].disagreements > 0, "forge {forge}: {:?}", h.nodes[1]);
        assert!(h.last_error.as_deref().is_some_and(|e| e.contains("not confirmed")), "forge {forge}: {:?}", h.last_error);

        // both nodes say so: confirmed
        second.forge.store(forge, Ordering::Relaxed);
        let o = m.follower.step().await;
        let StepOutcome::Gap(why) = o else { panic!("forge {forge}: {o:?}") };
        assert!(kobd::indexer::follower::is_pruned_gap(&why), "forge {forge}: {why}");
    }
}

/// Appends `blocks` chain blocks of noise to the mock chain, salted by `salt` so that no transaction repeats.
fn grow(node: &MockNode, blocks: u64, salt: u64) {
    for i in 0..blocks {
        node.push_block((0..6).map(|j| noise_tx(salt + i * 100 + j, j % 3 == 0)).collect());
    }
}

/// A secondary whose window answers take a minute each, next to a healthy primary (simulated time). Its late windows are
/// asked of the primary too after a few seconds, and the first answer is used. After three such windows it is benched for
/// bodies and gets none. Every catch-up takes about as long as the primary's alone, and the slow node delivered no window.
/// Before, every window it held took the whole minute, in chain order, and held up every window behind it.
#[tokio::test(start_paused = true)]
async fn a_slow_secondary_is_hedged_and_benched_and_the_catch_up_keeps_the_primary_pace() {
    let c = Ctx::new();
    let _ = chain(&c, 160);
    let node = c.hs.node.clone();
    let alone = multi(node.anchor(), vec![(spec("pace-alone", true, 2), Peer::new(&node, Mode::Honest))]);
    let t0 = tokio::time::Instant::now();
    alone.sync(2_000).await;
    let base = t0.elapsed();

    let slow = Peer::new(&node, Mode::Slow(Duration::from_secs(60)));
    let m = multi(
        node.anchor(),
        vec![(spec("pace-primary", true, 2), Peer::new(&node, Mode::Honest)), (spec("pace-slow", true, 2), slow.clone())],
    );
    // Catch-ups of 160 chain blocks, then of 60 more each, until the slow node is benched.
    let mut rounds = 0;
    loop {
        let t0 = tokio::time::Instant::now();
        m.sync(2_000).await;
        let took = t0.elapsed();
        let s = m.health.snapshot().nodes[1].clone();
        println!(
            "round {rounds}: {took:?} (the primary alone: {base:?}); slow node asked for {} windows, {s:?}",
            slow.windows.load(Ordering::Relaxed)
        );
        assert!(took < Duration::from_secs(30) && took <= base + Duration::from_secs(30), "{took:?} against {base:?} alone");
        assert_eq!(s.windows_ok, 0, "{s:?}");
        if s.state == "benched" {
            break;
        }
        rounds += 1;
        assert!(rounds < 8, "never benched: {s:?}");
        grow(&node, 60, 10_000_000 * rounds);
    }
    let h = m.health.snapshot();
    assert_eq!(h.cursor_hash, Some(node.tip()));
    assert_eq!(m.snapshot(), fresh_snapshot(&node.chain()), "the same rows as a clean replay");
    let s = &h.nodes[1];
    assert_eq!(s.benched, 1, "{s:?}");
    assert!(s.hedged >= BENCH_STRIKES as u64, "{s:?}");
    assert!(s.bench_reason.as_deref().is_some_and(|r| r.starts_with("late")), "{s:?}");
    assert!(h.nodes[0].windows_ok > 0 && h.nodes[0].in_flight == 0 && s.in_flight == 0, "{:?}", h.nodes);

    // Benched: the next catch-up does not ask it at all and waits for nothing.
    let asked = slow.windows.load(Ordering::Relaxed);
    grow(&node, 60, 90_000_000);
    let t0 = tokio::time::Instant::now();
    m.sync(2_000).await;
    assert!(t0.elapsed() <= base + Duration::from_secs(1), "{:?}", t0.elapsed());
    assert_eq!(slow.windows.load(Ordering::Relaxed), asked, "no window for a benched node");
    assert_eq!(m.health.snapshot().cursor_hash, Some(node.tip()));
    assert_eq!(m.snapshot(), fresh_snapshot(&node.chain()), "the same rows as a clean replay");
}

/// A secondary whose windows keep failing, hanging until they time out or losing the connection, is benched for bodies.
/// Benched, it still vouches for the authority's single steps. Healthy again after the bench, it serves windows again.
#[tokio::test(start_paused = true)]
async fn a_failing_secondary_is_benched_for_bodies_still_cross_checks_and_returns_once_healthy() {
    for (k, down) in [HANGS, RESETS].into_iter().enumerate() {
        let c = Ctx::new();
        let _ = chain(&c, 80);
        let node = c.hs.node.clone();
        let flaky = Peer::new(&node, Mode::Honest);
        flaky.down.store(down, Ordering::Relaxed);
        let peer =
            NodeSpec { url: format!("flaky-{k}"), fetch: true, submit: true, connections: 2, cross_check: true, may_lead: true };
        let m = multi(
            node.anchor(),
            vec![(spec(&format!("flaky-primary-{k}"), true, 2), Peer::new(&node, Mode::Honest)), (peer, flaky.clone())],
        );
        let salt = 100_000_000 * (k as u64 + 1);
        // Catch-ups until it is benched. A node that lost its connection backs off first, so time passes between them.
        let mut rounds = 0;
        loop {
            let t0 = tokio::time::Instant::now();
            m.sync(2_000).await;
            assert!(t0.elapsed() < Duration::from_secs(30), "{:?}", t0.elapsed());
            let f = m.health.snapshot().nodes[1].clone();
            if f.state == "benched" {
                break;
            }
            rounds += 1;
            assert!(rounds < 8, "never benched: {f:?}");
            tokio::time::sleep(Duration::from_secs(f.backoff_secs + 1)).await;
            grow(&node, 60, salt + 1_000_000 * rounds);
        }
        let h = m.health.snapshot();
        assert_eq!(h.cursor_hash, Some(node.tip()));
        assert_eq!(m.snapshot(), fresh_snapshot(&node.chain()), "the same rows as a clean replay");
        let f = &h.nodes[1];
        assert_eq!((f.benched, f.windows_ok), (1, 0), "{f:?}");
        assert!(f.bench_secs > 0 && f.bench_secs <= BENCH_FIRST.as_secs(), "{f:?}");
        if down == RESETS {
            assert!(f.windows_failed >= BENCH_STRIKES as u64 && f.hedged == 0, "{f:?}");
            assert!(f.bench_reason.as_deref().is_some_and(|r| r.contains("reset")), "{f:?}");
        } else {
            assert!(f.hedged >= BENCH_STRIKES as u64, "{f:?}");
        }

        // Past its backoff and within the bench: single steps near the sink are still vouched for by it.
        assert!(f.backoff_secs + 1 < f.bench_secs, "{f:?}");
        tokio::time::sleep(Duration::from_secs(f.backoff_secs + 1)).await;
        let ids = flaky.ids.load(Ordering::Relaxed);
        grow(&node, 3, salt + 50_000_000);
        m.sync(100).await;
        assert_eq!(m.health.snapshot().cursor_hash, Some(node.tip()));
        assert!(flaky.ids.load(Ordering::Relaxed) > ids, "a node benched for bodies still cross-checks");
        assert_eq!(m.health.snapshot().nodes[1].state, "benched");

        // Healthy again, and the bench is over: it serves windows again.
        flaky.down.store(0, Ordering::Relaxed);
        tokio::time::sleep(BENCH_FIRST).await;
        grow(&node, 80, salt + 60_000_000);
        m.sync(2_000).await;
        let h = m.health.snapshot();
        assert_eq!(h.cursor_hash, Some(node.tip()));
        assert_eq!(m.snapshot(), fresh_snapshot(&node.chain()), "the same rows as a clean replay");
        let f = &h.nodes[1];
        assert!(f.windows_ok > 0, "it serves windows again: {f:?}");
        assert_eq!((f.state.as_str(), f.benched, f.bench_secs), ("ok", 1, 0), "{f:?}");
    }
}
