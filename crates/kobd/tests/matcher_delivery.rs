//! Bid delivery modes in the matcher (`docs/design/bid-delivery-spec.md` §4, §6): a mode-1 bid's fills spend and grow
//! its bought-custody (one token input, counted by the planner), a mode-2 bid's fill tops up the maker's receive UTXO
//! only when its `topupTip` pays the measured extra fee, one top-up per token at its lowest-index delivering leg, form 1
//! when the slots or the threshold forbid it, and the successors of a chained step are the next step's inputs. Every
//! transaction is validated in the rusty-kaspa v2.1.0 engine.

#[path = "matcher_common/mod.rs"]
mod common;

use std::collections::{BTreeMap, BTreeSet};

use common::*;
use kob_protocol::state::*;
use kob_protocol::tx::{SignedTx, TokenUtxo};
use kobd::matcher::book::{outpoint, topup_receive, ListedOrder, Outpoint};
use kobd::matcher::chain::{apply, UNACCEPTED_DAA};
use kobd::matcher::engine::{is_topup_input, tick_with, Prepared};
use kobd::matcher::family::Families;

fn token_inputs(p: &Prepared) -> usize {
    p.signed.tx.inputs.iter().filter(|i| i.utxo.covenant_id == Some(TOKEN)).count()
}

fn spends(p: &Prepared, op: Outpoint) -> bool {
    p.signed.tx.inputs.iter().any(|i| (i.transaction_id, i.index) == op)
}

fn topups(p: &Prepared) -> Vec<Outpoint> {
    let ins = &p.signed.tx.inputs;
    (0..ins.len()).filter(|i| is_topup_input(&p.lowered, *i)).map(|i| (ins[i].transaction_id, ins[i].index)).collect()
}

fn slots() -> usize {
    T3.token_slots().expect("KCC20Ref slots").0
}

/// A mode-1 bid costs one token input once it holds its bought-custody: with two asks it fills the program's three
/// inputs, and the next step of the tick fills its unaccepted continuation into the grown custody. Before its first fill
/// it costs none, and the first fill creates the custody the next step spends.
#[test]
fn a_mode1_bid_counts_its_custody_input_and_chains_it() {
    assert_eq!(slots(), 3);
    for bought in [0, 2 * WHOLE] {
        let mut orders = vec![m1_bid(1, 1, P260, 8 * WHOLE, bought)];
        orders.extend((0..4).map(|k| l_ask(10 + k, ask(10 + k as u8, P250, 2 * WHOLE, T3))));
        let inp = input(&book(orders));
        let r = run(&inp, &cfg());
        assert!(r.prepared.len() >= 2, "bought {bought}: the crossing takes two chained steps");
        for p in &r.prepared {
            assert!(token_inputs(p) <= slots(), "bought {bought}: {} token inputs", token_inputs(p));
        }
        // step 0 spends the listed custody (when there is one), step 1 the custody step 0 created
        let (p0, p1) = (&r.prepared[0], &r.prepared[1]);
        if let Some(c) = &inp.orders[0].bought_custody {
            assert!(spends(p0, outpoint(&c.utxo)), "the first fill spends the bought-custody");
        }
        assert!(p1.plan.amount_of(&cid(1)) > 0, "bought {bought}: the continuation fills again");
        assert!(
            p1.signed.tx.inputs.iter().any(|i| i.transaction_id == p0.txid() && i.utxo.covenant_id == Some(TOKEN)),
            "bought {bought}: step 1 spends the bought-custody step 0 created"
        );
        assert_eq!(amount_all(&r, cid(1)), 8 * WHOLE);
    }
    assert_no_budget_slack();
}

/// The chained book after a mode-1 fill: the continuation's `bought` grew by the fill, and its bought-custody is the
/// delivery output, exactly `bought`, unaccepted.
#[test]
fn a_chained_step_lists_the_grown_bought_custody() {
    let orders = vec![m1_bid(1, 1, P260, 8 * WHOLE, 2 * WHOLE), l_ask(10, ask(2, P250, 3 * WHOLE, T3))];
    let inp = input(&book(orders));
    let r = run(&inp, &cfg());
    let p = &r.prepared[0];
    let mut by_id: BTreeMap<[u8; 32], ListedOrder> = inp.orders.iter().map(|o| (o.id(), o.clone())).collect();
    let (continued, _) = apply(&mut by_id, &p.plan, &p.lowered.built, pk(MATCHER));
    assert!(continued.contains(&cid(1)));
    let o = &by_id[&cid(1)];
    assert_eq!(o.order.state.delivery().unwrap().bought, 5 * WHOLE);
    let c = o.bought_custody.as_ref().expect("the grown bought-custody");
    assert_eq!((c.state.amount(), c.utxo.transaction_id, c.utxo.block_daa_score), (5 * WHOLE, p.txid(), UNACCEPTED_DAA));
    assert!(o.custody_ok(), "the continuation lists exactly its bought-custody");
}

/// A mode-2 fill tops up the maker's receive UTXO when the tip pays the measured extra fee; with a tip one sompi short
/// of it, or at a fee rate the default tip no longer pays, the fill delivers a new receive UTXO (form 1).
#[test]
fn a_top_up_only_when_the_tip_pays_its_extra_fee() {
    let extra = kob_protocol::budget::topup_extra(T3).unwrap() as i64;
    let case = |tip: i64, rate: u64| {
        let r0 = receive(1, 5 * WHOLE, 500);
        let orders = vec![m2_bid(1, 1, P260, 3 * WHOLE, 500, tip, vec![r0.clone()]), l_ask(10, ask(2, P250, 3 * WHOLE, T3))];
        let mut c = cfg();
        c.planner.fee_rate = rate;
        let r = run(&input(&book(orders)), &c);
        let p = &r.prepared[0];
        assert_eq!(p.plan.amount_of(&cid(1)), 3 * WHOLE);
        (topups(p) == vec![outpoint(&r0.utxo)], topups(p).is_empty())
    };
    assert_eq!(case(default_tip(), 100), (true, false), "the default tip pays the top-up");
    assert_eq!(case(extra, 100), (true, false), "a tip of exactly the extra fee pays it");
    assert_eq!(case(extra - 1, 100), (false, true), "a tip short of the extra fee: form 1");
    assert_eq!(case(default_tip(), 200), (false, true), "at twice the floor rate the default tip is short: form 1");
    assert_no_budget_slack();
}

/// One top-up per token: two mode-2 bids of different makers with receive UTXOs fill in one transaction, one of them
/// topped up at its token's first input, the other delivering a new receive UTXO.
#[test]
fn one_top_up_per_token() {
    let (ra, rb) = (receive(1, 4 * WHOLE, 500), receive(3, 4 * WHOLE, 500));
    let orders = vec![
        m2_bid(1, 1, P260, 2 * WHOLE, 500, default_tip(), vec![ra]),
        m2_bid(2, 3, P260, 2 * WHOLE, 500, default_tip(), vec![rb]),
        l_ask(10, ask(2, P250, 4 * WHOLE, T3)),
    ];
    let r = run(&input(&book(orders)), &cfg());
    let p = &r.prepared[0];
    assert_eq!((p.plan.amount_of(&cid(1)), p.plan.amount_of(&cid(2))), (2 * WHOLE, 2 * WHOLE));
    assert_eq!(topups(p).len(), 1, "exactly one top-up for the token");
    let first_tok = p.signed.tx.inputs.iter().position(|i| i.utxo.covenant_id == Some(TOKEN)).unwrap();
    assert!(is_topup_input(&p.lowered, first_tok), "the receive UTXO is the token's first input");
}

/// Bid-delivery-spec §6.5: on a 3 / 3 program one top-up and one mode-1 bid against one resting ask fill in one
/// transaction (receive UTXO, bought-custody, ask custody). With two mode-1 bids holding custodies the inputs are full:
/// the mode-2 bid delivers a new receive UTXO.
#[test]
fn the_receive_input_takes_a_free_slot_only() {
    let r1 = receive(1, 4 * WHOLE, 500);
    let orders = vec![
        m2_bid(1, 1, P260, 2 * WHOLE, 500, default_tip(), vec![r1.clone()]),
        m1_bid(2, 3, P260, 2 * WHOLE, WHOLE),
        l_ask(10, ask(2, P250, 6 * WHOLE, T3)),
    ];
    let r = run(&input(&book(orders)), &cfg());
    let p = &r.prepared[0];
    assert_eq!((p.plan.amount_of(&cid(1)), p.plan.amount_of(&cid(2))), (2 * WHOLE, 2 * WHOLE));
    assert_eq!((topups(p), token_inputs(p)), (vec![outpoint(&r1.utxo)], 3));

    let orders = vec![
        m2_bid(1, 1, P260, 2 * WHOLE, 500, default_tip(), vec![r1]),
        m1_bid(2, 3, P260, 2 * WHOLE, WHOLE),
        m1_bid(3, 4, P260, 2 * WHOLE, WHOLE),
        l_ask(10, ask(2, P250, 6 * WHOLE, T3)),
    ];
    let r = run(&input(&book(orders)), &cfg());
    for p in &r.prepared {
        assert!(token_inputs(p) <= slots());
        if p.plan.amount_of(&cid(1)) > 0 && token_inputs(p) == slots() {
            assert!(topups(p).is_empty(), "no slot for the receive UTXO: form 1");
        }
    }
    assert_eq!(amount_all(&r, cid(1)), 2 * WHOLE);
}

/// The receive UTXO a fill tops up: the largest of the maker's with a threshold of at least `recvThreshold` and below
/// the fill, the lowest outpoint on a tie; never another key's, another commitment's or a plain UTXO.
#[test]
fn the_top_up_target_is_the_largest_usable_receive_utxo() {
    let mut other_ext = receive(1, 99 * WHOLE, 500);
    if let TokenState::Kcc20(k) = &mut other_ext.state {
        k.extension_commitment = [0x11; 32];
    }
    let plain = TokenUtxo { state: TokenState::from(Kcc20State::p2pk(99 * WHOLE, pk(1), EXT)), ..receive(1, 0, 0) };
    let low_thr = receive(1, 50 * WHOLE, 100); // below recvThreshold 500
    let high_thr = receive(1, 60 * WHOLE, 2_500); // not below a fill of 2,000
    let not_maker = receive(3, 70 * WHOLE, 500);
    let (a, b) = (receive(1, 7 * WHOLE, 500), receive(1, 7 * WHOLE, 600));
    let all = vec![other_ext, plain, low_thr, high_thr.clone(), not_maker, a.clone(), b.clone()];
    let o = m2_bid(1, 1, P260, 2 * WHOLE, 500, default_tip(), all);
    let want = if outpoint(&a.utxo) < outpoint(&b.utxo) { &a } else { &b };
    assert_eq!(topup_receive(&o, 2 * WHOLE).map(|r| outpoint(&r.utxo)), Some(outpoint(&want.utxo)));
    assert_eq!(topup_receive(&o, 3 * WHOLE).map(|r| outpoint(&r.utxo)), Some(outpoint(&high_thr.utxo)));
    assert_eq!(topup_receive(&o, 500), None, "a fill of the threshold cannot top up (§6.6)");
    let m1 = BidState { mode: MODE_ACCUMULATE, recv_threshold: 0, topup_tip: 0, ..bid(1, P260, T3) };
    let o1 = ListedOrder { order: kob_protocol::tx::OrderUtxo { state: AnyState::KobBid(m1), ..o.order.clone() }, ..o.clone() };
    assert_eq!(topup_receive(&o1, 2 * WHOLE), None, "only a mode-2 order tops up");
}

/// The chained book after a top-up: every order of the maker that listed the receive UTXO lists its successor, the
/// delivery output of the topped-up leg holding `have + n`, unaccepted.
#[test]
fn a_chained_step_lists_the_receive_successor() {
    let r0 = receive(1, 4 * WHOLE, 500);
    let orders = vec![
        m2_bid(1, 1, P260, 2 * WHOLE, 500, default_tip(), vec![r0.clone()]),
        m2_bid(2, 1, P245, 2 * WHOLE, 500, default_tip(), vec![r0.clone()]),
        l_ask(10, ask(2, P250, 2 * WHOLE, T3)),
    ];
    let inp = input(&book(orders));
    let r = run(&inp, &cfg());
    let p = &r.prepared[0];
    assert_eq!(topups(p), vec![outpoint(&r0.utxo)]);
    let mut by_id: BTreeMap<[u8; 32], ListedOrder> = inp.orders.iter().map(|o| (o.id(), o.clone())).collect();
    apply(&mut by_id, &p.plan, &p.lowered.built, pk(MATCHER));
    let next = &by_id[&cid(2)].receive;
    assert_eq!(next.len(), 1);
    assert_eq!(
        (next[0].utxo.transaction_id, next[0].state.amount(), next[0].utxo.block_daa_score),
        (p.txid(), 6 * WHOLE, UNACCEPTED_DAA)
    );
    assert_eq!(next[0].state.borrow(), r0.state.borrow(), "the successor keeps the receive settings");
}

/// A token program refusing the receive UTXO's input in the pre-simulation rebuilds the batch with form 1: the bid
/// still fills, as a new receive UTXO, and no order is suspected.
#[test]
fn a_refused_top_up_is_rebuilt_with_form_1() {
    let r0 = receive(1, 4 * WHOLE, 500);
    let orders = vec![m2_bid(1, 1, P260, 2 * WHOLE, 500, default_tip(), vec![r0.clone()]), l_ask(10, ask(2, P250, 2 * WHOLE, T3))];
    let inp = input(&book(orders));
    let op = outpoint(&r0.utxo);
    let refuse = |s: &SignedTx| match s.tx.inputs.iter().position(|i| (i.transaction_id, i.index) == op) {
        Some(i) => Err(format!("input {i}: the token program refused the borrow path")),
        None => kob_protocol::verify::validate_signed(s).map_err(|e| e.to_string()),
    };
    let r = tick_with(&inp, &cfg(), &Families::default(), &signer(), &refuse);
    assert!(r.suspects.is_empty(), "{:?}", r.suspects);
    let p = &r.prepared[0];
    assert_eq!(p.plan.amount_of(&cid(1)), 2 * WHOLE);
    assert!(topups(p).is_empty() && !spends(p, op));
    let ids: BTreeSet<[u8; 32]> = p.plan.fills.iter().map(|f| f.cand.id).collect();
    assert!(ids.contains(&cid(1)));
}
