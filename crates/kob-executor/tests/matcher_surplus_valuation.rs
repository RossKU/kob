//! The surplus-inventory valuation (`PlannerConfig::inventory`) and the accounting of what the operator keeps.
//!
//! * A plain KAS bid values a surplus only for an amount its own quantity rules accept (at least its minimum fill, or all
//!   it has left): a bid that could never take the surplus (its minimum fill is above it) does not value it, whatever it
//!   quotes, and `minAmount` never lowers the bound below that rule. Otherwise a bid quoting any price could make the
//!   operator sign batches whose KAS accounting is negative (it pays the whole fee) for a surplus worth almost nothing.
//! * The KAS the accounting credits to the operator's own token outputs (`operator_token_kas_out`, computed from the plan)
//!   equals what the built transaction puts on token outputs owned by the operator key, and a kept surplus is exactly the
//!   operator's token output.

#[path = "matcher_common/mod.rs"]
mod common;

use common::pair::*;
use common::*;
use kob_executor::matcher::book::ListedOrder;
use kob_executor::matcher::engine::{tick, EngineConfig, Prepared, TickReport};
use kob_executor::matcher::family::Families;
use kob_executor::matcher::planner::{InventoryPolicy, InventoryToken, UnitPrice};
use kob_protocol::artifacts::TemplateId;
use kob_protocol::state::*;
use kob_protocol::tx::SigPlan;

fn policy(tokens: Vec<InventoryToken>) -> EngineConfig {
    let mut c = cfg();
    c.planner.inventory = InventoryPolicy { accept_surplus_tokens: true, tokens, ..InventoryPolicy::default() };
    c
}

fn run_any(orders: Vec<ListedOrder>, cfg: &EngineConfig) -> TickReport {
    let b = book(orders);
    let inp = input(&b);
    let r = tick(&inp, cfg, &Families::default(), &signer());
    for p in &r.prepared {
        assert!(p.validation.is_some(), "every prepared transaction is engine-validated");
    }
    r
}

/// One token output of a built transaction: (token, output index, output value, owner, is_user, amount).
type TokenOut = ([u8; 32], usize, u64, [u8; 32], bool, i64);

/// Per token: the token outputs of a built transaction as (output index, output value, owner, is_user, amount), from the
/// token program's own `next_states` (in output order).
fn token_outputs(p: &Prepared) -> Vec<TokenOut> {
    let built = &p.lowered.built;
    let mut out = vec![];
    let mut done: Vec<[u8; 32]> = vec![];
    for (k, plan) in built.plans.iter().enumerate() {
        let Some(token) = built.tx.inputs[k].utxo.covenant_id else { continue };
        let states: Vec<TokenState> = match plan {
            SigPlan::TokenLeader { next_states, .. } => next_states.iter().cloned().map(TokenState::from).collect(),
            SigPlan::KronToken { next_states, .. } => next_states.iter().cloned().map(TokenState::from).collect(),
            _ => continue,
        };
        if done.contains(&token) {
            continue;
        }
        done.push(token);
        let idx: Vec<usize> = built
            .tx
            .outputs
            .iter()
            .enumerate()
            .filter(|(_, o)| o.covenant.as_ref().map(|c| c.covenant_id) == Some(token))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(idx.len(), states.len(), "one next state per output of the token");
        for (i, s) in idx.into_iter().zip(states) {
            out.push((token, i, built.tx.outputs[i].value, s.owner(), s.is_user(), s.amount()));
        }
    }
    out
}

/// Token outputs the BUILT transaction gives the operator key: (token, carrier, amount).
fn operator_token_outputs(p: &Prepared) -> Vec<([u8; 32], u64, i64)> {
    token_outputs(p).into_iter().filter(|x| x.4 && x.3 == pk(MATCHER)).map(|x| (x.0, x.2, x.5)).collect()
}

// ------------------------------------------------------------------------------------------------- valuation

/// 1 whole A (T8) sold by a pair ASK at RATE (1.000 B per A) to a pair BID at 1.001 B per A, untipped: the bid pays
/// floor(1000 * 1001 / 1000) = 1001 units of B, the ask needs exactly 1000: a netting surplus of ONE base unit of B
/// (0.001 B, about 0.002 KAS at the 2 KAS books).
fn one_unit_crossing() -> Vec<ListedOrder> {
    vec![l_pair(1, T8, T8, pask(1, T8, T8, WHOLE, RATE, 0), 1_000), l_pair(2, T8, T8, pbid(2, T8, T8, WHOLE, RATE + 1, 0), 1_000)]
}

/// A plain KAS bid of B quoting `price` sompi per whole B, minimum fill 2 base units, funded for 3 (so a fill of 1 unit
/// neither reaches its minimum fill nor ends it: the batch cannot sell it the 1-unit surplus).
fn b_bid(id: u32, price: i64) -> ListedOrder {
    let mut o = l_bid_b(id, T8, T8, 30, price, 3);
    if let AnyState::KobBid(s) = &mut o.order.state {
        s.min_fill = 2;
    }
    o
}

fn tusd(min_amount: Option<i64>) -> InventoryToken {
    InventoryToken { token: TOKEN_B, ref_price: None, min_amount, max_amount: None }
}

/// The KAS of a batch is never negative only because a bid that cannot take the surplus valued it.
fn no_unbacked_keep(r: &TickReport) {
    for p in &r.prepared {
        assert!(p.plan.kept.is_empty(), "a surplus kept on a bid that cannot take it: {:?} ({:?})", p.plan.kept, p.accounting);
    }
}

#[test]
fn a_bid_that_cannot_take_the_surplus_does_not_value_it() {
    // a bid at 1,000 KAS per whole B (500x the 2 KAS book), i.e. 1 KAS per base unit, minimum fill 2 base units
    let high = 100_000_000_000i64;
    let mut v = one_unit_crossing();
    v.push(b_bid(900, high));
    // a B book at 2 KAS (a resting ask with a wallet minimum fill: it never fills the 2-unit bid)
    let mut ask_b = l_ask_b(901, T8, T8, 31, P200, 5 * WHOLE);
    if let AnyState::KobAsk(s) = &mut ask_b.order.state {
        s.min_fill = WHOLE;
    }
    v.push(ask_b);

    // (1) `minAmount: 0`: the one-unit surplus is below the bid's minimum fill, so that bid does not value it and nothing is
    // kept (no batch pays the fee for a unit worth 0.002 KAS)
    let r = run_any(v.clone(), &policy(vec![tusd(Some(0))]));
    no_unbacked_keep(&r);
    assert!(r.prepared.iter().all(|p| p.accounting.profit >= 0), "{:?}", r.prepared.iter().map(|p| &p.accounting).collect::<Vec<_>>());

    // (2) the same book with the bid at the market (2 KAS per B): nothing either
    let mut market = one_unit_crossing();
    market.push(b_bid(900, P200));
    assert!(run_any(market, &policy(vec![tusd(Some(0))])).prepared.is_empty());

    // (3) the default minAmount (one valued bid's minimum fill): nothing
    assert!(run_any(v, &policy(vec![tusd(None)])).prepared.is_empty());

    // (4) TWAP pair orders (100 whole A, maxFill 1 whole, interval 10 DAA) netting one slice per interval, each slice leaving
    // the same one-unit surplus: no slice is built on the bid's valuation
    let twap = |id: u32, maker: u8, ask: bool, price: i64| {
        let mut s = pair_state(maker, ask, T8, T8, 100 * WHOLE, price, 0, TIF_GTC);
        s.max_fill = WHOLE;
        s.min_fill = WHOLE;
        s.interval = 10;
        if !ask {
            s.custody = s.bid_escrow(s.amount_left, 100).expect("escrow");
        }
        l_pair(id, T8, T8, s, 1_000)
    };
    let mut t = vec![twap(1, 1, true, RATE), twap(2, 2, false, RATE + 1), b_bid(900, high)];
    t.push(l_ask_b(901, T8, T8, 31, P200, 5 * WHOLE));
    let r = run_any(t, &policy(vec![tusd(Some(0))]));
    no_unbacked_keep(&r);
    assert!(r.prepared.iter().all(|p| p.accounting.profit >= 0));

    // (5) the owner's reference price still values the same unit (the owner's own valuation, not a bid's)
    let rp =
        InventoryToken { token: TOKEN_B, ref_price: Some(UnitPrice { sompi: 2 * KAS, per: 1 }), min_amount: None, max_amount: None };
    let r = run_any(one_unit_crossing(), &policy(vec![rp]));
    assert_eq!(r.prepared.len(), 1, "skipped: {:?} anomalies {:?}", r.skipped, r.anomalies);
    assert_eq!(r.prepared[0].plan.kept.iter().map(|k| (k.token, k.amount)).collect::<Vec<_>>(), vec![(TOKEN_B, 1)]);
    assert_eq!(operator_token_outputs(&r.prepared[0]), vec![(TOKEN_B, KEEP_CARRIER_DEFAULT, 1)]);
}

const KEEP_CARRIER_DEFAULT: u64 = kob_executor::matcher::planner::KEEP_CARRIER;

/// A surplus at least the bid's minimum fill is sold to that bid in the same transaction (the bid pays for it), not kept.
#[test]
fn a_surplus_the_bid_can_take_is_sold_to_it() {
    // 1 whole A at 1.002: a 2-unit surplus = the bid's minimum fill
    let mut v =
        vec![l_pair(1, T8, T8, pask(1, T8, T8, WHOLE, RATE, 0), 1_000), l_pair(2, T8, T8, pbid(2, T8, T8, WHOLE, RATE + 2, 0), 1_000)];
    v.push(b_bid(900, 100_000_000_000));
    for min_amount in [None, Some(0)] {
        let r = run_any(v.clone(), &policy(vec![tusd(min_amount)]));
        let sold: i64 = r.prepared.iter().map(|p| p.plan.amount_of(&cid(900))).sum();
        let kept: i64 = r.prepared.iter().flat_map(|p| p.plan.kept.iter()).map(|k| k.amount).sum();
        assert!(sold == 2 && kept == 0, "minAmount {min_amount:?}: sold {sold}, kept {kept}");
    }
}

// ------------------------------------------------------------------------------------------------- accounting

/// Accounting vs the built transaction, over random pair books with the inventory policy on for both tokens (bid-valued
/// with `minAmount` 0, or a `refPrice`), every pair kind, both KAS books, KCC-20 and KRON.
#[test]
fn operator_token_accounting_matches_the_built_transaction() {
    let iters: u64 = std::env::var("KOB_SURPLUS_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(60);
    let mut seed: u64 = 0x1234_5678_9abc_def1;
    let mut next = |m: u64| {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (seed >> 33) % m.max(1)
    };
    let mut built = 0usize;
    let mut kept_txs = 0usize;
    let mut mismatches: Vec<String> = vec![];
    for it in 0..iters {
        let (pa, pb) = match next(4) {
            0 => (T8, T8),
            1 => (T3, T8),
            2 => (TemplateId::KronToken2433, TemplateId::KronToken2433),
            _ => (T8, TemplateId::KronToken2433),
        };
        let (ta, tb) = (token_a(pa), token_b(pa, pb));
        let mut v: Vec<ListedOrder> = vec![];
        let mut id = 1u32;
        for _ in 0..(1 + next(4)) {
            let ask = next(2) == 0;
            let s = pair_state(
                id as u8,
                ask,
                pa,
                pb,
                (1 + next(6) as i64) * WHOLE + next(3) as i64 * 7,
                900 + next(400) as i64,
                PTIP * next(2) as i64,
                TIF_GTC,
            );
            v.push(l_pair(id, pa, pb, s, 1_000));
            id += 1;
        }
        for _ in 0..next(3) {
            let ask = next(2) == 0;
            let (tp, stop) = if ask {
                (1_100 + next(300) as i64, 900 + next(200) as i64)
            } else {
                (800 + next(200) as i64, 1_000 + next(300) as i64)
            };
            let mut c = cond_pair(id as u8, ask, pa, pb, (1 + next(4) as i64) * WHOLE, if next(3) == 0 { 0 } else { tp }, stop);
            if next(4) == 0 {
                c.armed = 1;
            }
            v.push(l_cond_pair(id, pa, pb, c, 1_000));
            id += 1;
        }
        for _ in 0..next(2) {
            let buy = next(2) == 0;
            let mut e = ifd_pair(id as u8, buy, pa, pb, (1 + next(4) as i64) * WHOLE, 950 + next(150) as i64);
            if next(3) == 0 {
                e.rpt_amount = 1 + 10 * WHOLE;
            }
            e.tip = PTIP;
            e.custody = e.b_custody_needed().unwrap();
            v.push(l_ifd_pair(id, pa, pb, e, 1_000));
            id += 1;
        }
        for k in 0..next(3) {
            let p = 180_000_000 + next(80) as i64 * 1_000_000;
            v.push(l_ask_a(500 + k as u32, pa, 40, p, (1 + next(5) as i64) * WHOLE));
            v.push(l_bid_a(520 + k as u32, pa, 41, p - 10_000_000 + next(30) as i64 * 1_000_000, (1 + next(5) as i64) * WHOLE));
            let q = 180_000_000 + next(80) as i64 * 1_000_000;
            v.push(l_ask_b(540 + k as u32, pa, pb, 42, q, (1 + next(5) as i64) * WHOLE));
            v.push(l_bid_b(560 + k as u32, pa, pb, 43, q - 10_000_000 + next(30) as i64 * 1_000_000, (1 + next(5) as i64) * WHOLE));
        }
        let rp = |t: [u8; 32]| InventoryToken {
            token: t,
            ref_price: Some(UnitPrice { sompi: 2 * KAS, per: WHOLE as u64 }),
            min_amount: None,
            max_amount: None,
        };
        let pol = match next(3) {
            0 => policy(vec![
                InventoryToken { token: ta, ref_price: None, min_amount: Some(0), max_amount: None },
                InventoryToken { token: tb, ref_price: None, min_amount: Some(0), max_amount: None },
            ]),
            1 => policy(vec![rp(ta), rp(tb)]),
            _ => cfg(),
        };
        let r = run_any(v, &pol);
        for p in &r.prepared {
            built += 1;
            let ops = operator_token_outputs(p);
            let carriers: u64 = ops.iter().map(|x| x.1).sum();
            if carriers != p.lowered.operator_token_kas_out {
                mismatches.push(format!(
                    "it {it} ({} / {}): the accounting credits {} sompi of operator token carriers, the transaction holds {} ({:?})",
                    pa.name(),
                    pb.name(),
                    p.lowered.operator_token_kas_out,
                    carriers,
                    ops
                ));
            }
            if !p.plan.kept.is_empty() {
                kept_txs += 1;
            }
            for k in &p.plan.kept {
                let got: i64 = ops.iter().filter(|x| x.0 == k.token).map(|x| x.2).sum();
                if got != k.amount {
                    mismatches
                        .push(format!("it {it}: kept {} of {:?} planned, {} delivered to the operator", k.amount, k.token[0], got));
                }
            }
            for x in &ops {
                if !p.plan.kept.iter().any(|k| k.token == x.0) {
                    mismatches.push(format!("it {it}: an operator token output of an unkept token {:?}: {:?}", x.0[0], x));
                }
            }
            assert!(p.accounting.profit.saturating_add(p.plan.kept_value()) >= 0);
        }
    }
    eprintln!("accounting: {built} transactions, {kept_txs} keep a surplus; mismatches: {}", mismatches.len());
    for m in &mismatches {
        eprintln!("  {m}");
    }
    assert!(built > 0);
    assert!(mismatches.is_empty(), "{} accounting mismatches", mismatches.len());
}
