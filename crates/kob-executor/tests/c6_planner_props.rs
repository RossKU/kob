//! C6 planner property tests: random global books (two tokens of different programs, plain orders of both,
//! pair orders A / B (the three kinds, both sides, including pair market auctions), market orders, stops, if-done entries, TWAP / DCA, makers with several
//! orders), and hostile variants of them. For every tick:
//!
//! * no panic (the tick runs under `catch_unwind`);
//! * every prepared transaction passed the rusty-kaspa v2.1.0 engine;
//! * every prepared transaction passes the C6 value oracle (`kob-tests/tests/c6/oracle.rs`): no maker receives less
//!   than its terms at its worst all-in price, the supply of every token is conserved, every order continuation keeps
//!   its template and immutable terms, one input per covenant id;
//! * the tick is deterministic: the same input gives the same transactions, and a permuted listing gives the same
//!   transactions (the planner ranks by price, tip and age, never by listing position).
//!
//! `KOB_C6_BOOKS` (default 40) books, `KOB_C6_SEED`; `KOB_C6_DUMP=<dir>` writes every prepared builder request there
//! (JSON `Action`s: planner-shaped seeds for the C6 mutation fuzzer, `KOB_C6_EXTRA_SEEDS`).

#[path = "matcher_common/mod.rs"]
mod common;

#[path = "../../kob-tests/tests/c6/oracle.rs"]
#[allow(dead_code, clippy::all)]
mod oracle;

use std::collections::BTreeSet;

use common::pair::*;
use common::*;
use kob_executor::matcher::book::{ListedOrder, MemoryBook};
use kob_executor::matcher::engine::{tick, EngineConfig, TickInput, TickReport};
use kob_executor::matcher::family::{Families, Family};
use kob_protocol::artifacts::TemplateId;
use kob_protocol::state::*;
use kob_protocol::tx::{SigPlan, TokenUtxo};
use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};

const PA: [TemplateId; 4] = [TemplateId::Kcc20Ref, TemplateId::Kcc20Ref8x8, TemplateId::Kcc20P2, TemplateId::Kcc20KaspaCom025];
const PB: [TemplateId; 3] = [TemplateId::Kcc20Ref8x8, TemplateId::Kcc20Ref, TemplateId::KronToken2433];

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// An amount in base units of `lo..=hi` whole tokens: a whole number of tokens or (half the time) any amount in that range,
/// so that amounts which are not multiples of a whole token exercise the rounding.
fn amount(r: &mut StdRng, lo: i64, hi: i64) -> i64 {
    if r.gen_bool(0.5) {
        r.gen_range(lo..=hi) * WHOLE
    } else {
        r.gen_range((lo * WHOLE).max(1)..=hi * WHOLE)
    }
}

/// A minimum fill: one whole token (the wallet's usual), any fill, a fraction or more than a whole token.
fn min_fill(r: &mut StdRng) -> i64 {
    [WHOLE, WHOLE, 1, 250, 1_500][r.gen_range(0..5)]
}

/// A random global book: orders of token A (every kind), plain orders of token B, pair orders A / B.
fn random_book(r: &mut StdRng, base: u32) -> (MemoryBook, String) {
    let pa = PA[r.gen_range(0..PA.len())];
    let pb = PB[r.gen_range(0..PB.len())];
    let mut orders = vec![];
    let mut id = base;
    let mut next = || {
        id += 1;
        id
    };
    let maker = |r: &mut StdRng| r.gen_range(1..=9u8);
    let px = |r: &mut StdRng, mid: i64| mid + r.gen_range(-8..=8) * 1_000_000;
    let n_a = r.gen_range(3..=18);
    for _ in 0..n_a {
        let m = maker(r);
        let k = r.gen_range(0..100);
        let o = if k < 25 {
            let mut a = ask(m, px(r, 250_000_000), amount(r, 1, 8), pa);
            a.min_fill = min_fill(r);
            a.tip = [0, TIP, 2 * TIP][r.gen_range(0..3)];
            a.tif = [0, 0, 0, TIF_IOC, TIF_FOK][r.gen_range(0..5)];
            if r.gen_bool(0.1) {
                a.interval = 600;
                a.max_fill = amount(r, 1, 3);
            }
            let o = l_ask(next(), a.clone());
            if a.tif != 0 {
                fresh(o)
            } else {
                o
            }
        } else if k < 50 {
            let mut b = bid(m, px(r, 250_000_000), pa);
            b.min_fill = min_fill(r);
            b.tip = [0, TIP, 2 * TIP][r.gen_range(0..3)];
            b.tif = [0, 0, 0, TIF_IOC, TIF_FOK][r.gen_range(0..5)];
            if r.gen_bool(0.1) {
                b.interval = 600;
                b.max_fill = amount(r, 1, 3);
            }
            let o = l_bid(next(), b.clone(), amount(r, 1, 8));
            if b.tif != 0 {
                fresh(o)
            } else {
                o
            }
        } else if k < 57 {
            fresh(l_ask(next(), market_ask(m, px(r, 252_000_000), amount(r, 1, 6), NOW as i64 - r.gen_range(0..200), pa)))
        } else if k < 64 {
            fresh(l_bid(next(), market_bid(m, px(r, 248_000_000), NOW as i64 - r.gen_range(0..200), pa), amount(r, 1, 6)))
        } else if k < 74 {
            let stop = px(r, 250_000_000);
            let mut s = cond_ask(m, stop + r.gen_range(5..=40) * 1_000_000, stop, amount(r, 1, 6), pa);
            s.min_fill = min_fill(r);
            s.min_rest_daa = r.gen_range(0..=600);
            if r.gen_bool(0.3) {
                s.armed = 1;
            }
            l_cond_ask(next(), s, NOW - 700)
        } else if k < 84 {
            let stop = px(r, 250_000_000);
            let mut s = cond_bid(m, stop - r.gen_range(5..=40) * 1_000_000, stop, amount(r, 1, 6), pa);
            s.min_fill = min_fill(r);
            s.min_rest_daa = r.gen_range(0..=600);
            if r.gen_bool(0.3) {
                s.armed = 1;
            }
            l_cond_bid(next(), s, NOW - 700)
        } else if k < 92 {
            let mut s = ifd_bid(m, px(r, 250_000_000), amount(r, 1, 6), pa);
            s.min_fill = min_fill(r);
            if r.gen_bool(0.3) {
                s.rpt_amount = amount(r, 0, 12);
            }
            l_ifd_bid(next(), s, 1_000)
        } else {
            let mut s = ifd_ask(m, px(r, 250_000_000), amount(r, 1, 6), pa);
            s.min_fill = min_fill(r);
            if r.gen_bool(0.3) {
                s.rpt_amount = amount(r, 0, 12);
            }
            l_ifd_ask(next(), s, 1_000)
        };
        orders.push(o);
    }
    // token B: plain asks and bids around 2.00
    for _ in 0..r.gen_range(1..=6) {
        let m = maker(r);
        if r.gen_bool(0.6) {
            orders.push(l_ask_b(next(), pa, pb, m, px(r, P200), amount(r, 1, 8)));
        } else {
            orders.push(l_bid_b(next(), pa, pb, m, px(r, P200), amount(r, 1, 8)));
        }
    }
    // pair orders A / B: KobPair asks and bids (some pair market auctions), armed KobCondPair limit legs, KobIfdPair entries
    for _ in 0..r.gen_range(0..=4) {
        let m = maker(r);
        let ask = r.gen_bool(0.55);
        let rate = if ask { r.gen_range(WHOLE * 9 / 10..=WHOLE * 13 / 10) } else { r.gen_range(WHOLE * 115 / 100..=WHOLE * 17 / 10) };
        let tip = [0, 10, 1_000, PTIP][r.gen_range(0..4)];
        let tif = [0, 0, TIF_IOC, TIF_FOK][r.gen_range(0..4)];
        let kind = r.gen_range(0..10);
        let o = if kind < 6 {
            let mut x = pair_state(m, ask, pa, pb, amount(r, 1, 6), rate, tip, tif);
            if r.gen_bool(0.3) {
                // a pair market order: an IOC auction over 200 DAA (an ask decays down, a bid rises up)
                x.tif = TIF_IOC;
                x.slope = (x.price * 3 / 100 + 199) / 200;
                x.decay_step = 1;
                x.price_end = if ask { x.price * 97 / 100 } else { x.price * 103 / 100 };
                x.active_from = NOW as i64 - r.gen_range(0..200);
                x.expiry_daa = NOW as i64 + 300;
                if !ask {
                    x.custody = x.bid_escrow(x.amount_left, 4).expect("escrow");
                }
            }
            x.min_fill = min_fill(r);
            let fresh_it = x.tif != 0;
            let o = l_pair(next(), pa, pb, x, 1_000);
            if fresh_it {
                fresh(o)
            } else {
                o
            }
        } else if kind < 8 {
            // a resting limit leg, or a stop leg (armed or unarmed: it triggers next to its evidence)
            let mut c = if r.gen_bool(0.5) {
                cond_pair(m, ask, pa, pb, amount(r, 1, 6), rate, 0)
            } else {
                let stop = r.gen_range(WHOLE * 115 / 100..=WHOLE * 14 / 10);
                let mut c = cond_pair(m, ask, pa, pb, amount(r, 1, 6), 0, stop);
                c.armed = i64::from(r.gen_bool(0.3));
                c.min_rest_daa = r.gen_range(0..=60);
                c
            };
            c.tip = tip;
            c.min_fill = min_fill(r);
            l_cond_pair(next(), pa, pb, c, NOW - 700)
        } else {
            let mut e = ifd_pair(m, !ask, pa, pb, amount(r, 1, 6), rate);
            e.tip = tip;
            e.min_fill = min_fill(r);
            if r.gen_bool(0.4) {
                // a buy-first entry triggers at or below its limit, a sell-first one at or above it (sanity: beyond_limit)
                let pct = r.gen_range(0..=15);
                e.entry_stop = if ask { rate + rate * pct / 100 } else { rate - rate * pct / 100 };
            }
            l_ifd_pair(next(), pa, pb, e, NOW - 700)
        };
        orders.push(o);
    }
    // the books of token B the bids route through (bids of B) and the asks of A
    for _ in 0..r.gen_range(0..=3) {
        let m = maker(r);
        orders.push(l_bid_b(next(), pa, pb, m, px(r, P200), amount(r, 1, 8)));
        orders.push(l_ask_a(next(), pa, m, px(r, 250_000_000), amount(r, 1, 8)));
    }
    (MemoryBook { daa_score: NOW + 5, orders, wallet_tokens: vec![] }, format!("{} / {}", pa.name(), pb.name()))
}

/// Hostile copy: a few numeric state fields of a few orders overwritten with extremes (what a hostile maker can list).
fn hostile(r: &mut StdRng, b: &MemoryBook) -> (MemoryBook, String) {
    let mut b = b.clone();
    let mut what = String::new();
    // exactly one order, one field: a panic site is attributed to it
    let p = if r.gen_bool(0.5) { 0.25 } else { 0.0 };
    let pick = r.gen_range(0..b.orders.len().max(1));
    for (j, o) in b.orders.iter_mut().enumerate() {
        if j != pick && !r.gen_bool(p) {
            continue;
        }
        let mut v = serde_json::to_value(&o.order.state).unwrap();
        let f = v["state"].as_object_mut().unwrap();
        let ints: Vec<String> = f
            .iter()
            .filter(|(_, x)| x.as_str().is_some_and(|t| t.len() < 21 && t.parse::<i64>().is_ok()))
            .map(|(k, _)| k.clone())
            .collect();
        if ints.is_empty() {
            continue;
        }
        let k = ints[r.gen_range(0..ints.len())].clone();
        let nv = [0i64, 1, -1, i64::MAX, i64::MIN, 1 << 62, -(1 << 40), 600, 10_001][r.gen_range(0..9)];
        f.insert(k.clone(), serde_json::Value::String(nv.to_string()));
        let kind = o.order.state.template_id().name();
        if let Ok(s) = serde_json::from_value::<AnyState>(v) {
            o.order.state = s;
            what += &format!("{kind}.{k} = {nv}; ");
        }
    }
    (b, what)
}

static PANICS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
thread_local! {
    static CURRENT: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

/// Records every panic (even one the tick catches itself) with the hostile field being exercised.
fn install_panic_recorder() {
    std::panic::set_hook(Box::new(|info| {
        let loc = info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_default();
        let cur = CURRENT.with(|c| c.borrow().clone());
        let msg = info
            .payload()
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| info.payload().downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_default();
        PANICS.lock().unwrap().push(format!("{loc} [{cur}] {}", msg.chars().take(160).collect::<String>()));
    }));
}

fn tick_of(inp: &TickInput, cfg: &EngineConfig) -> Result<TickReport, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| tick(inp, cfg, &Families::default(), &signer()))).map_err(|e| {
        e.downcast_ref::<String>().cloned().or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default()
    })
}

/// Oracle and structural checks of every prepared transaction of a report.
fn check_report(r: &TickReport, what: &str, fails: &mut Vec<String>, dump: Option<&str>, n_dumped: &mut usize) {
    for p in &r.prepared {
        if p.validation.is_none() {
            fails.push(format!("{what}: an unvalidated transaction"));
        }
        // derive every script public key of the request on this thread (the oracle names outputs from them)
        if let Ok(a) = serde_json::from_value::<kob_protocol::build::Action>(p.lowered.request.clone()) {
            let _ = kob_protocol::build::build_with(&a, &kob_executor::matcher::lower::budgets);
            if let Some(dir) = dump {
                *n_dumped += 1;
                let _ = std::fs::write(format!("{dir}/planner_{:06}.json", *n_dumped), serde_json::to_string(&a).unwrap());
            }
        }
        let m = oracle::MTx::from_built(&p.lowered.built);
        // one input per order covenant id
        let ids: Vec<[u8; 32]> = m
            .ins
            .iter()
            .filter(|i| matches!(i.plan, SigPlan::Entry { .. } | SigPlan::Router { .. }))
            .filter_map(|i| i.entry.covenant_id.map(|h| h.as_bytes()))
            .collect();
        if ids.iter().collect::<BTreeSet<_>>().len() != ids.len() {
            fails.push(format!("{what}: an order covenant spent twice in one transaction"));
        }
        for f in oracle::oracle(&m) {
            fails.push(format!("{what}: ORACLE {}: {} (roles {:?})", f.kind, f.detail, p.lowered.built.roles));
        }
    }
}

fn txids(r: &TickReport) -> Vec<[u8; 32]> {
    r.prepared.iter().map(|p| p.txid()).collect()
}

#[test]
fn c6_planner_books_validate_pay_every_maker_and_are_deterministic() {
    let books = env_u64("KOB_C6_BOOKS", 40);
    let mut r = StdRng::seed_from_u64(env_u64("KOB_C6_SEED", 0xc6b0));
    let dump = std::env::var("KOB_C6_DUMP").ok();
    if let Some(d) = &dump {
        std::fs::create_dir_all(d).unwrap();
    }
    let mut n_dumped = 0usize;
    let cfg = EngineConfig { tick_budget_ms: 0, book_budget_ms: 0, ..cfg() };
    let (mut fails, mut txs, mut panics, mut nondet, mut perm_diff) = (vec![], 0usize, vec![], vec![], vec![]);
    let mut hostile_txs = 0usize;
    let mut pair_fills = 0usize;
    install_panic_recorder();
    for b in 0..books {
        let (book, name) = random_book(&mut r, 10_000 * (b as u32 + 1));
        let inp = input(&book);
        let what = format!("book {b} ({name}, {} orders)", book.orders.len());
        match tick_of(&inp, &cfg) {
            Err(e) => panics.push(format!("{what}: PANIC {e}")),
            Ok(rep) => {
                txs += rep.prepared.len();
                pair_fills += rep.prepared.iter().flat_map(|p| p.plan.fills.iter()).filter(|f| f.cand.pair.is_some()).count();
                if std::env::var("KOB_C6_VERBOSE").is_ok() {
                    println!(
                        "{what}: {} txs, skipped {:?}, quarantined {:?}, anomalies {:?}",
                        rep.prepared.len(),
                        rep.skipped,
                        rep.quarantined,
                        rep.anomalies
                    );
                }
                check_report(&rep, &what, &mut fails, dump.as_deref(), &mut n_dumped);
                if !rep.anomalies.is_empty() {
                    // a minimal reproduction of the anomaly (delta-debugged view)
                    let min = ddmin_orders(&inp.orders, &|os: &[ListedOrder]| {
                        let i2 = TickInput {
                            orders: os.to_vec(),
                            clock: inp.clock,
                            funding: inp.funding.clone(),
                            excluded: inp.excluded.clone(),
                        };
                        tick_of(&i2, &cfg).is_ok_and(|r| !r.anomalies.is_empty())
                    });
                    for a in &rep.anomalies {
                        fails.push(format!("{what}: anomaly {a:?}\nminimal view:\n{}", describe_orders(&min)));
                    }
                }
                // determinism: the same input again
                if let Ok(again) = tick_of(&inp, &cfg) {
                    if txids(&again) != txids(&rep) {
                        nondet.push(format!("{what}: {} vs {} transactions", rep.prepared.len(), again.prepared.len()));
                    }
                }
                // listing order: a permutation of the same book
                let mut orders = inp.orders.clone();
                orders.shuffle(&mut r);
                let shuffled = TickInput { orders, clock: inp.clock, funding: inp.funding.clone(), excluded: inp.excluded.clone() };
                if let Ok(perm) = tick_of(&shuffled, &cfg) {
                    if txids(&perm) != txids(&rep) {
                        perm_diff.push(format!("{what}: {} vs {} transactions", rep.prepared.len(), perm.prepared.len()));
                    }
                }
            }
        }
        CURRENT.with(|c| c.borrow_mut().clear());
        // hostile variant of the same book: no panic, and whatever it builds passes the oracle
        let (hb, hwhat) = hostile(&mut r, &book);
        CURRENT.with(|c| *c.borrow_mut() = hwhat.clone());
        let hin = input(&hb);
        match tick_of(&hin, &cfg) {
            Err(e) => panics.push(format!("{what} (hostile): PANIC {e}")),
            Ok(rep) => {
                hostile_txs += rep.prepared.len();
                check_report(&rep, &format!("{what} (hostile)"), &mut fails, None, &mut n_dumped);
            }
        }
    }
    println!("{books} books: {txs} transactions ({pair_fills} pair fills; {hostile_txs} from the hostile variants), {n_dumped} requests dumped");
    for x in panics.iter().chain(&fails).chain(&nondet).take(40) {
        println!("FAIL {x}");
    }
    for x in perm_diff.iter().take(10) {
        println!("LISTING-ORDER DEPENDENT {x}");
    }
    let _ = std::panic::take_hook();
    let sites: BTreeSet<String> = PANICS.lock().unwrap().iter().cloned().collect();
    for s in sites.iter().take(40) {
        println!("PANIC SITE {s}");
    }
    println!("{} distinct panic sites (caught inside the tick or not)", sites.len());
    println!(
        "{} panics, {} oracle/structure failures, {} nondeterministic, {} listing-order dependent",
        panics.len(),
        fails.len(),
        nondet.len(),
        perm_diff.len()
    );
    assert!(txs > 0, "no book produced a transaction");
    assert!(pair_fills > 0, "no book filled a pair order");
    assert!(panics.is_empty(), "the tick panicked");
    assert!(fails.is_empty(), "prepared transactions violate the oracle");
    assert!(nondet.is_empty(), "the tick is not deterministic");
    assert!(perm_diff.is_empty(), "the plan depends on the listing order of the book");
}

#[allow(dead_code)]
fn _unused(_: TokenUtxo, _: Family) {}
