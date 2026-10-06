//! C5 liveness audit, indexer views: a trailing stop whose ratchet the indexer cannot derive loses its state.
//!
//! `processor::derive_after` proves a continuation by splicing candidate values into the spent script's mutable windows and
//! comparing the P2SH. For a trailing `update` the candidate stop prices are `stop +- k * trailStep` for the multiples `k` the
//! trigger evidence justifies PLUS a fixed `1..=64` (`trail_candidates`). The evidence prices come from `evidence_touch` over
//! `rec.inputs[i].reveal`, and the extractor only reveals inputs that are TRACKED orders (`record.rs`: `order_utxo_daa(..)`).
//! The covenant accepts ANY plain `KobBid` of the token as evidence (it checks the template, not a placement record), and
//! `update` is permissionless. So a ratchet of more than 64 steps next to an evidence bid the indexer never saw (a raw
//! covenant output without a KOB1 record, or any other order whose reveal was dropped) matches no candidate:
//! the continuation is stored with `state = NULL`, `order_state.state_known = 0`.
//!
//! Consequences (all checked below): the order is left out of `listed_orders` (matcher / keepers never see it), the view has no
//! `state` (`snapshotFromOrderView` throws `state-unknown`), and `export_orders` falls back to `o.genesis_state`
//! (`COALESCE(u.state, o.genesis_state)`), a script that no longer exists on chain, so a maker import verifies `not_found`.
//! The web node-only fallback (`records.ts` `candidates`) cannot enumerate `stopPrice` either ("trailing stops are not
//! enumerable"), so after such a ratchet no tool of the product can see or cancel the order from the indexer.

mod common;

use common::*;
use kob_executor::testkit::*;
use kob_protocol::build::*;
use kob_protocol::family::Family;
use kob_protocol::state::*;

/// A resting bid of MAKER_B created WITHOUT a placement record: a raw covenant output, a plain `KobBid` for the chain and for
/// the covenant's evidence rules, invisible to the indexer.
async fn unindexed_bid(c: &Ctx, price: i64) -> Resting {
    let state = AnyState::KobBid(bid(MAKER_B, price));
    let AnyState::KobBid(b) = &state else { unreachable!() };
    let escrow = b.escrow(10 * WHOLE, 3).unwrap() as u64;
    let req = c.w.create(state.clone(), escrow, MAKER_B, 0);
    let mut built = build(&Action::CreateOrder(req)).expect("build the bid");
    built.tx.payload.clear(); // no KOB1 placement record
    let create = c.w.resign(built, true);
    c.push(&[&create]).await;
    Resting { create, fam: Family::Kcc20, side: SIDE_BID, state }
}

const STEP: i64 = 100_000;
const GAP: i64 = 5_000_000;
const EVIDENCE: i64 = 235_000_000;

fn trailing() -> CondAskState {
    // stop 2.00, tp 3.00: a bid at 2.35 justifies (2.35 - 0.05 - 2.00) / 0.001 = 300 steps, far past the fixed 1..=64
    CondAskState { trail_step: STEP, trail_gap: GAP, trail_wait: 600, ..cond_ask(MAKER_A) }
}

/// Places the trailing stop and ratchets it next to `evidence` (`indexed`: with a placement record). Returns the order id and
/// the state the chain holds afterwards.
async fn ratchet(c: &Ctx, indexed: bool) -> (kob_executor::hex::Hash32, CondAskState) {
    let co = trailing();
    let create = c.w.create_tx(AnyState::KobCondAsk(co.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    let r = if indexed { resting(c, SIDE_BID, EVIDENCE).await } else { unindexed_bid(c, EVIDENCE).await };
    let order_daa = c.w.utxo(&create, 0).block_daa_score;
    let lock = rested(c, &r, co.min_rest_daa).max(order_daa + 600);
    let mut b = batch(&c.w, lock, vec![]);
    let k = r.add_to(c, &mut b, 5 * WHOLE);
    b.updates = vec![update(c.w.order(&create, 0, AnyState::KobCondAsk(co.clone())), k)];
    let trail = c.w.sign(&Action::Batch(b));
    c.push(&[&trail]).await;
    let steps = co.trail_steps(EVIDENCE);
    assert_eq!(steps, 300);
    (cov, CondAskState { stop_price: co.stop_price + steps * STEP, ..co })
}

#[tokio::test]
async fn a_ratchet_next_to_an_indexed_evidence_bid_keeps_its_proven_state() {
    // control (passes today): with the evidence order indexed the multiple k = 300 comes from its revealed price
    let c = Ctx::new();
    let (cov, expect) = ratchet(&c, true).await;
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobCondAsk(expect)));
    assert!(view(&c, &cov).state_known);
}

#[tokio::test]
async fn a_ratchet_next_to_an_unindexed_evidence_bid_keeps_its_proven_state() {
    // C5: expected to fail until derive_after reads evidence prices from every revealed plain KobBid / KobAsk input (the
    // template is pinned: reveal inputs by template, not only tracked orders), or enumerates k up to
    // (tpPrice - 1 - stop) / trailStep instead of 64.
    let c = Ctx::new();
    let (cov, expect) = ratchet(&c, false).await;
    let v = view(&c, &cov);
    assert!(v.state_known, "the trailing stop's state was lost: the maker's cancel cannot be built from the view");
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobCondAsk(expect)));
}

#[tokio::test]
async fn export_orders_never_emits_a_script_that_no_longer_exists() {
    // C5: expected to fail with the same cause; once a state IS unknown, `export_orders` must not fall back to the genesis
    // state (omit the order or flag it) because an import of that span can only report `not_found`.
    let c = Ctx::new();
    let (cov, expect) = ratchet(&c, false).await;
    let current = AnyState::KobCondAsk(expect).encode();
    let export = {
        let g = c.hs.ingest.lock().unwrap();
        kob_executor::recover::export_orders(g.conn(), Some(&pk(MAKER_A)[..]), true).unwrap()
    };
    let e = export.orders.iter().find(|e| e.covenant_id == Some(cov)).expect("the live order is exported");
    assert_eq!(e.state.0, current, "the export carries the CURRENT script's state (a maker's import is verified against the node)");
}
