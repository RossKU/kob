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
    let cfg = MaintenanceConfig { merge: false, ..Default::default() };
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
    let cfg = MaintenanceConfig { merge: false, ..Default::default() };
    let sale = |min_fill: i64| {
        let bid = l_bid_a(100, K3, 2, 250_000_000, 2 * WHOLE);
        let ask = l_ask_with(300, token_a(K3), K3, 2, 270_000_000, 10 * WHOLE, |a| a.min_fill = min_fill);
        let mut inp = input(vec![own(K3, 2 * WHOLE, CARRIER)], vec![funding(50 * KAS)]);
        inp.orders = vec![bid, ask];
        tick(&inp, &cfg, &signer())
    };
    let r = sale(10 * WHOLE);
    assert!(r.jobs.iter().all(|j| j.kind != MaintKind::Sell), "sold against an ask no buyer of 2 can take: {:?}", r.jobs.len());
    let r = sale(WHOLE / 10);
    assert!(r.jobs.iter().any(|j| j.kind == MaintKind::Sell), "{:?}", r.skipped);
}

/// R2D-1: an all-or-nothing ask of exactly the operator's holding offers the whole amount, but only to a buyer of exactly
/// that amount: the poster's own sale. It does not set the floor, nor does an ask whose minimum fill is above what a small
/// buyer (a tenth of the holding) takes. One a small buyer can take does.
#[test]
fn an_exact_size_all_or_nothing_ask_does_not_set_the_sale_floor() {
    let cfg = MaintenanceConfig { merge: false, ..Default::default() };
    let sale = |amount: i64, min_fill: i64| {
        let bid = l_bid_a(100, K3, 2, 250_000_000, 2 * WHOLE);
        let ask = l_ask_with(300, token_a(K3), K3, 2, 270_000_000, amount, |a| a.min_fill = min_fill);
        let mut inp = input(vec![own(K3, 2 * WHOLE, CARRIER)], vec![funding(50 * KAS)]);
        inp.orders = vec![bid, ask];
        tick(&inp, &cfg, &signer())
    };
    let r = sale(2 * WHOLE, 2 * WHOLE);
    assert!(r.jobs.iter().all(|j| j.kind != MaintKind::Sell), "sold against an exact-size all-or-nothing ask");
    let r = sale(10 * WHOLE, WHOLE);
    assert!(r.jobs.iter().all(|j| j.kind != MaintKind::Sell), "sold against an ask no small buyer can take");
    let r = sale(2 * WHOLE, WHOLE / 5);
    assert!(r.jobs.iter().any(|j| j.kind == MaintKind::Sell), "{:?}", r.skipped);
}
