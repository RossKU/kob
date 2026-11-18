//! Security tests of the keepers and the runner: a keeper request read back from the in-flight
//! journal is signed only when it pays the operator alone; the pause file is checked again right
//! before each submit; an unreadable subsidy window grants no subsidy.

#[path = "matcher_common/mod.rs"]
mod common;

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use common::*;
use kob_protocol::build::Action;
use kob_protocol::state::*;
use kobd::keepers::{reprice, tick, Job, JobKind, KeeperConfig, KeeperInput};
use kobd::matcher::book::{Clock, ListedOrder, MemoryBook};
use kobd::matcher::node::*;
use kobd::matcher::run::{InFlightJob, Roles, RunConfig, Runner, ViewSource};
use kobd::matcher::submit::Tracked;
use kobd::matcher::wallet::p2pk_spk_string;
use serde_json::Value;

const KILL_DAA: u64 = 5_600;
const ATTACKER: u8 = 33;

/// An IOC ask listed at DAA 5,000 (killable from 5,600) with a 10 KAS refund tip.
fn ioc(n: u32) -> ListedOrder {
    let mut s = ask(1, P250, 5 * WHOLE, T3);
    s.tif = TIF_IOC;
    s.expiry_daa = NO_EXPIRY;
    s.refund_tip = 10 * KAS as i64;
    listed(cid(n), AnyState::KobAsk(s), CARRIER, 5_000)
}

/// The funded kill of `ioc(1)`, built by a keeper tick.
fn kill() -> Job {
    let inp = KeeperInput {
        orders: vec![ioc(1)],
        clock: Clock { daa: KILL_DAA, utc: UTC },
        funding: vec![funding(20 * KAS)],
        excluded: BTreeSet::new(),
        excluded_outpoints: BTreeSet::new(),
        known_unprofitable: BTreeSet::new(),
    };
    let mut r = tick(&inp, &KeeperConfig::default(), &signer());
    assert_eq!(r.jobs.len(), 1, "{:?}", r.skipped);
    let j = r.jobs.remove(0);
    assert_eq!(j.kind, JobKind::Kill);
    j
}

fn refund_of(a: &mut Action) -> &mut kob_protocol::build::RefundOrder {
    match a {
        Action::RefundOrder(r) => r,
        _ => panic!("not a refund"),
    }
}

/// `reprice` signs a journaled request only when every funding input and the change are the
/// operator's. A request with no funding and someone else's change never gets the operator's spare
/// coin.
#[test]
fn a_replacement_never_pays_change_or_funding_to_another_key() {
    let j = kill();
    let cfg = KeeperConfig { subsidy_max: KAS, ..KeeperConfig::default() };
    let spare = funding(30 * KAS);
    assert!(reprice(j.kind, j.order, &j.action, 200, 100, Some(&spare), &cfg, &signer()).is_ok(), "the operator's own request");
    // the journal line of the attack: no funding, the attacker's change
    let mut a = j.action.clone();
    refund_of(&mut a).funding.clear();
    refund_of(&mut a).change = Some(pk(ATTACKER));
    let e = reprice(j.kind, j.order, &a, 200, 100, Some(&spare), &cfg, &signer()).unwrap_err();
    assert!(e.contains("not the operator's key"), "{e}");
    // a funding input of another key, the change the operator's
    let mut a = j.action.clone();
    refund_of(&mut a).funding[0].pubkey = pk(ATTACKER);
    refund_of(&mut a).change = Some(pk(MATCHER));
    let e = reprice(j.kind, j.order, &a, 200, 100, Some(&spare), &cfg, &signer()).unwrap_err();
    assert!(e.contains("funding input"), "{e}");
    // a spare coin of another key is never added
    let mut a = j.action.clone();
    refund_of(&mut a).funding.clear();
    refund_of(&mut a).change = None;
    let foreign = kob_protocol::tx::KeyUtxo { pubkey: pk(ATTACKER), ..spare.clone() };
    if let Ok(job) = reprice(j.kind, j.order, &a, 200, 100, Some(&foreign), &cfg, &signer()) {
        assert!(refund_of(&mut job.action.clone()).funding.is_empty());
    }
}

/// A node that answers what a keeper step asks. `pause_on_funding` creates the pause file when the step reads its funding,
/// after the step's own pause check.
#[derive(Clone, Default)]
struct MockNode(Arc<Mutex<State>>);

#[derive(Default)]
struct State {
    daa: u64,
    submits: usize,
    pause_on_funding: Option<std::path::PathBuf>,
}

impl NodeApi for MockNode {
    async fn server_info(&self) -> Result<ServerInfo, RpcError> {
        let s = self.0.lock().unwrap();
        Ok(ServerInfo {
            server_version: "2.1.0".into(),
            network_id: "testnet-10".into(),
            is_synced: true,
            has_utxo_index: true,
            virtual_daa_score: s.daa,
        })
    }
    async fn dag_info(&self) -> Result<DagInfo, RpcError> {
        let s = self.0.lock().unwrap();
        Ok(DagInfo { network: "testnet-10".into(), sink: "s0".into(), virtual_daa_score: s.daa })
    }
    async fn submit(&self, _tx: Value) -> Result<[u8; 32], RpcError> {
        self.0.lock().unwrap().submits += 1;
        Ok([0; 32])
    }
    async fn chain_from(&self, _start: &str) -> Result<ChainUpdate, RpcError> {
        Ok(ChainUpdate::default())
    }
    async fn utxos_by_addresses(&self, _a: &[String]) -> Result<Vec<AddressUtxo>, RpcError> {
        if let Some(p) = self.0.lock().unwrap().pause_on_funding.as_ref() {
            std::fs::write(p, b"").unwrap();
        }
        Ok(vec![AddressUtxo {
            transaction_id: [0xf1; 32],
            index: 0,
            amount: 100 * KAS,
            script_public_key: p2pk_spk_string(&pk(MATCHER)),
            block_daa_score: 500,
            covenant_id: None,
        }])
    }
}

fn runner(
    node: MockNode,
    orders: Vec<ListedOrder>,
    state_dir: Option<std::path::PathBuf>,
) -> Runner<MockNode, ViewSource<MemoryBook>> {
    let mut b = book(orders);
    b.daa_score = node.0.lock().unwrap().daa;
    let mut cfg = RunConfig { roles: Roles { matcher: false, keeper: true }, max_book_lag: None, state_dir, ..RunConfig::default() };
    cfg.keeper.subsidy_max = KAS;
    cfg.keeper.max_replacements = 2;
    Runner::new(node, ViewSource(b), Box::new(signer()), cfg)
}

/// A tracked kill as the journal keeps it.
fn tracked(j: &Job) -> Tracked {
    Tracked {
        txid: j.signed.tx.id,
        spends: j.spends.clone(),
        orders: [j.order].into_iter().collect(),
        owners: Default::default(),
        addresses: Default::default(),
        parent: None,
        submitted_daa: KILL_DAA,
        rpc_tx: serde_json::json!({}),
        accepted: None,
        kind: "kill".into(),
        profit: j.profit,
        fee: j.built.fee.fee,
        fee_rate: j.built.fee.fee_rate,
        unconfirmed: false,
        deadline: None,
        resend_until: None,
        retired: None,
    }
}

/// At a restart a journaled keeper job whose request pays another key is dropped (its transaction
/// is still followed); the operator's own is kept.
#[test]
fn a_journaled_job_that_pays_another_key_is_dropped_at_the_start() {
    let j = kill();
    let job = |action: Action| InFlightJob {
        kind: j.kind,
        order: j.order,
        action,
        replacements: 0,
        sent_daa: KILL_DAA,
        subsidy: 0,
        done: false,
    };
    let mut bad = j.action.clone();
    refund_of(&mut bad).funding.clear();
    refund_of(&mut bad).change = Some(pk(ATTACKER));
    for (action, kept) in [(j.action.clone(), true), (bad, false)] {
        let dir = tempfile::tempdir().unwrap();
        let (mut jn, _) = kobd::matcher::journal::Journal::open(&dir.path().join(kobd::matcher::journal::FILE_NAME)).unwrap();
        let t = tracked(&j);
        jn.add(&t, Some(&job(action)));
        drop(jn);
        let r = runner(MockNode::default(), vec![], Some(dir.path().to_path_buf()));
        assert!(r.tracker.txs.contains_key(&t.txid), "the transaction is followed either way");
        assert_eq!(r.keeper_inflight.contains_key(&t.txid), kept);
    }
}

/// The pause file set during a step (here while the step reads its funding) stops the submits that
/// follow.
#[tokio::test]
async fn a_pause_set_during_the_step_stops_its_submits() {
    let dir = tempfile::tempdir().unwrap();
    let pause = dir.path().join("pause");
    let node = MockNode::default();
    node.0.lock().unwrap().daa = KILL_DAA;
    // control: without the pause the kill goes out
    let mut r = runner(node.clone(), vec![ioc(1)], None);
    let rep = r.step().await.unwrap();
    assert_eq!((rep.built.len(), rep.submitted.len(), node.0.lock().unwrap().submits), (1, 1, 1));
    let node = MockNode::default();
    node.0.lock().unwrap().daa = KILL_DAA;
    node.0.lock().unwrap().pause_on_funding = Some(pause.clone());
    let mut r = runner(node.clone(), vec![ioc(1)], None);
    r.cfg.pause_file = Some(pause.clone());
    let rep = r.step().await.unwrap();
    assert!(pause.exists());
    assert!(rep.paused, "the step reports the pause");
    assert_eq!(node.0.lock().unwrap().submits, 0, "nothing was submitted");
    assert!(rep.submitted.is_empty() && r.tracker.txs.is_empty());
}

/// A subsidy window file that does not parse grants no subsidy at the start (not a fresh budget),
/// and is kept as is.
#[test]
fn a_corrupt_subsidy_window_grants_nothing_at_the_start() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join(kobd::matcher::run::SUBSIDY_FILE);
    std::fs::write(&p, b"{corrupt").unwrap();
    let r = runner(MockNode::default(), vec![], Some(dir.path().to_path_buf()));
    assert!(r.keeper_subsidy.is_unreadable());
    assert_eq!(r.keeper_subsidy.left(&r.cfg.keeper, KILL_DAA, kobd::matcher::book::utc_now()), 0);
    assert_eq!(std::fs::read(&p).unwrap(), b"{corrupt");
    // without the file: the full budget
    std::fs::remove_file(&p).unwrap();
    let r = runner(MockNode::default(), vec![], Some(dir.path().to_path_buf()));
    assert_eq!(r.keeper_subsidy.left(&r.cfg.keeper, KILL_DAA, kobd::matcher::book::utc_now()), r.cfg.keeper.subsidy_budget);
}
