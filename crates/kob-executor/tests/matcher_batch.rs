//! The global batch (`docs/spec/matcher.md` §3): one transaction fills several token books, pair orders and every
//! order class at once; the planner is a pure function of the view; a book set larger than one transaction is split at the
//! physical limits, most profitable first; large books are planned without candidate caps within the tick budget. Every
//! transaction is built by `kob_protocol` and validated in the rusty-kaspa v2.1.0 script engine (the covenants and token
//! programs are the reference).
//!
//! `KOB_FUZZ_ITERS` / `KOB_FUZZ_SEED` steer the mixed-batch fuzz; the 10 000-orders-per-side measurement is `#[ignore]`d
//! (`cargo test -p kob-executor --test matcher_batch -- --ignored --nocapture`).

#[path = "matcher_common/mod.rs"]
mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use common::pair::*;
use common::*;
use kob_executor::matcher::batch::{plan_batch, BatchInput};
use kob_executor::matcher::book::{ListedOrder, MemoryBook};
use kob_executor::matcher::candidate::Class;
use kob_executor::matcher::engine::{tick, EngineConfig, Prepared, TickInput, TickReport};
use kob_executor::matcher::family::{Families, Family};
use kob_executor::matcher::planner::{PlannerConfig, PHYSICAL_TX_BYTES};
use kob_protocol::artifacts::TemplateId;
use kob_protocol::state::*;
use kob_protocol::tx::TokenUtxo;

const K3: TemplateId = TemplateId::KronToken2433;

/// A token covenant id of the batch fixtures (distinct from TOKEN, TOKEN_B and the KRON ids of the other suites).
fn tok(n: u8) -> [u8; 32] {
    let mut t = [0xa0u8; 32];
    t[0] = n;
    t
}

/// The order moved to token `t` (state and custody): the fixtures build every order on TOKEN.
fn retoken(mut o: ListedOrder, t: [u8; 32]) -> ListedOrder {
    match &mut o.order.state {
        AnyState::KobAsk(s) | AnyState::KobAskKron(s) => s.token_cov_id = t,
        AnyState::KobBid(s) | AnyState::KobBidKron(s) => s.token_cov_id = t,
        AnyState::KobCondAsk(s) | AnyState::KobCondAskKron(s) => s.token_cov_id = t,
        AnyState::KobCondBid(s) | AnyState::KobCondBidKron(s) => s.token_cov_id = t,
        AnyState::KobIfdBid(s) | AnyState::KobIfdBidKron(s) => s.token_cov_id = t,
        AnyState::KobIfdAsk(s) | AnyState::KobIfdAskKron(s) => s.token_cov_id = t,
        _ => {}
    }
    if let Some(c) = &mut o.custody {
        c.utxo.covenant_id = Some(t);
    }
    o
}

/// An order of program `p` on token `t` (KRON programs: the KRON twin of the fixture).
fn on(o: ListedOrder, p: TemplateId, t: [u8; 32]) -> ListedOrder {
    let o = if p.family() == Family::Kron { kron_order(&o) } else { o };
    retoken(o, t)
}

fn ask_on(id: u32, p: TemplateId, t: [u8; 32], maker: u8, price: i64, amount: i64) -> ListedOrder {
    let tpl = if p.family() == Family::Kron { T3 } else { p };
    on(l_ask(id, ask(maker, price, amount, tpl)), p, t)
}

fn bid_on(id: u32, p: TemplateId, t: [u8; 32], maker: u8, price: i64, amount: i64) -> ListedOrder {
    let tpl = if p.family() == Family::Kron { T3 } else { p };
    on(l_bid(id, bid(maker, price, tpl), amount), p, t)
}

/// A pair order of tokens `a` (program `pa`) / `b` (program `pb`) at `rate` base units of B per whole A: an ASK selling
/// `amount` base units of A, or a BID buying them.
#[allow(clippy::too_many_arguments)]
fn pair_on(
    id: u32,
    pa: TemplateId,
    a: [u8; 32],
    pb: TemplateId,
    b: [u8; 32],
    ask: bool,
    amount: i64,
    rate: i64,
    tif: i64,
) -> ListedOrder {
    let o = pair_retoken(l_pair(id, pa, pb, pair_state(1, ask, pa, pb, amount, rate, 0, tif), 1_000), a, b);
    if tif == TIF_GTC {
        o
    } else {
        fresh(o)
    }
}

/// Token covenant ids of a view.
fn tokens_of(inp: &TickInput) -> BTreeSet<[u8; 32]> {
    let mut s = BTreeSet::new();
    for o in &inp.orders {
        match o.order.state.pair_tokens() {
            Some(t) => {
                s.insert(t.a.cov_id);
                s.insert(t.b.cov_id);
            }
            None => {
                s.insert(o.order.state.token_cov_id());
            }
        }
    }
    s
}

/// Invariants of every transaction of a multi-token batch: engine-valid, one input per outpoint and per order id, every
/// token within its family's `MAX_TOK_IN`, no stray spent, no FOK partial, never at a loss.
fn check_batch(p: &Prepared, inp: &TickInput) {
    assert!(p.validation.is_some(), "engine-validated");
    let tx = &p.signed.tx;
    let toks = tokens_of(inp);
    let mut ops = BTreeSet::new();
    let mut per_tok: BTreeMap<[u8; 32], usize> = BTreeMap::new();
    let mut ids = BTreeSet::new();
    for i in &tx.inputs {
        assert!(ops.insert((i.transaction_id, i.index)), "an outpoint spent twice");
        match i.utxo.covenant_id {
            Some(c) if toks.contains(&c) => *per_tok.entry(c).or_default() += 1,
            Some(c) => assert!(ids.insert(c), "one covenant input per order id"),
            None => {}
        }
    }
    for (t, n) in &per_tok {
        let fam = inp
            .orders
            .iter()
            .find_map(|o| match o.order.state.pair_tokens() {
                Some(pt) => [pt.a, pt.b].into_iter().find(|k| k.cov_id == *t).and_then(|k| k.family_of()),
                None => (o.order.state.token_cov_id() == *t).then(|| o.order.state.family()),
            })
            .expect("a token of the view");
        assert!(*n <= fam.max_tok_in(), "{n} inputs of one {fam:?} token");
    }
    for o in &inp.orders {
        for s in &o.strays {
            assert!(!ops.contains(&(s.utxo.transaction_id, s.utxo.index)), "a stray was spent");
        }
    }
    for f in &p.plan.fills {
        let Some(o) = inp.orders.iter().find(|o| o.id() == f.cand.id) else { continue };
        if let AnyState::KobAsk(s) = o.base_state() {
            if s.tif == TIF_FOK {
                assert_eq!(f.amount, s.amount_left, "FOK ask partially filled");
            }
        }
        if let Some((lo, hi)) = f.cand.fok {
            assert!(f.amount >= lo && f.amount <= hi, "FOK partially filled");
        }
    }
    assert!(p.accounting.profit >= 1, "a batch at a loss: {}", p.accounting.profit);
    check_triggers(p, inp);
}

fn run_batch(inp: &TickInput, cfg: &EngineConfig) -> TickReport {
    let cfg = &EngineConfig { token_carrier: OP_TOKEN_CARRIER, ..cfg.clone() };
    let r = tick(inp, cfg, &Families::default(), &signer());
    assert_eq!(r.slack_retries, 0, "a compute budget of the table was one unit short");
    assert!(r.anomalies.is_empty(), "anomalies: {:?}", r.anomalies);
    assert!(r.quarantined.is_empty(), "quarantined: {:?}", r.quarantined);
    for p in &r.prepared {
        check_batch(p, inp);
    }
    check_pair(&r, inp);
    r
}

fn view(orders: Vec<ListedOrder>) -> TickInput {
    let mut i = input(&book(orders));
    i.funding = vec![funding(500 * KAS)];
    i
}

// ------------------------------------------------------------------ one transaction, many books

#[test]
fn one_transaction_fills_two_token_books_a_pair_order_and_an_ioc() {
    let (c, d) = (tok(1), tok(2));
    let orders = vec![
        // book C: 2 bids x 2 asks (N:M)
        bid_on(1, T8, c, 1, P260, 3 * WHOLE),
        bid_on(2, T8, c, 2, 258_000_000, 2 * WHOLE),
        ask_on(3, T8, c, 3, P250, 3 * WHOLE),
        ask_on(4, T8, c, 4, P255, 2 * WHOLE),
        // book D: an IOC ask (class 1) that sells into two resting bids, a bid below everything
        fresh(on(l_ask(10, AskState { tif: TIF_IOC, ..ask(5, P250, 4 * WHOLE, T8) }), T8, d)),
        bid_on(11, T8, d, 6, P260, 2 * WHOLE),
        bid_on(12, T8, d, 7, 258_000_000, 3 * WHOLE),
        bid_on(13, T8, d, 8, 240_000_000, 3 * WHOLE),
        // a pair ASK TOKEN -> TOKEN_B through a bid of TOKEN and an ask of TOKEN_B
        l_pair(20, T8, T8, pask(9, T8, T8, 3 * WHOLE, RATE, 0), 1_000),
        l_bid_a(21, T8, 10, P260, 3 * WHOLE),
        l_ask_b(22, T8, T8, 11, P200, 3 * WHOLE),
    ];
    let inp = view(orders);
    let r = run_batch(&inp, &cfg());
    assert_eq!(
        r.prepared.len(),
        1,
        "one transaction for every book: {:?}",
        r.prepared.iter().map(|p| p.plan.fills.len()).collect::<Vec<_>>()
    );
    println!(
        "one batch, four token books and a route: {} fills, {} bytes, fee {} sompi, profit {}",
        r.prepared[0].plan.fills.len(),
        r.prepared[0].accounting.bytes,
        r.prepared[0].accounting.fee,
        r.prepared[0].accounting.profit
    );
    let p = &r.prepared[0];
    // the IOC takes everything that crosses in its book (maximal), before the resting bids below it are considered
    assert_eq!(p.plan.amount_of(&cid(10)), 4 * WHOLE);
    assert!(p.plan.fills.iter().any(|f| f.cand.id == cid(10) && f.cand.class == Class::Immediate));
    assert_eq!(p.plan.amount_of(&cid(11)) + p.plan.amount_of(&cid(12)), 4 * WHOLE);
    assert_eq!(p.plan.amount_of(&cid(13)), 0, "2.40 crosses nothing");
    // book C fully crossed at its limits
    assert_eq!(p.plan.amount_of(&cid(1)) + p.plan.amount_of(&cid(2)), 5 * WHOLE);
    assert_eq!(p.plan.amount_of(&cid(3)) + p.plan.amount_of(&cid(4)), 5 * WHOLE);
    // the route: exact A, the maker paid its rate (check_pair), the pair leg leads the transaction
    assert_eq!(p.plan.amount_of(&cid(20)), 3 * WHOLE);
    assert_eq!(p.plan.amount_of(&cid(21)), 3 * WHOLE);
    assert_eq!(p.plan.amount_of(&cid(22)), 3 * WHOLE);
    assert_eq!(p.signed.tx.inputs[0].utxo.covenant_id, Some(cid(20)));
    // four token books in one transaction, each token with its own leader and slots
    let toks: BTreeSet<[u8; 32]> = p.plan.books().iter().map(|k| k.token).collect();
    assert_eq!(toks, [c, d, TOKEN, TOKEN_B].into_iter().collect());
    for t in [c, d, TOKEN, TOKEN_B] {
        let ins = p.signed.tx.inputs.iter().filter(|i| i.utxo.covenant_id == Some(t)).count();
        assert!((1..=8).contains(&ins), "{ins} inputs of one token");
    }
    // the operator keeps both spreads, the route margin and every tip, less one fee: the exact rounded KAS of every plain
    // fill (a bid pays floor(n x (quote + tip) / scale), an ask receives ceil(n x (quote - tip) / scale)) and every pair
    // order's KAS tip floor(n x tip / scale)
    let margin: i64 = p
        .plan
        .fills
        .iter()
        .map(|f| match &f.cand.pair {
            Some(x) => x.info.tip_kas(f.amount),
            None if f.cand.side == kob_executor::matcher::candidate::Side::Bid => f.cand.value(f.amount).expect("value"),
            None => -f.cand.value(f.amount).expect("value"),
        })
        .sum();
    assert_eq!(p.plan.margin, margin);
    assert_eq!(p.accounting.profit, margin - p.accounting.fee as i64);
}

#[test]
fn a_kron_book_a_kcc20_book_and_a_pair_order_between_the_families_share_one_transaction() {
    let (k, c) = (tok(3), tok(4));
    let orders = vec![
        bid_on(1, K3, k, 1, P260, 4 * WHOLE),
        ask_on(2, K3, k, 2, P250, 4 * WHOLE),
        bid_on(3, T3, c, 3, P260, 2 * WHOLE),
        ask_on(4, T3, c, 4, P250, 2 * WHOLE),
        // KCC-20 A -> KRON B (TOKEN_COV_KRON)
        l_pair(20, T8, K3, pask(9, T8, K3, 2 * WHOLE, RATE, 0), 1_000),
        l_bid_a(21, T8, 10, P260, 2 * WHOLE),
        l_ask_b(22, T8, K3, 11, P200, 2 * WHOLE),
    ];
    let inp = view(orders);
    let r = run_batch(&inp, &cfg());
    assert_eq!(r.prepared.len(), 1);
    let p = &r.prepared[0];
    assert_eq!(p.plan.families().len(), 2);
    for id in [1, 2] {
        assert_eq!(p.plan.amount_of(&cid(id)), 4 * WHOLE);
    }
    for id in [3, 4, 20, 21, 22] {
        assert_eq!(p.plan.amount_of(&cid(id)), 2 * WHOLE, "order {id}");
    }
}

// ------------------------------------------------------------------ determinism

fn mixed_view() -> Vec<ListedOrder> {
    let (c, d) = (tok(5), tok(6));
    let mut v = vec![];
    for i in 0..6u32 {
        v.push(bid_on(100 + i, T8, c, 1 + i as u8 % 3, P260 - i as i64 * 1_000_000, 2 * WHOLE));
        v.push(ask_on(200 + i, T8, c, 4 + i as u8 % 3, P250 + i as i64 * 1_000_000, 2 * WHOLE));
        v.push(bid_on(300 + i, K3, d, 1 + i as u8 % 3, P260 - i as i64 * 500_000, WHOLE));
        v.push(ask_on(400 + i, K3, d, 4 + i as u8 % 3, P250 + i as i64 * 500_000, WHOLE));
    }
    v.push(fresh(on(l_bid(500, BidState { tif: TIF_IOC, ..bid(7, P255, T8) }, 3 * WHOLE), T8, c)));
    v.push(l_pair(600, T8, T8, pask(9, T8, T8, 4 * WHOLE, RATE, 0), 1_000));
    v.push(l_bid_a(601, T8, 10, P260, 2 * WHOLE));
    v.push(l_bid_a(602, T8, 11, P255, 2 * WHOLE));
    v.push(l_ask_b(603, T8, T8, 12, P200, 3 * WHOLE));
    v.push(l_ask_b(604, T8, T8, 13, 205_000_000, 3 * WHOLE));
    v
}

#[test]
fn the_batch_is_a_pure_function_of_the_view_t_and_the_configuration() {
    let orders = mixed_view();
    let base = view(orders.clone());
    let r0 = run_batch(&base, &cfg());
    assert!(!r0.prepared.is_empty());
    type Fingerprint = Vec<([u8; 32], Vec<([u8; 32], i64)>)>;
    let fingerprint = |r: &TickReport| -> Fingerprint {
        r.prepared.iter().map(|p| (p.txid(), p.plan.fills.iter().map(|f| (f.cand.id, f.amount)).collect())).collect()
    };
    let f0 = fingerprint(&r0);
    // the same view in other orders: the same transactions, byte for byte
    let mut rot = orders.clone();
    for k in 1..5 {
        rot.rotate_left(7);
        if k % 2 == 0 {
            rot.reverse();
        }
        let mut inp = view(vec![]);
        inp.orders = rot.clone();
        inp.funding = base.funding.clone();
        let r = run_batch(&inp, &cfg());
        assert_eq!(fingerprint(&r), f0, "permutation {k}");
    }
    // the planner alone, twice
    let by_id: BTreeMap<[u8; 32], ListedOrder> = orders.iter().map(|o| (o.id(), o.clone())).collect();
    let fams: BTreeSet<Family> = [Family::Kcc20, Family::Kron].into_iter().collect();
    let none = BTreeSet::new();
    let plan = || {
        plan_batch(
            &BatchInput {
                by_id: &by_id,
                lock_time: NOW,
                utc: UTC,
                excluded: &none,
                unaccepted: &none,
                families: &fams,
                max_bytes: PHYSICAL_TX_BYTES,
                only: None,
                deadline: None,
            },
            &PlannerConfig::default(),
        )
    };
    assert_eq!(plan(), plan());
}

// ------------------------------------------------------------------ split at the physical limit

/// `n` books of one bid and two asks each on tokens `tok(first..)`; every `rich`-th book pays a wider spread.
fn many_books(n: u8, rich: u8) -> Vec<ListedOrder> {
    let mut v = vec![];
    for k in 0..n {
        let t = tok(0x20 + k);
        let base = 1_000 * (k as u32 + 1);
        let bp = if rich > 0 && k % rich == 0 { 290_000_000 } else { P260 };
        // funded for two fills (a delivery carrier each): the book may be split between two transactions
        let mut b = bid_on(base, T8, t, 1 + k % 4, bp, 2 * WHOLE);
        b.order.utxo.amount += DC as u64;
        v.push(b);
        v.push(ask_on(base + 1, T8, t, 5 + k % 4, P250, WHOLE));
        v.push(ask_on(base + 2, T8, t, 5 + (k + 1) % 4, P250, WHOLE));
    }
    v
}

#[test]
fn a_book_set_larger_than_one_transaction_is_split_at_the_physical_limit_most_profitable_first() {
    let n = 48u8;
    let inp = view(many_books(n, 3));
    let t0 = Instant::now();
    let r = run_batch(&inp, &cfg());
    println!("{n} books (fees {:?}): ", r.prepared.iter().map(|p| p.accounting.fee).collect::<Vec<_>>());
    println!(
        "{n} books: {} transactions in {:?}: {:?}",
        r.prepared.len(),
        t0.elapsed(),
        r.prepared.iter().map(|p| (p.plan.fills.len(), p.accounting.bytes, p.accounting.profit)).collect::<Vec<_>>()
    );
    assert!(r.prepared.len() >= 2, "the book set exceeds one transaction");
    for p in &r.prepared {
        let m = p.validation.as_ref().expect("validated").mass;
        assert!(m.within_block_limits(), "{m:?}");
        assert!(p.accounting.bytes <= PHYSICAL_TX_BYTES);
    }
    // the first transaction is filled near the physical limit (no artificial cap)
    assert!(r.prepared[0].accounting.bytes > PHYSICAL_TX_BYTES * 3 / 4, "{} bytes", r.prepared[0].accounting.bytes);
    // chained on the operator's change
    for w in r.prepared.windows(2) {
        assert_eq!(w[1].parent, Some(w[0].txid()));
    }
    // every book is filled in the tick; the first transaction holds wide-spread books only (they do not all fit in it)
    for k in 0..n {
        let base = 1_000 * (k as u32 + 1);
        assert_eq!(amount_all(&r, cid(base)), 2 * WHOLE, "book {k}");
        if r.prepared[0].plan.amount_of(&cid(base)) > 0 {
            assert_eq!(k % 3, 0, "a narrow-spread book {k} took room from a wide one in the first transaction");
        }
    }
    let rich_first = (0..n).filter(|k| k % 3 == 0 && r.prepared[0].plan.amount_of(&cid(1_000 * (*k as u32 + 1))) > 0).count();
    assert!(rich_first >= 10, "{rich_first} wide-spread books in the first transaction");
    let density = |p: &Prepared| p.accounting.profit as f64 / p.accounting.bytes as f64;
    assert!(density(&r.prepared[0]) > density(&r.prepared[1]));
    // an operator cap splits further, and never above it
    let mut k = cfg();
    k.planner.max_tx_bytes = 60_000;
    let r2 = run_batch(&inp, &k);
    assert!(r2.prepared.len() > r.prepared.len());
    assert!(r2.prepared.iter().all(|p| p.accounting.bytes <= 60_000));
}

// ------------------------------------------------------------------ large books, no caps

/// One token with `n` bids and `n` asks, all crossing with a profit (the head of the book fills a transaction).
fn big_book(n: u32, t: [u8; 32], base: u32) -> Vec<ListedOrder> {
    let mut v = Vec::with_capacity(2 * n as usize);
    for i in 0..n {
        v.push(bid_on(base + i, T8, t, 1 + (i % 4) as u8, P260 + (i as i64 % 97) * 10_000, 2 * WHOLE));
        v.push(ask_on(base + n + i, T8, t, 5 + (i % 4) as u8, P250 - (i as i64 % 89) * 10_000, WHOLE));
    }
    v
}

fn time_plan(orders: &[ListedOrder]) -> (Duration, usize) {
    let by_id: BTreeMap<[u8; 32], ListedOrder> = orders.iter().map(|o| (o.id(), o.clone())).collect();
    let fams: BTreeSet<Family> = [Family::Kcc20, Family::Kron].into_iter().collect();
    let none = BTreeSet::new();
    let t0 = Instant::now();
    let p = plan_batch(
        &BatchInput {
            by_id: &by_id,
            lock_time: NOW,
            utc: UTC,
            excluded: &none,
            unaccepted: &none,
            families: &fams,
            max_bytes: PHYSICAL_TX_BYTES,
            only: None,
            deadline: None,
        },
        &PlannerConfig::default(),
    );
    (t0.elapsed(), p.map_or(0, |p| p.fills.len()))
}

#[test]
fn a_two_thousand_order_book_is_planned_without_caps() {
    let orders = big_book(1_000, tok(0x60), 100_000);
    let (d, fills) = time_plan(&orders);
    println!("1 000 bids + 1 000 asks, one token: one batch planned in {d:?} ({fills} fills)");
    assert!(fills > 10);
    assert!(d < Duration::from_secs(5), "{d:?}");
}

/// The measurement quoted in `docs/spec/matcher.md` §9: 10 000 orders per side (one book, and ten books of 1 000 per side), planned and ticked
/// with no candidate cap.
#[test]
#[ignore]
fn ten_thousand_orders_per_side() {
    let one = big_book(10_000, tok(0x61), 1_000_000);
    let (d, fills) = time_plan(&one);
    println!("10 000 bids + 10 000 asks, one token: one batch planned in {d:?} ({fills} fills)");
    let mut ten = vec![];
    for k in 0..10u8 {
        ten.extend(big_book(1_000, tok(0x70 + k), 2_000_000 + 10_000 * k as u32));
    }
    let (d, fills) = time_plan(&ten);
    println!("10 books x (1 000 bids + 1 000 asks): one batch planned in {d:?} ({fills} fills)");
    // nothing profitable: every pair crosses at a loss (a shape that once cost 48 s per tick, at 10x)
    let mut loss = vec![];
    for i in 0..10_000u32 {
        let mut b = bid(1, P250 + 1_000 + (i as i64 % 50) * 10, T8);
        b.tip = 0;
        let mut a = ask(2, P250 - 1_000 - (i as i64 % 50) * 10, WHOLE, T8);
        a.tip = 0;
        loss.push(retoken(l_bid(3_000_000 + i, b, WHOLE), tok(0x62)));
        loss.push(retoken(l_ask(3_100_000 + i, a), tok(0x62)));
    }
    let (d, fills) = time_plan(&loss);
    println!("10 000 bids + 10 000 asks crossing at a loss: planned in {d:?} ({fills} fills)");
    // a full tick over the one-token book (plans, builds, signs and validates until the tick budget runs out)
    let inp = view(one.clone());
    let t0 = Instant::now();
    let r = tick(&inp, &cfg(), &Families::default(), &signer());
    let fills: usize = r.prepared.iter().map(|p| p.plan.fills.len()).sum();
    println!(
        "tick over 10 000 + 10 000: {:?}, {} transactions, {fills} fills, {} bytes in the first",
        t0.elapsed(),
        r.prepared.len(),
        r.prepared.first().map_or(0, |p| p.accounting.bytes)
    );
    assert!(r.anomalies.is_empty(), "{:?}", r.anomalies);
}

// ------------------------------------------------------------------ mixed-batch fuzz

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
    /// Base units of `lo..=hi` whole tokens: whole tokens, or (half the time) any amount in that range.
    fn amount(&mut self, lo: i64, hi: i64) -> i64 {
        if self.chance(50) {
            self.range(lo, hi) * WHOLE
        } else {
            self.range(lo * WHOLE, hi * WHOLE)
        }
    }
    /// A minimum fill: one whole token (the usual), any fill, a fraction or more than a whole token.
    fn min_fill(&mut self) -> i64 {
        self.pick(&[WHOLE, WHOLE, 1, 300, 1_500])
    }
}

#[test]
fn random_mixed_batches_are_engine_valid_and_keep_every_rule() {
    let iters: u64 = std::env::var("KOB_FUZZ_ITERS").ok().and_then(|s| s.parse().ok()).unwrap_or(30);
    let seed: u64 = std::env::var("KOB_FUZZ_SEED").ok().and_then(|s| s.parse().ok()).unwrap_or(0x6d37_ba7c);
    let mut r = Rng(seed | 1);
    const PROGS: [TemplateId; 5] = [T3, TemplateId::Kcc20Ref4x5, T8, TemplateId::Kcc20Ref16x16, K3];
    let (mut txs, mut fills, mut multi, mut pairs) = (0usize, 0usize, 0usize, 0usize);
    for it in 0..iters {
        let n_tok = r.range(2, 4) as u8;
        let progs: Vec<TemplateId> = (0..n_tok).map(|_| r.pick(&PROGS)).collect();
        let toks: Vec<[u8; 32]> = (0..n_tok).map(|k| tok(0x40 + 8 * (it as u8 % 16) + k)).collect();
        let mut orders = vec![];
        let base = 100_000 * (it as u32 + 1);
        let mut id = base;
        let mut next = || {
            id += 1;
            id
        };
        for k in 0..n_tok as usize {
            let (p, t) = (progs[k], toks[k]);
            for _ in 0..r.range(1, 6) {
                let price = 250_000_000 + r.range(-4, 4) * 2_500_000;
                let tif = r.pick(&[TIF_GTC, TIF_GTC, TIF_GTC, TIF_IOC, TIF_FOK]);
                let o = if r.chance(50) {
                    let (maker, amount) = (r.range(1, 6) as u8, r.amount(1, 5));
                    let mut a = ask(maker, price, amount, if p.family() == Family::Kron { T3 } else { p });
                    a.min_fill = r.min_fill();
                    a.tif = tif;
                    on(l_ask(next(), a), p, t)
                } else {
                    let mut b = bid(r.range(1, 6) as u8, price, if p.family() == Family::Kron { T3 } else { p });
                    b.tif = tif;
                    b.min_fill = r.min_fill();
                    let amount = r.amount(1, 5);
                    on(l_bid(next(), b, amount), p, t)
                };
                orders.push(if tif == TIF_GTC { o } else { fresh(o) });
            }
            // an armed stop sell of a KCC-20 token now and then (class 2)
            if p.family() == Family::Kcc20 && r.chance(25) {
                let (maker, amount) = (r.range(1, 6) as u8, r.amount(1, 3));
                let mut s = cond_ask(maker, 0, 252_000_000, amount, p);
                s.min_fill = r.min_fill();
                s.armed = 1;
                orders.push(retoken(l_cond_ask(next(), s, NOW - 100), t));
            }
        }
        // pair orders between two of the tokens (A / B, A != B): asks and bids
        for _ in 0..r.range(0, 2) {
            let a = r.range(0, n_tok as i64 - 1) as usize;
            let b = (a + r.range(1, n_tok as i64 - 1) as usize) % n_tok as usize;
            let tif = r.pick(&[TIF_GTC, TIF_GTC, TIF_IOC, TIF_FOK]);
            let ask = r.chance(60);
            let amount = r.amount(1, 4);
            // an ask sells A into bids of A (2.60) and buys B from asks of B (2.00); a bid sells B into bids of B and buys A
            // from asks of A (2.50)
            let rate = if ask { WHOLE * r.range(80, 100) / 100 } else { WHOLE * r.range(130, 160) / 100 };
            let mut x = pair_on(next(), progs[a], toks[a], progs[b], toks[b], ask, amount, rate, tif);
            let mf = r.min_fill();
            if let AnyState::KobPair(s) = &mut x.order.state {
                s.min_fill = mf;
            }
            if r.chance(15) {
                let xid = x.id();
                let (hold, held) = if ask { (a, progs[a]) } else { (b, progs[b]) };
                x.strays.push(TokenUtxo { utxo: utxo(CARRIER, 1_500, Some(toks[hold])), state: tstate(held, 7, xid, true) });
            }
            orders.push(x);
            // the books it routes through, sometimes
            if r.chance(70) {
                let (n1, n2) = (r.amount(1, 3), r.amount(1, 3));
                if ask {
                    orders.push(bid_on(next(), progs[a], toks[a], 8, P260, n1));
                    orders.push(ask_on(next(), progs[b], toks[b], 9, 200_000_000, n2));
                } else {
                    orders.push(bid_on(next(), progs[b], toks[b], 8, 200_000_000, n1));
                    orders.push(ask_on(next(), progs[a], toks[a], 9, P250, n2));
                }
            }
        }
        let inp = view(orders);
        let rep = tick(&inp, &EngineConfig { token_carrier: OP_TOKEN_CARRIER, ..cfg() }, &Families::default(), &signer());
        if !rep.anomalies.is_empty() {
            // a minimal reproduction of the anomaly (delta-debugged view)
            let ec = EngineConfig { token_carrier: OP_TOKEN_CARRIER, ..cfg() };
            let min = ddmin_orders(&inp.orders, &|os: &[ListedOrder]| {
                let i2 =
                    TickInput { orders: os.to_vec(), clock: inp.clock, funding: inp.funding.clone(), excluded: inp.excluded.clone() };
                !tick(&i2, &ec, &Families::default(), &signer()).anomalies.is_empty()
            });
            panic!("book {it} {progs:?}: {:?}\nminimal view:\n{}", rep.anomalies, describe_orders(&min));
        }
        assert!(rep.quarantined.is_empty(), "book {it}: {:?}", rep.quarantined);
        assert_eq!(rep.slack_retries, 0);
        check_pair(&rep, &inp);
        for p in &rep.prepared {
            check_batch(p, &inp);
            txs += 1;
            fills += p.plan.fills.len();
            if p.plan.books().iter().map(|k| k.token).collect::<BTreeSet<_>>().len() > 1 {
                multi += 1;
            }
            pairs += p.plan.fills.iter().filter(|f| f.cand.pair.is_some()).count();
        }
    }
    println!("mixed fuzz: {iters} views, {txs} transactions ({multi} over several tokens), {fills} fills, {pairs} pair fills");
    assert!(multi > 0, "the fuzzer must build multi-token batches");
}

#[test]
fn the_book_view_sizes_nothing_but_the_physical_limit() {
    // the default planner has no size cap and no candidate cap: only the physical limit and the programs' slots
    let p = PlannerConfig::default();
    assert_eq!(p.max_tx_bytes, 0);
    assert_eq!(p.max_candidates_per_group, 0);
    assert_eq!(p.tx_byte_budget(), PHYSICAL_TX_BYTES);
    assert_eq!(PHYSICAL_TX_BYTES, 250_000);
    let _ = MemoryBook::default();
}

/// KNOWN GAP (found by the mixed-batch fuzz at 250 iterations / seed 22 and by the C6 property test at seed 33; no pair order
/// is involved, reported to the executor agent): an IOC ask walking a decaying auction against a resting bid whose value
/// leaves a small KAS remainder. The planner plans a fill whose payout the builder refuses ("dust payout: small outputs below
/// 2000000 sompi; the transaction's storage mass ... exceeds the block limit (KIP-9)"), so the book is skipped as an anomaly
/// every tick. The planner has no model of that builder rule.
#[test]
#[ignore = "planner gap: a plain fill that leaves a dust payout is planned and refused by the builder (anomaly)"]
fn a_plain_fill_that_would_leave_a_dust_payout_is_not_planned() {
    let p2 = TemplateId::Kcc20P2;
    let a = AskState {
        tif: TIF_IOC,
        active_from: 999_801,
        expiry_daa: 1_000_101,
        slope: 37_650,
        price_end: 243_470_000,
        decay_step: 1,
        min_fill: 1_000,
        amount_left: 3_670,
        ..ask(1, 251_000_000, 3_670, p2)
    };
    let mut ask_o = listed(cid(3), AnyState::KobAsk(a), CARRIER, 999_950);
    ask_o.order.utxo.block_daa_score = 999_950;
    let b = BidState { tip: 0, min_fill: 1, ..bid(2, 255_000_000, p2) };
    let bid_o = listed(cid(15), AnyState::KobBid(b), 1_943_500_000, 1_000);
    let inp = view(vec![ask_o, bid_o]);
    let r = tick(&inp, &EngineConfig { token_carrier: OP_TOKEN_CARRIER, ..cfg() }, &Families::default(), &signer());
    assert!(r.anomalies.is_empty(), "anomalies: {:?}", r.anomalies);
}
