//! Security test of the maintenance jobs (`kobd::maintenance`): a merge never puts the operator's
//! funding into a token carrier. A KRON merge's funding input is only the presence and comes back
//! whole; the merged carrier is what the merged carriers pay after the fee, or there is no merge.

#[path = "matcher_common/mod.rs"]
mod common;

use std::collections::BTreeSet;

use common::pair::*;
use common::*;
use kob_protocol::artifacts::TemplateId;
use kob_protocol::tx::*;
use kobd::maintenance::{tick, MaintKind, MaintenanceConfig, MaintenanceInput, OwnToken};
use kobd::matcher::book::Clock;

const K3: TemplateId = TemplateId::KronToken2433;

/// An operator token UTXO of program `p` holding `amount` base units on a `carrier` sompi carrier.
fn own(p: TemplateId, amount: i64, carrier: u64) -> OwnToken {
    OwnToken {
        program: p,
        token: TokenUtxo { utxo: utxo(carrier, 900, Some(token_a(p))), state: tstate(p, amount, pk(MATCHER), false) },
    }
}

fn input(tokens: Vec<OwnToken>, funding: Vec<KeyUtxo>) -> MaintenanceInput {
    MaintenanceInput {
        tokens,
        orders: vec![],
        clock: Clock { daa: NOW + 5, utc: UTC },
        funding,
        excluded: BTreeSet::new(),
        excluded_outpoints: BTreeSet::new(),
    }
}

fn change(j: &kobd::maintenance::MaintJob) -> u64 {
    j.built.fee.change_output.map(|i| j.built.tx.outputs[i as usize].value).unwrap_or(0)
}

#[test]
fn a_kron_merge_of_small_carriers_takes_nothing_from_the_funding() {
    let cfg = MaintenanceConfig { sell: false, ..Default::default() };
    // two junk KRON holdings on 0.5 KAS carriers: the merge would want a 10 KAS carrier
    let tokens = vec![own(K3, 10, KAS / 2), own(K3, 20, KAS / 2)];
    let r = tick(&input(tokens, vec![funding(50 * KAS), funding(3 * KAS)]), &cfg, &signer());
    assert_eq!(r.jobs.len(), 1, "{:?}", r.skipped);
    let j = &r.jobs[0];
    assert_eq!(j.kind, MaintKind::Merge);
    assert!(j.validation.is_some());
    let fee = j.built.fee.fee;
    // the presence coin (3 KAS) comes back whole, the fee and the carrier out of the 1 KAS of carriers
    assert!(change(j) >= 3 * KAS, "change {} below the funding", change(j));
    let carrier = (KAS - fee) as i64 - (change(j) - 3 * KAS) as i64;
    assert!(carrier > 0 && carrier as u64 <= KAS - fee, "carrier {carrier}, fee {fee}");
    assert_eq!(j.released, KAS as i64 - carrier);
    assert_eq!(j.profit, -(fee as i64), "the operator pays only the fee");
    // carriers too small to pay a covenant output and the fee: no merge at all
    let dust = COVENANT_OUTPUT_MIN / 2;
    let r = tick(&input(vec![own(K3, 10, dust), own(K3, 20, dust)], vec![funding(50 * KAS)]), &cfg, &signer());
    assert!(r.jobs.is_empty());
    assert!(r.skipped.iter().any(|(_, why)| why.contains("cannot pay")), "{:?}", r.skipped);
}

#[test]
fn a_merge_of_full_carriers_keeps_the_configured_carrier() {
    let cfg = MaintenanceConfig { sell: false, ..Default::default() };
    let r = tick(&input(vec![own(K3, 10, CARRIER), own(K3, 20, CARRIER)], vec![funding(3 * KAS)]), &cfg, &signer());
    assert_eq!(r.jobs.len(), 1, "{:?}", r.skipped);
    assert_eq!(r.jobs[0].released, CARRIER as i64, "one 10 KAS carrier kept, one freed");
    // KCC-20 (no funding input): the same rule
    let r = tick(&input(vec![own(T8, 1, KAS), own(T8, 2, KAS)], vec![]), &cfg, &signer());
    assert_eq!(r.jobs.len(), 1, "{:?}", r.skipped);
    let j = &r.jobs[0];
    assert!(j.released > 0 && j.released < 2 * KAS as i64, "merged onto what the carriers pay: released {}", j.released);
}

/// Security review 2026-11-19 E-M1: a sale's rest never gets a carrier out of the operator's funding. A junk KRON token
/// sent to the operator on a small carrier, a small bid for part of it and an ask to pass the price floor: the rest's
/// carrier is what the sold carrier and the proceeds pay after the fee, and the funding (the presence) comes back whole.
#[test]
fn a_kron_sale_leaves_its_rest_on_what_the_sale_pays() {
    let cfg = MaintenanceConfig { merge: false, sell: true, ..Default::default() };
    // 2 whole junk tokens on a 0.5 KAS carrier; a bid for 1 whole at 2.60 (pays about 2.6 KAS), an ask at 2.70
    let bid = l_bid_a(100, K3, 2, P260, WHOLE);
    let ask = l_ask_a(300, K3, 9, 270_000_000, 10 * WHOLE);
    let mut inp = input(vec![own(K3, 2 * WHOLE, KAS / 2)], vec![funding(50 * KAS)]);
    inp.orders = vec![bid, ask];
    let r = tick(&inp, &cfg, &signer());
    assert_eq!(r.jobs.len(), 1, "{:?}", r.skipped);
    let j = &r.jobs[0];
    assert_eq!(j.kind, MaintKind::Sell);
    assert!(j.validation.is_some());
    assert!(change(j) >= 50 * KAS, "change {} below the 50 KAS of funding", change(j));
    // the rest's carrier is below the configured 10 KAS: at most the 0.5 KAS carrier and the proceeds
    let rest = KAS as i64 / 2 - j.released;
    assert!(rest > 0 && (rest as u64) < 4 * KAS, "the rest's carrier: {rest}");
    // a rest the sale cannot carry at all: no sale
    let dust = COVENANT_OUTPUT_MIN / 4;
    let tiny = l_bid_a(101, K3, 2, 1, WHOLE); // pays almost nothing
    let mut inp = input(vec![own(K3, 2 * WHOLE, dust)], vec![funding(50 * KAS)]);
    inp.orders = vec![tiny, l_ask_a(301, K3, 9, 1, 10 * WHOLE)];
    let r = tick(&inp, &cfg, &signer());
    assert!(r.jobs.iter().all(|j| change(j) >= 50 * KAS), "{:?}", r.skipped);
}

/// Audit 2026-11-23 D-1: the sale's price floor counts only asks that can deliver the amount. An all-or-nothing ask (its
/// minimum fill above the holding) posted at a low price next to the poster's own bid does not lower the floor; the same
/// ask that a buyer of the holding can take does.
#[test]
fn an_all_or_nothing_ask_does_not_set_the_sale_floor() {
    let cfg = MaintenanceConfig { merge: false, sell: true, ..Default::default() };
    let sale = |min_fill: i64| {
        let bid = l_bid_a(100, K3, 2, 250_000_000, 2 * WHOLE);
        let ask = l_ask_with(300, token_a(K3), K3, 2, 270_000_000, 10 * WHOLE, |a| a.min_fill = min_fill);
        let mut inp = input(vec![own(K3, 2 * WHOLE, CARRIER)], vec![funding(50 * KAS)]);
        inp.orders = vec![bid, ask];
        tick(&inp, &cfg, &signer())
    };
    let r = sale(10 * WHOLE);
    assert!(r.jobs.iter().all(|j| j.kind != MaintKind::Sell), "sold against an ask no buyer of 2 can take: {:?}", r.jobs.len());
    let r = sale(WHOLE);
    assert!(r.jobs.iter().any(|j| j.kind == MaintKind::Sell), "{:?}", r.skipped);
}

/// R2D-1: an all-or-nothing ask of exactly the operator's holding offers the whole amount, but only to a buyer of exactly
/// that amount: the poster's own sale. It does not set the floor, nor does an ask whose fill sizes range over less than
/// half the holding, or a stack of all-or-nothing asks. One that buyers of many sizes can take does.
#[test]
fn an_exact_size_all_or_nothing_ask_does_not_set_the_sale_floor() {
    let cfg = MaintenanceConfig { merge: false, sell: true, ..Default::default() };
    let sale = |amount: i64, min_fill: i64| {
        let bid = l_bid_a(100, K3, 2, 250_000_000, 2 * WHOLE);
        let ask = l_ask_with(300, token_a(K3), K3, 2, 270_000_000, amount, |a| a.min_fill = min_fill);
        let mut inp = input(vec![own(K3, 2 * WHOLE, CARRIER)], vec![funding(50 * KAS)]);
        inp.orders = vec![bid, ask];
        tick(&inp, &cfg, &signer())
    };
    let r = sale(2 * WHOLE, 2 * WHOLE);
    assert!(r.jobs.iter().all(|j| j.kind != MaintKind::Sell), "sold against an exact-size all-or-nothing ask");
    let r = sale(2 * WHOLE + 1, 2 * WHOLE);
    assert!(r.jobs.iter().all(|j| j.kind != MaintKind::Sell), "sold against an ask for buyers of 2 whole tokens or one unit more");
    let r = sale(2 * WHOLE, WHOLE / 5);
    assert!(r.jobs.iter().any(|j| j.kind == MaintKind::Sell), "{:?}", r.skipped);
    // a stack of all-or-nothing asks that together hold the amount: each sells to a buyer of exactly its size only
    let bid = l_bid_a(100, K3, 2, 250_000_000, 2 * WHOLE);
    let aon = |id| l_ask_with(id, token_a(K3), K3, 2, 270_000_000, WHOLE, |a| a.min_fill = WHOLE);
    let mut inp = input(vec![own(K3, 2 * WHOLE, CARRIER)], vec![funding(50 * KAS)]);
    inp.orders = vec![bid, aon(301), aon(302)];
    let r = tick(&inp, &cfg, &signer());
    assert!(r.jobs.iter().all(|j| j.kind != MaintKind::Sell), "sold against a stack of all-or-nothing asks");
}

/// R3D-1: the fill-size range is measured on what one fill may take. An ask of 1.5 times the holding whose `maxFill`
/// equals its minimum fill sells to a buyer of exactly that size only, so it does not set the floor; the same ask that
/// takes fills from a fifth of the holding does.
#[test]
fn a_max_fill_capped_ask_does_not_set_the_sale_floor() {
    let cfg = MaintenanceConfig { merge: false, sell: true, ..Default::default() };
    let sale = |min_fill: i64| {
        let bid = l_bid_a(100, K3, 2, 250_000_000, 2 * WHOLE);
        let ask = l_ask_with(300, token_a(K3), K3, 2, 270_000_000, 3 * WHOLE, |a| {
            a.min_fill = min_fill;
            a.max_fill = 2 * WHOLE;
        });
        let mut inp = input(vec![own(K3, 2 * WHOLE, CARRIER)], vec![funding(50 * KAS)]);
        inp.orders = vec![bid, ask];
        tick(&inp, &cfg, &signer())
    };
    let r = sale(2 * WHOLE);
    assert!(r.jobs.iter().all(|j| j.kind != MaintKind::Sell), "sold against an ask for buyers of exactly 2 whole tokens");
    let r = sale(WHOLE / 5);
    assert!(r.jobs.iter().any(|j| j.kind == MaintKind::Sell), "{:?}", r.skipped);
}

/// R3D-1: an ask counts for the floor only once its UTXO is `ASK_REFERENCE_AGE` DAA old. An honest ask placed (or last
/// filled or amended) less than a minute ago is ignored, so there is no sale; the same ask a minute old sets the floor.
#[test]
fn a_fresh_ask_does_not_set_the_sale_floor() {
    let cfg = MaintenanceConfig { merge: false, sell: true, ..Default::default() };
    let sale = |age: u64| {
        let bid = l_bid_a(100, K3, 2, 250_000_000, 2 * WHOLE);
        let mut ask = l_ask_with(300, token_a(K3), K3, 2, 270_000_000, 10 * WHOLE, |a| a.min_fill = WHOLE / 5);
        let inp0 = input(vec![], vec![]);
        ask.order.utxo.block_daa_score = inp0.clock.daa - age;
        let mut inp = input(vec![own(K3, 2 * WHOLE, CARRIER)], vec![funding(50 * KAS)]);
        inp.orders = vec![bid, ask];
        tick(&inp, &cfg, &signer())
    };
    let r = sale(kobd::maintenance::ASK_REFERENCE_AGE - 1);
    assert!(r.jobs.iter().all(|j| j.kind != MaintKind::Sell), "sold against an ask younger than a minute");
    assert!(r.skipped.iter().any(|(_, why)| why.contains("no resting asks")), "{:?}", r.skipped);
    let r = sale(kobd::maintenance::ASK_REFERENCE_AGE);
    assert!(r.jobs.iter().any(|j| j.kind == MaintKind::Sell), "{:?}", r.skipped);
}

// R4D-1: an ask counts toward the sale floor only once it has been open to buyers at the price it now offers for
// `ASK_REFERENCE_AGE` DAA, not once its UTXO is that old. Each test below is one order kind or field that could make an
// ask look older or more fillable than it was (docs/ops/executor.md, "The sale's price floor"), in both families.

/// The input's clock (DAA score).
const DAA: u64 = NOW + 5;
const AGE: u64 = kobd::maintenance::ASK_REFERENCE_AGE;

/// Whether the operator's 2 whole tokens of program `p` are sold into a bid at 2.50 when `asks` are the book's asks. With
/// no ask that counts there is no reference and so no sale: a sale means one of `asks` counted.
fn sells(p: TemplateId, asks: Vec<kobd::matcher::book::ListedOrder>) -> bool {
    let cfg = MaintenanceConfig { merge: false, sell: true, ..Default::default() };
    let mut inp = input(vec![own(p, 2 * WHOLE, CARRIER)], vec![funding(50 * KAS)]);
    inp.orders = vec![l_bid_a(100, p, 2, 250_000_000, 2 * WHOLE)];
    inp.orders.extend(asks);
    tick(&inp, &cfg, &signer()).jobs.iter().any(|j| j.kind == MaintKind::Sell)
}

/// An honest GTC ask at 2.70 of 10 whole tokens taking fills from a fifth of a token, its UTXO and custody accepted at
/// `daa`, changed by `f`.
fn honest_at(p: TemplateId, daa: u64, f: impl FnOnce(&mut kob_protocol::state::AskState)) -> kobd::matcher::book::ListedOrder {
    let mut o = l_ask_with(300, token_a(p), p, 2, 270_000_000, 10 * WHOLE, |a| {
        a.min_fill = WHOLE / 5;
        f(a)
    });
    o.order.utxo.block_daa_score = daa;
    if let Some(c) = &mut o.custody {
        c.utxo.block_daa_score = daa;
    }
    o
}

/// Both families: the KCC-20 reference program and the KRON one.
const PROGRAMS: [TemplateId; 2] = [T3, K3];

#[test]
fn an_old_honest_ask_still_sets_the_sale_floor() {
    for p in PROGRAMS {
        assert!(sells(p, vec![honest_at(p, 1_000, |_| {})]), "{p:?}");
        assert!(sells(p, vec![honest_at(p, DAA - AGE, |_| {})]), "{p:?}: open exactly a minute");
        assert!(!sells(p, vec![honest_at(p, DAA - AGE + 1, |_| {})]), "{p:?}: open less than a minute");
    }
}

/// Variant A of R4D-1: the UTXO is a minute old, but `activeFrom` kept every buyer out until now.
#[test]
fn an_ask_that_activates_late_does_not_set_the_sale_floor() {
    for p in PROGRAMS {
        let t = (DAA - 5) as i64; // the sale's lock time
        assert!(!sells(p, vec![honest_at(p, DAA - AGE, |a| a.active_from = t)]), "{p:?}: active only now");
        assert!(!sells(p, vec![honest_at(p, 1_000, |a| a.active_from = (DAA - AGE + 1) as i64)]), "{p:?}");
        assert!(sells(p, vec![honest_at(p, 1_000, |a| a.active_from = (DAA - AGE) as i64)]), "{p:?}: active a minute");
    }
}

/// Variant B: a TWAP slice is fillable only `interval` DAA after its UTXO, so the minute starts there.
#[test]
fn a_twap_slice_counts_a_minute_after_it_opens() {
    for p in PROGRAMS {
        let twap = |age: u64| honest_at(p, DAA - age, |a| a.interval = AGE as i64);
        assert!(!sells(p, vec![twap(AGE + 5)]), "{p:?}: the slice opened at the sale's lock time");
        assert!(!sells(p, vec![twap(2 * AGE - 1)]), "{p:?}");
        assert!(sells(p, vec![twap(2 * AGE)]), "{p:?}: the slice open a minute");
    }
}

/// Variant C: a decaying ask (auction) quotes less every step, so its present price was not on offer a minute ago. It
/// never counts, even once it rests at its end price.
#[test]
fn a_decaying_ask_does_not_set_the_sale_floor() {
    for p in PROGRAMS {
        // from 27.00 down to 2.70, reached exactly at the sale's lock time (origin activeFrom = 0, step 1,000 DAA)
        let t = (DAA - 5) as i64;
        let decay = |end_at: i64| {
            honest_at(p, 1_000, |a| {
                a.price = 2_700_000_000;
                a.price_end = 270_000_000;
                a.slope = (a.price - a.price_end) / (end_at / a.decay_step);
            })
        };
        assert!(!sells(p, vec![decay(t)]), "{p:?}: at 2.70 only from now");
        assert!(!sells(p, vec![decay(t / 2)]), "{p:?}: an auction resting at its end price");
    }
}

/// The covenants read the custody's DAA score for an ask's exposure: a custody newer than the order UTXO starts the minute.
#[test]
fn an_ask_counts_from_its_newer_custody() {
    for p in PROGRAMS {
        let with_custody = |cdaa: u64| {
            let mut o = honest_at(p, 1_000, |_| {});
            o.custody.as_mut().expect("custody").utxo.block_daa_score = cdaa;
            o
        };
        assert!(!sells(p, vec![with_custody(DAA - AGE + 1)]), "{p:?}");
        assert!(sells(p, vec![with_custody(DAA - AGE)]), "{p:?}");
    }
}

/// An ask the node has not accepted (mempool, DAA score unknown) has no exposure at all.
#[test]
fn an_unaccepted_ask_does_not_set_the_sale_floor() {
    for p in PROGRAMS {
        let mut o = honest_at(p, 1_000, |_| {});
        o.order.utxo.block_daa_score = u64::MAX;
        assert!(!sells(p, vec![o]), "{p:?}");
    }
}

/// An ask past its expiry (soft expiry, its UTC deadline) is no longer on offer, however long it was.
#[test]
fn an_expired_ask_does_not_set_the_sale_floor() {
    for p in PROGRAMS {
        assert!(!sells(p, vec![honest_at(p, 1_000, |a| a.expiry_daa = (DAA - 5) as i64)]), "{p:?}: soft expiry");
        let mut o = honest_at(p, 1_000, |_| {});
        o.deadline = Some(UTC);
        assert!(!sells(p, vec![o]), "{p:?}: UTC deadline");
    }
}

/// A FOK ask, even one of exactly the holding that takes small minimum fills, sells to one buyer size only.
#[test]
fn a_fok_ask_does_not_set_the_sale_floor() {
    for p in PROGRAMS {
        let mut o = l_ask_with(300, token_a(p), p, 2, 270_000_000, 2 * WHOLE, |a| {
            a.min_fill = WHOLE / 5;
            a.tif = kob_protocol::state::TIF_FOK;
        });
        o.order.utxo.block_daa_score = 1_000;
        assert!(!sells(p, vec![o]), "{p:?}");
    }
}

/// Only plain asks count: a conditional ask (take-profit / stop, also as a booked exit that merges its entry), an
/// if-done entry, a curve and a pair ask at the same low price leave no reference.
#[test]
fn only_plain_asks_set_the_sale_floor() {
    let twin = |p: TemplateId, o: kobd::matcher::book::ListedOrder| if p == K3 { kron_order(&o) } else { o };
    for p in PROGRAMS {
        let cond = twin(p, l_cond_ask(301, cond_ask(2, 270_000_000, 0, 10 * WHOLE, T3), 1_000));
        let stop = twin(p, l_cond_ask(302, cond_ask(2, 0, 270_000_000, 10 * WHOLE, T3), 1_000));
        let ifd = twin(p, l_ifd_ask(303, ifd_ask(2, 270_000_000, 10 * WHOLE, T3), 1_000));
        assert!(!sells(p, vec![cond, stop, ifd]), "{p:?}");
    }
    // a curve (KCC-20 only) whose every level is at 2.70
    let (h, pre, suf) = tok_fields(T3);
    let curve = kob_protocol::state::CurveAskState {
        maker: pk(2),
        token_cov_id: TOKEN,
        token_tpl_hash: h,
        tpl_prefix_len: pre,
        tpl_suffix_len: suf,
        scale: SCALE,
        min_fill: 1,
        tip: 0,
        active_from: 0,
        level_size: WHOLE,
        total: 8 * WHOLE,
        amount_left: 8 * WHOLE,
        extension_commitment: EXT,
        table: vec![270_000_000; 8],
    };
    let curve = listed(cid(304), kob_protocol::state::AnyState::KobCurveAsk(curve), CARRIER, 1_000);
    assert!(!sells(T3, vec![curve]));
    // a pair ask selling the token for another token
    let pair = l_pair(305, T3, T8, pask(2, T3, T8, 10 * WHOLE, RATE, PTIP), 1_000);
    assert!(!sells(T3, vec![pair]));
}
