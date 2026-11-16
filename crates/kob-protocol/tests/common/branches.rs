//! Branch fixtures (C6): one fill of every order kind in every branch combination the builders name in an input's
//! compute-budget role (tif, TWAP / DCA, decay / rise, take-profit or stop leg, triggered / armed auction / armed whole
//! band, rest / sell-out, stop entries), on one token program. The golden scenarios exercise most of them; these close
//! the rest (a market or Dutch sell filled to the last base unit, the last TWAP slice or DCA fill, an armed stop selling out,
//! a stop entry's final fill, ...), so the compute-budget table measures every role the builders can produce
//! (`tests/budget_table.rs`).
#![allow(dead_code)]

use kob_protocol::artifacts::TemplateId;
use kob_protocol::build::*;
use kob_protocol::state::*;
use kob_protocol::Family;

use super::*;

/// Every branch shape on token program `p` (KRON programs: converted with [`kron_action`]).
pub fn branch_shapes(p: TemplateId) -> Vec<(String, Action)> {
    let mut v = vec![];
    let lock = NOW;
    for tif in [TIF_GTC, TIF_IOC, TIF_FOK] {
        for interval in [0, 600] {
            for decay in [false, true] {
                for full in [false, true] {
                    if tif == TIF_FOK && !full {
                        continue;
                    }
                    let mut a = AskState { tif, interval, ..ask(MAKER_A, P250, p) };
                    if decay {
                        a.slope = 1_000_000;
                        a.price_end = 200_000_000;
                        a.active_from = 1_000;
                    }
                    let n = if full { 10 } else { 4 };
                    let c = cov(0xa1);
                    let mut b = batch(vec![Leg::Ask {
                        order: order(10, carrier(), c, 1_000, a),
                        custody: custody(11, 10, c, 1_000),
                        amount: n * WHOLE,
                        t: decay.then_some(lock as i64),
                    }]);
                    b.funding = vec![key_utxo(12, TAKER, 1_000 * KAS)];
                    v.push((format!("branch.ask.tif{tif}.interval{interval}.decay{decay}.full{full}"), Action::Batch(b)));
                    let mut s = BidState { tif, interval, ..bid(MAKER_B, P245, p) };
                    if decay {
                        s.slope = 1_000_000;
                        s.price_end = 260_000_000;
                        s.active_from = 1_000;
                    }
                    let funded = if full { 4 } else { 10 };
                    let value = s.escrow(funded * WHOLE, 1).unwrap() as u64;
                    let mut b = batch(vec![Leg::Bid {
                        order: order(20, value, cov(0xb1), 1_000, s),
                        amount: 4 * WHOLE,
                        t: decay.then_some(lock as i64),
                        delivery: None,
                    }]);
                    b.taker_tokens = vec![tok(21, 4 * WHOLE, pk(TAKER), SCHEME_P2PK, 1_000)];
                    b.change = Some(pk(TAKER));
                    v.push((format!("branch.bid.tif{tif}.interval{interval}.rise{decay}.close{full}"), Action::Batch(b)));
                }
            }
        }
    }
    for full in [false, true] {
        let n = if full { 10 } else { 4 };
        for mode in ["tp", "trigger", "auction1", "auctionOrigin", "wholeBand"] {
            let mut s = cond_ask(MAKER_A, p);
            let (leg, ev) = match mode {
                "tp" => (0u8, None),
                "trigger" => (1, Some(ev_ask(198_000_000, 1, p))),
                "auction1" => {
                    s.armed = 1;
                    (1, None)
                }
                "auctionOrigin" => {
                    s.armed = lock as i64 - 50;
                    (1, None)
                }
                _ => {
                    s.armed = 1;
                    s.band_daa = 0;
                    (1, None)
                }
            };
            let c = cov(0xc1);
            let mut b = batch(vec![Leg::CondAsk {
                order: order(30, carrier(), c, lock - 100, s),
                custody: custody(31, 10, c, lock - 100),
                amount: n * WHOLE,
                leg,
                evidence: None,
                t: None,
                merge: None,
            }]);
            b.funding = vec![key_utxo(33, TAKER, 1_000 * KAS)];
            if let Some(e) = ev {
                b.legs.push(e);
                if let Leg::CondAsk { evidence, .. } = &mut b.legs[0] {
                    *evidence = Some(1);
                }
            }
            v.push((format!("branch.condAsk.{mode}.full{full}"), Action::Batch(b)));
            let mut s = cond_bid(MAKER_B, p);
            let (leg, ev) = match mode {
                "tp" => (0u8, None),
                "trigger" => (1, Some(ev_bid(302_000_000, 1, p))),
                "auction1" => {
                    s.armed = 1;
                    (1, None)
                }
                "auctionOrigin" => {
                    s.armed = lock as i64 - 50;
                    (1, None)
                }
                _ => {
                    s.armed = 1;
                    s.band_daa = 0;
                    (1, None)
                }
            };
            let value = s.escrow(2).unwrap() as u64;
            let mut b = batch(vec![Leg::CondBid {
                order: order(40, value, cov(0xe1), lock - 100, s),
                amount: n * WHOLE,
                leg,
                evidence: None,
                t: None,
                merge: None,
                delivery: None,
            }]);
            b.taker_tokens = vec![tok(41, (n + i64::from(ev.is_some())) * WHOLE, pk(TAKER), SCHEME_P2PK, 1_000)];
            b.change = Some(pk(TAKER));
            if let Some(e) = ev {
                b.legs.push(e);
                if let Leg::CondBid { evidence, .. } = &mut b.legs[0] {
                    *evidence = Some(1);
                }
            }
            v.push((format!("branch.condBid.{mode}.full{full}"), Action::Batch(b)));
        }
    }
    // repeat: none, a repeat count the fill exhausts (the entry waits for merges, its exit not booked), a booked exit
    for rk in 0..3 {
        for full in [false, true] {
            for mode in ["limit", "trigger", "auction", "wholeBand"] {
                let held = 10;
                let n = if full { held } else { 4 };
                // repeat amount: none, the fill exhausts it (1 + n: the exit is not booked), books the exit (1 + 20 whole)
                let rpt = [0, 1 + n * WHOLE, 1 + 20 * WHOLE][rk];
                let mut s = IfdBidState { min_fill: WHOLE, rpt_amount: rpt, ..ifd_bid(MAKER_A, held, p) };
                let mut ev = None;
                match mode {
                    "limit" => {}
                    "trigger" => {
                        s.entry_stop = 255_000_000;
                        ev = Some(ev_bid(256_000_000, 1, p));
                    }
                    "auction" => {
                        s.entry_stop = 255_000_000;
                        s.armed = 1;
                    }
                    _ => {
                        s.entry_stop = 255_000_000;
                        s.armed = 1;
                        s.band_daa = 0;
                    }
                }
                let value = s.escrow().unwrap() as u64;
                let mut b = batch(vec![Leg::IfdBid {
                    order: order(50, value, cov(0xd1), lock - 150, s),
                    amount: n * WHOLE,
                    evidence: None,
                    t: None,
                }]);
                b.taker_tokens = vec![tok(51, (n + i64::from(ev.is_some())) * WHOLE, pk(TAKER), SCHEME_P2PK, 1_000)];
                b.change = Some(pk(TAKER));
                if let Some(e) = ev {
                    b.legs.push(e);
                    if let Leg::IfdBid { evidence, .. } = &mut b.legs[0] {
                        *evidence = Some(1);
                    }
                }
                v.push((format!("branch.ifdBid.{mode}.final{full}.rpt{rk}"), Action::Batch(b)));
                // sell-first: the sell-stop entry triggers at or below entryStop, which is at or above the limit
                let mut s = IfdAskState { min_fill: WHOLE, rpt_amount: rpt, ..ifd_ask(MAKER_A, p) };
                let mut ev = None;
                match mode {
                    "limit" => {}
                    "trigger" => {
                        s.entry_stop = P255;
                        ev = Some(ev_ask(254_000_000, 1, p));
                    }
                    "auction" => {
                        s.entry_stop = P255;
                        s.armed = 1;
                    }
                    _ => {
                        s.entry_stop = P255;
                        s.armed = 1;
                        s.band_daa = 0;
                    }
                }
                let value = s.escrow(carrier() as i64).unwrap() as u64;
                let c = cov(0xf1);
                let mut b = batch(vec![Leg::IfdAsk {
                    order: order(60, value, c, lock - 150, s),
                    custody: custody(61, held, c, lock - 150),
                    amount: n * WHOLE,
                    evidence: None,
                    t: None,
                }]);
                b.funding = vec![key_utxo(62, TAKER, 1_000 * KAS)];
                if let Some(e) = ev {
                    b.legs.push(e);
                    if let Leg::IfdAsk { evidence, .. } = &mut b.legs[0] {
                        *evidence = Some(1);
                    }
                }
                v.push((format!("branch.ifdAsk.{mode}.final{full}.rpt{rk}"), Action::Batch(b)));
            }
        }
    }
    // repeat merges into a stop entry armed by update and not filled yet (armed = 1, band auction): the merge records the
    // band origin (one more branch of the entry's merge than the golden merges take)
    for (n, entry_n) in [(3, 6), (4, 6)] {
        let e = IfdBidState {
            rpt_amount: 1 + 16 * WHOLE,
            amount_left: entry_n * WHOLE,
            entry_stop: 255_000_000,
            armed: 1,
            ..ifd_bid(MAKER_A, 10, p)
        };
        let ev = (e.merge_budget(entry_n * WHOLE).unwrap() + entry_n * (dc() + ec()) + 2 * ec()) as u64;
        let mut b = batch(vec![Leg::CondAsk {
            order: order(30, carrier(), cov(0xc1), 2_000, booked_ask_exit(p, 4)),
            custody: custody(31, 4, cov(0xc1), 2_000),
            amount: n * WHOLE,
            leg: 0,
            evidence: None,
            t: None,
            merge: Some(order(50, ev, cov(0xd1), 1_000, e)),
        }]);
        b.funding = vec![key_utxo(33, TAKER, 1_000 * KAS)];
        v.push((format!("branch.rpt.bid.merge.armedEntry.n{n}"), Action::Batch(b)));
        let e = IfdAskState {
            rpt_amount: 1 + 16 * WHOLE,
            amount_left: entry_n * WHOLE,
            entry_stop: P255,
            armed: 1,
            ..ifd_ask(MAKER_A, p)
        };
        let exit = booked_bid_exit(p, 4);
        let held = exit.amount_left;
        let xv = (e.proceeds(held, e.price).unwrap() + e.prefund_of(held).unwrap() + e.exit_carrier) as u64;
        let ev = (carrier() as i64 + e.prefund_of(entry_n * WHOLE).unwrap() + entry_n * ec()) as u64;
        let mut b = batch(vec![Leg::CondBid {
            order: order(40, xv, cov(0xe1), 2_000, exit),
            amount: n * WHOLE,
            leg: 0,
            evidence: None,
            t: None,
            merge: Some(SellFirstEntry {
                entry: order(60, ev, cov(0xf1), 1_000, e),
                custody: Some(custody(61, entry_n, cov(0xf1), 1_000)),
            }),
            delivery: None,
        }]);
        b.taker_tokens = vec![tok(41, n * WHOLE, pk(TAKER), SCHEME_P2PK, 1_500)];
        b.change = Some(pk(TAKER));
        v.push((format!("branch.rpt.ask.merge.armedEntry.n{n}"), Action::Batch(b)));
    }
    // a repeating entry filled for all it has left WITHOUT booking (its re-arms are used up, rptAmount = 1; while at least a
    // minimum fill of them is left a fill takes at most that many and is booked): it continues with nothing left and waits
    // for the merges of its earlier exits (the `.fill.wait` / `.settle.wait` roles)
    let wb = IfdBidState { rpt_amount: 1, ..ifd_bid(MAKER_A, 4, p) };
    v.push(("branch.ifd.bid.repeat.waitUnbooked".into(), Action::Batch(ifd_fill(wb, 4, None, 1_000))));
    let wa = IfdAskState { amount_left: 4 * WHOLE, rpt_amount: 1, ..ifd_ask(MAKER_A, p) };
    let wav = wa.escrow(carrier() as i64).unwrap() as u64;
    v.push(("branch.ifd.ask.repeat.waitUnbooked".into(), Action::Batch(ifda_fill(wa, 4, wav, None, 1_000))));
    // delivery modes (KCC-20, docs/design/bid-delivery-spec.md): every branch of a GTC bid and of a conditional bid in mode 1
    // (its first fill, a later one continuing its bought-custody: `.custody`) and in mode 2 (a plain UTXO, a new receive UTXO, a
    // top-up: `.topup`), so the table measures each role at its worst delivery
    if p.family() == Family::Kcc20 {
        v.extend(delivery_branch_shapes(p));
    }
    if p.family() == Family::Kron {
        v = v.into_iter().map(|(n, a)| (n, kron_action_on(p, a))).collect();
    }
    // a program with fewer token slots than the fixtures assume keeps the shapes that fit them (`scenarios_on`)
    if p.token_slots().is_some_and(|(i, o)| i < 3 || o < 3) {
        v.retain(|(_, a)| fits_slots(a));
    }
    v
}

/// The delivery forms a branch shape fills with: mode 1 first / later (with its bought-custody), mode 2 plain / new receive
/// UTXO / top-up. The top-ups only on a program with a measured top-up.
fn delivery_forms_of(p: TemplateId) -> Vec<&'static str> {
    let mut v = vec!["acc0", "acc", "plain", "recv"];
    if kob_protocol::budget::topup_extra(p).is_some() {
        v.push("topup");
    }
    v
}

/// The delivery block and inputs of a branch fill in form `f` by `maker` (custody owned by `order_cov`, `held` bought).
fn branch_delivery(f: &str, maker: u8, order_cov: [u8; 32], held: i64) -> ((i64, i64), Option<BidDelivery>) {
    match f {
        "acc0" => ((MODE_ACCUMULATE, 0), None),
        "acc" => ((MODE_ACCUMULATE, held), bid_dlv(Some(bought_custody(29, held, order_cov, 1_500)), None, None)),
        "plain" => ((MODE_TOPUP, 0), bid_dlv(None, None, Some(0))),
        "recv" => ((MODE_TOPUP, 0), None),
        _ => ((MODE_TOPUP, 0), bid_dlv(None, Some(recv_tok(28, 2 * WHOLE, pk(maker), RECV_T, 1_500)), None)),
    }
}

/// Every branch of a GTC `KobBid` (DCA, rising, continue / close, the close with and without its refund) and of a
/// `KobCondBid` (every leg mode, rest / sell-out) in every delivery form ([`delivery_forms_of`]).
pub fn delivery_branch_shapes(p: TemplateId) -> Vec<(String, Action)> {
    let mut v = vec![];
    let lock = NOW;
    for f in delivery_forms_of(p) {
        for interval in [0, 600] {
            for decay in [false, true] {
                for end in ["cont", "close", "closeDust"] {
                    let ((mode, bought), delivery) = branch_delivery(f, MAKER_B, cov(0xb1), 3 * WHOLE);
                    let mut s = mode_bid(BidState { interval, ..bid(MAKER_B, P245, p) }, mode, bought);
                    if decay {
                        s.slope = 1_000_000;
                        s.price_end = 260_000_000;
                        s.active_from = 1_000;
                    }
                    // the fill takes 4 whole tokens; it ends the bid when less than one more minimum fill is left (a
                    // bid holding its bought-custody reserves no carrier for its next fill)
                    let value = match end {
                        "cont" => s.escrow(10 * WHOLE, 1).unwrap(),
                        "close" if bought > 0 => s.used(4 * WHOLE).unwrap() + 2 * KAS as i64,
                        "close" => s.escrow(4 * WHOLE, 1).unwrap() + 2 * KAS as i64,
                        _ if bought > 0 => s.used(4 * WHOLE).unwrap() + KAS as i64 / 2,
                        _ => s.escrow(4 * WHOLE, 1).unwrap(),
                    } as u64;
                    let mut b = batch(vec![Leg::Bid {
                        order: order(20, value, cov(0xb1), 1_000, s),
                        amount: 4 * WHOLE,
                        t: decay.then_some(lock as i64),
                        delivery,
                    }]);
                    b.taker_tokens = vec![tok(21, 4 * WHOLE, pk(TAKER), SCHEME_P2PK, 1_000)];
                    b.change = Some(pk(TAKER));
                    v.push((format!("branch.bid.{f}.interval{interval}.rise{decay}.{end}"), Action::Batch(b)));
                }
            }
        }
        for full in [false, true] {
            let n = if full { 10 } else { 4 };
            for leg_mode in ["tp", "trigger", "auction1", "auctionOrigin", "wholeBand"] {
                let ((mode, bought), delivery) = branch_delivery(f, MAKER_B, cov(0xe1), 3 * WHOLE);
                let mut s = mode_cond_bid(cond_bid(MAKER_B, p), mode, bought);
                let (leg, ev) = match leg_mode {
                    "tp" => (0u8, None),
                    "trigger" => (1, Some(ev_bid(302_000_000, 1, p))),
                    "auction1" => {
                        s.armed = 1;
                        (1, None)
                    }
                    "auctionOrigin" => {
                        s.armed = lock as i64 - 50;
                        (1, None)
                    }
                    _ => {
                        s.armed = 1;
                        s.band_daa = 0;
                        (1, None)
                    }
                };
                let value = s.escrow(2).unwrap() as u64;
                let mut b = batch(vec![Leg::CondBid {
                    order: order(40, value, cov(0xe1), lock - 100, s),
                    amount: n * WHOLE,
                    leg,
                    evidence: None,
                    t: None,
                    merge: None,
                    delivery,
                }]);
                b.taker_tokens = vec![tok(41, (n + i64::from(ev.is_some())) * WHOLE, pk(TAKER), SCHEME_P2PK, 1_000)];
                b.change = Some(pk(TAKER));
                if let Some(e) = ev {
                    b.legs.push(e);
                    if let Leg::CondBid { evidence, .. } = &mut b.legs[0] {
                        *evidence = Some(1);
                    }
                }
                v.push((format!("branch.condBid.{f}.{leg_mode}.full{full}"), Action::Batch(b)));
            }
        }
    }
    v
}
