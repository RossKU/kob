//! The follower and the write path: paging, errors, unknown cursors, gaps, reorg reverts, the reorg
//! window, and the property that an incrementally maintained database equals one rebuilt from
//! scratch or from the record log, under random traffic and random reorgs.

mod common;

use common::*;
use kob_executor::hex::Hash32;
use kob_executor::indexer::db::open_memory;
use kob_executor::indexer::follower::StepOutcome;
use kob_executor::indexer::ingest::{Ingest, IngestConfig, IngestError};
use kob_executor::indexer::recordlog::RecordLog;
use kob_executor::indexer::status::FollowerState;
use kob_executor::rpc::RpcError;
use kob_executor::testkit::*;
use kob_protocol::build::*;
use kob_protocol::family::Family;
use kob_protocol::state::*;
use kob_protocol::tx::*;

/// Create a small ask (two whole tokens) with a distinct price so every call makes a different order.
fn small_ask(c: &Ctx, price_bump: i64) -> SignedTx {
    let a = AskState { amount_left: 2 * WHOLE, ..ask(MAKER_A, P250 + price_bump) };
    c.w.create_tx(AnyState::KobAsk(a), CARRIER, MAKER_A, 2 * WHOLE)
}

#[tokio::test]
async fn follower_pages_through_a_capped_response() {
    let c = Ctx::new();
    c.hs.node.set_added_cap(2);
    for i in 0..7 {
        let create = small_ask(&c, i);
        c.w.include(&c.hs.node, &[&create]);
    }
    let out = c.hs.sync().await;
    let applied = out.iter().filter(|o| matches!(o, StepOutcome::Applied { .. })).count();
    assert!(applied >= 4, "{out:?}");
    assert_eq!(c.hs.query::<i64>("SELECT COUNT(*) FROM orders", []), 7);
    assert_eq!(c.hs.health.snapshot().cursor_hash, Some(c.hs.node.tip()));
    assert_eq!(c.hs.health.snapshot().state, FollowerState::Following);
}

#[tokio::test]
async fn ibd_and_transport_errors_never_advance_the_cursor() {
    let c = Ctx::new();
    let create = small_ask(&c, 0);
    c.w.include(&c.hs.node, &[&create]);
    c.hs.node.set_ibd(true);
    let out = c.hs.follower.step().await;
    assert!(matches!(out, StepOutcome::Retry(_)), "{out:?}");
    assert_eq!(c.hs.health.snapshot().state, FollowerState::NodeUnavailable);
    assert_eq!(c.hs.query::<i64>("SELECT COUNT(*) FROM orders", []), 0);
    c.hs.node.set_ibd(false);
    c.hs.node.inject_error(RpcError::Transport("connection reset".into()));
    assert!(matches!(c.hs.follower.step().await, StepOutcome::Retry(_)));
    c.hs.sync().await;
    assert_eq!(c.hs.status(&c.w.cov(&create, 0)), "open");
}

#[tokio::test]
async fn unknown_cursor_walks_back_to_a_stored_block_the_node_knows() {
    let c = Ctx::new();
    let create = small_ask(&c, 0);
    let relevant = c.w.include(&c.hs.node, &[&create]);
    for _ in 0..3 {
        c.hs.node.push_block(vec![]);
    }
    c.hs.sync().await;
    assert_eq!(c.hs.health.snapshot().cursor_hash, Some(c.hs.node.tip()));
    // the node forgets the cursor block (pruned / replaced): the indexer falls back to a stored
    // block it still knows and re-follows from there
    c.hs.node.forget(c.hs.node.tip());
    c.hs.node.push_block(vec![]);
    let out = c.hs.follower.step().await;
    assert!(matches!(out, StepOutcome::Rewound { .. }), "{out:?}");
    let cursor = c.hs.health.snapshot().cursor_hash.unwrap();
    assert!(c.hs.node.chain_hashes().contains(&cursor));
    assert_ne!(cursor, c.hs.node.tip());
    c.hs.sync().await;
    assert_eq!(c.hs.status(&c.w.cov(&create, 0)), "open");
    let _ = relevant;
}

#[tokio::test]
async fn nothing_known_or_no_retention_root_is_a_gap_and_halts() {
    let c = Ctx::new();
    let create = small_ask(&c, 0);
    c.w.include(&c.hs.node, &[&create]);
    c.hs.node.push_block(vec![]);
    c.hs.sync().await;
    let calls = c.hs.node.vspc_calls();
    c.hs.node.set_retention_floor(3);
    let out = c.hs.follower.step().await;
    assert!(matches!(&out, StepOutcome::Gap(m) if m.contains("retention")), "{out:?}");
    assert_eq!(c.hs.health.snapshot().state, FollowerState::Gap);
    assert!(c.hs.health.snapshot().gap_reason.is_some());
    assert_eq!(c.hs.node.vspc_calls(), calls + 1);

    // a node that has never heard of any stored block
    let c2 = Ctx::new();
    let create = small_ask(&c2, 0);
    c2.w.include(&c2.hs.node, &[&create]);
    c2.hs.node.push_block(vec![]);
    c2.hs.sync().await;
    for b in c2.hs.node.chain_hashes() {
        c2.hs.node.forget(b);
    }
    c2.hs.node.push_block(vec![]);
    let out = c2.hs.follower.step().await;
    assert!(matches!(out, StepOutcome::Gap(_)), "{out:?}");
}

#[tokio::test]
async fn wrong_network_is_a_gap_at_start() {
    let node = MockNode::new("mainnet");
    let hs = Harness::with_node(node, open_memory("testnet-10").unwrap(), None, wide_window());
    let out = hs.follower.step().await;
    assert!(matches!(&out, StepOutcome::Gap(m) if m.contains("mainnet")), "{out:?}");
}

#[tokio::test]
async fn apply_batch_refuses_inconsistent_input() {
    let c = Ctx::new();
    c.hs.node.push_block(vec![]);
    c.hs.sync().await;
    let mut ing = c.hs.ingest.lock().unwrap();
    let cursor = ing.cursor().unwrap().unwrap();
    use kob_executor::rpc::types::VspcBatch;
    // requested from a hash that is not the cursor
    let err = ing.apply_batch(h("nope", 1), &VspcBatch::default()).unwrap_err();
    assert!(matches!(err, IngestError::Stale { .. }));
    // removed list that does not start at the cursor
    let err = ing.apply_batch(cursor.hash, &VspcBatch { removed: vec![h("nope", 2)], added: vec![], wire_bytes: 0 }).unwrap_err();
    assert!(matches!(err, IngestError::InconsistentRemoved(_)));
    // removed without added must not move the cursor
    let err = ing.apply_batch(cursor.hash, &VspcBatch { removed: vec![cursor.hash], added: vec![], wire_bytes: 0 }).unwrap_err();
    assert!(matches!(err, IngestError::EmptyAfterReorg));
    assert_eq!(ing.cursor().unwrap().unwrap(), cursor);
}

#[tokio::test]
async fn a_reorg_reverts_fills_creates_and_custody_exactly() {
    let c = Ctx::new();
    let a = ask(MAKER_A, P250);
    let create = c.w.create_tx(AnyState::KobAsk(a.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    let before = c.hs.snapshot();
    let custody = c.w.token_at(&create, 1, Kcc20State::custody(10 * WHOLE, cov.0, EXT));
    let leg = Leg::Ask { order: c.w.order(&create, 0, a.clone()), custody, amount: 4 * WHOLE, t: None };
    let fill = c.w.sign(&Action::Batch(batch(&c.w, c.daa(), vec![leg])));
    c.push(&[&fill]).await;
    assert_eq!(c.hs.status(&cov), "partial");
    // the fill's block is reorged out and replaced by an empty one
    c.hs.node.reorg(1, vec![vec![]]);
    let out = c.hs.sync().await;
    assert!(out.iter().any(|o| matches!(o, StepOutcome::Applied { removed: 1, .. })), "{out:?}");
    assert_eq!(c.hs.status(&cov), "open");
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobAsk(a)));
    assert_eq!(c.hs.event_kinds(&cov), ["create"]);
    assert_eq!(c.hs.query::<i64>("SELECT COUNT(*) FROM token_utxos WHERE spent_block IS NULL", []), 1);
    assert_eq!(c.hs.health.snapshot().reorgs_total, 1);
    // the rows equal a database that never saw the fill (block ids differ by construction)
    let g = c.hs.snapshot();
    assert!(g.lines().filter(|l| l.starts_with("event ")).count() == 1);
    let _ = before;
    // and the fill re-included in the next block applies again
    c.w.include(&c.hs.node, &[&fill]);
    c.hs.sync().await;
    assert_eq!(c.hs.status(&cov), "partial");
}

#[tokio::test]
async fn chain_blocks_older_than_the_reorg_window_are_deleted_and_their_rows_stay() {
    let node = MockNode::new("testnet-10");
    let cfg = IngestConfig { reorg_window_daa: 50, checkpoint_daa: u64::MAX / 4, ..IngestConfig::default() };
    let hs = Harness::with_node(node, open_memory("testnet-10").unwrap(), None, cfg);
    let c = Ctx::with(hs);
    let create = small_ask(&c, 0);
    c.w.include(&c.hs.node, &[&create]);
    c.hs.node.push_empty(20);
    c.hs.sync().await;
    let blocks: i64 = c.hs.query("SELECT COUNT(*) FROM blocks", []);
    assert_eq!(blocks, 21, "everything is inside the window so far");
    c.hs.node.push_empty(200);
    c.hs.sync().await;
    let blocks: i64 = c.hs.query("SELECT COUNT(*) FROM blocks", []);
    assert!(blocks <= 52, "only the last 50 DAA of chain blocks are kept, got {blocks}");
    // the order it stamped is final and still served
    let cov = c.w.cov(&create, 0);
    assert_eq!(c.hs.status(&cov), "open");
    // block sequence numbers are never reused: a new relevant block gets a fresh stamp
    let create2 = small_ask(&c, 1);
    c.w.include(&c.hs.node, &[&create2]);
    c.hs.sync().await;
    let g1: i64 = c.hs.query("SELECT genesis_block FROM orders WHERE covenant_id = ?1", [&cov.0[..]]);
    let g2: i64 = c.hs.query("SELECT genesis_block FROM orders WHERE covenant_id = ?1", [&c.w.cov(&create2, 0).0[..]]);
    assert!(g2 > g1 + 200);
}

// ---------------------------------------------------------------------------------------------
// property: incremental == fresh == log replay under random traffic and random reorgs

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// A live order the generator can act on: the transaction that produced its current UTXO and where.
#[derive(Clone)]
struct Lv {
    cov: Hash32,
    state: AnyState,
    tx: SignedTx,
    /// The transaction holding the custody (`tx`, except after an in-place amend, which leaves the custody where it was).
    ctx: SignedTx,
    idx: usize,
    custody: Option<usize>,
    round: usize,
}

struct Gen {
    live: Vec<Lv>,
    salt: i64,
    /// The token family of the traffic. States are kept as the KCC-20-typed twins (`into_family`) with the family's
    /// token fields; the family shows in the token programs, custody states and the kinds created.
    fam: Family,
}

impl Gen {
    /// A KCC-20 fixture as the family's order (KCC-20-typed).
    fn fx(&self, s: AnyState) -> AnyState {
        if self.fam == Family::Kron {
            kron(s).into_family(Family::Kcc20)
        } else {
            s
        }
    }

    fn next_salt(&mut self) -> i64 {
        self.salt += 1;
        self.salt
    }

    /// One random valid transaction over the generator's own model of the chain.
    fn action(&mut self, c: &Ctx, rng: &mut Rng, round: usize) -> Option<SignedTx> {
        let kind = rng.below(10);
        let candidates: Vec<usize> = (0..self.live.len()).filter(|i| self.live[*i].round < round).collect();
        let pick = |rng: &mut Rng| candidates[rng.below(candidates.len() as u64) as usize];
        match kind {
            0 | 1 => {
                let s = self.next_salt();
                let n = (3 + rng.below(8) as i64) * WHOLE;
                let a = AskState { amount_left: n, ..ask(MAKER_A, P250 + s) };
                let AnyState::KobAsk(a) = self.fx(AnyState::KobAsk(a)) else { unreachable!() };
                let tx = c.w.create_tx(AnyState::KobAsk(a.clone()).into_family(self.fam), CARRIER, MAKER_A, n);
                self.live.push(Lv {
                    cov: c.w.cov(&tx, 0),
                    state: AnyState::KobAsk(a),
                    tx: tx.clone(),
                    ctx: tx.clone(),
                    idx: 0,
                    custody: Some(1),
                    round,
                });
                Some(tx)
            }
            2 => {
                let s = self.next_salt();
                let b = bid(MAKER_B, P245 + s);
                let AnyState::KobBid(b) = self.fx(AnyState::KobBid(b)) else { unreachable!() };
                let tx = c.w.create_tx(
                    AnyState::KobBid(b.clone()).into_family(self.fam),
                    b.escrow(6 * WHOLE, 3).unwrap() as u64,
                    MAKER_B,
                    0,
                );
                self.live.push(Lv {
                    cov: c.w.cov(&tx, 0),
                    state: AnyState::KobBid(b),
                    tx: tx.clone(),
                    ctx: tx.clone(),
                    idx: 0,
                    custody: None,
                    round,
                });
                Some(tx)
            }
            3 => {
                let s = self.next_salt();
                let ib = IfdBidState { rpt_amount: 1 + 20 * WHOLE, min_fill: 3 * WHOLE, price: P260 + s, ..ifd_bid(MAKER_A, 10) };
                let AnyState::KobIfdBid(ib) = self.fx(AnyState::KobIfdBid(ib)) else { unreachable!() };
                let tx = c.w.create_tx(AnyState::KobIfdBid(ib.clone()).into_family(self.fam), ib.escrow().unwrap() as u64, MAKER_A, 0);
                self.live.push(Lv {
                    cov: c.w.cov(&tx, 0),
                    state: AnyState::KobIfdBid(ib),
                    tx: tx.clone(),
                    ctx: tx.clone(),
                    idx: 0,
                    custody: None,
                    round,
                });
                Some(tx)
            }
            _ if candidates.is_empty() => None,
            4 | 5 => {
                // fill: ask (partial or full), bid (partial), repeat entry (creates a booked exit)
                let i = pick(rng);
                let lv = self.live[i].clone();
                let lock = c.daa();
                match lv.state.clone() {
                    AnyState::KobAsk(a) => {
                        let n = (1 + rng.below((a.amount_left / WHOLE) as u64) as i64) * WHOLE;
                        let custody = c.w.token_at(&lv.ctx, lv.custody?, TokenState::custody(self.fam, a.amount_left, lv.cov.0, EXT));
                        let leg = Leg::Ask { order: c.w.order(&lv.tx, lv.idx, a.clone()), custody, amount: n, t: None };
                        let tx = c.w.try_sign(&Action::Batch(batch(&c.w, lock, vec![leg])))?;
                        self.advance(i, &tx, AnyState::KobAsk(AskState { amount_left: a.amount_left - n, ..a }), round, true);
                        Some(tx)
                    }
                    AnyState::KobBid(b) => {
                        let n = (1 + rng.below(2) as i64) * WHOLE;
                        let mut bt =
                            batch(&c.w, lock, vec![Leg::Bid { order: c.w.order(&lv.tx, lv.idx, b.clone()), amount: n, t: None }]);
                        bt.taker_tokens = vec![c.w.token_for(self.fam, TAKER, n)];
                        let tx = c.w.try_sign(&Action::Batch(bt))?;
                        self.advance(i, &tx, AnyState::KobBid(b), round, false);
                        Some(tx)
                    }
                    AnyState::KobIfdBid(e) => {
                        if e.amount_left <= 0 {
                            return None;
                        }
                        let left = e.amount_left / WHOLE;
                        let n = (if left <= 3 { left } else { 3 + rng.below((left - 2) as u64) as i64 }) * WHOLE;
                        let mut bt = batch(
                            &c.w,
                            lock,
                            vec![Leg::IfdBid { order: c.w.order(&lv.tx, lv.idx, e.clone()), amount: n, evidence: None, t: None }],
                        );
                        bt.taker_tokens = vec![c.w.token_for(self.fam, TAKER, n)];
                        let tx = c.w.try_sign(&Action::Batch(bt))?;
                        let entry_daa = lv.tx_daa(c);
                        let booking = (e.rpt_amount > n)
                            .then(|| Booking { parent: lv.cov.0, until: rpt_until(e.expiry_daa, entry_daa).unwrap() });
                        let exit = e.exit_for(n, booking).unwrap();
                        let next = IfdBidState {
                            amount_left: e.amount_left - n,
                            rpt_amount: if e.rpt_amount > n { e.rpt_amount - n } else { e.rpt_amount },
                            ..e
                        };
                        let (xi, xcov) = find_fresh(&tx, &[lv.cov])?;
                        let xcust = find_custody_for(self.fam, &tx, &xcov, n);
                        self.advance(i, &tx, AnyState::KobIfdBid(next), round, false);
                        self.live.push(Lv {
                            cov: xcov,
                            state: AnyState::KobCondAsk(exit),
                            tx: tx.clone(),
                            ctx: tx.clone(),
                            idx: xi,
                            custody: xcust,
                            round,
                        });
                        Some(tx)
                    }
                    _ => None,
                }
            }
            6 => {
                // a booked exit takes profit with its entry's merge
                let xs: Vec<usize> = candidates
                    .iter()
                    .copied()
                    .filter(|i| matches!(&self.live[*i].state, AnyState::KobCondAsk(x) if x.is_booked()))
                    .collect();
                if xs.is_empty() {
                    return None;
                }
                let xi = xs[rng.below(xs.len() as u64) as usize];
                let x = self.live[xi].clone();
                let AnyState::KobCondAsk(xs_) = x.state.clone() else { return None };
                let ei = self.live.iter().position(|l| l.cov.0 == xs_.parent && l.round < round)?;
                let e = self.live[ei].clone();
                let AnyState::KobIfdBid(es) = e.state.clone() else { return None };
                let m = (1 + rng.below((xs_.amount_left / WHOLE) as u64) as i64) * WHOLE;
                let leg = Leg::CondAsk {
                    order: c.w.order(&x.tx, x.idx, xs_.clone()),
                    custody: c.w.token_at(&x.ctx, x.custody?, TokenState::custody(self.fam, xs_.amount_left, x.cov.0, EXT)),
                    amount: m,
                    leg: 0,
                    evidence: None,
                    t: None,
                    merge: Some(c.w.order(&e.tx, e.idx, es.clone())),
                };
                let tx = c.w.try_sign(&Action::Batch(batch(&c.w, c.daa(), vec![leg])))?;
                let left = xs_.amount_left - m;
                let ncust = find_custody_for(self.fam, &tx, &x.cov, left);
                self.advance(ei, &tx, AnyState::KobIfdBid(IfdBidState { amount_left: es.amount_left + m, ..es }), round, false);
                if left == 0 {
                    self.live.retain(|l| l.cov != x.cov);
                } else {
                    let j = self.live.iter().position(|l| l.cov == x.cov)?;
                    self.live[j] = Lv {
                        cov: x.cov,
                        state: AnyState::KobCondAsk(CondAskState { amount_left: left, ..xs_ }),
                        tx: tx.clone(),
                        ctx: tx.clone(),
                        idx: find_cov(&tx, &x.cov)?,
                        custody: ncust,
                        round,
                    };
                }
                Some(tx)
            }
            7 => {
                // cancel (asks, bids and entries; exits stay for the merge traffic)
                let i = pick(rng);
                let lv = self.live[i].clone();
                let custody = match (&lv.state, lv.custody) {
                    (AnyState::KobAsk(a), Some(ci)) => {
                        Some(c.w.token_at(&lv.ctx, ci, TokenState::custody(self.fam, a.amount_left, lv.cov.0, EXT)))
                    }
                    _ => None,
                };
                if lv.state.holds_tokens() && custody.is_none() {
                    return None;
                }
                if matches!(lv.state, AnyState::KobCondAsk(_)) {
                    return None;
                }
                let tx = c.w.try_sign(&Action::CancelOrder(CancelOrder {
                    prefund: None,
                    order: c.w.order(&lv.tx, lv.idx, lv.state.clone().into_family(self.fam)),
                    custody,
                    foreign: vec![],
                    strays: vec![],
                    tokens: vec![],
                    funding: vec![c.w.coin(MAKER_A, 10)],
                    change: None,
                    replace: None,
                    lock_time: 0,
                    records: vec![],
                    fee: FeeOptions::default(),
                }))?;
                self.live.remove(i);
                Some(tx)
            }
            8 => {
                // in-place amend of an ask (the maker's cancel continues the id with a new price; the custody stays)
                let i = pick(rng);
                let lv = self.live[i].clone();
                let AnyState::KobAsk(a) = lv.state.clone() else { return None };
                let s = self.next_salt();
                let amended = AskState { price: P250 + 7 * s, ..a.clone() };
                let tx = c.w.try_sign(&Action::AmendOrder(AmendOrder {
                    order: c.w.order(&lv.tx, lv.idx, AnyState::KobAsk(a).into_family(self.fam)),
                    amended: AnyState::KobAsk(amended.clone()).into_family(self.fam),
                    value: None,
                    funding: vec![],
                    change: None,
                    lock_time: 0,
                    deadline: None,
                    records: vec![],
                    fee: FeeOptions::default(),
                }))?;
                self.live[i] = Lv {
                    cov: lv.cov,
                    state: AnyState::KobAsk(amended),
                    tx: tx.clone(),
                    ctx: lv.ctx,
                    idx: 0,
                    custody: lv.custody,
                    round,
                };
                Some(tx)
            }
            _ => {
                let i = pick(rng);
                Some(c.w.stray_for(self.fam, self.live[i].cov, 1 + rng.below(9) as i64, TAKER))
            }
        }
    }

    /// The order continues in `tx` with `state` (or is gone when the transaction has no continuation).
    fn advance(&mut self, i: usize, tx: &SignedTx, state: AnyState, round: usize, has_custody: bool) {
        let cov = self.live[i].cov;
        match find_cov(tx, &cov) {
            Some(idx) => {
                let custody =
                    if has_custody { state.custody_amount().and_then(|a| find_custody_for(self.fam, tx, &cov, a)) } else { None };
                self.live[i] = Lv { cov, state, tx: tx.clone(), ctx: tx.clone(), idx, custody, round };
            }
            None => {
                self.live.remove(i);
            }
        }
    }
}

impl Lv {
    fn tx_daa(&self, c: &Ctx) -> i64 {
        c.w.utxo(&self.tx, self.idx).block_daa_score as i64
    }
}

/// Traffic counters of one run: (fills, rearms, cancels, exits, strays, reorgs, reverted blocks, in-place amends).
///
/// `link`: a link slower than the chain (a VSPC response with more transactions than this times out) and noise
/// transactions in every block, so the follower must split its batches to make progress.
/// `parallel` > 1: the follower fetches that many windows at once (planned from any lag, two chain blocks to start).
async fn run_property(seed: u64, rounds: usize, fam: Family, link: Option<usize>, parallel: usize) -> [i64; 8] {
    let mut c = Ctx::new();
    c.w.validate = false; // the builders' output is engine-validated by the other tests; this one needs volume
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
    let mut gen = Gen { live: vec![], salt: 1000 * seed as i64, fam };
    let dir = tempfile::tempdir().unwrap();
    // an indexer that logs, to be rebuilt from its record log at the end
    let log_dir = dir.path().join("records");
    let (log, _) = RecordLog::open(&log_dir, 1 << 20, 0).unwrap();
    let hs = Harness::with_follower(
        MockNode::new("testnet-10"),
        open_memory("testnet-10").unwrap(),
        Some(log),
        wide_window(),
        processor(),
        |f| {
            f.fetch_parallel = parallel;
            f.prefetch_min_lag_blue = 0;
            f.prefetch_initial_blocks = 2;
        },
    );
    hs.node.set_link_txs(link);
    c.hs = hs;
    let mut noise_salt = seed * 1_000_000;
    for round in 1..=rounds {
        let mut txs = vec![];
        for _ in 0..rng.below(4) {
            if let Some(t) = gen.action(&c, &mut rng, round) {
                c.w.note_daa(&[&t], c.daa() + 1);
                txs.push(t);
            }
        }
        if round > 4 && rng.below(4) == 0 {
            // reorg: replace up to 3 blocks, re-including their transactions shuffled (some dropped), plus new ones
            let chain = c.hs.node.chain();
            let depth = 1 + rng.below(3.min(chain.len() as u64 - 1)) as usize;
            let mut pool: Vec<_> = chain[chain.len() - depth..].iter().flat_map(|b| b.txs.clone()).collect();
            for i in (1..pool.len()).rev() {
                let j = rng.below(i as u64 + 1) as usize;
                pool.swap(i, j);
            }
            pool.retain(|_| rng.below(6) != 0);
            let mut new_blocks: Vec<Vec<_>> = (0..depth + rng.below(2) as usize).map(|_| vec![]).collect();
            for tx in pool {
                let k = rng.below(new_blocks.len() as u64) as usize;
                new_blocks[k].push(tx);
            }
            new_blocks.last_mut().unwrap().extend(txs.iter().map(|t| wire_tx(&t.tx)));
            c.hs.node.reorg(depth, new_blocks);
        } else {
            let mut block: Vec<_> = txs.iter().map(|t| wire_tx(&t.tx)).collect();
            if link.is_some() {
                for _ in 0..rng.below(7) {
                    noise_salt += 1;
                    block.push(noise_tx(noise_salt, rng.below(2) == 0));
                }
            }
            c.hs.node.push_block(block);
            if link.is_some() {
                // empty blocks between, so a batch window can end before the next chain block
                c.hs.node.push_empty(rng.below(3) as usize);
            }
        }
        if rng.below(3) != 0 {
            if link.is_some() {
                c.hs.sync_through_retries(2_000).await;
            } else {
                c.hs.sync().await;
            }
            assert_eq!(c.hs.snapshot(), fresh_snapshot(&c.hs.node.chain()), "seed {seed} round {round}");
        }
    }
    if link.is_some() {
        c.hs.sync_through_retries(2_000).await;
    } else {
        c.hs.sync().await;
    }
    let snap = c.hs.snapshot();
    assert_eq!(snap, fresh_snapshot(&c.hs.node.chain()), "seed {seed} final");
    assert!(snap.contains("event "), "seed {seed} produced no traffic");

    // rebuild from the record log alone (no node data): the same rows
    let log = kob_executor::indexer::recordlog::read_all(&log_dir).unwrap();
    let mut ing = Ingest::new(open_memory("testnet-10").unwrap(), processor(), None).with_config(wide_window());
    ing.init_cursor(&kob_executor::indexer::ingest::Cursor { hash: log.records[0].1.start, daa: 0 }).unwrap();
    ing.replay(log.records).unwrap();
    assert_eq!(snapshot(ing.conn()), snap, "seed {seed}: rebuild from the record log");
    let q = |sql: &str| c.hs.query::<i64>(sql, []);
    let hsnap = c.hs.health.snapshot();
    if parallel > 1 {
        assert!(hsnap.prefetch_bytes_peak > 0 && c.hs.node.hash_calls() > 0, "seed {seed}: windows were fetched ahead: {hsnap:?}");
    }
    [
        q("SELECT COUNT(*) FROM order_events WHERE kind = 'fill'"),
        q("SELECT COUNT(*) FROM order_events WHERE kind = 'rearm'"),
        q("SELECT COUNT(*) FROM order_events WHERE kind = 'cancel'"),
        q("SELECT COUNT(*) FROM orders WHERE parent IS NOT NULL"),
        q("SELECT COUNT(*) FROM token_utxos WHERE role = 'stray'"),
        hsnap.reorgs_total as i64,
        hsnap.reverted_blocks_total as i64,
        q("SELECT COUNT(*) FROM order_events WHERE kind = 'amend'"),
    ]
}

#[tokio::test]
async fn property_incremental_equals_fresh_and_log_replay_under_random_reorgs() {
    // the same traffic in both token families (KCC-20 and KRON)
    for fam in [Family::Kcc20, Family::Kron] {
        let mut total = [0i64; 8];
        for seed in [1u64, 2, 3, 4, 5, 6, 7, 8] {
            let s = run_property(seed, 45, fam, None, 1).await;
            for (t, v) in total.iter_mut().zip(s) {
                *t += v;
            }
        }
        println!("{fam:?} traffic (fills, rearms, cancels, exits, strays, reorgs, reverted blocks, amends): {total:?}");
        assert!(total.iter().all(|t| *t > 0), "the generator must exercise every path in {fam:?}: {total:?}");
    }
}

#[tokio::test]
async fn unknown_cursor_finds_the_newest_known_block_by_bisection() {
    let c = Ctx::new();
    let create = small_ask(&c, 0);
    c.w.include(&c.hs.node, &[&create]);
    c.hs.node.push_empty(40);
    c.hs.sync().await;
    let chain = c.hs.node.chain_hashes();
    // the node lost the newest 12 blocks it once reported (a replaced tail it no longer keeps)
    for b in &chain[chain.len() - 12..] {
        c.hs.node.forget(*b);
    }
    c.hs.node.reorg(0, vec![vec![]]);
    let calls_before = c.hs.node.vspc_calls();
    let out = c.hs.follower.step().await;
    assert!(matches!(out, StepOutcome::Rewound { blocks: 12 }), "{out:?}");
    assert_eq!(c.hs.health.snapshot().cursor_hash, Some(chain[chain.len() - 13]));
    assert!(c.hs.node.vspc_calls() > calls_before);
    c.hs.sync().await;
    assert_eq!(c.hs.status(&c.w.cov(&create, 0)), "open");
}

#[tokio::test]
async fn a_reorg_of_unrelated_blocks_is_not_logged_but_a_relevant_one_is() {
    let dir = tempfile::tempdir().unwrap();
    let log_dir = dir.path().join("records");
    let (log, _) = RecordLog::open(&log_dir, 1 << 20, 0).unwrap();
    let hs = Harness::with_node(MockNode::new("testnet-10"), open_memory("testnet-10").unwrap(), Some(log), wide_window());
    let c = Ctx::with(hs);
    let create = small_ask(&c, 0);
    c.w.include(&c.hs.node, &[&create]);
    c.hs.node.push_empty(3);
    c.hs.sync().await;
    let frames = kob_executor::indexer::recordlog::record_count(&log_dir).unwrap();
    // unrelated blocks are replaced: nothing in the database changes, nothing is written
    c.hs.node.reorg(2, vec![vec![noise_tx(1, true)], vec![]]);
    c.hs.sync().await;
    assert_eq!(kob_executor::indexer::recordlog::record_count(&log_dir).unwrap(), frames);
    // the block that carried the order is replaced: the revert is logged
    c.hs.node.reorg(4, vec![vec![]; 4]);
    c.hs.sync().await;
    assert_eq!(c.hs.query::<i64>("SELECT COUNT(*) FROM orders", []), 0);
    assert!(kob_executor::indexer::recordlog::record_count(&log_dir).unwrap() > frames);
}

/// The follower's request after a timeout is never the same request again (window, timeout or start changes).
fn assert_no_repeated_failure(node: &MockNode) {
    let log = node.vspc_log();
    for w in log.windows(2) {
        let (a, b) = (&w[0], &w[1]);
        assert!(a.3 || (a.0, a.1, a.2) != (b.0, b.1, b.2), "a timed-out request was repeated unchanged: {a:?} then {b:?}");
    }
}

#[tokio::test]
async fn a_follower_behind_a_flood_pages_through_a_backlog_it_cannot_fetch_at_once() {
    let c = Ctx::new();
    // 80 blocks of 40 noise transactions (3,200 in all) with orders in between, behind a link that carries
    // 150 transactions per request: the whole backlog in one batch (the old follower's request) never arrives
    let mut salt = 0;
    let mut creates = vec![];
    for i in 0..80u64 {
        let mut block: Vec<_> = (0..40)
            .map(|_| {
                salt += 1;
                noise_tx(salt, salt % 3 == 0)
            })
            .collect();
        if i % 10 == 0 {
            let create = small_ask(&c, i as i64);
            c.w.note_daa(&[&create], c.daa() + 1);
            block.push(wire_tx(&create.tx));
            creates.push(create);
        }
        c.hs.node.push_block(block);
    }
    c.hs.node.set_link_txs(Some(150));
    let out = c.hs.sync_through_retries(500).await;
    let h = c.hs.health.snapshot();
    assert_eq!(h.cursor_hash, Some(c.hs.node.tip()), "{out:?}");
    assert_eq!(h.state, FollowerState::Following);
    assert!(h.vspc_timeouts_total >= 1, "the link must have refused a batch: {out:?}");
    assert!(h.batch_window_blue.unwrap() < 80, "{h:?}");
    assert_eq!(h.txs_fetched_total, 80 * 40 + creates.len() as u64, "every accepted transaction exactly once");
    assert!(h.wire_bytes_total > 0 && h.bytes_per_tx().unwrap() > 100, "{h:?}");
    assert!(h.metrics_text().contains("kob_indexer_vspc_timeouts_total"));
    assert_eq!(c.hs.query::<i64>("SELECT COUNT(*) FROM orders", []), creates.len() as i64);
    for t in &creates {
        assert_eq!(c.hs.status(&c.w.cov(t, 0)), "open");
    }
    // the same rows as a clean replay of the chain
    assert_eq!(c.hs.snapshot(), fresh_snapshot(&c.hs.node.chain()));
    assert_no_repeated_failure(&c.hs.node);
    // while it was behind it never claimed to be caught up
    let applied_bounded = c.hs.node.vspc_log().iter().filter(|r| r.1.is_some() && r.3).count();
    assert!(applied_bounded >= 3, "it paged: {:?}", c.hs.node.vspc_log());
}

#[tokio::test]
async fn a_single_chain_block_larger_than_the_link_gets_a_longer_timeout_not_the_same_request() {
    let c = Ctx::new();
    c.hs.node.push_block((0..500).map(|i| noise_tx(i, false)).collect());
    let create = small_ask(&c, 0);
    c.w.include(&c.hs.node, &[&create]);
    // 500 transactions in one chain block through a link of 150 per timeout: only a 4x timeout carries it
    c.hs.node.set_link_txs(Some(150));
    c.hs.sync_through_retries(200).await;
    assert_eq!(c.hs.health.snapshot().cursor_hash, Some(c.hs.node.tip()));
    assert_eq!(c.hs.status(&c.w.cov(&create, 0)), "open");
    assert!(c.hs.node.vspc_log().iter().any(|r| r.2 >= 4 && r.3), "{:?}", c.hs.node.vspc_log());
    assert_no_repeated_failure(&c.hs.node);
}

#[tokio::test]
async fn the_matcher_gate_stays_closed_while_the_follower_pages() {
    let c = Ctx::new();
    for i in 0..30 {
        c.hs.node.push_block((0..20).map(|j| noise_tx(i * 100 + j, false)).collect());
    }
    c.hs.node.set_link_txs(Some(50));
    // the health the matcher's gate reads: never caught up before the cursor reaches the sink
    for _ in 0..500 {
        let o = c.hs.follower.step().await;
        let h = c.hs.health.snapshot();
        if h.cursor_hash != Some(c.hs.node.tip()) {
            assert!(!h.caught_up(), "caught up while {:?} blue behind the sink: {o:?}", h.lag_blue);
        }
        if matches!(o, StepOutcome::Idle) {
            break;
        }
    }
    assert_eq!(c.hs.health.snapshot().cursor_hash, Some(c.hs.node.tip()));
    assert!(c.hs.health.snapshot().caught_up());
}

#[tokio::test]
async fn property_incremental_equals_fresh_and_log_replay_through_a_slow_link() {
    let mut total = [0i64; 8];
    for seed in [11u64, 12, 13, 14] {
        let s = run_property(seed, 45, Family::Kcc20, Some(3 + (seed as usize % 4)), 1).await;
        for (t, v) in total.iter_mut().zip(s) {
            *t += v;
        }
    }
    assert!(total[0] > 0 && total[5] > 0, "fills and reorgs under a slow link: {total:?}");
}

// ---------------------------------------------------------------------------------------------
// parallel fetch (`fetch_parallel` > 1): windows fetched ahead over several connections, applied in chain order

/// A harness whose follower fetches `parallel` windows at once, planned from any lag, `initial` chain blocks each to
/// start, holding at most `max_bytes` ahead of the cursor.
fn parallel_harness(parallel: usize, initial: usize, max_bytes: u64, log: Option<RecordLog>) -> Harness {
    Harness::with_follower(MockNode::new("testnet-10"), open_memory("testnet-10").unwrap(), log, wide_window(), processor(), |f| {
        // the single-step window starts at the same size (a mock chain block is one blue score)
        f.batch_initial_blue = initial as u64;
        f.batch_target = std::time::Duration::from_millis(40);
        f.fetch_parallel = parallel;
        f.prefetch_min_lag_blue = 0;
        f.prefetch_initial_blocks = initial;
        f.prefetch_max_bytes = max_bytes;
    })
}

/// `blocks` chain blocks of `per_block` noise transactions each, with an order placed every tenth block.
fn backlog(c: &Ctx, blocks: u64, per_block: u64) -> Vec<SignedTx> {
    let mut creates = vec![];
    for i in 0..blocks {
        let mut block: Vec<_> = (0..per_block).map(|j| noise_tx(i * 1_000 + j, j % 3 == 0)).collect();
        if i % 10 == 0 {
            let create = small_ask(c, i as i64);
            c.w.note_daa(&[&create], c.daa() + 1);
            block.push(wire_tx(&create.tx));
            creates.push(create);
        }
        c.hs.node.push_block(block);
    }
    creates
}

/// Step to the sink, recording the cursor after every step; panics on a gap.
async fn sync_recording(hs: &Harness, max_steps: usize) -> Vec<Hash32> {
    let mut cursors = vec![];
    for _ in 0..max_steps {
        let o = hs.follower.step().await;
        assert!(!matches!(o, StepOutcome::Gap(_)), "gap: {o:?}");
        if let Some(c) = hs.health.snapshot().cursor_hash {
            cursors.push(c);
        }
        if matches!(o, StepOutcome::Idle) {
            return cursors;
        }
    }
    panic!("the follower did not reach the sink in {max_steps} steps");
}

#[tokio::test]
async fn parallel_fetch_applies_windows_in_chain_order_and_equals_a_clean_replay() {
    let dir = tempfile::tempdir().unwrap();
    let log_dir = dir.path().join("records");
    let (log, _) = RecordLog::open(&log_dir, 1 << 22, 0).unwrap();
    let c = Ctx::with(parallel_harness(4, 8, 256 << 20, Some(log)));
    let creates = backlog(&c, 200, 20);
    // a remote link: 2 ms per answer plus 40 us per transaction on each connection; only overlapping requests on
    // several connections carry more
    let link = (std::time::Duration::from_millis(2), std::time::Duration::from_micros(40));
    c.hs.node.set_vspc_link(link.0, link.1);
    let t0 = std::time::Instant::now();
    let cursors = sync_recording(&c.hs, 1_000).await;
    let took = t0.elapsed();
    let chain = c.hs.node.chain_hashes();
    let h = c.hs.health.snapshot();
    assert_eq!(h.cursor_hash, Some(c.hs.node.tip()));
    assert_eq!(h.state, FollowerState::Following);
    // strictly in chain order: every cursor is a chain block further along than the one before (or the same, idle)
    let pos: Vec<usize> = cursors.iter().map(|c| chain.iter().position(|b| b == c).expect("a chain block")).collect();
    assert!(pos.windows(2).all(|w| w[0] <= w[1]), "the cursor went back: {pos:?}");
    assert!(pos.len() >= 10, "it paged in windows: {pos:?}");
    // concurrently, over several connections
    assert!(c.hs.node.vspc_max_in_flight() >= 3, "max in flight {}", c.hs.node.vspc_max_in_flight());
    assert!(h.prefetch_bytes_peak > 0 && h.prefetch_window_blocks >= 1, "{h:?}");
    assert_eq!(h.prefetch_discards_total, 0, "no reorg, nothing discarded: {h:?}");
    // every accepted transaction exactly once, every block once
    assert_eq!(h.txs_fetched_total, 200 * 20 + creates.len() as u64, "{h:?}");
    assert_eq!(h.blocks_applied_total, 200);
    assert_eq!(c.hs.query::<i64>("SELECT COUNT(*) FROM orders", []), creates.len() as i64);
    // the same rows as a clean replay of the chain and as a rebuild from the record log
    let snap = c.hs.snapshot();
    assert_eq!(snap, fresh_snapshot(&c.hs.node.chain()));
    let log = kob_executor::indexer::recordlog::read_all(&log_dir).unwrap();
    let mut ing = Ingest::new(open_memory("testnet-10").unwrap(), processor(), None).with_config(wide_window());
    ing.init_cursor(&kob_executor::indexer::ingest::Cursor { hash: log.records[0].1.start, daa: 0 }).unwrap();
    ing.replay(log.records).unwrap();
    assert_eq!(snapshot(ing.conn()), snap);

    // the same backlog one window at a time: the same rows, and slower by about the overlap
    let c1 = Ctx::with(parallel_harness(1, 8, 256 << 20, None));
    let _ = backlog(&c1, 200, 20);
    c1.hs.node.set_vspc_link(link.0, link.1);
    let t1 = std::time::Instant::now();
    c1.hs.sync_through_retries(1_000).await;
    let took1 = t1.elapsed();
    assert_eq!(c1.hs.node.vspc_max_in_flight(), 1);
    assert_eq!(c1.hs.snapshot(), snap);
    println!("4,000 transactions behind a link of 40 us per transaction: 4 parallel windows {took:?}, one at a time {took1:?}");
    assert!(took < took1, "parallel {took:?} vs one at a time {took1:?}");
}

#[tokio::test]
async fn a_reorg_discards_the_windows_fetched_past_the_fork_and_no_orphan_is_applied() {
    let c = Ctx::with(parallel_harness(4, 4, 256 << 20, None));
    let creates = backlog(&c, 60, 5);
    let old_chain = c.hs.node.chain_hashes();
    // the chain after block 30 is replaced (with an order of its own) while the first windows are in flight: the first
    // window was fetched from the old chain, the later ones see the new one
    let late = small_ask(&c, 999);
    c.w.note_daa(&[&late], c.daa() - 30 + 1);
    let mut replacement: Vec<Vec<_>> = (0..35).map(|i| vec![noise_tx(50_000 + i, false)]).collect();
    replacement[0].push(wire_tx(&late.tx));
    c.hs.node.reorg_at_vspc_call(2, 30, replacement);
    c.hs.node.set_vspc_delay(std::time::Duration::from_millis(5));
    let cursors = sync_recording(&c.hs, 1_000).await;
    let h = c.hs.health.snapshot();
    assert_eq!(h.cursor_hash, Some(c.hs.node.tip()));
    assert!(h.prefetch_discards_total >= 1, "the windows past the fork were dropped: {h:?}");
    // nothing of the old branch was ever applied, so nothing had to be reverted
    assert_eq!(h.reverted_blocks_total, 0, "{h:?}");
    assert_eq!(h.reorgs_total, 0, "{h:?}");
    for orphan in &old_chain[31..] {
        assert!(!cursors.contains(orphan), "the cursor stood on an orphaned block {orphan}");
    }
    // the orders of the surviving chain and the new branch, as a clean replay of the new chain
    let survivors = creates
        .iter()
        .filter(|t| c.hs.query::<i64>("SELECT COUNT(*) FROM orders WHERE genesis_txid = ?1", [&t.tx.id[..]]) == 1)
        .count();
    assert_eq!(survivors, 3, "the orders placed in blocks 0, 10 and 20 survive the fork after block 30");
    assert_eq!(c.hs.status(&c.w.cov(&late, 0)), "open");
    assert_eq!(c.hs.snapshot(), fresh_snapshot(&c.hs.node.chain()));
}

#[tokio::test]
async fn a_reorg_after_windows_were_applied_is_reverted_as_by_the_single_step_follower() {
    let c = Ctx::with(parallel_harness(4, 3, 256 << 20, None));
    let _ = backlog(&c, 40, 4);
    c.hs.node.set_vspc_delay(std::time::Duration::from_millis(2));
    // a few windows in, the tail of the chain (some of it already applied) is replaced
    for _ in 0..3 {
        c.hs.follower.step().await;
    }
    let late = small_ask(&c, 777);
    c.w.note_daa(&[&late], c.daa() - 38 + 1);
    let mut replacement: Vec<Vec<_>> = (0..45).map(|i| vec![noise_tx(60_000 + i, i % 2 == 0)]).collect();
    replacement[0].push(wire_tx(&late.tx));
    c.hs.node.reorg(38, replacement);
    sync_recording(&c.hs, 1_000).await;
    assert_eq!(c.hs.health.snapshot().cursor_hash, Some(c.hs.node.tip()));
    assert_eq!(c.hs.status(&c.w.cov(&late, 0)), "open");
    assert_eq!(c.hs.snapshot(), fresh_snapshot(&c.hs.node.chain()));
}

#[tokio::test]
async fn prefetched_bytes_stay_within_the_budget() {
    // uniform blocks of 20 transactions; a budget of about three windows of four blocks, eight connections
    let probe = Ctx::with(parallel_harness(1, 4, 256 << 20, None));
    for i in 0..8u64 {
        probe.hs.node.push_block((0..20).map(|j| noise_tx(i * 100 + j, false)).collect());
    }
    probe.hs.sync().await;
    let per_block = probe.hs.health.snapshot().wire_bytes_total / 8;
    let budget = 3 * 4 * per_block + per_block / 2;

    let c = Ctx::with(parallel_harness(8, 4, budget, None));
    for i in 0..200u64 {
        c.hs.node.push_block((0..20).map(|j| noise_tx(i * 100 + j, false)).collect());
    }
    c.hs.node.set_vspc_delay(std::time::Duration::from_millis(5));
    // the window would grow on fast fetches; the budget caps it so that 8 windows would fit, and the planner then
    // holds no more than the budget
    sync_recording(&c.hs, 2_000).await;
    let h = c.hs.health.snapshot();
    assert_eq!(h.cursor_hash, Some(c.hs.node.tip()));
    assert!(h.prefetch_bytes_peak <= budget, "peak {} over the budget {budget}", h.prefetch_bytes_peak);
    assert!(h.prefetch_bytes_peak >= 4 * per_block, "it did prefetch: {h:?}");
    assert!(c.hs.node.vspc_max_in_flight() >= 2, "{}", c.hs.node.vspc_max_in_flight());
    assert_eq!(h.txs_fetched_total, 200 * 20);
    assert_eq!(c.hs.snapshot(), fresh_snapshot(&c.hs.node.chain()));
}

#[tokio::test]
async fn prefetch_holds_its_budget_when_windows_come_back_two_to_three_times_larger_than_estimated() {
    // the soak's flood: the estimate learnt on calm blocks, then blocks of 2x and 3x the size (TN10 10-02: 341 MB held
    // against a 268 MB budget). The bound must hold whether the transport refuses an oversized answer (the websocket
    // client) or the source ignores the limit and the prefetch drops the answer itself.
    let probe = Ctx::with(parallel_harness(1, 4, 256 << 20, None));
    for i in 0..8u64 {
        probe.hs.node.push_block((0..8).map(|j| noise_tx(i * 100 + j, false)).collect());
    }
    probe.hs.sync().await;
    let small = probe.hs.health.snapshot().wire_bytes_total / 8;
    // about three windows of four calm blocks
    let budget = 3 * 4 * small + small / 2;
    for ignore in [false, true] {
        let c = Ctx::with(parallel_harness(4, 4, budget, None));
        let mut txs = 0u64;
        for i in 0..160u64 {
            let n = if i < 30 {
                8
            } else if i % 2 == 0 {
                16
            } else {
                24
            };
            c.hs.node.push_block((0..n).map(|j| noise_tx(i * 100 + j, j % 3 == 0)).collect());
            txs += n;
        }
        c.hs.node.set_ignore_size_limit(ignore);
        c.hs.node.set_vspc_delay(std::time::Duration::from_millis(3));
        sync_recording(&c.hs, 5_000).await;
        let h = c.hs.health.snapshot();
        assert_eq!(h.cursor_hash, Some(c.hs.node.tip()), "ignore {ignore}");
        assert!(h.prefetch_bytes_peak <= budget, "ignore {ignore}: peak {} over the budget {budget}: {h:?}", h.prefetch_bytes_peak);
        assert!(h.prefetch_oversize_total >= 1, "ignore {ignore}: larger windows than estimated were refused: {h:?}");
        assert_eq!(c.hs.node.too_large() >= 1, !ignore, "ignore {ignore}: the transport refused them unless told not to");
        assert!(h.prefetch_bytes_peak >= 4 * small, "ignore {ignore}: it did prefetch: {h:?}");
        assert!(c.hs.node.vspc_max_in_flight() >= 2, "ignore {ignore}: {}", c.hs.node.vspc_max_in_flight());
        // every transaction exactly once, the same rows as a clean replay
        assert_eq!(h.txs_fetched_total, txs, "ignore {ignore}: {h:?}");
        assert_eq!(c.hs.snapshot(), fresh_snapshot(&c.hs.node.chain()), "ignore {ignore}");
    }
}

#[tokio::test]
async fn parallel_windows_that_time_out_are_split_and_the_backlog_still_arrives_exactly_once() {
    let c = Ctx::with(parallel_harness(4, 16, 256 << 20, None));
    let creates = backlog(&c, 80, 40);
    // a link of 150 transactions per request: a 16-block window (640) never arrives, 2 blocks do
    c.hs.node.set_link_txs(Some(150));
    c.hs.sync_through_retries(2_000).await;
    let h = c.hs.health.snapshot();
    assert_eq!(h.cursor_hash, Some(c.hs.node.tip()));
    assert!(h.vspc_timeouts_total >= 1, "{h:?}");
    assert!(h.prefetch_window_blocks <= 4, "the window shrank: {h:?}");
    assert_eq!(h.txs_fetched_total, 80 * 40 + creates.len() as u64, "{h:?}");
    assert_eq!(c.hs.snapshot(), fresh_snapshot(&c.hs.node.chain()));
}

#[tokio::test]
async fn property_incremental_equals_fresh_and_log_replay_with_parallel_fetch() {
    let mut total = [0i64; 8];
    for seed in [21u64, 22, 23, 24, 25, 26] {
        let link = (seed % 2 == 0).then_some(3 + (seed as usize % 4));
        let s = run_property(seed, 45, Family::Kcc20, link, 4).await;
        for (t, v) in total.iter_mut().zip(s) {
            *t += v;
        }
    }
    assert!(total[0] > 0 && total[5] > 0, "fills and reorgs with parallel fetch: {total:?}");
}
