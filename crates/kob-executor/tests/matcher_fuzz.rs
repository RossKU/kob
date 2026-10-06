//! Planner fuzzing: random books of every order kind on every token program. Every transaction
//! the matcher prepares must pass the rusty-kaspa v2.1.0 engine (no covenant rule violated), and
//! no plan may fill a FOK partially, spend a stray, exceed 8 token inputs or spend one covenant id
//! twice (`common::check_invariants`). `KOB_FUZZ_ITERS` sets the number of books (default 60),
//! `KOB_FUZZ_SEED` the seed.

#[path = "matcher_common/mod.rs"]
mod common;

use common::*;
use kob_executor::matcher::book::{ListedOrder, MemoryBook};
use kob_executor::matcher::engine::EngineConfig;
use kob_executor::matcher::family::{Families, Family};
use kob_protocol::artifacts::TemplateId;
use kob_protocol::state::*;
use kob_protocol::tx::TokenUtxo;

/// xorshift64*: deterministic, no dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn range(&mut self, lo: i64, hi: i64) -> i64 {
        lo + (self.next() % ((hi - lo + 1) as u64)) as i64
    }
    fn chance(&mut self, pct: u64) -> bool {
        self.next() % 100 < pct
    }
    fn pick<T: Copy>(&mut self, xs: &[T]) -> T {
        xs[(self.next() % xs.len() as u64) as usize]
    }
}

const PROGRAMS: [TemplateId; 5] =
    [TemplateId::Kcc20Ref, TemplateId::Kcc20Ref4x5, TemplateId::Kcc20Ref8x8, TemplateId::Kcc20Ref16x16, TemplateId::Kcc20KaspaCom025];

fn price(r: &mut Rng) -> i64 {
    // 2.40 .. 2.60 in 0.005 steps.
    240_000_000 + r.range(0, 40) * 500_000
}

/// An amount in base units of `lo..=hi` whole tokens: whole tokens, or (half the time) any amount in that range, so that
/// amounts which are not multiples of a whole token exercise the minimum fill and the rounding.
fn amount(r: &mut Rng, lo: i64, hi: i64) -> i64 {
    let whole = r.range(lo, hi) * WHOLE;
    let odd = r.range((lo * WHOLE).max(1), hi * WHOLE);
    r.pick(&[whole, odd])
}

/// A minimum fill: one whole token (the usual), any fill, a fraction or more than a whole token.
fn min_fill(r: &mut Rng) -> i64 {
    r.pick(&[WHOLE, WHOLE, 1, 250, 1_500, 2 * WHOLE])
}

fn random_order(r: &mut Rng, id: u32, tpl: TemplateId) -> ListedOrder {
    let maker = r.range(1, 4) as u8;
    let tip = r.pick(&[0, 0, 50_000, 100_000, 300_000]);
    let amt = amount(r, 1, 8);
    let mf = min_fill(r);
    match r.range(0, 9) {
        0..=2 => {
            let mut a = ask(maker, price(r), amt, tpl);
            a.tip = tip;
            a.min_fill = mf;
            let tif = r.pick(&[0, 0, 0, TIF_IOC, TIF_FOK]);
            a.tif = tif;
            if r.chance(10) {
                a.interval = 600;
                a.max_fill = amount(r, 1, 3);
            }
            let o = l_ask(id, a);
            if tif != 0 {
                fresh(o)
            } else {
                o
            }
        }
        3..=5 => {
            let mut b = bid(maker, price(r), tpl);
            b.tip = tip;
            b.min_fill = mf;
            let tif = r.pick(&[0, 0, 0, TIF_IOC, TIF_FOK]);
            b.tif = tif;
            let v = (b.used(amt).expect("budget") + r.range(1, 3) * DC) as u64;
            let o = listed(cid(id), AnyState::KobBid(b), v, 1_000);
            if tif != 0 {
                fresh(o)
            } else {
                o
            }
        }
        6 => {
            let stop = price(r) - 5_000_000;
            let mut c = cond_ask(maker, price(r) + 10_000_000, stop, amt, tpl);
            c.min_fill = mf;
            c.tip = tip;
            if r.chance(30) {
                c.trail_step = 1_000_000;
                c.trail_gap = 2_000_000;
            }
            if r.chance(50) {
                c.armed = 1;
                l_cond_ask(id, c, NOW - r.range(0, 400) as u64)
            } else {
                l_cond_ask(id, c, 2_000)
            }
        }
        7 => {
            let mut c = cond_bid(maker, price(r) - 10_000_000, price(r) + 5_000_000, amt, tpl);
            c.min_fill = mf;
            c.tip = tip;
            if r.chance(30) {
                c.trail_step = 1_000_000;
                c.trail_gap = 2_000_000;
            }
            if r.chance(50) {
                c.armed = 1;
                l_cond_bid(id, c, NOW - r.range(0, 400) as u64)
            } else {
                l_cond_bid(id, c, 2_000)
            }
        }
        8 => {
            let mut e = ifd_bid(maker, price(r), amt, tpl);
            e.min_fill = r.pick(&[WHOLE, 2 * WHOLE, 3 * WHOLE, 1, 700]);
            e.tip = tip;
            if r.chance(30) {
                // a buy-stop entry: triggered by a resting bid filled at or above its trigger
                e.entry_stop = e.price - 5_000_000;
            }
            l_ifd_bid(id, e, 1_000)
        }
        _ => {
            let mut e = ifd_ask(maker, price(r), amt, tpl);
            e.min_fill = r.pick(&[WHOLE, 2 * WHOLE, 3 * WHOLE, 1, 700]);
            e.tip = tip;
            if r.chance(30) {
                // a sell-stop entry: triggered by a resting ask filled at or below its trigger
                e.entry_stop = e.price + 5_000_000;
            }
            l_ifd_ask(id, e, 1_000)
        }
    }
}

#[test]
fn random_books_never_violate_a_covenant_rule() {
    let iters: u64 = std::env::var("KOB_FUZZ_ITERS").ok().and_then(|s| s.parse().ok()).unwrap_or(60);
    let seed: u64 = std::env::var("KOB_FUZZ_SEED").ok().and_then(|s| s.parse().ok()).unwrap_or(0x4b4f_4231);
    let mut r = Rng(seed | 1);
    let (mut txs, mut fills, mut books_with_tx) = (0usize, 0usize, 0usize);
    let mut seen: std::collections::BTreeMap<&str, usize> = Default::default();
    for it in 0..iters {
        let tpl = r.pick(&PROGRAMS);
        let n = r.range(2, 12) as u32;
        let mut orders: Vec<ListedOrder> = (0..n).map(|i| random_order(&mut r, 1_000 * (it as u32 + 1) + i, tpl)).collect();
        // A repeat position: a repeating buy-first or sell-first entry with booked exits.
        if r.chance(25) {
            orders.extend(repeat_position(&mut r, 900_000 + it as u32 * 10, tpl));
        }
        // Strays on some orders.
        for o in orders.iter_mut() {
            if r.chance(10) {
                let id = o.id();
                o.strays.push(custody(r.range(1, 5 * WHOLE), id, 1_500));
            }
        }
        let b = MemoryBook { daa_score: NOW + 5, orders, wallet_tokens: vec![] };
        let mut k = cfg();
        // stops are triggered and armed inside the batches; sometimes without arming updates
        k.planner.arm = !r.chance(20);
        let rep = run(&input(&b), &k);
        for (_, why) in &rep.skipped {
            assert!(
                !why.contains("engine") && !why.contains("lowering") && !why.contains("finalize"),
                "book {it}: the planner produced an invalid plan: {why}"
            );
        }
        // Even a refusal followed by a successful retry is a planner defect, except a chained
        // step the engine defers until its parent is accepted.
        for (_, step, why) in &rep.anomalies {
            assert!(*step > 0 && why.contains("engine"), "book {it}: the planner produced an invalid plan (step {step}): {why}");
            *seen.entry("chainDeferred").or_default() += 1;
        }
        if !rep.prepared.is_empty() {
            books_with_tx += 1;
        }
        for p in &rep.prepared {
            check_repeat_rules(p, &b.orders);
            let mut count = |k: &'static str, c: bool| {
                if c {
                    *seen.entry(k).or_default() += 1;
                }
            };
            count("merge", p.plan.fills.iter().any(|f| f.cand.merge.is_some()));
            count("trigger", p.plan.triggered() > 0);
            count("arm", p.plan.updates.iter().any(|u| u.kind == kob_executor::matcher::planner::UpdateKind::Arm));
            count("trail", p.plan.updates.iter().any(|u| u.kind == kob_executor::matcher::planner::UpdateKind::Trail));
            count("chained", p.step > 0);
            count("fok", p.plan.fills.iter().any(|f| f.cand.fok.is_some()));
            count("ioc", p.plan.fills.iter().any(|f| f.cand.ioc));
            count("stopLeg", p.plan.fills.iter().any(|f| f.cand.leg == 1));
            count("ifdEntry", p.plan.fills.iter().any(|f| matches!(f.cand.kind, TemplateId::KobIfdBid | TemplateId::KobIfdAsk)));
            txs += 1;
            fills += p.plan.fills.len();
            // Plans only ever fill what crosses: every fill's all-in bound is honoured by the
            // engine; the operator never loses on a match.
            assert!(p.validation.is_some());
        }
    }
    println!("fuzz: {iters} books, {books_with_tx} with transactions, {txs} transactions, {fills} fills, all engine-valid");
    println!("coverage (transactions with): {seen:?}");
    assert!(txs > 0, "the fuzzer must exercise the builder");
}

/// A repeating entry (id `base`) with one or two booked exits (ids `base + 1`, `base + 2`).
fn repeat_position(r: &mut Rng, base: u32, tpl: TemplateId) -> Vec<ListedOrder> {
    let until = if r.chance(80) { EXPIRY } else { (NOW - 10) as i64 };
    let exits = r.range(1, 2);
    let mut out = vec![];
    if r.chance(50) {
        let entry = if r.chance(20) { 0 } else { amount(r, 1, 4) };
        let p = price(r);
        let exit = CondAskState { expiry_daa: NO_EXPIRY, ..cond_ask(1, p + 5_000_000, p - 10_000_000, WHOLE, tpl) };
        let e = IfdBidState {
            rpt_amount: 17 * WHOLE,
            amount_left: entry,
            exit_state: IfdBidState::commit_exit(&exit),
            ..ifd_bid(1, p, 10 * WHOLE, tpl)
        };
        // the escrow of at least one whole token (an empty entry waits for its exits) and one spare exit carrier
        let ev = (IfdBidState { amount_left: entry.max(WHOLE), ..e.clone() }.escrow().expect("escrow") + EC) as u64;
        out.push(listed(cid(base), AnyState::KobIfdBid(e.clone()), ev, 1_000));
        for k in 0..exits {
            let n = amount(r, 1, 4);
            let mut x = e.exit_for(n, Some(Booking { parent: cid(base), until })).unwrap();
            if r.chance(40) {
                x.armed = 1;
            }
            let daa = if x.armed == 1 { NOW - r.range(0, 300) as u64 } else { 2_000 + k as u64 };
            out.push(listed(cid(base + 1 + k as u32), AnyState::KobCondAsk(x), CARRIER, daa));
        }
    } else {
        let entry = if r.chance(20) { 0 } else { amount(r, 1, 4) };
        let p = price(r);
        let exit = CondBidState { expiry_daa: NO_EXPIRY, ..cond_bid(1, p - 5_000_000, p + 10_000_000, WHOLE, tpl) };
        let e = IfdAskState {
            rpt_amount: 17 * WHOLE,
            amount_left: entry,
            exit_state: IfdAskState::commit_exit(&exit),
            ..ifd_ask(1, p, 10 * WHOLE, tpl)
        };
        // carrier, prefund and exit carriers of the entry's amount, and one spare exit carrier
        let ev = (e.escrow(CARRIER as i64).expect("escrow") + EC) as u64;
        out.push(listed(cid(base), AnyState::KobIfdAsk(e.clone()), ev, 1_000));
        for k in 0..exits {
            let n = amount(r, 1, 4);
            let x = e.exit_for(n, Some(Booking { parent: cid(base), until })).unwrap();
            // a booked exit holds the proceeds and the prefund of its amount (rounded up, as the entry's fill moved them)
            let xv = (x.rpt_proceeds(n).expect("proceeds") + x.rpt_prefund(n).expect("prefund") + e.exit_carrier) as u64;
            out.push(listed(cid(base + 1 + k as u32), AnyState::KobCondBid(x), xv, 2_000 + k as u64));
        }
    }
    out
}

/// §6.1: a booked exit's take-profit before `rptUntil` carries its entry's merge; a stop-loss
/// never carries the entry; one merge per entry per transaction.
fn check_repeat_rules(p: &kob_executor::matcher::engine::Prepared, orders: &[ListedOrder]) {
    let in_tx: std::collections::BTreeSet<[u8; 32]> = p.signed.tx.inputs.iter().filter_map(|i| i.utxo.covenant_id).collect();
    let mut merged = std::collections::BTreeSet::new();
    for f in &p.plan.fills {
        let Some(o) = orders.iter().find(|o| o.id() == f.cand.id) else { continue };
        let (booked, parent, until) = match &o.order.state {
            AnyState::KobCondAsk(s) => (s.is_booked(), s.parent, s.rpt_until),
            AnyState::KobCondBid(s) => (s.is_booked(), s.parent, s.rpt_until),
            _ => continue,
        };
        if !booked {
            continue;
        }
        if f.cand.leg == 1 {
            assert!(f.cand.merge.is_none() && !in_tx.contains(&parent), "a stop-loss carried its entry");
        } else if (p.plan.lock_time as i64) < until {
            assert_eq!(f.cand.merge, Some(parent), "a take-profit before rptUntil without its merge");
            assert!(merged.insert(parent), "two merges of one entry in one transaction");
        }
    }
}

/// Pair orders in random books: KAS bids and asks of both tokens A and B (and orders of the other books), pair orders of the
/// three kinds (`KobPair` asks and bids of every time in force, armed `KobCondPair` limit legs, `KobIfdPair` entries of
/// both sides) on every program pair, amounts and minimum fills in base units (whole tokens or not). Every transaction must
/// pass the engine, pair orders net against each other or route through the KAS books, no pair order shares a transaction
/// with another one of the tick, the operator keeps no token and no stray is spent (`common::pair::check_pair`).
#[test]
fn random_pair_books_are_engine_valid_and_leave_the_operator_no_token() {
    use common::pair::*;
    let iters: u64 = std::env::var("KOB_FUZZ_ITERS").ok().and_then(|s| s.parse().ok()).unwrap_or(40);
    let seed: u64 = std::env::var("KOB_FUZZ_SEED").ok().and_then(|s| s.parse().ok()).unwrap_or(0x4b4f_4232);
    let mut r = Rng(seed | 1);
    const PROGS: [TemplateId; 5] =
        [TemplateId::Kcc20Ref, TemplateId::Kcc20Ref4x5, TemplateId::Kcc20Ref8x8, TemplateId::Kcc20Ref16x16, TemplateId::KronToken2433];
    let (mut txs, mut pair_fills, mut sold, mut netted) = (0usize, 0usize, 0i64, 0usize);
    let (mut pair_updates, mut pair_triggered) = (0usize, 0usize);
    let mut tifs = [0usize; 3];
    let mut kinds: std::collections::BTreeMap<&'static str, usize> = Default::default();
    for it in 0..iters {
        let (pa, pb) = (r.pick(&PROGS), r.pick(&PROGS));
        let base = 10_000 * (it as u32 + 1);
        let mut orders: Vec<ListedOrder> = vec![];
        for k in 0..r.range(1, 4) as u32 {
            let tif = r.pick(&[TIF_GTC, TIF_GTC, TIF_IOC, TIF_FOK]);
            let ask = r.chance(55);
            // an ask is routed when a bid of A pays more than its B costs, a bid when its B sells for more than its A costs
            let rate = if ask { WHOLE * r.range(90, 125) / 100 } else { WHOLE * r.range(115, 170) / 100 };
            let (maker, amt, tip) = (r.range(1, 4) as u8, amount(&mut r, 1, 8), r.pick(&[0, 0, 5, 30, PTIP]));
            let kind = r.range(0, 9);
            let mut o = if kind < 6 {
                let x = PairState { min_fill: min_fill(&mut r), ..pair_state(maker, ask, pa, pb, amt, rate, tip, tif) };
                l_pair(base + k, pa, pb, x, 1_000)
            } else if kind < 8 {
                // a resting limit leg of the conditional kind, or a stop leg (armed, or unarmed: it triggers next to its
                // evidence, two KAS-book fills of the implied rate or a resting pair order's fill)
                let stop = WHOLE * r.range(115, 140) / 100;
                let c = if r.chance(50) {
                    CondPairState { min_fill: min_fill(&mut r), tip, ..cond_pair(maker, ask, pa, pb, amt, rate, 0) }
                } else {
                    let armed = r.pick(&[0, 0, 1]);
                    CondPairState {
                        min_fill: min_fill(&mut r),
                        tip,
                        armed,
                        min_rest_daa: r.range(0, 60),
                        ..cond_pair(maker, ask, pa, pb, amt, 0, stop)
                    }
                };
                l_cond_pair(base + k, pa, pb, c, 1_000)
            } else {
                // a buy-first entry triggers at or below its limit, a sell-first one at or above it (sanity: beyond_limit)
                let entry_stop = match (r.chance(40), ask) {
                    (false, _) => 0,
                    (true, true) => rate + rate * r.range(0, 15) / 100,
                    (true, false) => rate - rate * r.range(0, 15) / 100,
                };
                let e = IfdPairState { min_fill: min_fill(&mut r), tip, entry_stop, ..ifd_pair(maker, !ask, pa, pb, amt, rate) };
                l_ifd_pair(base + k, pa, pb, e, 1_000)
            };
            if tif != TIF_GTC && kind < 6 {
                o = fresh(o);
            }
            if r.chance(20) {
                // strays of both tokens on the pair order: never spent
                let id = o.id();
                o.strays.push(TokenUtxo { utxo: utxo(CARRIER, 1_500, Some(token_a(pa))), state: tstate(pa, 7, id, true) });
                o.strays.push(TokenUtxo { utxo: utxo(CARRIER, 1_500, Some(token_b(pa, pb))), state: tstate(pb, 9, id, true) });
            }
            orders.push(o);
        }
        // KAS books of both tokens: bids and asks of A (2.40 .. 2.60) and of B (1.90 .. 2.60), some routes pay, some do not
        for k in 0..r.range(0, 5) as u32 {
            let (maker, p, amt) = (r.range(5, 8) as u8, price(&mut r), amount(&mut r, 1, 6));
            let mut b = l_bid_a(base + 100 + k, pa, maker, p, amt);
            if r.chance(15) {
                if let AnyState::KobBid(s) | AnyState::KobBidKron(s) = &mut b.order.state {
                    s.tif = r.pick(&[TIF_IOC, TIF_FOK]);
                }
                b = fresh(b);
            }
            orders.push(b);
        }
        for k in 0..r.range(0, 4) as u32 {
            let (maker, p, amt) = (r.range(5, 8) as u8, price(&mut r), amount(&mut r, 1, 6));
            orders.push(l_ask_a(base + 150 + k, pa, maker, p, amt));
        }
        for k in 0..r.range(0, 5) as u32 {
            let p = 190_000_000 + r.range(0, 35) * 2_000_000;
            let (maker, amt) = (r.range(5, 8) as u8, amount(&mut r, 1, 6));
            let mut a = l_ask_b(base + 200 + k, pa, pb, maker, p, amt);
            if r.chance(30) {
                // a minimum fill that may force a route to buy more B than a pair ask needs (the excess goes to its maker)
                let mf = r.pick(&[1, 1_500, 3 * WHOLE]);
                if let AnyState::KobAsk(s) | AnyState::KobAskKron(s) = &mut a.order.state {
                    s.min_fill = mf;
                }
            }
            orders.push(a);
        }
        for k in 0..r.range(0, 4) as u32 {
            let p = 190_000_000 + r.range(0, 35) * 2_000_000;
            let (maker, amt) = (r.range(5, 8) as u8, amount(&mut r, 1, 6));
            orders.push(l_bid_b(base + 250 + k, pa, pb, maker, p, amt));
        }
        // an ask of A that may cross the bids in A's own book first
        if r.chance(25) && fam(pa) == Family::Kcc20 {
            let (p, amt) = (price(&mut r), amount(&mut r, 1, 4));
            orders.push(l_ask(base + 300, ask(9, p, amt, pa)));
        }
        let b = book(orders);
        let inp = input(&b);
        let cfg = EngineConfig { token_carrier: OP_TOKEN_CARRIER, ..cfg() };
        let rep = kob_executor::matcher::engine::tick(&inp, &cfg, &Families::default(), &signer());
        assert!(
            rep.anomalies.is_empty(),
            "book {it} ({} / {}): the planner produced an invalid plan: {:?}",
            pa.name(),
            pb.name(),
            rep.anomalies
        );
        assert!(rep.quarantined.is_empty(), "book {it}: {:?}", rep.quarantined);
        check_pair(&rep, &inp);
        for p in &rep.prepared {
            txs += 1;
            let ops: std::collections::BTreeSet<_> = p.signed.tx.inputs.iter().map(|i| (i.transaction_id, i.index)).collect();
            for o in &inp.orders {
                for s in &o.strays {
                    assert!(!ops.contains(&(s.utxo.transaction_id, s.utxo.index)), "book {it}: a stray was spent");
                }
            }
            let ps: Vec<_> = p.plan.fills.iter().filter(|f| f.cand.pair.is_some()).collect();
            if ps.len() > 1 && p.plan.fills.iter().all(|f| f.cand.pair.is_some()) {
                netted += 1;
            }
            pair_updates +=
                p.plan.updates.iter().filter(|u| inp.orders.iter().any(|o| o.id() == u.id && o.order.state.is_pair())).count();
            pair_triggered += ps.iter().filter(|f| f.evidence.is_some()).count();
            for f in ps {
                pair_fills += 1;
                sold += f.amount;
                let o = inp.orders.iter().find(|o| o.id() == f.cand.id).unwrap();
                *kinds.entry(o.order.state.template_id().name()).or_default() += 1;
                if let AnyState::KobPair(x) = o.base_state() {
                    tifs[x.tif as usize] += 1;
                }
            }
        }
    }
    println!(
        "pair fuzz: {iters} books, {txs} transactions, {pair_fills} pair fills ({sold} base units of A; GTC/IOC/FOK {tifs:?}; {kinds:?}), {netted} pure netting, {pair_updates} pair arms / ratchets, {pair_triggered} triggered pair fills"
    );
    assert!(pair_fills > 0, "the fuzzer must fill pair orders");
}
