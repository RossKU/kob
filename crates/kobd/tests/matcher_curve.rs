//! Tests `KobCurveAsk` in the matcher (`docs/design/curve-ask-spec.md`). The tests cover:
//!
//! * One fill per curve per transaction over at most 8 levels, each crossed level at least its
//!   price, levels in table order, and the curve's minimum fill over the whole fill.
//! * A larger buy chained over several transactions, and a bid of each delivery mode.
//! * Keepers and x402 quotes leave curves alone.
//!
//! Every transaction is engine-validated.

#[path = "matcher_common/mod.rs"]
mod common;

use std::collections::BTreeSet;

use common::*;
use kob_protocol::state::*;
use kobd::matcher::book::{Clock, ListedOrder, MemoryBook};
use kobd::matcher::engine::{tick, TickReport};
use kobd::matcher::family::Families;

const P265: i64 = 265_000_000;
const P270: i64 = 270_000_000;

/// A curve of maker `maker`: one level per price of `table`, one whole token each, `left` base units left.
fn curve(maker: u8, table: &[i64], left: i64) -> CurveAskState {
    let (h, p, s) = tok_fields(T3);
    let total = table.len() as i64 * WHOLE;
    CurveAskState {
        maker: pk(maker),
        token_cov_id: TOKEN,
        token_tpl_hash: h,
        tpl_prefix_len: p,
        tpl_suffix_len: s,
        scale: SCALE,
        min_fill: 1,
        tip: 0,
        active_from: 0,
        level_size: WHOLE,
        total,
        amount_left: left,
        extension_commitment: EXT,
        table: table.to_vec(),
    }
}

fn l_curve(id: u32, s: CurveAskState) -> ListedOrder {
    listed(cid(id), AnyState::KobCurveAsk(s), CARRIER, 1_000)
}

/// A tick (no KRON twin: a curve is KCC-20 only), each transaction checked by the common invariants.
fn run_curve(orders: Vec<ListedOrder>) -> TickReport {
    let inp = input(&book(orders));
    let r = tick(&inp, &cfg(), &Families::default(), &signer());
    assert!(r.anomalies.is_empty(), "{:?}", r.anomalies);
    for p in &r.prepared {
        assert!(p.validation.is_some(), "unvalidated transaction");
        check_invariants(p, &inp);
    }
    assert_no_budget_slack();
    r
}

/// The curve's fills: (step, base units) of every prepared transaction, and the curve legs of each.
fn curve_fills(r: &TickReport, id: [u8; 32]) -> Vec<(usize, i64)> {
    let mut v = vec![];
    for p in &r.prepared {
        let legs = p.plan.fills.iter().filter(|f| f.cand.id == id).count();
        assert!(legs <= 1, "one leg per curve per transaction");
        let n = p.plan.amount_of(&id);
        if n > 0 {
            v.push((p.step, n));
        }
    }
    v
}

/// What the curve's maker gets for a fill of `n` from `s`, along its table.
fn proceeds(s: &CurveAskState, n: i64) -> i64 {
    s.proceeds(n).expect("within 8 levels")
}

#[test]
fn a_bid_takes_the_levels_up_to_its_price_in_one_leg() {
    let c = curve(1, &[P250, P255, P260, P265, P270], 5 * WHOLE);
    let r = run_curve(vec![l_bid(2, bid(2, P260, T3), 10 * WHOLE), l_curve(1, c.clone())]);
    assert_eq!(curve_fills(&r, cid(1)), vec![(0, 3 * WHOLE)], "levels 0 to 2, not P265: {:?}", r.skipped);
    let p = &r.prepared[0];
    // the matcher earns the bid's all-in spend less the curve's proceeds, level by level
    assert_eq!(p.plan.margin, 3 * (P260 + TIP) - proceeds(&c, 3 * WHOLE));
    assert_eq!(proceeds(&c, 3 * WHOLE), P250 + P255 + P260);
    // the maker's payout at the curve's input index
    let i = p.signed.tx.inputs.iter().position(|x| x.utxo.covenant_id == Some(cid(1))).expect("the curve input");
    assert_eq!(p.signed.tx.outputs[i].value as i64, proceeds(&c, 3 * WHOLE));
}

#[test]
fn a_cross_stops_at_a_level_boundary() {
    // half of level 0 sold: a bid just below level 1 takes the rest of level 0, one at level 1's price takes level 1 too
    let c = curve(1, &[P250, P260, P270], 3 * WHOLE - WHOLE / 2);
    // (all in: the bid's price and tip against the level's price)
    let b = |p: i64| BidState { min_fill: 1, ..bid(2, p, T3) };
    let r = run_curve(vec![l_bid(2, b(P260 - TIP - 1), 5 * WHOLE), l_curve(1, c.clone())]);
    assert_eq!(curve_fills(&r, cid(1)), vec![(0, WHOLE / 2)]);
    let r = run_curve(vec![l_bid(2, b(P260 - TIP), 5 * WHOLE), l_curve(1, c)]);
    assert_eq!(curve_fills(&r, cid(1)), vec![(0, WHOLE / 2 + WHOLE)], "a level at exactly the bid's price crosses");
}

#[test]
fn levels_fill_in_table_order_only() {
    // a cheaper level behind a dearer one is never taken before it
    let r = run_curve(vec![l_bid(2, bid(2, P255, T3), 5 * WHOLE), l_curve(1, curve(1, &[P260, P245, P245], 3 * WHOLE))]);
    assert!(curve_fills(&r, cid(1)).is_empty());
    // a dear first level is taken alone rather than skipped for the cheap one behind it
    let one = BidState { min_fill: 1, ..bid(2, P265, T3) };
    let r = run_curve(vec![l_bid(2, one, WHOLE), l_curve(1, curve(1, &[P260, P245], 2 * WHOLE))]);
    assert_eq!(curve_fills(&r, cid(1)), vec![(0, WHOLE)]);
    // the same levels in order cross
    let r = run_curve(vec![l_bid(2, bid(2, P255, T3), 5 * WHOLE), l_curve(1, curve(1, &[P245, P245, P260], 3 * WHOLE))]);
    assert_eq!(curve_fills(&r, cid(1)), vec![(0, 2 * WHOLE)]);
}

#[test]
fn eight_levels_per_fill_and_a_larger_buy_is_chained() {
    let c = curve(1, &[P250; 12], 12 * WHOLE);
    // the bid funds two deliveries
    let b = bid(2, P260, T3);
    let funded = listed(cid(2), AnyState::KobBid(b.clone()), b.escrow(12 * WHOLE, 2).expect("escrow") as u64, 1_000);
    let r = run_curve(vec![funded, l_curve(1, c)]);
    let fills = curve_fills(&r, cid(1));
    assert_eq!(fills[0], (0, 8 * WHOLE), "8 levels in the first transaction: {fills:?}");
    assert_eq!(fills.iter().map(|f| f.1).sum::<i64>(), 12 * WHOLE, "the rest in the chained ones: {fills:?}");
    assert!(fills.iter().skip(1).all(|f| f.0 > 0 && f.1 <= 8 * WHOLE));
    // the chained transaction spends the first one's continuation of the curve
    let second = r.prepared.iter().find(|p| p.step > 0 && p.plan.amount_of(&cid(1)) > 0).expect("a chained fill");
    let first = &r.prepared[0];
    assert!(second.signed.tx.inputs.iter().any(|i| i.utxo.covenant_id == Some(cid(1)) && i.transaction_id == first.txid()));
}

#[test]
fn a_fill_inside_one_level_and_the_sell_out() {
    // 3.5 levels left; a bid for all of it at a high price sells the curve out in one fill
    let c = curve(1, &[P245, P245, P250, P250], 3 * WHOLE + WHOLE / 2);
    let r = run_curve(vec![l_bid(2, bid(2, P260, T3), 10 * WHOLE), l_curve(1, c.clone())]);
    assert_eq!(curve_fills(&r, cid(1)), vec![(0, c.amount_left)]);
    let tx = &r.prepared[0].signed.tx;
    assert!(!tx.outputs.iter().any(|o| o.covenant.as_ref().is_some_and(|x| x.covenant_id == cid(1))), "sold out: no continuation");
    // a bid for less than a level
    let r = run_curve(vec![l_bid(2, bid(2, P260, T3), WHOLE / 4), l_curve(1, c)]);
    assert_eq!(curve_fills(&r, cid(1)), vec![(0, WHOLE / 4)]);
}

#[test]
fn the_minimum_fill_holds_over_the_whole_fill() {
    // half a level left in level 0, minFill two whole tokens: a fill reaches into the next levels or does not happen
    let c = CurveAskState { min_fill: 2 * WHOLE, ..curve(1, &[P250, P250, P250, P250], 4 * WHOLE - WHOLE / 2) };
    let r = run_curve(vec![l_bid(2, bid(2, P260, T3), 5 * WHOLE), l_curve(1, c.clone())]);
    assert_eq!(curve_fills(&r, cid(1)), vec![(0, c.amount_left)]);
    let r = run_curve(vec![l_bid(2, BidState { min_fill: 1, ..bid(2, P260, T3) }, WHOLE), l_curve(1, c)]);
    assert!(curve_fills(&r, cid(1)).is_empty(), "one whole token is below the curve's minimum fill");
}

#[test]
fn a_bid_of_each_delivery_mode_crosses_a_curve() {
    let c = || l_curve(1, curve(1, &[P250, P250, P255, P270], 4 * WHOLE));
    let bids = vec![
        ("mode 0", l_bid(2, bid(2, P260, T3), 3 * WHOLE)),
        ("mode 1", m1_bid(2, 2, P260, 3 * WHOLE, 0)),
        ("mode 1 holding", m1_bid(2, 2, P260, 3 * WHOLE, 2 * WHOLE)),
        ("mode 2", m2_bid(2, 2, P260, 3 * WHOLE, WHOLE, default_tip(), vec![receive(2, 5 * WHOLE, WHOLE)])),
    ];
    for (name, b) in bids {
        let r = run_curve(vec![b, c()]);
        assert_eq!(curve_fills(&r, cid(1)), vec![(0, 3 * WHOLE)], "{name}: {:?}", r.skipped);
        assert_eq!(r.prepared[0].plan.amount_of(&cid(2)), 3 * WHOLE, "{name}");
    }
}

#[test]
fn an_unbound_curve_takes_only_the_custody_of_its_genesis() {
    let s = CurveAskState { token_cov_id: [0; 32], ..curve(1, &[P250, P255], 2 * WHOLE) };
    let mut o = l_curve(1, s);
    // a custody from another transaction: not offered
    let r = run_curve(vec![l_bid(2, bid(2, P260, T3), 2 * WHOLE), o.clone()]);
    assert!(curve_fills(&r, cid(1)).is_empty());
    // the genesis' own custody: filled, and the continuation is bound
    let k = o.custody.as_mut().unwrap();
    (k.utxo.transaction_id, k.utxo.index) = (o.order.utxo.transaction_id, 1);
    let r = run_curve(vec![l_bid(2, bid(2, P260, T3), WHOLE), o]);
    assert_eq!(curve_fills(&r, cid(1)), vec![(0, WHOLE)]);
    let bound = CurveAskState { amount_left: WHOLE, ..curve(1, &[P250, P255], 2 * WHOLE) };
    let spk = kob_protocol::tx::spk_to_string(&bound.spk());
    assert!(r.prepared[0].signed.tx.outputs.iter().any(|x| x.script_public_key == spk), "the continuation names the token");
}

#[test]
fn keepers_leave_a_curve_alone() {
    use kobd::keepers::{KeeperConfig, KeeperInput};
    // the operator's own curve, old and with a stray: no refund, kill, close or sweep
    let mut o = l_curve(1, curve(MATCHER, &[P250, P255], 2 * WHOLE));
    o.order.utxo.block_daa_score = 10;
    o.strays = vec![custody(WHOLE, cid(1), 1_500)];
    let inp = KeeperInput {
        orders: vec![o],
        clock: Clock { daa: NOW + 100_000_000, utc: UTC },
        funding: vec![funding(20 * KAS)],
        excluded: BTreeSet::new(),
        excluded_outpoints: BTreeSet::new(),
        known_unprofitable: BTreeSet::new(),
    };
    let r = kobd::keepers::tick(&inp, &KeeperConfig { sweep_own_strays: true, ..KeeperConfig::default() }, &signer());
    assert!(r.jobs.is_empty(), "{:?}", r.jobs.iter().map(|j| j.kind).collect::<Vec<_>>());
    // the same with a plain ask: swept (the rule above is the curve's)
    let mut a = l_ask(2, ask(MATCHER, P250, 5 * WHOLE, T3));
    a.strays = vec![custody(WHOLE, cid(2), 1_500)];
    let inp = KeeperInput { orders: vec![a], clock: Clock { daa: NOW, utc: UTC }, ..inp };
    let r = kobd::keepers::tick(&inp, &KeeperConfig { sweep_own_strays: true, ..KeeperConfig::default() }, &signer());
    assert_eq!(r.jobs.len(), 1);
}

#[test]
fn x402_quotes_ignore_curves() {
    use kobd::x402::quote::{quote_legs, QuoteRequest, Take};
    let req = QuoteRequest { token: TOKEN, take: Take::BuyFromAsks, amount: WHOLE, limit_price: None, exclude: vec![] };
    let only_curve = MemoryBook {
        daa_score: NOW + 5,
        orders: vec![l_curve(1, curve(1, &[P250, P255], 2 * WHOLE))],
        wallet_tokens: vec![],
        keeper_only: vec![],
    };
    assert!(quote_legs(&only_curve, &req, UTC).is_none(), "no curve leg is quoted");
    let with_ask = book(vec![l_curve(1, curve(1, &[P245], WHOLE)), l_ask(2, ask(2, P250, 5 * WHOLE, T3))]);
    let legs = quote_legs(&with_ask, &req, UTC).expect("the plain ask");
    let curve_out = kob_x402::chain::Outpoint::new(with_ask.orders[0].order.utxo.transaction_id, with_ask.orders[0].order.utxo.index);
    assert!(legs.iter().all(|l| l.outpoint() != curve_out));
}

#[test]
fn the_sanity_gate_bounds_what_the_rest_of_a_curve_pays() {
    // each level within the protocol's gate (below 2^62), eight of them past what any order may move
    let big = CurveAskState { scale: 1, level_size: 1 << 40, total: 8 << 40, amount_left: 8 << 40, ..curve(1, &[1 << 20; 8], 0) };
    assert!(AnyState::KobCurveAsk(big.clone()).check_numbers().is_ok());
    assert_eq!(kobd::sanity::check(&AnyState::KobCurveAsk(big.clone())), Err("bad_state:notional:overflow".into()));
    // half of it sold: within bounds
    let half = CurveAskState { amount_left: 1 << 40, ..big };
    assert_eq!(kobd::sanity::check(&AnyState::KobCurveAsk(half)), Ok(()));
    assert_eq!(kobd::sanity::check(&AnyState::KobCurveAsk(curve(1, &[P250, P255], 2 * WHOLE))), Ok(()));
}
