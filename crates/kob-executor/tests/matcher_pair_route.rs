//! Pair orders routed through the KAS books in the matcher's global batch (`matcher::batch`, `matcher::pair`) on in-memory
//! books: the port of the retired cross limit suite to `KobPair` orders, both sides. A pair ASK of A for B is filled through
//! plain KAS bids of A and plain KAS asks of B in one transaction, a pair BID through plain KAS bids of B (its B sold) and
//! plain KAS asks of A (its A bought), at the token slot limits of both programs, with FOK / IOC rules, buying exactly the T
//! its delivery needs (an ask's `ceil(n × p / scale)` of B, a bid's exactly n of A; more only when an ask's minimum fill forces
//! it, and then to the maker), best prices first, ranked with direct orders by price → tip → age, never at a loss, never
//! sharing an order with another transaction of the tick except as a chained continuation. Every transaction is
//! engine-validated and leaves the operator no token. Amounts are base units (`WHOLE` = one whole token of the fixtures),
//! prices base units of B per whole A.
//!
//! Not ported (their rule no longer exists, option (2) of the pair design): the old route restrictions R1 / R2 (a token sold
//! by cross limits only into plain bids, a token bought only from plain orders; a pair leg may now share a transaction with
//! any KAS order), "a token sold and bought by cross limits in one transaction" (allowed), and the cross caps of 4 A outputs and
//! 4 B inputs (the program slot limits alone bound a route).

#[path = "matcher_common/mod.rs"]
mod common;

use common::pair::*;
use common::*;
use kob_executor::matcher::book::ListedOrder;
use kob_executor::matcher::engine::{EngineConfig, Prepared, TickReport};
use kob_protocol::artifacts::{token_template, TemplateId};
use kob_protocol::state::*;
use kob_protocol::tx::spk_to_string;

const K3: TemplateId = TemplateId::KronToken2433;
const X: u32 = 1;
/// The rate (B base units per whole A) of the pair BIDs: its B sold at 2.00 KAS buys its A at 2.50 KAS with 0.30 KAS left.
const BR: i64 = 1_400;
const P270: i64 = 270_000_000;

/// A pair ASK of `amount` base units of A at one whole B per whole A (no tip), KAS bids of A at 2.60 and KAS asks of B at 2.00
/// of the given amounts (the route sells a whole A for 2.60 KAS and buys a whole B for 2.00).
fn ask_scene(pa: TemplateId, pb: TemplateId, amount: i64, tif: i64, bids: &[i64], asks: &[i64]) -> Vec<ListedOrder> {
    let mut v = vec![l_pair(X, pa, pb, pair_state(1, true, pa, pb, amount, RATE, 0, tif), 1_000)];
    for (k, n) in bids.iter().enumerate() {
        v.push(l_bid_a(100 + k as u32, pa, 2, P260, *n));
    }
    for (k, n) in asks.iter().enumerate() {
        v.push(l_ask_b(200 + k as u32, pa, pb, 3, P200, *n));
    }
    if tif != TIF_GTC {
        v[0] = fresh(v[0].clone());
    }
    v
}

/// A pair BID of `amount` base units of A at 1.4 B per A (no tip), KAS bids of B at 2.00 and KAS asks of A at 2.50 of the given
/// amounts (the route sells 1.4 whole B for 2.80 KAS and buys a whole A for 2.50).
fn bid_scene(pa: TemplateId, pb: TemplateId, amount: i64, tif: i64, bids_b: &[i64], asks_a: &[i64]) -> Vec<ListedOrder> {
    let mut v = vec![l_pair(X, pa, pb, pair_state(1, false, pa, pb, amount, BR, 0, tif), 1_000)];
    for (k, n) in bids_b.iter().enumerate() {
        v.push(l_bid_b(100 + k as u32, pa, pb, 2, P200, *n));
    }
    for (k, n) in asks_a.iter().enumerate() {
        v.push(l_ask_a(200 + k as u32, pa, 3, P250, *n));
    }
    if tif != TIF_GTC {
        v[0] = fresh(v[0].clone());
    }
    v
}

/// Whether the transaction pays `amount` base units of the token of program `p` to key `maker` at the input index of order
/// `id` (the pair order's delivery, or its custody return, is positional).
fn pays_at_input(p: &Prepared, id: [u8; 32], prog: TemplateId, amount: i64, maker: u8) -> bool {
    let tx = &p.signed.tx;
    let i = tx.inputs.iter().position(|i| i.utxo.covenant_id == Some(id)).expect("the pair order is spent");
    tx.outputs[i].script_public_key == spk_to_string(&tstate(prog, amount, pk(maker), false).spk_with(token_template(prog)))
}

/// Whether the transaction pays `amount` base units of the token of program `prog` to key `maker` at the input index of the
/// custody UTXO `c` (the rest or the return of a custody is positional).
fn pays_at_custody(p: &Prepared, c: &kob_protocol::tx::TokenUtxo, prog: TemplateId, amount: i64, maker: u8) -> bool {
    let tx = &p.signed.tx;
    let i = tx
        .inputs
        .iter()
        .position(|i| (i.transaction_id, i.index) == (c.utxo.transaction_id, c.utxo.index))
        .expect("the custody is spent");
    tx.outputs[i].script_public_key == spk_to_string(&tstate(prog, amount, pk(maker), false).spk_with(token_template(prog)))
}

/// An order of a prepared transaction is never in another transaction of the tick unless chained on the earlier one.
fn assert_chained_sharing_only(r: &TickReport) {
    let txids: std::collections::BTreeSet<[u8; 32]> = r.prepared.iter().map(|p| p.txid()).collect();
    for (k, p) in r.prepared.iter().enumerate() {
        let ids = p.plan.spent_ids();
        for (j, q) in r.prepared.iter().enumerate() {
            if j == k {
                continue;
            }
            let later = if j > k { q } else { p };
            for id in ids.intersection(&q.plan.spent_ids()) {
                let i = later.signed.tx.inputs.iter().find(|i| i.utxo.covenant_id == Some(*id)).expect("spent");
                assert!(txids.contains(&i.transaction_id), "an order shared between two transactions of a tick without chaining");
            }
        }
    }
}

// ------------------------------------------------------------------ partial fills, slots

#[test]
fn a_partial_fill_through_one_bid_and_one_ask_in_every_family_pair() {
    for (pa, pb) in [(T3, T8), (T8, T3), (T3, K3), (K3, T3), (K3, K3), (T8, T8)] {
        let name = format!("{} / {}", pa.name(), pb.name());
        // an ASK of 10 A: 4 A through the one bid of A, 4 B (one whole B per whole A) from the one ask of B
        let r = run_pair(ask_scene(pa, pb, 10 * WHOLE, TIF_GTC, &[4 * WHOLE], &[4 * WHOLE]), &cfg());
        assert_eq!(r.prepared.len(), 1, "ask {name}");
        assert_eq!(amount_in(&r, cid(X)), 4 * WHOLE, "ask {name}");
        let ids = r.prepared[0].plan.spent_ids();
        assert!(ids.contains(&cid(100)) && ids.contains(&cid(200)), "ask {name}");
        // pair legs first: the pair order is input 0
        assert_eq!(r.prepared[0].signed.tx.inputs[0].utxo.covenant_id, Some(cid(X)));
        // a BID of 10 A: its B sold into the one bid of B, exactly the 4 A of the one ask of A bought
        let r = run_pair(bid_scene(pa, pb, 10 * WHOLE, TIF_GTC, &[8 * WHOLE], &[4 * WHOLE]), &cfg());
        assert_eq!(r.prepared.len(), 1, "bid {name}");
        assert_eq!(amount_in(&r, cid(X)), 4 * WHOLE, "bid {name}");
        assert_eq!(amount_in(&r, cid(200)), 4 * WHOLE, "bid {name}: exactly the A of the delivery");
        assert_eq!(amount_in(&r, cid(100)), 5_600, "bid {name}: exactly floor(4,000 x 1.4) of B sold");
        assert_eq!(r.prepared[0].signed.tx.inputs[0].utxo.covenant_id, Some(cid(X)));
    }
}

#[test]
fn token_slot_limits_of_3_by_3_programs() {
    // Kcc20Ref: 3 inputs / 3 outputs per token. An ASK keeping a rest has one A output for it: two bid deliveries and the rest.
    let r = run_pair(ask_scene(T3, T3, 5 * WHOLE, TIF_GTC, &[WHOLE; 5], &[WHOLE; 5]), &cfg());
    assert_eq!(amount_in(&r, cid(X)), 2 * WHOLE, "two bid deliveries and the rest");
    // sold out: three bid deliveries, three ask custodies
    let r = run_pair(ask_scene(T3, T3, 3 * WHOLE, TIF_GTC, &[WHOLE; 5], &[WHOLE; 5]), &cfg());
    assert_eq!(amount_in(&r, cid(X)), 3 * WHOLE);
    assert_eq!(r.prepared[0].plan.fills.len(), 7);
    // a BID: its A bought from asks of A (one custody input each, three inputs of a 3x3 program at most; a 1.4 B per A bid of
    // 1 whole A is worth 1,400 B of one bid of B). Two whole A: two bid deliveries of B and the escrow's rest (its slack
    // base unit per fill comes back to the maker) fill the three B outputs
    let r = run_pair(bid_scene(T3, T3, 5 * WHOLE, TIF_GTC, &[1_400; 5], &[WHOLE; 5]), &cfg());
    assert_eq!(amount_in(&r, cid(X)), 2 * WHOLE, "two B deliveries and the escrow's rest");
    // the A side: a big bid of B takes any B, the asks of A (three inputs) bound the fill
    let r = run_pair(bid_scene(T3, T3, 5 * WHOLE, TIF_GTC, &[20 * WHOLE], &[WHOLE; 5]), &cfg());
    assert_eq!(amount_in(&r, cid(X)), 3 * WHOLE, "three ask custodies of A");
}

#[test]
fn the_program_slot_limits_alone_bound_a_route_there_are_no_pair_caps() {
    // Kcc20Ref8x8: 8 inputs / 8 outputs per token, no cap of the pair order itself: 6 whole A sold out through 6 bids
    let r = run_pair(ask_scene(T8, T8, 6 * WHOLE, TIF_GTC, &[WHOLE; 8], &[WHOLE; 8]), &cfg());
    assert_eq!(amount_in(&r, cid(X)), 6 * WHOLE, "six bid deliveries (sold out)");
    // 10 whole A: seven bid deliveries and the rest are the eight A outputs
    let r = run_pair(ask_scene(T8, T8, 10 * WHOLE, TIF_GTC, &[WHOLE; 10], &[WHOLE; 10]), &cfg());
    assert_eq!(amount_in(&r, cid(X)), 7 * WHOLE, "seven bid deliveries and the rest");
    // one bid takes everything; B then needs more asks than the program's inputs when each ask holds one whole token
    let r = run_pair(ask_scene(T8, T8, 9 * WHOLE, TIF_GTC, &[9 * WHOLE], &[WHOLE; 10]), &cfg());
    assert_eq!(amount_in(&r, cid(X)), 8 * WHOLE, "eight ask custodies of B");
    // KRON (4 inputs / 5 outputs) as token A: four deliveries sold out; five A outputs hold four deliveries and the rest
    let r = run_pair(ask_scene(K3, T8, 4 * WHOLE, TIF_GTC, &[WHOLE; 5], &[4 * WHOLE]), &cfg());
    assert_eq!(amount_in(&r, cid(X)), 4 * WHOLE);
    let r = run_pair(ask_scene(K3, T8, 6 * WHOLE, TIF_GTC, &[WHOLE; 6], &[6 * WHOLE]), &cfg());
    assert_eq!(amount_in(&r, cid(X)), 4 * WHOLE, "four deliveries and the rest");
    // a KRON token B: four custody inputs
    let r = run_pair(ask_scene(T8, K3, 6 * WHOLE, TIF_GTC, &[6 * WHOLE], &[WHOLE; 6]), &cfg());
    assert_eq!(amount_in(&r, cid(X)), 4 * WHOLE, "four ask custodies of KRON B");
}

// ------------------------------------------------------------------ time in force

#[test]
fn fok_is_all_or_nothing_and_ioc_takes_the_largest_fill() {
    // ASK
    let r = run_pair(ask_scene(T8, T8, 5 * WHOLE, TIF_FOK, &[4 * WHOLE], &[5 * WHOLE]), &cfg());
    assert!(r.prepared.is_empty(), "a FOK pair order is never filled partially");
    let r = run_pair(ask_scene(T8, T8, 5 * WHOLE, TIF_FOK, &[4 * WHOLE, WHOLE], &[5 * WHOLE]), &cfg());
    assert_eq!(amount_in(&r, cid(X)), 5 * WHOLE);
    let r = run_pair(ask_scene(T8, T8, 10 * WHOLE, TIF_IOC, &[3 * WHOLE], &[5 * WHOLE]), &cfg());
    assert_eq!(amount_in(&r, cid(X)), 3 * WHOLE, "all the route carries; the rest returns to the maker");
    let tx = &r.prepared[0].signed.tx;
    let custody_in = tx.inputs.iter().position(|i| i.utxo.covenant_id == Some(TOKEN)).unwrap();
    let back = spk_to_string(&tstate(T8, 7 * WHOLE, pk(1), false).spk_with(token_template(T8)));
    assert_eq!(tx.outputs[custody_in].script_public_key, back, "the IOC return at the custody input index");
    // BID: its A bought exactly
    let r = run_pair(bid_scene(T8, T8, 5 * WHOLE, TIF_FOK, &[20 * WHOLE], &[4 * WHOLE]), &cfg());
    assert!(r.prepared.is_empty(), "a FOK pair bid is never filled partially");
    let r = run_pair(bid_scene(T8, T8, 5 * WHOLE, TIF_FOK, &[20 * WHOLE], &[4 * WHOLE, WHOLE]), &cfg());
    assert_eq!(amount_in(&r, cid(X)), 5 * WHOLE);
    assert_eq!(amount_in(&r, cid(100)), 7_000, "floor(5,000 x 1.4) of B sold");
    let ioc = bid_scene(T8, T8, 10 * WHOLE, TIF_IOC, &[20 * WHOLE], &[3 * WHOLE]);
    let escrow = match &ioc[0].order.state {
        AnyState::KobPair(s) => s.custody,
        _ => unreachable!(),
    };
    let custody = ioc[0].custody.clone().expect("the B escrow");
    let r = run_pair(ioc, &cfg());
    assert_eq!(amount_in(&r, cid(X)), 3 * WHOLE, "all the asks of A hold");
    // the unspent B escrow returns to the maker at the custody input index
    assert!(pays_at_custody(&r.prepared[0], &custody, T8, escrow - 4_200, 1), "the IOC return: the escrow less the 4,200 B sold");
}

#[test]
fn fok_and_ioc_kas_orders_are_only_used_whole() {
    let mut v = ask_scene(T8, T8, 10 * WHOLE, TIF_GTC, &[], &[10 * WHOLE]);
    let mut b = l_bid_a(100, T8, 2, P260, 10 * WHOLE);
    if let AnyState::KobBid(s) = &mut b.order.state {
        s.tif = TIF_FOK;
    }
    v.push(fresh(b));
    // the FOK bid completes only when less than one minimum fill of buying power is left (more than 9 whole tokens of its
    // 10): the pair ask has 10, so the bid is used whole
    let r = run_pair(v.clone(), &cfg());
    assert_eq!(amount_in(&r, cid(100)), 10 * WHOLE);
    // with 4 whole tokens left the FOK bid cannot be used
    v[0] = l_pair(X, T8, T8, pask(1, T8, T8, 4 * WHOLE, RATE, 0), 1_000);
    let r = run_pair(v, &cfg());
    assert!(r.prepared.is_empty());
    // a pair BID buys the A of a FOK ask of A whole, or not at all
    let fok_ask = || {
        let mut a = l_ask_a(200, T8, 3, P250, 10 * WHOLE);
        if let AnyState::KobAsk(s) = &mut a.order.state {
            s.tif = TIF_FOK;
        }
        fresh(a)
    };
    let v = vec![l_pair(X, T8, T8, pbid(1, T8, T8, 10 * WHOLE, BR, 0), 1_000), l_bid_b(100, T8, T8, 2, P200, 30 * WHOLE), fok_ask()];
    let r = run_pair(v, &cfg());
    assert_eq!((amount_in(&r, cid(X)), amount_in(&r, cid(200))), (10 * WHOLE, 10 * WHOLE), "the FOK ask of A is bought whole");
    let v = vec![l_pair(X, T8, T8, pbid(1, T8, T8, 4 * WHOLE, BR, 0), 1_000), l_bid_b(100, T8, T8, 2, P200, 30 * WHOLE), fok_ask()];
    assert!(run_pair(v, &cfg()).prepared.is_empty(), "4 whole A cannot complete a FOK ask of 10");
}

// ------------------------------------------------------------------ amounts, prices

#[test]
fn the_route_buys_exactly_the_t_of_the_delivery_and_the_operator_keeps_no_token_dust() {
    // v3 (no lots): 0.7 whole B per whole A and a KAS tip of 10 sompi per whole A: 4 whole A need ceil(4,000 x 700 / 1,000) =
    // 2,800 base units of B, and the route buys exactly that from the ask; the maker receives exactly 2,800, the ask keeps
    // its rest, the operator gets no token output of B (no dust UTXO with a carrier)
    let mut v = ask_scene(T8, T8, 10 * WHOLE, TIF_GTC, &[4 * WHOLE], &[10 * WHOLE]);
    v[0] = l_pair(X, T8, T8, pask(1, T8, T8, 10 * WHOLE, 700, 10), 1_000);
    let r = run_pair(v, &cfg());
    assert_eq!(amount_in(&r, cid(X)), 4 * WHOLE);
    assert_eq!(amount_in(&r, cid(200)), 2_800, "exactly the B the delivery needs");
    let tx = &r.prepared[0].signed.tx;
    let maker = spk_to_string(&tstate(T8, 2_800, pk(1), false).spk_with(token_template(T8)));
    assert_eq!(tx.outputs[0].script_public_key, maker, "the maker receives exactly ceil(n x rate / scale) of B");
    let b_outs = tx.outputs.iter().filter(|o| o.covenant.as_ref().map(|c| c.covenant_id) == Some(TOKEN_B)).count();
    assert_eq!(b_outs, 2, "token B outputs: the maker's delivery and the ask's rest only");
    for amount in [200, 1_000, 2_800, 7_200] {
        let op = spk_to_string(&tstate(T8, amount, pk(MATCHER), false).spk_with(token_template(T8)));
        assert!(!tx.outputs.iter().any(|o| o.script_public_key == op), "no operator B output");
    }
    assert!(r.prepared[0].accounting.profit > 0);
    // a BID pays exactly floor(n x rate / scale) of B and receives exactly n of A: 4 whole A at 1.4 buy exactly the 4,000 A
    // the ask of A holds back and sell exactly 5,600 B
    let mut v = bid_scene(T8, T8, 10 * WHOLE, TIF_GTC, &[20 * WHOLE], &[10 * WHOLE]);
    v[0] = l_pair(X, T8, T8, pbid(1, T8, T8, 4 * WHOLE, BR, 10), 1_000);
    let r = run_pair(v, &cfg());
    assert_eq!(amount_in(&r, cid(X)), 4 * WHOLE);
    assert_eq!(
        (amount_in(&r, cid(200)), amount_in(&r, cid(100))),
        (4 * WHOLE, 5_600),
        "exactly n of A bought, exactly floor(n x p) of B sold"
    );
    assert!(pays_at_input(&r.prepared[0], cid(X), T8, 4 * WHOLE, 1), "the maker receives exactly n of A");
    let tx = &r.prepared[0].signed.tx;
    for amount in [400, 1_000, 4_000, 6_000] {
        let op = spk_to_string(&tstate(T8, amount, pk(MATCHER), false).spk_with(token_template(T8)));
        assert!(!tx.outputs.iter().any(|o| o.script_public_key == op), "no operator token output");
    }
}

#[test]
fn an_ask_minimum_fill_may_force_the_route_to_buy_more_the_excess_goes_to_the_maker_a_bid_buys_exactly() {
    // ASK: 3,777 of A at 0.333: the delivery needs 1,258 B; the ask of B holds at least 2,000 per fill: 2,000 bought, all of
    // it delivered to the maker (the pair ASK's receipt is a minimum)
    let scene = |mf: i64| {
        let mut b = l_ask_b(200, T8, T8, 3, P200, 10 * WHOLE);
        if let AnyState::KobAsk(s) = &mut b.order.state {
            s.min_fill = mf;
        }
        vec![l_pair(X, T8, T8, pask(1, T8, T8, 3_777, 333, 0), 1_000), l_bid_a(100, T8, 2, P260, 3_777), b]
    };
    let r = run_pair(scene(2_000), &cfg());
    assert_eq!((amount_in(&r, cid(X)), amount_in(&r, cid(200))), (3_777, 2_000));
    assert!(pays_at_input(&r.prepared[0], cid(X), T8, 2_000, 1), "the excess rides on the maker's delivery");
    let r = run_pair(scene(1), &cfg());
    assert!(pays_at_input(&r.prepared[0], cid(X), T8, 1_258, 1), "no minimum fill in the way: exactly ceil(n x rate / scale)");
    // BID: 3,777 of A at 1.4 buys exactly the A it receives; an ask of A whose minimum fill (2,000) the bid's fill reaches is
    // bought exactly (3,777 over the ask's rest?): the bid's receipt is exact, so a purchase never exceeds n
    let scene = |ask_mf: i64| {
        let mut a = l_ask_a(200, T8, 3, P250, 10 * WHOLE);
        if let AnyState::KobAsk(s) = &mut a.order.state {
            s.min_fill = ask_mf;
        }
        vec![l_pair(X, T8, T8, pbid(1, T8, T8, 3_777, BR, 0), 1_000), l_bid_b(100, T8, T8, 2, P200, 30 * WHOLE), a]
    };
    let r = run_pair(scene(1_000), &cfg());
    assert_eq!((amount_in(&r, cid(X)), amount_in(&r, cid(200))), (3_777, 3_777), "exactly n of A");
    assert_eq!(amount_in(&r, cid(100)), 3_777 * BR / WHOLE, "floor(n x 1.4) of B sold");
    // an ask whose minimum fill exceeds what the bid wants cannot be used by it
    let r = run_pair(scene(5_000), &cfg());
    assert!(r.prepared.is_empty(), "a purchase of A above n would leave A with the operator: {:?}", r.prepared.len());
}

#[test]
fn best_prices_first_and_nothing_at_a_loss() {
    // ASK
    let mut v = ask_scene(T8, T8, 2 * WHOLE, TIF_GTC, &[], &[]);
    v.push(l_bid_a(100, T8, 2, P255, 5 * WHOLE));
    v.push(l_bid_a(101, T8, 2, P260, 5 * WHOLE));
    v.push(l_ask_b(200, T8, T8, 3, 210_000_000, 5 * WHOLE));
    v.push(l_ask_b(201, T8, T8, 3, P200, 5 * WHOLE));
    let r = run_pair(v, &cfg());
    let ids = r.prepared[0].plan.spent_ids();
    assert!(ids.contains(&cid(101)) && ids.contains(&cid(201)), "the best bid of A and the cheapest ask of B");
    assert!(!ids.contains(&cid(100)) && !ids.contains(&cid(200)));
    // the bids pay less than the asks cost: no route
    let mut v = ask_scene(T8, T8, 2 * WHOLE, TIF_GTC, &[], &[]);
    v.push(l_bid_a(100, T8, 2, P200, 5 * WHOLE));
    v.push(l_ask_b(200, T8, T8, 3, P260, 5 * WHOLE));
    assert!(run_pair(v, &cfg()).prepared.is_empty());
    // a margin that does not reach min_profit is not taken either
    let mut k = cfg();
    k.planner.min_profit = 1_000 * KAS as i64;
    assert!(run_pair(ask_scene(T8, T8, 4 * WHOLE, TIF_GTC, &[4 * WHOLE], &[4 * WHOLE]), &k).prepared.is_empty());
    // BID: the best bid of B (the highest) and the cheapest ask of A
    let mut v = bid_scene(T8, T8, 2 * WHOLE, TIF_GTC, &[], &[]);
    v.push(l_bid_b(100, T8, T8, 2, 190_000_000, 20 * WHOLE));
    v.push(l_bid_b(101, T8, T8, 2, P200, 20 * WHOLE));
    v.push(l_ask_a(200, T8, 3, 260_000_000, 5 * WHOLE));
    v.push(l_ask_a(201, T8, 3, P250, 5 * WHOLE));
    let r = run_pair(v, &cfg());
    let ids = r.prepared[0].plan.spent_ids();
    assert!(ids.contains(&cid(101)) && ids.contains(&cid(201)), "the best bid of B and the cheapest ask of A");
    assert!(!ids.contains(&cid(100)) && !ids.contains(&cid(200)));
    // B sells for less than the A it buys costs: no route
    let mut v = bid_scene(T8, T8, 2 * WHOLE, TIF_GTC, &[], &[]);
    v.push(l_bid_b(100, T8, T8, 2, 150_000_000, 20 * WHOLE));
    v.push(l_ask_a(200, T8, 3, 280_000_000, 5 * WHOLE));
    assert!(run_pair(v, &cfg()).prepared.is_empty());
    assert!(run_pair(bid_scene(T8, T8, 4 * WHOLE, TIF_GTC, &[20 * WHOLE], &[4 * WHOLE]), &k).prepared.is_empty());
}

#[test]
fn orders_are_never_shared_between_transactions_of_a_tick() {
    // two pair asks of the pair compete for one bid of 3 whole tokens: one route only
    let mut v = ask_scene(T8, T8, 3 * WHOLE, TIF_GTC, &[3 * WHOLE], &[10 * WHOLE]);
    v.push(l_pair(2, T8, T8, pask(4, T8, T8, 3 * WHOLE, RATE, 0), 1_000));
    let r = run_pair(v, &cfg());
    assert_eq!(r.prepared.len(), 1);
    assert_chained_sharing_only(&r);
    // the bid of A also crosses an ask of A in A's own book: price → tip → age decides (matcher.md §3.5). The pair ask's implied
    // quote (the B it buys, at 2.00 per whole token) beats the direct ask at 2.50: the route takes the bid, the ask waits
    let mut v = ask_scene(T8, T8, 3 * WHOLE, TIF_GTC, &[3 * WHOLE], &[10 * WHOLE]);
    v.push(l_ask(300, ask(5, P250, 5 * WHOLE, T8)));
    let r = run_pair(v, &cfg());
    assert_eq!(amount_in(&r, cid(X)), 3 * WHOLE);
    assert_eq!(amount_in(&r, cid(100)), 3 * WHOLE);
    assert_eq!(amount_in(&r, cid(300)), 0);
    // a direct ask below the route's quote (1.90) takes the bid first: the pair ask waits
    let mut v = ask_scene(T8, T8, 3 * WHOLE, TIF_GTC, &[3 * WHOLE], &[10 * WHOLE]);
    v.push(l_ask(300, ask(5, 190_000_000, 5 * WHOLE, T8)));
    let r = run_pair(v, &cfg());
    assert_eq!(amount_in(&r, cid(X)), 0);
    assert_eq!(amount_in(&r, cid(100)), 3 * WHOLE);
    assert_eq!(amount_in(&r, cid(300)), 3 * WHOLE);
    assert_chained_sharing_only(&r);
    // the same for a BID: its A is bought from the ask of A at 2.50; a direct bid of A above 2.50 takes that ask first (the
    // bid of A at 2.60 meets the ask at 2.50 in A's own book), the pair BID's implied bid of A (2.80 for what its B fetches)
    // beats it
    let mut v = bid_scene(T8, T8, 3 * WHOLE, TIF_GTC, &[20 * WHOLE], &[3 * WHOLE]);
    v.push(l_bid(300, bid(5, P260, T8), 5 * WHOLE));
    let r = run_pair(v, &cfg());
    assert_eq!(amount_in(&r, cid(X)), 3 * WHOLE, "the pair bid's implied bid of A (2.80) ranks above the direct bid (2.60)");
    assert_eq!(amount_in(&r, cid(300)), 0);
    let mut v = bid_scene(T8, T8, 3 * WHOLE, TIF_GTC, &[20 * WHOLE], &[3 * WHOLE]);
    v.push(l_bid(300, bid(5, 290_000_000, T8), 5 * WHOLE));
    let r = run_pair(v, &cfg());
    assert_eq!(amount_in(&r, cid(X)), 0, "a direct bid of A at 2.90 takes the ask first");
    assert_eq!(amount_in(&r, cid(300)), 3 * WHOLE);
    assert_chained_sharing_only(&r);
}

#[test]
fn an_expired_or_inactive_pair_order_is_left_to_the_keepers() {
    for ask in [true, false] {
        let scene = |s: PairState, daa: u64| {
            let mut v = if ask {
                ask_scene(T8, T8, 4 * WHOLE, TIF_GTC, &[4 * WHOLE], &[4 * WHOLE])
            } else {
                bid_scene(T8, T8, 4 * WHOLE, TIF_GTC, &[20 * WHOLE], &[4 * WHOLE])
            };
            v[0] = l_pair(X, T8, T8, s, daa);
            v
        };
        let base = || if ask { pask(1, T8, T8, 4 * WHOLE, RATE, 0) } else { pbid(1, T8, T8, 4 * WHOLE, BR, 0) };
        assert!(run_pair(scene(PairState { expiry_daa: (NOW - 100) as i64, ..base() }, 1_000), &cfg()).prepared.is_empty());
        assert!(run_pair(scene(PairState { active_from: (NOW + 100) as i64, ..base() }, 1_000), &cfg()).prepared.is_empty());
        // an IOC past its kill time (600 DAA after its UTXO)
        let ioc = if ask {
            pair_state(1, true, T8, T8, 4 * WHOLE, RATE, 0, TIF_IOC)
        } else {
            pair_state(1, false, T8, T8, 4 * WHOLE, BR, 0, TIF_IOC)
        };
        assert!(run_pair(scene(ioc, NOW - 700), &cfg()).prepared.is_empty());
    }
}

// ------------------------------------------------------------------ triggers

#[test]
fn a_pair_batch_also_arms_a_stop_its_plain_fills_trigger() {
    // The pair ask sells A into a plain bid of A at 2.60: that fill (a resting bid at or above 2.55) is evidence for a listed
    // buy stop of A. The batch arms it with an update next to the route.
    let mut orders = ask_scene(T3, T8, 10 * WHOLE, TIF_GTC, &[4 * WHOLE], &[4 * WHOLE]);
    orders.push(l_cond_bid(50, cond_bid(5, 200_000_000, 255_000_000, 4, T3), 2_000));
    let r = run_pair(orders, &cfg());
    assert_eq!(r.prepared.len(), 1);
    let p = &r.prepared[0];
    assert_eq!(amount_in(&r, cid(X)), 4 * WHOLE);
    assert_eq!(amount_in(&r, cid(50)), 0);
    let ev = p.plan.fills.iter().position(|f| f.cand.id == cid(100)).expect("the plain bid of A");
    assert_eq!(p.plan.updates.iter().map(|u| (u.id, u.evidence)).collect::<Vec<_>>(), vec![(cid(50), ev)]);
    check_triggers(p, &input(&book(vec![])));
    // a pair BID buys A from a plain ask of A at 2.50: a resting ask at or below 2.55 is evidence for a listed sell stop of A
    let mut orders = bid_scene(T3, T8, 10 * WHOLE, TIF_GTC, &[20 * WHOLE], &[4 * WHOLE]);
    orders.push(l_cond_ask(50, cond_ask(5, 300_000_000, 255_000_000, 4 * WHOLE, T3), 2_000));
    let r = run_pair(orders, &cfg());
    assert_eq!(r.prepared.len(), 1);
    let p = &r.prepared[0];
    assert_eq!(amount_in(&r, cid(X)), 4 * WHOLE);
    let ev = p.plan.fills.iter().position(|f| f.cand.id == cid(200)).expect("the plain ask of A");
    let armed: Vec<_> = p.plan.updates.iter().map(|u| (u.id, u.evidence)).collect();
    assert!(
        armed == vec![(cid(50), ev)] || p.plan.amount_of(&cid(50)) > 0,
        "the sell stop is armed (or triggered) next to its evidence"
    );
    check_triggers(p, &input(&book(vec![])));
}

// ------------------------------------------------------------------ the widest shape, budgets, funding

/// Roles and script units of the pair order inputs of a prepared transaction.
fn pair_inputs(p: &Prepared) -> Vec<(String, u64, u16)> {
    let v = p.validation.as_ref().expect("engine-validated");
    p.lowered
        .built
        .roles
        .iter()
        .zip(v.units.iter().zip(&v.budgets))
        .filter(|(r, _)| r.starts_with("KobPair.settle."))
        .map(|(r, (u, b))| (r.clone(), *u, *b))
        .collect()
}

#[test]
fn two_pair_orders_in_one_route_on_the_widest_shape() {
    // The soak's shape (2026-10-01): two pair asks selling A for B in one transaction, one selling out and one keeping a rest,
    // through three plain bids of A (with the rest: the four token-A outputs) and four asks of one whole B (the four token-B
    // inputs). Its closing input needs the most script units of the batch: the committed budgets must cover it.
    let mut v = vec![
        l_pair(1, T8, T8, pask(1, T8, T8, 2 * WHOLE, RATE, 0), 900),
        l_pair(2, T8, T8, pask(4, T8, T8, 10 * WHOLE, RATE, 0), 1_000),
    ];
    for (k, n) in [1, 1, 2].into_iter().enumerate() {
        v.push(l_bid_a(100 + k as u32, T8, 2, P260, n * WHOLE));
    }
    for k in 0..4 {
        v.push(l_ask_b(200 + k, T8, T8, 3, P200, WHOLE));
    }
    let r = run_pair(v, &cfg());
    assert_eq!(r.prepared.len(), 1, "skipped: {:?}", r.skipped);
    assert_eq!((amount_in(&r, cid(1)), amount_in(&r, cid(2))), (2 * WHOLE, 2 * WHOLE));
    let x = pair_inputs(&r.prepared[0]);
    let mut roles: Vec<&str> = x.iter().map(|(r, ..)| r.as_str()).collect();
    roles.sort_unstable();
    assert_eq!(
        roles,
        ["KobPair.settle.ask.close@KCC20Ref_8x8+KCC20Ref_8x8", "KobPair.settle.ask.rest@KCC20Ref_8x8+KCC20Ref_8x8"],
        "one pair ask sells out, the other keeps a rest"
    );
    for (role, units, budget) in &x {
        assert!(kob_protocol::budget::budget_for_units(*units) <= *budget, "{role}: {units} units over its committed budget {budget}");
    }
    // the same on the BID side: two pair bids buying A from asks of A, selling B into bids of B
    let mut v =
        vec![l_pair(1, T8, T8, pbid(1, T8, T8, 2 * WHOLE, BR, 0), 900), l_pair(2, T8, T8, pbid(4, T8, T8, 10 * WHOLE, BR, 0), 1_000)];
    for (k, n) in [4, 4, 8].into_iter().enumerate() {
        v.push(l_bid_b(100 + k as u32, T8, T8, 2, P200, n * WHOLE));
    }
    for k in 0..4 {
        v.push(l_ask_a(200 + k, T8, 3, P250, WHOLE));
    }
    let r = run_pair(v, &cfg());
    assert_eq!(r.prepared.len(), 1, "skipped: {:?}", r.skipped);
    let total = amount_in(&r, cid(1)) + amount_in(&r, cid(2));
    assert!((3 * WHOLE..=4 * WHOLE).contains(&total), "the four asks of A of one whole token: {total}");
    assert_eq!(pair_inputs(&r.prepared[0]).len(), 2);
}

/// A KCC-20 adapter that commits 1 budget unit for a pair order input (far too little) unless the engine passes a measured
/// budget: a shape the budget generator would miss.
struct ShortPairBudgets;

impl kob_executor::matcher::family::FamilyAdapter for ShortPairBudgets {
    fn family(&self) -> kob_executor::matcher::family::Family {
        kob_executor::matcher::family::Family::Kcc20
    }
    fn lower(
        &self,
        plan: &kob_executor::matcher::planner::Plan,
        cx: &kob_executor::matcher::lower::LowerCtx,
    ) -> Result<kob_executor::matcher::family::Lowered, String> {
        use kob_executor::matcher::lower::{budgets, lower_batch, operator_token_kas};
        let batch = lower_batch(plan, cx)?;
        let (kin, kout) = operator_token_kas(plan, &batch);
        let action = kob_protocol::build::Action::Batch(batch);
        let f = |role: &str| match cx.budget_floor.get(role) {
            Some(b) => Ok(*b),
            None => budgets(role).map(|b| if role.starts_with("KobPair.") { 1 } else { b }),
        };
        let built = kob_protocol::build::build_with(&action, &f).map_err(|e| e.to_string())?;
        Ok(kob_executor::matcher::family::Lowered {
            request: serde_json::to_value(&action).map_err(|e| e.to_string())?,
            built,
            operator_token_kas_in: kin,
            operator_token_kas_out: kout,
        })
    }
}

#[test]
fn a_short_budget_is_measured_and_the_batch_still_goes_through() {
    // The table's safety net: when the engine rejects a batch for a budget the table gives too little, the matcher measures
    // the inputs' script units and retries once with the budgets they need, instead of skipping the batch tick after tick.
    for ask in [true, false] {
        let mut families = kob_executor::matcher::family::Families::default();
        families.register(Box::new(ShortPairBudgets));
        let scene = if ask {
            ask_scene(T8, T8, 10 * WHOLE, TIF_GTC, &[4 * WHOLE], &[4 * WHOLE])
        } else {
            bid_scene(T8, T8, 10 * WHOLE, TIF_GTC, &[20 * WHOLE], &[4 * WHOLE])
        };
        let b = book(scene);
        let inp = input(&b);
        let cfg = EngineConfig { token_carrier: OP_TOKEN_CARRIER, ..cfg() };
        let r = kob_executor::matcher::engine::tick(&inp, &cfg, &families, &signer());
        assert_eq!(r.prepared.len(), 1, "skipped: {:?}, anomalies: {:?}", r.skipped, r.anomalies);
        assert_eq!(r.slack_retries, 1);
        assert_eq!(amount_in(&r, cid(X)), 4 * WHOLE);
        let v = r.prepared[0].validation.as_ref().unwrap();
        let i = r.prepared[0].lowered.built.roles.iter().position(|r| r.starts_with("KobPair.")).unwrap();
        assert_eq!(v.budgets[i], kob_protocol::budget::budget_for_units(v.units[i]), "the measured budget, exactly");
        check_pair(&r, &inp);
    }
}

/// Funding inputs (P2PK inputs of the operator) of a prepared transaction.
fn funding_inputs(p: &Prepared) -> usize {
    p.lowered.built.roles.iter().filter(|r| *r == "p2pk").count()
}

#[test]
fn a_pair_route_needs_no_operator_funding_for_token_dust() {
    // The soak's "insufficient funds: need N, have N - (1..9 KAS)": a route whose excess token went to the operator put a 10
    // KAS carrier on it. A route buys exactly what the delivery needs (no lots, no excess): the route's own KAS spread pays
    // the fee, one small funding UTXO is enough (it only carries the change).
    let mut v = ask_scene(T8, T8, 10 * WHOLE, TIF_GTC, &[4 * WHOLE], &[10 * WHOLE]);
    v[0] = l_pair(X, T8, T8, pask(1, T8, T8, 10 * WHOLE, 700, 10), 1_000);
    let mut w = bid_scene(T8, T8, 10 * WHOLE, TIF_GTC, &[20 * WHOLE], &[4 * WHOLE]);
    w[0] = l_pair(X, T8, T8, pbid(1, T8, T8, 10 * WHOLE, BR, 10), 1_000);
    for scene in [v, w] {
        let b = book(scene);
        let mut inp = input(&b);
        inp.funding = vec![funding(3 * KAS)];
        let cfg = EngineConfig { token_carrier: OP_TOKEN_CARRIER, ..cfg() };
        let r = kob_executor::matcher::engine::tick(&inp, &cfg, &kob_executor::matcher::family::Families::default(), &signer());
        assert_eq!(r.prepared.len(), 1, "skipped: {:?}, anomalies: {:?}", r.skipped, r.anomalies);
        assert_eq!(funding_inputs(&r.prepared[0]), 1);
        assert_eq!(amount_in(&r, cid(X)), 4 * WHOLE);
        assert!(r.prepared[0].accounting.change > 3 * KAS, "the funding comes back with the route's spread");
        check_pair(&r, &inp);
    }
}

#[test]
fn a_fragmented_funding_pool_is_merged_into_the_batchs_change() {
    // six accepted UTXOs (more than the target of four): the batch spends the largest and consolidates the two smallest
    let b = book(ask_scene(T8, T8, 10 * WHOLE, TIF_GTC, &[4 * WHOLE], &[4 * WHOLE]));
    let mut inp = input(&b);
    inp.funding = [50, 40, 30, 20, 12, 11].into_iter().map(|k| funding(k * KAS)).collect();
    let cfg0 = EngineConfig { token_carrier: OP_TOKEN_CARRIER, ..cfg() };
    let families = kob_executor::matcher::family::Families::default();
    let r = kob_executor::matcher::engine::tick(&inp, &cfg0, &families, &signer());
    assert_eq!(r.prepared.len(), 1);
    let p = &r.prepared[0];
    assert_eq!(funding_inputs(p), 3);
    assert_eq!(p.accounting.funding, 73 * KAS, "50 + 11 + 12");
    assert!(p.accounting.change > 72 * KAS, "one change output carries them all");
    check_pair(&r, &inp);
    // switched off: the largest only
    let mut k = cfg0.clone();
    k.funding.consolidate = 0;
    let r = kob_executor::matcher::engine::tick(&inp, &k, &families, &signer());
    assert_eq!(funding_inputs(&r.prepared[0]), 1);
}

// ------------------------------------------------------------------ pair market orders

/// A pair market ASK: an IOC pair order of `whole` whole A, `elapsed` DAA into an auction that decays by `slope` B base units per
/// DAA from `start` down to `end` per whole A, at the lock time `NOW`.
fn pair_market_ask(start: i64, end: i64, slope: i64, elapsed: i64, whole: i64) -> ListedOrder {
    let x = PairState {
        price_end: end,
        slope,
        decay_step: 1,
        active_from: NOW as i64 - elapsed,
        expiry_daa: NOW as i64 + 300,
        ..pair_state(1, true, T8, T8, whole * WHOLE, start, 0, TIF_IOC)
    };
    fresh(l_pair(X, T8, T8, x, 1_000))
}

/// A pair market BID: an IOC pair bid of `whole` whole A whose rate rises by `slope` per DAA from `start` up to `end` (its escrow
/// covers the top).
fn pair_market_bid(start: i64, end: i64, slope: i64, elapsed: i64, whole: i64) -> ListedOrder {
    let mut x = PairState {
        price_end: end,
        slope,
        decay_step: 1,
        active_from: NOW as i64 - elapsed,
        expiry_daa: NOW as i64 + 300,
        ..pair_state(1, false, T8, T8, whole * WHOLE, start, 0, TIF_IOC)
    };
    x.custody = x.bid_escrow(x.amount_left, 4).expect("escrow");
    fresh(l_pair(X, T8, T8, x, 1_000))
}

/// The first `elapsed` in `0..=200` at which the scene fills the pair order (None: never).
fn first_fill(scene: impl Fn(i64) -> Vec<ListedOrder>) -> Option<(i64, TickReport)> {
    (0..=200).find_map(|e| {
        let r = run_pair(scene(e), &cfg());
        (amount_in(&r, cid(X)) > 0).then_some((e, r))
    })
}

#[test]
fn a_pair_market_order_fills_at_the_first_profitable_point_of_its_auction() {
    // ASK: ten whole A through a bid at 3.00 (30.01 KAS with its tip); B costs 1.999 KAS per whole B to the route (2.00 less
    // the ask's tip). The auction decays from 1,510 by 2 B per DAA: at the start the exact 15,100 B (30.18 KAS) cost more than
    // the whole bid pays; the order fills as soon as the exact B (price(t) x 10) leaves the fee and the margin
    let scene = |elapsed: i64| {
        vec![
            pair_market_ask(1_510, 1_200, 2, elapsed, 10),
            l_bid_a(100, T8, 2, 300_000_000, 10 * WHOLE),
            l_ask_b(200, T8, T8, 3, P200, 20 * WHOLE),
        ]
    };
    let r = run_pair(scene(0), &cfg());
    assert_eq!(amount_in(&r, cid(X)), 0, "not at a loss at the start of the auction");
    let (e, r) = first_fill(scene).expect("the auction reaches a profitable point");
    assert!(e > 0 && e < 100, "the first profitable point: {e} DAA into the auction");
    assert_eq!(amount_in(&r, cid(X)), 10 * WHOLE, "filled as soon as the route pays, long before the worst bound");
    let p = &r.prepared[0];
    let rate = 1_510 - 2 * e;
    assert_eq!(amount_in(&r, cid(200)), 10 * rate, "the route buys exactly 10 x the auction's rate at the lock time");
    // run_pair checked the delivery: at least ceil(n x rate(t) / scale) at the order's input index
    assert!(pays_at_input(p, cid(X), T8, 10 * rate, 1));
    // BID: the rate rises from 1,100 by 2 B per DAA up to 1,500: its B sold into a bid of B at 2.00 buys its A at 2.50 only
    // once the B it sells (10 x rate) is worth more than the 24.99 KAS of the A and the fee
    let scene = |elapsed: i64| {
        vec![
            pair_market_bid(1_100, 1_500, 2, elapsed, 10),
            l_bid_b(100, T8, T8, 2, P200, 60 * WHOLE),
            l_ask_a(200, T8, 3, P250, 20 * WHOLE),
        ]
    };
    let r = run_pair(scene(0), &cfg());
    assert_eq!(amount_in(&r, cid(X)), 0, "not at a loss at the start of the auction");
    let (e, r) = first_fill(scene).expect("the auction reaches a profitable point");
    assert!(e > 0 && e < 200, "the first profitable point: {e} DAA into the auction");
    assert_eq!(amount_in(&r, cid(X)), 10 * WHOLE);
    assert_eq!(amount_in(&r, cid(100)), 10 * (1_100 + 2 * e), "the route sells exactly 10 x the auction's rate at the lock time");
    assert_eq!(amount_in(&r, cid(200)), 10 * WHOLE, "and buys exactly the A");
}

#[test]
fn an_auctioning_pair_order_is_ranked_at_its_rate_at_t() {
    // two IOC pair asks compete for one bid of A of 3 whole tokens: a fixed one at 1,000 B per whole A and a pair market order
    // whose auction has relaxed to 1,150 - 170 = 980 at t: it buys less B, so its implied ask of A is the better price and it
    // takes the bid (at 0 DAA, its start rate 1,150 loses to the fixed one)
    let fixed = || fresh(l_pair(2, T8, T8, pair_state(4, true, T8, T8, 3 * WHOLE, RATE, 0, TIF_IOC), 1_000));
    let scene = |elapsed: i64| {
        vec![
            pair_market_ask(1_150, 900, 1, elapsed, 3),
            l_bid_a(100, T8, 2, P260, 3 * WHOLE),
            l_ask_b(200, T8, T8, 3, P200, 10 * WHOLE),
            fixed(),
        ]
    };
    let r = run_pair(scene(170), &cfg());
    assert_eq!(amount_in(&r, cid(X)), 3 * WHOLE);
    assert_eq!(amount_in(&r, cid(2)), 0);
    let r = run_pair(scene(0), &cfg());
    assert_eq!(amount_in(&r, cid(X)), 0);
    assert_eq!(amount_in(&r, cid(2)), 3 * WHOLE);
    // two IOC pair bids compete for one ask of A of 3 whole tokens: a fixed one at 1,400 B per whole A and a rising market one
    // that has reached 1,300 + 150 = 1,450 at t: it pays more B (its Buy leg's implied bid of A is higher) and takes the ask
    // (at 0 DAA, its start rate 1,300 loses to the fixed one)
    let fixed = || fresh(l_pair(2, T8, T8, pair_state(4, false, T8, T8, 3 * WHOLE, BR, 0, TIF_IOC), 1_000));
    let scene = |elapsed: i64| {
        vec![
            pair_market_bid(1_300, 1_600, 1, elapsed, 3),
            l_bid_b(100, T8, T8, 2, P200, 20 * WHOLE),
            l_ask_a(200, T8, 3, P250, 3 * WHOLE),
            fixed(),
        ]
    };
    let r = run_pair(scene(150), &cfg());
    assert_eq!((amount_in(&r, cid(X)), amount_in(&r, cid(2))), (3 * WHOLE, 0));
    let r = run_pair(scene(0), &cfg());
    assert_eq!((amount_in(&r, cid(X)), amount_in(&r, cid(2))), (0, 3 * WHOLE));
}

/// Two IOC pair asks of A for B compete for one bid of A: the one with the lower rate (cheaper B: the better implied price per
/// unit of A) walks first in both books, whatever its size; for two pair bids the higher rate wins the one ask of A. Before,
/// the legs tied on their quote per unit of A and a large worse order took every ask of B first, so neither route could form.
#[test]
fn ioc_pair_orders_are_walked_in_rate_order_in_both_books() {
    let fixed = || fresh(l_pair(2, T8, T8, pair_state(4, true, T8, T8, 3 * WHOLE, RATE, 0, TIF_IOC), 1_000));
    for (whole, tif, rate, x_whole, fixed_whole) in
        [(3, TIF_IOC, 1_030, 0, 3), (10, TIF_GTC, 1_030, 0, 3), (10, TIF_IOC, 1_001, 0, 3), (10, TIF_IOC, 990, 3, 0)]
    {
        let other = l_pair(X, T8, T8, pair_state(1, true, T8, T8, whole * WHOLE, rate, 0, tif), 1_000);
        let other = if tif == TIF_IOC { fresh(other) } else { other };
        let r =
            run_pair(vec![other, l_bid_a(100, T8, 2, P260, 3 * WHOLE), l_ask_b(200, T8, T8, 3, P200, 10 * WHOLE), fixed()], &cfg());
        assert_eq!(
            (amount_in(&r, cid(X)), amount_in(&r, cid(2))),
            (x_whole * WHOLE, fixed_whole * WHOLE),
            "ask: {whole} whole tokens, tif {tif}, rate {rate}"
        );
    }
    let fixed = || fresh(l_pair(2, T8, T8, pair_state(4, false, T8, T8, 3 * WHOLE, BR, 0, TIF_IOC), 1_000));
    for (whole, tif, rate, x_whole, fixed_whole) in
        [(3, TIF_IOC, 1_370, 0, 3), (10, TIF_GTC, 1_370, 0, 3), (10, TIF_IOC, 1_399, 0, 3), (10, TIF_IOC, 1_410, 3, 0)]
    {
        let other = l_pair(X, T8, T8, pair_state(1, false, T8, T8, whole * WHOLE, rate, 0, tif), 1_000);
        let other = if tif == TIF_IOC { fresh(other) } else { other };
        let r =
            run_pair(vec![other, l_bid_b(100, T8, T8, 2, P200, 40 * WHOLE), l_ask_a(200, T8, 3, P250, 3 * WHOLE), fixed()], &cfg());
        assert_eq!(
            (amount_in(&r, cid(X)), amount_in(&r, cid(2))),
            (x_whole * WHOLE, fixed_whole * WHOLE),
            "bid: {whole} whole tokens, tif {tif}, rate {rate}"
        );
    }
}

// ------------------------------------------------------------------ opposite pair orders

/// Live TN10 soak (2026-10-01, TETH / TUSD): two orders of opposite directions (A for B and B for A) whose legs both cross at
/// the best KAS prices. Each took one leg first (its purchase, ranked above the plain bids), blocking the other's second leg;
/// the reconciliation then dropped both. Neither filled while both rested: 7 to 13 min for a 2 to 3 KAS route in the soak. Now
/// opposite pair orders (an ASK and a BID of one pair, or an ASK of A/B with an ASK of B/A) share the transaction or are netted.
#[test]
fn opposite_pair_orders_never_block_each_other() {
    // both directions pay: A and B bid at 2.60 and ask at 2.70 per whole token; the ASK sells A for 0.5 B per A (it costs 1.35
    // KAS of B for 2.60 KAS of A), the BID pays 2.0 B per A (5.20 KAS of B sold for 2.70 KAS of A)
    let books = || {
        vec![
            l_bid_a(100, T8, 2, P260, 8 * WHOLE),
            l_ask_a(101, T8, 2, P270, 8 * WHOLE),
            l_bid_b(200, T8, T8, 2, P260, 8 * WHOLE),
            l_ask_b(201, T8, T8, 2, P270, 8 * WHOLE),
        ]
    };
    let mut v = vec![
        l_pair(X, T8, T8, pask(1, T8, T8, 4 * WHOLE, 500, 0), 1_000),
        l_pair(2, T8, T8, pbid(4, T8, T8, 4 * WHOLE, 2_000, 0), 1_000),
    ];
    v.extend(books());
    let r = run_pair(v, &cfg());
    assert_eq!((amount_in(&r, cid(X)), amount_in(&r, cid(2))), (4 * WHOLE, 4 * WHOLE), "an ASK and a BID of one pair");
    // the same with an ASK of A/B and an ASK of B/A (the pair of the other direction: base B, quote A, 0.5 A per whole B)
    let swapped = pair_retoken(l_pair(2, T8, T8, pask(4, T8, T8, 4 * WHOLE, 500, 0), 1_000), TOKEN_B, TOKEN);
    let mut v = vec![l_pair(X, T8, T8, pask(1, T8, T8, 4 * WHOLE, 500, 0), 1_000), swapped];
    v.extend(books());
    let r = run_pair(v, &cfg());
    assert_eq!((amount_in(&r, cid(X)), amount_in(&r, cid(2))), (4 * WHOLE, 4 * WHOLE), "an ASK of A/B and an ASK of B/A");
    // the live shape: the BID crosses at the best KAS prices but loses on every fill (1.02 B per A sold at 2.60 = 2.65 KAS for
    // the A at 2.70), a resting overlap no matcher fills through the KAS books, alone ...
    let mut v = vec![l_pair(2, T8, T8, pbid(4, T8, T8, 4 * WHOLE, 1_020, 0), 1_000)];
    v.extend(books());
    assert!(run_pair(v, &cfg()).prepared.is_empty(), "a route that loses on every fill is not built");
    // ... while the ASK pays through the KAS books; next to the ASK the BID is netted against it (the ASK sells A at 0.5, the
    // BID buys it at 1.02: the surplus B the BID pays beyond the ASK's receipt is sold into the bids of B, or taken by the ASK's
    // delivery), so both fill
    let mut v = vec![
        l_pair(X, T8, T8, pask(1, T8, T8, 4 * WHOLE, 500, 0), 1_000),
        l_pair(2, T8, T8, pbid(4, T8, T8, 4 * WHOLE, 1_020, 0), 1_000),
    ];
    v.extend(books());
    let r = run_pair(v, &cfg());
    assert_eq!((amount_in(&r, cid(X)), amount_in(&r, cid(2))), (4 * WHOLE, 4 * WHOLE));
    assert_eq!(r.prepared.len(), 1);
    // without any KAS book the two orders of one pair net (with tips that pay the fee) and with none they wait
    let net = |tip: i64| {
        vec![
            l_pair(X, T8, T8, pask(1, T8, T8, 4 * WHOLE, RATE, tip), 1_000),
            l_pair(2, T8, T8, pbid(4, T8, T8, 4 * WHOLE, RATE, tip), 1_000),
        ]
    };
    let r = run_pair(net(PTIP), &cfg());
    assert_eq!((amount_in(&r, cid(X)), amount_in(&r, cid(2))), (4 * WHOLE, 4 * WHOLE));
    assert!(run_pair(net(0), &cfg()).prepared.is_empty());
}

// ------------------------------------------------------------------ live soak regressions (pair asks)

/// Live TN10 soak (2026-10-01, TETH>TUSD): a v2.6 route bought B in whole ask lots and delivered the remainder of the last one
/// to the maker, so its largest fill could lose where a smaller one paid (29 whole A at 0.621 KAS needed 7.511 whole B: 8
/// whole lots at 2.329 = 18.63 KAS for 18.01 KAS). Protocol v3 buys exactly `ceil(n × rate / scale)` of B, so the route's margin
/// is linear in its fill (up to one base unit of rounding per leg): the largest fill pays when any does, and the same book
/// fills the whole pair order, through every time in force (a FOK one too), at exactly 7,511 base units of B.
#[test]
fn a_route_buys_exactly_its_b_so_the_largest_fill_pays() {
    let scene = |tif: i64| {
        let x = l_pair(X, T8, T8, pair_state(1, true, T8, T8, 29 * WHOLE, 259, 0, tif), 1_000);
        let x = if tif == TIF_GTC { x } else { fresh(x) };
        vec![x, l_bid_a(100, T8, 2, 62_000_000, 40 * WHOLE), l_ask_b(200, T8, T8, 3, 233_000_000, 10 * WHOLE)]
    };
    for tif in [TIF_GTC, TIF_IOC, TIF_FOK] {
        let r = run_pair(scene(tif), &cfg());
        assert_eq!((amount_in(&r, cid(X)), amount_in(&r, cid(200))), (29 * WHOLE, 7_511), "tif {tif}: the whole fill pays");
    }
}

/// Live TN10 soak (2026-10-01 10:59 UTC, TETH>TUSD; the book reduced to the orders that matter, prices as on chain, rates
/// scaled to the fixture tokens): an IOC pair ask of 26 whole A and resting ones of 15 and 32 sell A for B through one bid of A
/// (22 whole at 6.211 KAS) and two asks of B (5 whole at 23.276, 4 at 23.421). The IOC walked first and took the whole bid at
/// a loss, and the route was dropped whole; bounded to the largest fill that pays, the rest of the bid tempted the resting
/// pair orders into the same loss, so no single drop raised the profit and the batch was given up. In v3 the losing step is an
/// ask's minimum fill (one whole token): 22 whole A need 5,566 B, the cheap ask holds 5,000 and the 566 more would force a
/// whole token of the dearer ask. The largest fill that pays is the most the cheap ask covers, floor(5,000 x 1,000 / 253) =
/// 19,762 base units of A; on the 2,238 left of the bid either resting pair order would buy a whole B token for a fraction of
/// one and loses.
#[test]
fn live_an_ioc_pair_ask_is_not_lost_behind_resting_ones_on_one_bid() {
    let v = vec![
        fresh(l_pair(X, T8, T8, pair_state(1, true, T8, T8, 26 * WHOLE, 253, 0, TIF_IOC), 1_000)),
        l_pair(2, T8, T8, pask(4, T8, T8, 15 * WHOLE, 257, 0), 900),
        l_pair(3, T8, T8, pask(6, T8, T8, 32 * WHOLE, 262, 0), 950),
        l_bid_a(100, T8, 2, 621_100_000, 22 * WHOLE),
        l_ask_b(200, T8, T8, 3, 2_327_600_000, 5 * WHOLE),
        l_ask_b(201, T8, T8, 3, 2_342_130_000, 4 * WHOLE),
    ];
    let r = run_pair(v, &cfg());
    assert_eq!((amount_in(&r, cid(X)), amount_in(&r, cid(2)), amount_in(&r, cid(3))), (19_762, 0, 0));
    assert_eq!(r.prepared[0].plan.amount_of(&cid(X)), 19_762, "in the first transaction of the tick");
    assert_eq!((amount_in(&r, cid(200)), amount_in(&r, cid(201))), (5 * WHOLE, 0), "exactly the cheap ask");
}

/// Live TN10 soak (2026-10-02 02:58:52 to 02:59:20 UTC, TETH>TUSD `e7302001`; the book at DAA 585,769,450 delta-debugged to
/// one bid of A and asks of B, prices as on chain, rates scaled to the fixture tokens): the best pair order pays only up to what
/// the cheap ask of B covers. The rest of the bid goes to the next resting pair order, which loses on it (in v3: a whole token
/// of the dear ask, its minimum fill, for a fraction of one), and each further one takes the same rest at the same loss once the
/// one before is dropped. The profit step dropped at most 1 + 8 of these substitutes: with ten or more behind the paying one it
/// gave the batch up and nothing at all was built for 28 s (the plain crossings of the book starved with it).
#[test]
fn live_a_paying_pair_ask_is_not_lost_behind_ten_losing_ones_on_one_bid() {
    let scene = |losers: u32| {
        let mut v = vec![l_pair(X, T8, T8, pask(1, T8, T8, 15 * WHOLE, 267, 0), 1_000)];
        for k in 0..losers {
            // 268..272 B per whole A: each would pay alone up to what the cheap ask covers and loses on the bid's rest
            v.push(l_pair(10 + k, T8, T8, pask(10 + k as u8, T8, T8, 26 * WHOLE, 268 + (k as i64 % 5), 0), 900 + k as u64));
        }
        v.push(l_bid_a(100, T8, 2, 662_522_000, 12 * WHOLE));
        // the cheap ask covers floor(3,000 x 1,000 / 267) = 11,235 base units of A of the paying pair order; the dear one
        // (30 KAS per whole B, minimum fill one whole token) pays for nothing
        v.push(l_ask_b(200, T8, T8, 3, 2_393_340_000, 3 * WHOLE));
        v.push(l_ask_b(201, T8, T8, 3, 3_000_000_000, 4 * WHOLE));
        v
    };
    for losers in [9, 10, 16] {
        let r = run_pair(scene(losers), &cfg());
        assert_eq!(amount_in(&r, cid(X)), 11_235, "{losers} losing pair orders behind the paying one");
        assert!((0..losers).all(|k| amount_in(&r, cid(10 + k)) == 0), "{losers}: no losing pair order fills");
    }
}

/// Live TN10 soak (2026-10-03 10:41 UTC, TBTC>TUSD; the view at DAA 586,904,845, prices as on chain, rates scaled to the
/// fixture tokens: a whole TBTC of 4.01 KAS sells for 0.161 to 0.166 of a whole TUSD of 23.73 KAS). Ten pair orders had been
/// filled down to one whole token each, whose B (0.16 of a whole TUSD) cost a whole v2.6 lot: the standalone bound left all ten
/// out and the pair book stayed crossed by up to 4 % for hours. v3 buys exactly the B each delivery needs where the asks of B
/// allow small fills (minimum fill 1 base unit here): each pair order pays on its own, every one that fills buys exactly its
/// `ceil(n × rate / scale)` and no B is wasted.
#[test]
fn live_small_pair_order_remainders_buy_exactly_their_b() {
    let rates = [166, 163, 162, 163, 164, 166, 165, 161, 166, 164];
    let mut v: Vec<ListedOrder> = rates
        .iter()
        .enumerate()
        .map(|(k, &r)| l_pair(10 + k as u32, T8, T8, pask(10 + k as u8, T8, T8, WHOLE, r, 0), 900 + k as u64))
        .collect();
    v.push(l_bid_a(100, T8, 2, 401_390_000, 2 * WHOLE));
    v.push(l_bid_a(101, T8, 2, 400_940_000, 12 * WHOLE));
    for (id, price, amount) in [(200, 2_372_800_000, WHOLE), (201, 2_377_730_000, 3 * WHOLE)] {
        let mut a = l_ask_b(id, T8, T8, 3, price, amount);
        if let AnyState::KobAsk(s) = &mut a.order.state {
            s.min_fill = 1;
        }
        v.push(a);
    }
    let r = run_pair(v, &cfg());
    assert!(!r.prepared.is_empty(), "the remainders fill");
    let mut filled = 0;
    for p in &r.prepared {
        assert!(p.accounting.profit > 0);
        // per transaction: the asks of B sell exactly what the pair orders' deliveries need, no excess
        let pairs: Vec<usize> = (0..rates.len()).filter(|&k| p.plan.amount_of(&cid(10 + k as u32)) > 0).collect();
        let need: i64 = pairs.iter().map(|&k| rates[k]).sum();
        let bought = p.plan.amount_of(&cid(200)) + p.plan.amount_of(&cid(201));
        assert_eq!(bought, need, "exactly ceil(n x rate / scale) of B per pair order");
        filled += pairs.len();
    }
    for k in 0..rates.len() {
        assert!([0, WHOLE].contains(&amount_in(&r, cid(10 + k as u32))), "a pair order sells out or waits");
    }
    assert!(filled >= 6, "at least the six v2.6 filled through one shared lot: {filled}");
}

/// Live TN10 soak (2026-10-03 10:41 UTC, TUSD>TBTC; the view at DAA 586,904,845, prices as on chain, rates scaled to the fixture
/// tokens: a whole TUSD of 23.7 KAS buys 5.76 whole TBTC of 4.02 KAS). The soak bots sized pair orders at a fixed rate so that
/// the delivery landed a sliver ABOVE a whole v2.6 lot of B (4 whole A for 23.04 lots of B): a fill bought 24 lots and the 0.96
/// lot of waste exceeded the route's spread, so alone neither paid and only together (47 lots for 46.1) both filled. v3 buys
/// exactly 23,040 and 23,060 base units of B: alone each pays, together both fill with exactly 46,100.
#[test]
fn live_pair_orders_a_sliver_above_whole_tokens_of_b_fill_alone_and_together() {
    let books = || {
        vec![
            l_bid_a(100, T8, 2, 2_370_910_000, WHOLE),
            l_bid_a(101, T8, 2, 2_369_180_000, 3 * WHOLE),
            l_bid_a(102, T8, 2, 2_368_240_000, 5 * WHOLE),
            l_ask_b(200, T8, T8, 3, 401_550_000, 5 * WHOLE),
            l_ask_b(201, T8, T8, 3, 401_940_000, 22 * WHOLE),
            l_ask_b(202, T8, T8, 3, 402_100_000, 11 * WHOLE),
            l_ask_b(203, T8, T8, 3, 402_260_000, 12 * WHOLE),
        ]
    };
    let bought = |r: &TickReport| -> i64 { (200..204).map(|k| amount_in(r, cid(k))).sum() };
    // alone: each pays, with exactly its B
    for (rate, need) in [(5_760, 23_040), (5_765, 23_060)] {
        let mut v = vec![l_pair(X, T8, T8, pask(1, T8, T8, 4 * WHOLE, rate, 0), 1_000)];
        v.extend(books());
        let r = run_pair(v, &cfg());
        assert_eq!(amount_in(&r, cid(X)), 4 * WHOLE, "rate {rate} alone");
        assert_eq!(bought(&r), need, "rate {rate}: exactly ceil(4,000 x rate / 1,000) of B");
    }
    let mut v = vec![
        l_pair(X, T8, T8, pask(1, T8, T8, 4 * WHOLE, 5_760, 0), 1_000),
        l_pair(2, T8, T8, pask(4, T8, T8, 4 * WHOLE, 5_765, 0), 1_001),
    ];
    v.extend(books());
    let r = run_pair(v, &cfg());
    assert_eq!((amount_in(&r, cid(X)), amount_in(&r, cid(2))), (4 * WHOLE, 4 * WHOLE));
    assert_eq!(bought(&r), 46_100, "4 x 5.76 + 4 x 5.765 = 46.1 whole B, exactly");
    assert!(r.prepared[0].accounting.profit > 0);
}
