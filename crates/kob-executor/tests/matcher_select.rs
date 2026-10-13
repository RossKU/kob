//! Weighted work selection among competing executors (`docs/spec/matcher.md` §3.6, `matcher::select`).
//!
//! * A simulated race: two and three executors plan the same shared book every tick (one view, the same DAA score), submit
//!   in a random order to a mempool that takes the first transaction spending an input and refuses the others (`DoubleSpend`
//!   / `MissingInput`, a chained step whose parent was refused included), and the book moves by the transactions it took.
//!   The deterministic reference planner against the weighted selection: lost transactions per submitted transaction, the
//!   fills of the lost ones, the latency of the crossings (`--nocapture` prints the numbers).
//! * No starvation: at the lowest share every crossing is filled within `max_defer_daa` of its listing.
//! * The priority classes and the reference planner inside a unit: a unit with an immediate order is always planned, and a
//!   planned unit's fills are exactly the reference planner's fills of that unit alone.
//! * A seed reproduces the tick (matcher and keepers); different seeds select differently.
//!
//! `KOB_SELECT_DEBUG=1` prints every simulated tick (the selection, the orders waiting for 30 DAA or more, the transactions);
//! `explore_selection_parameters` (ignored) measures the exponent, the minimum share and the smoothing.

#[path = "matcher_common/mod.rs"]
mod common;

use std::collections::{BTreeMap, BTreeSet};

use common::*;
use kob_executor::keepers::{tick as keeper_tick, KeeperConfig, KeeperInput};
use kob_executor::matcher::book::{Clock, CovId, ListedOrder, Outpoint};
use kob_executor::matcher::engine::{tick, EngineConfig, TickInput, TickReport};
use kob_executor::matcher::family::Families;
use kob_executor::matcher::select::{Role, SelectConfig, Selector, TickSelect, PPM};
use kob_executor::matcher::wallet::LocalKeys;
use kob_protocol::state::*;
use kob_protocol::tx::{KeyUtxo, Utxo};
use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};

/// DAA per simulated tick (one second at 10 blocks per second).
const TICK_DAA: u64 = 10;

fn tok(n: u8) -> [u8; 32] {
    let mut t = [0xb0u8; 32];
    t[0] = n;
    t
}

fn retoken(mut o: ListedOrder, t: [u8; 32]) -> ListedOrder {
    match &mut o.order.state {
        AnyState::KobAsk(s) => s.token_cov_id = t,
        AnyState::KobBid(s) => s.token_cov_id = t,
        _ => {}
    }
    if let Some(c) = &mut o.custody {
        c.utxo.covenant_id = Some(t);
    }
    o
}

/// The order as listed at `daa` (its UTXO, custody and first listing).
fn at(mut o: ListedOrder, daa: u64) -> ListedOrder {
    o.order.utxo.block_daa_score = daa;
    o.seen_daa = daa;
    if let Some(c) = &mut o.custody {
        c.utxo.block_daa_score = daa;
    }
    o
}

fn keys(operator: u8) -> LocalKeys {
    LocalKeys { operator: pk(operator), keys: (1..=40u8).map(|n| (pk(n), sk(n))).collect() }
}

/// The funding UTXOs of executor `e` at tick `t` (distinct per executor and tick, accepted): several, so that the
/// transactions of its units do not chain on each other's change (`docs/ops/executor.md`, funding).
fn coins(e: u8, t: u64, key: u8) -> Vec<KeyUtxo> {
    (0..6u32)
        .map(|k| {
            let mut id = [0xc0u8; 32];
            id[0] = e;
            id[1..9].copy_from_slice(&t.to_le_bytes());
            id[9..13].copy_from_slice(&k.to_le_bytes());
            KeyUtxo {
                utxo: Utxo {
                    transaction_id: id,
                    index: 0,
                    amount: (100 + k as u64) * KAS,
                    block_daa_score: NOW - 100,
                    covenant_id: None,
                },
                pubkey: pk(key),
            }
        })
        .collect()
}

/// How an executor of the race selects.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Mode {
    /// The reference planner (`--no-weighted-select`).
    Reference,
    /// The weighted selection with the share its lost races set (the live default).
    Weighted,
    /// A fixed share (the starvation bound at the lowest share).
    Fixed(u32),
}

struct Exec {
    key: u8,
    mode: Mode,
    selector: Selector,
}

/// The shared book of the race and what happened to it.
struct World {
    by_id: BTreeMap<CovId, ListedOrder>,
    daa: u64,
    next: u32,
    /// Listing DAA of every crossing order, and the DAA of its first fill.
    born: BTreeMap<CovId, u64>,
    filled: BTreeMap<CovId, u64>,
    rng: StdRng,
}

impl World {
    fn new(seed: u64) -> World {
        World {
            by_id: BTreeMap::new(),
            daa: NOW,
            next: 1,
            born: BTreeMap::new(),
            filled: BTreeMap::new(),
            rng: StdRng::seed_from_u64(seed),
        }
    }

    /// A crossing on token `t`: an ask and a bid of the same size, at spreads that differ from crossing to crossing (the
    /// units' weights differ).
    fn cross(&mut self, t: [u8; 32]) {
        let whole = self.rng.gen_range(1..=6i64);
        // every crossing pays its own transaction (a thin one would wait for company, whatever the selection)
        let spread = self.rng.gen_range(5..=40i64) * 1_000_000;
        let (a, b) = (self.next, self.next + 1);
        self.next += 2;
        let maker = (a % 30 + 1) as u8;
        let ask_o = at(retoken(l_ask(a, ask(maker, P250, whole * WHOLE, T3)), t), self.daa);
        let bid_o = at(retoken(l_bid(b, bid(maker + 1, P250 + spread, T3), whole * WHOLE), t), self.daa);
        for o in [ask_o, bid_o] {
            self.born.insert(o.id(), self.daa);
            self.by_id.insert(o.id(), o);
        }
    }
}

/// What a race measured.
#[derive(Debug, Default, Clone, Copy)]
struct Race {
    submitted: u64,
    lost: u64,
    /// Fills (order legs) in the lost transactions.
    lost_fills: u64,
    /// Crossing orders filled, and their latency from listing to first fill (DAA).
    filled: u64,
    latency_sum: u64,
    latency_max: u64,
    unfilled: u64,
    /// Σ of the executors' shares after each tick, and the count (the mean share).
    share_sum: f64,
    shares: u64,
}

impl Race {
    fn lost_ratio(&self) -> f64 {
        self.lost as f64 / self.submitted.max(1) as f64
    }
    fn mean_latency_ticks(&self) -> f64 {
        self.latency_sum as f64 / self.filled.max(1) as f64 / TICK_DAA as f64
    }
}

fn select_for(e: &mut Exec, rng: &mut StdRng) -> Option<TickSelect> {
    match e.mode {
        Mode::Reference => None,
        Mode::Weighted => e.selector.tick(Role::Match),
        Mode::Fixed(ppm) => e.selector.tick(Role::Match).map(|t| TickSelect { seed: rng.gen(), share_ppm: ppm, ..t }),
    }
}

/// `n` executors race over `tokens` books for `ticks` ticks; each tick every token gets a new crossing with probability
/// `p_new` percent.
fn race(n: usize, mode: Mode, tokens: u8, ticks: u64, p_new: u32, seed: u64) -> Race {
    race_with(n, mode, &SelectConfig::default(), tokens, ticks, p_new, seed)
}

fn race_with(n: usize, mode: Mode, sc: &SelectConfig, tokens: u8, ticks: u64, p_new: u32, seed: u64) -> Race {
    let mut world = World::new(seed);
    let mut execs: Vec<Exec> = (0..n)
        .map(|i| Exec {
            key: 5 + i as u8,
            mode,
            selector: Selector::new(SelectConfig { enabled: true, seed: Some(seed ^ ((i as u64 + 1) * 0x9e37)), ..sc.clone() }),
        })
        .collect();
    let mut srng = StdRng::seed_from_u64(seed ^ 0xabcdef);
    let mut out = Race::default();
    for t in 0..ticks {
        for k in 0..tokens {
            if world.rng.gen_range(0..100) < p_new {
                world.cross(tok(k));
            }
        }
        let view: Vec<ListedOrder> = world.by_id.values().cloned().collect();
        // every executor plans the same view
        let mut sent: Vec<(usize, Vec<kob_executor::matcher::engine::Prepared>)> = vec![];
        for (i, e) in execs.iter_mut().enumerate() {
            let select = select_for(e, &mut srng);
            // the runner's bookkeeping: the history of crossable orders goes in, the tick's crossable orders come back
            let select_since = select.map(|_| e.selector.history.clone());
            let cfg = EngineConfig {
                validate: false,
                tick_budget_ms: 0,
                book_budget_ms: 0,
                select,
                select_since,
                ..EngineConfig::default()
            };
            let inp = TickInput {
                orders: view.clone(),
                clock: Clock { daa: world.daa, utc: UTC },
                funding: coins(i as u8, t, e.key),
                excluded: BTreeSet::new(),
            };
            let r: TickReport = tick(&inp, &cfg, &Families::default(), &keys(e.key));
            assert!(r.anomalies.is_empty(), "anomalies: {:?}", r.anomalies);
            e.selector.record(r.selection.as_ref().map(|s| s.crossable.as_slice()).unwrap_or(&[]), world.daa);
            if std::env::var_os("KOB_SELECT_DEBUG").is_some() {
                let late: Vec<(u32, u64)> = world
                    .by_id
                    .keys()
                    .filter_map(|id| world.born.get(id).map(|b| (u32::from_le_bytes(id[..4].try_into().unwrap()), world.daa - b)))
                    .filter(|(_, age)| *age >= 30)
                    .collect();
                eprintln!(
                    "t{t} e{i} sel {:?} skipped {:?} late {late:?} txs {:?}",
                    r.selection.as_ref().map(|s| (s.units, s.mandatory, s.overdue, s.optional, s.taken)),
                    r.skipped.iter().map(|x| &x.1).collect::<Vec<_>>(),
                    r.prepared.iter().map(|p| p.plan.fills.len()).collect::<Vec<_>>()
                );
            }
            sent.push((i, r.prepared));
        }
        // they reach the node in a random order, each executor's transactions in its own order
        sent.shuffle(&mut world.rng);
        let mut spent: BTreeSet<Outpoint> = BTreeSet::new();
        let mut accepted: BTreeSet<[u8; 32]> = BTreeSet::new();
        let mut lost = vec![0usize; n];
        let mut took = vec![0usize; n];
        for (i, txs) in sent {
            for p in txs {
                out.submitted += 1;
                let ins: Vec<Outpoint> = p.signed.tx.inputs.iter().map(|x| (x.transaction_id, x.index)).collect();
                let orphan = p.parent.is_some_and(|q| !accepted.contains(&q));
                if orphan || ins.iter().any(|o| spent.contains(o)) {
                    out.lost += 1;
                    out.lost_fills += p.plan.fills.len() as u64;
                    lost[i] += 1;
                    continue;
                }
                spent.extend(ins);
                accepted.insert(p.txid());
                took[i] += 1;
                for f in &p.plan.fills {
                    world.filled.entry(f.cand.id).or_insert(world.daa);
                }
                let (continued, _) =
                    kob_executor::matcher::chain::apply(&mut world.by_id, &p.plan, &p.lowered.built, pk(execs[i].key));
                // accepted at once: the continuations get the tick's DAA score
                for id in continued {
                    if let Some(o) = world.by_id.get_mut(&id) {
                        o.order.utxo.block_daa_score = world.daa;
                        if let Some(c) = &mut o.custody {
                            c.utxo.block_daa_score = world.daa;
                        }
                    }
                }
            }
        }
        for (i, e) in execs.iter_mut().enumerate() {
            e.selector.observe(Role::Match, lost[i], took[i]);
            if mode == Mode::Weighted {
                out.share_sum += e.selector.share_match;
                out.shares += 1;
            }
        }
        world.daa += TICK_DAA;
    }
    for (id, b) in &world.born {
        match world.filled.get(id) {
            Some(f) => {
                out.filled += 1;
                out.latency_sum += f - b;
                out.latency_max = out.latency_max.max(f - b);
            }
            None => out.unfilled += 1,
        }
    }
    // the orders still listed at the end crossed in the last ticks only
    out
}

fn show(what: &str, r: &Race) {
    eprintln!(
        "{what:<34} submitted {:>4}  lost {:>4} ({:>5.1} %)  fills in lost txs {:>4}  filled {:>4} unfilled {:>3}  latency mean {:.2} ticks, max {} DAA{}",
        r.submitted,
        r.lost,
        100.0 * r.lost_ratio(),
        r.lost_fills,
        r.filled,
        r.unfilled,
        r.mean_latency_ticks(),
        r.latency_max,
        if r.shares > 0 { format!(", mean share {:.2}", r.share_sum / r.shares as f64) } else { String::new() }
    );
}

/// The race of two, three and five executors: the weighted selection loses far fewer transactions than the deterministic
/// reference (where every executor builds the same batch and all but one lose it), and every crossing is still filled
/// within the starvation bound.
#[test]
fn weighted_selection_cuts_the_races_between_executors() {
    let (tokens, ticks, p_new) = (6u8, 200u64, 35u32);
    for n in [2usize, 3, 5] {
        let det = race(n, Mode::Reference, tokens, ticks, p_new, 11);
        let wsel = race(n, Mode::Weighted, tokens, ticks, p_new, 11);
        show(&format!("{n} executors, reference"), &det);
        show(&format!("{n} executors, weighted (adaptive)"), &wsel);
        assert!(det.lost_ratio() > 0.45, "the reference races: {det:?}");
        assert!(
            wsel.lost_ratio() < det.lost_ratio() * 0.6,
            "the weighted selection loses far fewer transactions: {wsel:?} vs {det:?}"
        );
        assert!(wsel.lost_fills < det.lost_fills / 3, "and wastes less than a third of the fills");
        assert!(wsel.mean_latency_ticks() < det.mean_latency_ticks() + 1.0, "at less than one tick of latency on average");
        assert!(wsel.latency_max <= 30 + TICK_DAA, "every crossing within the starvation bound: {wsel:?}");
        assert!(wsel.unfilled <= 4 * tokens as u64, "the book keeps clearing: {wsel:?}");
    }
}

/// No starvation: at the lowest share (a quarter of the optional units per executor and tick) every crossing is filled at
/// most `max_defer_daa` (30 DAA) after it is listed, by one executor alone or by several; the top unit is never starved.
#[test]
fn no_crossing_waits_longer_than_the_starvation_bound() {
    for n in [1usize, 2, 3] {
        let r = race(n, Mode::Fixed(PPM / 4), 5, 60, 40, 23 + n as u64);
        show(&format!("{n} executors, fixed share 0.25"), &r);
        // the first tick sees the new orders but may not fill them yet (their UTXO is newer than the lock time): the
        // bound counts from the tick they are crossable at
        assert!(r.latency_max <= 30 + TICK_DAA, "a crossing waited {} DAA: {r:?}", r.latency_max);
        assert!(r.filled > 0);
    }
}

fn fills(r: &TickReport) -> BTreeSet<(CovId, i64)> {
    r.prepared.iter().flat_map(|p| p.plan.fills.iter().map(|f| (f.cand.id, f.amount))).collect()
}

/// A view of four token books (each a work unit): a resting crossing on each of three tokens, and on the fourth an IOC
/// ask that takes two resting bids at their own prices, best first.
fn mixed(daa: u64) -> Vec<ListedOrder> {
    let mut v = vec![];
    for k in 0..3u8 {
        let id = 100 + 10 * k as u32;
        v.push(at(retoken(l_ask(id, ask(1, P250, 2 * WHOLE, T3)), tok(k)), daa - 5));
        v.push(at(retoken(l_bid(id + 1, bid(2, P260, T3), 2 * WHOLE), tok(k)), daa - 5));
    }
    let mut ioc = ask(3, P245, 3 * WHOLE, T3);
    ioc.tif = TIF_IOC;
    v.push(at(retoken(l_ask(200, ioc), tok(9)), daa - 5));
    v.push(at(retoken(l_bid(201, bid(4, P260, T3), 2 * WHOLE), tok(9)), daa - 50));
    v.push(at(retoken(l_bid(202, bid(5, P255, T3), 2 * WHOLE), tok(9)), daa - 50));
    v
}

fn tick_at(orders: Vec<ListedOrder>, daa: u64, select: Option<TickSelect>) -> TickReport {
    let cfg = EngineConfig { select, ..EngineConfig::default() };
    let inp = TickInput { orders, clock: Clock { daa, utc: UTC }, funding: vec![funding(500 * KAS)], excluded: BTreeSet::new() };
    let r = tick(&inp, &cfg, &Families::default(), &signer());
    assert!(r.anomalies.is_empty(), "{:?}", r.anomalies);
    for p in &r.prepared {
        assert!(p.validation.is_some(), "engine-validated");
        assert!(p.accounting.profit >= 1);
    }
    r
}

/// The priority classes and the reference planner inside a unit: at the lowest share, whatever the seed, the unit holding
/// the IOC is planned (class 1 is never left for later), and every planned unit fills exactly what the reference planner
/// fills on that unit alone (the IOC takes the best bid first, then the next, at their prices).
#[test]
fn immediate_orders_are_always_planned_and_a_unit_is_planned_as_the_reference_plans_it() {
    let daa = NOW;
    let view = mixed(daa);
    let reference = |t: [u8; 32]| -> BTreeSet<(CovId, i64)> {
        let only: Vec<ListedOrder> = view.iter().filter(|o| o.order.state.token_cov_id() == t).cloned().collect();
        fills(&tick_at(only, daa, None))
    };
    let ioc_ref = reference(tok(9));
    assert!(ioc_ref.contains(&(cid(201), 2 * WHOLE)) && ioc_ref.contains(&(cid(202), WHOLE)), "{ioc_ref:?}");
    let mut deferred_any = false;
    for seed in 0..12u64 {
        let sel = TickSelect { seed, share_ppm: PPM / 4, ..TickSelect::default() };
        let r = tick_at(view.clone(), daa, Some(sel));
        let s = r.selection.as_ref().expect("the tick sampled");
        assert_eq!(s.units, 4);
        assert_eq!(s.mandatory, 1, "the IOC's unit: {s:?}");
        assert_eq!(s.planned[0], [tok(9)].into_iter().collect(), "mandatory units first");
        deferred_any |= !s.deferred.is_empty();
        let got = fills(&r);
        for planned in &s.planned {
            let t = *planned.iter().next().unwrap();
            let want = reference(t);
            let mine: BTreeSet<(CovId, i64)> = got
                .iter()
                .filter(|(id, _)| view.iter().any(|o| o.id() == *id && o.order.state.token_cov_id() == t))
                .copied()
                .collect();
            assert_eq!(mine, want, "unit {} (seed {seed})", t[0]);
        }
        // one transaction per unit
        assert_eq!(r.prepared.len(), s.planned.len());
    }
    assert!(deferred_any, "a quarter share leaves units for later");
    // the reference (no selection): one transaction for the whole view
    let r = tick_at(view.clone(), daa, None);
    assert_eq!(r.prepared.len(), 1);
    assert!(r.selection.is_none());
    // a share of one is the reference
    let r1 = tick_at(view.clone(), daa, Some(TickSelect { seed: 3, share_ppm: PPM, ..TickSelect::default() }));
    assert_eq!(fills(&r1), fills(&r));
    assert_eq!(r1.prepared.len(), 1);
}

/// Crossings older than the starvation bound are mandatory for every executor, whatever the share.
#[test]
fn an_overdue_crossing_is_planned_by_every_executor() {
    let daa = NOW;
    // listed 40 DAA ago: past the 30 DAA bound
    let view: Vec<ListedOrder> =
        mixed(daa).into_iter().filter(|o| o.order.state.token_cov_id() != tok(9)).map(|o| at(o, daa - 40)).collect();
    for seed in 0..6u64 {
        let r = tick_at(view.clone(), daa, Some(TickSelect { seed, share_ppm: PPM / 4, ..TickSelect::default() }));
        let s = r.selection.as_ref().unwrap();
        assert_eq!((s.mandatory, s.overdue, s.optional), (3, 3, 0), "{s:?}");
        assert_eq!(fills(&r), fills(&tick_at(view.clone(), daa, None)));
    }
}

/// A seed reproduces the selection and the transactions; different seeds select differently.
#[test]
fn the_seed_reproduces_the_tick() {
    let daa = NOW;
    let view: Vec<ListedOrder> = mixed(daa).into_iter().filter(|o| o.order.state.token_cov_id() != tok(9)).collect();
    let run = |seed: u64| tick_at(view.clone(), daa, Some(TickSelect { seed, share_ppm: PPM / 2, ..TickSelect::default() }));
    let (a, b) = (run(77), run(77));
    assert_eq!(a.selection, b.selection);
    let txs = |r: &TickReport| {
        r.prepared.iter().map(|p| p.plan.fills.iter().map(|f| (f.cand.id, f.amount)).collect::<Vec<_>>()).collect::<Vec<_>>()
    };
    assert_eq!(txs(&a), txs(&b), "the same transactions, fill for fill");
    let picks: BTreeSet<Vec<BTreeSet<[u8; 32]>>> = (0..16u64).map(|s| run(s).selection.unwrap().planned).collect();
    assert!(picks.len() > 2, "different seeds take different units: {picks:?}");
    // the runner's source: the same seed gives the same ticks
    let ticks = |seed: u64| {
        let mut s = Selector::new(SelectConfig { enabled: true, seed: Some(seed), ..SelectConfig::default() });
        s.observe(Role::Match, 1, 0);
        (0..4).map(|_| s.tick(Role::Match).unwrap()).collect::<Vec<_>>()
    };
    assert_eq!(ticks(5), ticks(5));
    assert_ne!(ticks(5), ticks(6));
}

/// The keepers: due refunds are sampled by their tip per byte, a refund due for the starvation bound is built by every
/// keeper, and a seed reproduces the jobs.
#[test]
fn keepers_sample_their_due_jobs_and_never_starve_one() {
    let daa = NOW;
    let mut orders = vec![];
    for k in 0..6u32 {
        let mut a = ask(1, P250, 2 * WHOLE, T3);
        // due 5 DAA ago (optional) for the first five, 100 DAA ago (overdue) for the last
        a.expiry_daa = if k < 5 { (daa - 5) as i64 } else { (daa - 100) as i64 };
        orders.push(listed(cid(300 + k), AnyState::KobAsk(a), CARRIER, 1_000));
    }
    let keep = |seed: u64, share_ppm: u32| {
        let cfg = KeeperConfig { select: Some(TickSelect { seed, share_ppm, ..TickSelect::default() }), ..KeeperConfig::default() };
        let inp = KeeperInput {
            orders: orders.clone(),
            clock: Clock { daa, utc: UTC },
            funding: vec![funding(20 * KAS)],
            excluded: BTreeSet::new(),
            excluded_outpoints: BTreeSet::new(),
            known_unprofitable: BTreeSet::new(),
        };
        keeper_tick(&inp, &cfg, &signer())
    };
    let all = keep(0, PPM);
    assert_eq!(all.jobs.len(), 6, "a share of one builds every due job: {:?}", all.skipped);
    assert_eq!(all.sampled, 0);
    let mut seen_deferred: BTreeSet<CovId> = BTreeSet::new();
    for seed in 0..10u64 {
        let r = keep(seed, PPM * 2 / 5);
        assert_eq!(r.overdue, 1);
        assert_eq!(r.sampled, 5);
        assert_eq!(r.taken + r.deferred.len(), 5);
        assert!(r.taken == 2, "two of five in expectation, exactly two here: {r:?}", r = r.taken);
        let built: BTreeSet<CovId> = r.jobs.iter().map(|j| j.order).collect();
        assert!(built.contains(&cid(305)), "the overdue refund is always built");
        assert!(r.deferred.iter().all(|d| !built.contains(d)));
        seen_deferred.extend(r.deferred.iter().copied());
        let again = keep(seed, PPM * 2 / 5);
        assert_eq!(again.jobs.iter().map(|j| j.order).collect::<Vec<_>>(), r.jobs.iter().map(|j| j.order).collect::<Vec<_>>());
    }
    assert!(seen_deferred.len() >= 3, "different seeds leave different jobs");
}

/// The parameters of the selection over races of two, three and five executors (a measurement, not a check:
/// `cargo test -p kob-executor --test matcher_select -- --ignored --nocapture`).
#[test]
#[ignore]
fn explore_selection_parameters() {
    let (tokens, ticks, p_new) = (6u8, 200u64, 35u32);
    for n in [2usize, 3, 5] {
        show(&format!("{n} ex, reference"), &race(n, Mode::Reference, tokens, ticks, p_new, 11));
        for alpha in [0.0, 1.0, 3.0] {
            for min_share in [0.1, 0.25] {
                for smoothing in [0.25, 0.5] {
                    let sc = SelectConfig { alpha, min_share, smoothing, ..SelectConfig::default() };
                    let r = race_with(n, Mode::Weighted, &sc, tokens, ticks, p_new, 11);
                    show(&format!("{n} ex, a{alpha} min{min_share} sm{smoothing}"), &r);
                }
            }
        }
    }
}

/// Crossings on two tokens that pay a transaction only together (each covers the fee of its own legs, not a transaction's
/// envelope and funding on its own): the reference planner fills both in one transaction. The weighted selection plans each
/// work unit in transactions of its own; it must not leave such crossings unfilled for as long as it samples.
#[test]
fn crossings_that_pay_a_transaction_only_together_are_filled_when_the_selection_samples() {
    let daa = NOW;
    let view = |spread: i64, n: u8| -> Vec<ListedOrder> {
        let mut v = vec![];
        for k in 0..n {
            let id = 300 + 10 * k as u32;
            v.push(at(retoken(l_ask(id, ask(1, P250, 2 * WHOLE, T3)), tok(20 + k)), daa - 40));
            v.push(at(retoken(l_bid(id + 1, bid(2, P250 + spread, T3), 2 * WHOLE), tok(20 + k)), daa - 40));
        }
        v
    };
    // the smallest spread at which one crossing pays its own transaction
    let (mut lo, mut hi) = (0i64, 20_000_000i64);
    assert!(!fills(&tick_at(view(hi, 1), daa, None)).is_empty());
    while hi - lo > 1 {
        let m = lo + (hi - lo) / 2;
        if fills(&tick_at(view(m, 1), daa, None)).is_empty() {
            lo = m;
        } else {
            hi = m;
        }
    }
    // just below it, two of them pay one transaction together
    let spread = lo;
    let both = fills(&tick_at(view(spread, 2), daa, None));
    assert_eq!(both.len(), 4, "the reference fills both crossings in one transaction (spread {spread}): {both:?}");
    for seed in 0..6u64 {
        // overdue (listed 40 DAA ago) or not, at a share below one
        for share_ppm in [PPM / 4, PPM - 1] {
            let r = tick_at(view(spread, 2), daa, Some(TickSelect { seed, share_ppm, ..TickSelect::default() }));
            assert!(r.selection.is_some());
            assert_eq!(fills(&r), both, "seed {seed}, share {share_ppm} ppm: {:?}", r.selection);
        }
    }
}
