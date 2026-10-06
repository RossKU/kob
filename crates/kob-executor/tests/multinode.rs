//! Several nodes (`rpc::multi`): the primary is the chain authority, windows of transaction bodies come from every node and
//! are checked against the primary before they are applied. Mock nodes on one shared chain: one slow, one that does not know
//! the newest blocks, and liars of several kinds. The database must equal a clean replay of the chain, a liar must be found
//! and dropped, and nothing it altered may reach the database.

mod common;

use common::*;
use kob_executor::config::StartMode;
use kob_executor::hex::Hash32;
use kob_executor::indexer::db::open_memory;
use kob_executor::indexer::follower::{Follower, FollowerConfig, StepOutcome};
use kob_executor::indexer::ingest::Ingest;
use kob_executor::indexer::status::{FollowerState, HealthState};
use kob_executor::rpc::multi::{MultiNode, NodeSpec};
use kob_executor::rpc::types::*;
use kob_executor::rpc::{ChainSource, RpcError};
use kob_executor::testkit::*;
use kob_protocol::build::*;
use kob_protocol::state::*;
use kob_protocol::tx::*;
use std::sync::atomic::{AtomicUsize, Ordering};
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
}

struct Peer {
    node: Arc<MockNode>,
    mode: Mode,
    windows: AtomicUsize,
}

impl Peer {
    fn new(node: &Arc<MockNode>, mode: Mode) -> Arc<Peer> {
        Arc::new(Peer { node: node.clone(), mode, windows: AtomicUsize::new(0) })
    }
}

impl ChainSource for Peer {
    async fn vspc_v2(&self, req: VspcRequest) -> Result<RawVspcResponse, RpcError> {
        let windowed = req.data_verbosity_level == Verbosity::Full;
        if windowed {
            self.windows.fetch_add(1, Ordering::Relaxed);
        }
        if let Mode::Slow(d) = self.mode {
            tokio::time::sleep(d).await;
        }
        let mut r = self.node.vspc_v2(req).await?;
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
            _ => b,
        })
    }
    async fn chain_hashes(&self, start: Hash32) -> Result<ChainHashes, RpcError> {
        ChainSource::chain_hashes(&*self.node, start).await
    }
    async fn chain_with_ids(&self, start: Hash32, m: Option<u64>) -> Result<ChainIds, RpcError> {
        self.node.chain_with_ids(start, m).await
    }
}

fn spec(name: &str, fetch: bool, connections: usize) -> NodeSpec {
    NodeSpec { url: name.into(), fetch, submit: true, connections }
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
        .map(|c| g.conn().query_row("SELECT status FROM order_state WHERE covenant_id = ?1", [&c.0[..]], |r| r.get(0)).unwrap())
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
