//! Generates the compute-budget table by running every builder shape (the golden scenarios plus
//! the token slot grid, and the maker's cancel of every retired template) through the rusty-kaspa v2.1.0 engine, and checks it against the
//! committed `data/compute_budgets.json`. `KOB_REGEN=1` rewrites the file.
//!
//! An input's script-unit cost depends on the transaction around it: every other token input (an
//! ask's custody) adds about 120 units to each order covenant of the transaction, every other bid a
//! few. So every batch scenario is also measured inside the largest batches the builders accept
//! (`common::pad_batch`: extra asks of one whole token up to the token slot limit, then extra bids of one whole token up
//! to the output limit, and bids alone), and each role's budget is the maximum over all of them.
//!
//! The table is exact: for every role, the shape that needs the most units is rebuilt with that
//! role's budget lowered by one, and the engine must reject it (`ExceededCommittedScriptUnits` at an
//! input of that role), while the table budget passes.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;

use kob_protocol::artifacts::TemplateId;
use kob_protocol::budget::{budget_for_units, lookup, table};
use kob_protocol::build::{build_cancel_retired, build_with, Action, CancelRetired};
use kob_protocol::tx::{assemble, sign_locally, BuiltTx};
use kob_protocol::verify::{execute, measure_units};

/// Largest number of pad legs tried per kind (the builders refuse long before: 16 token inputs).
const MAX_PAD: usize = 24;

/// Wide pad shapes (base units per pad leg, ask price in sompi per whole token) measured besides the one-token pads: real books
/// have amounts and prices of every byte width, and the quote rule's split multiplication costs a little more with a
/// remainder (`n % scale`, `price % scale` non-zero) and larger operands (the Kcc20Ref4x5 table was one unit short for
/// 0.6-1% of random books before wide pads were measured). The widest amount only where the program's outputs hold it
/// (KRON: at most 10^9 base units per output).
const WIDE_PADS: [(i64, i64); 3] = [(63_001, 250_000_007), (16_777_217, 2_500_000_011), (16_777_216_001, 257_000_013)];

/// The provisional budget a measuring build commits (the engine measures without enforcing).
fn provisional(role: &str) -> kob_protocol::Result<u16> {
    Ok(lookup(role).unwrap_or(1))
}

fn builds(a: &Action) -> bool {
    build_with(a, &provisional).is_ok()
}

/// Units per input role of `a`.
fn units(name: &str, a: &Action) -> Vec<(String, u64)> {
    let keys = common::keys();
    let built = build_with(a, &provisional).unwrap_or_else(|e| panic!("{name}: build: {e}"));
    let sigs = sign_locally(&built, &keys).unwrap_or_else(|e| panic!("{name}: sign: {e}"));
    let (tx, entries) = assemble(&built, &sigs).unwrap_or_else(|e| panic!("{name}: assemble: {e}"));
    let u = measure_units(&tx, &entries).unwrap_or_else(|e| panic!("{name}: engine: {e}"));
    built.roles.into_iter().zip(u).collect()
}

/// Units per input role of a retired template's cancel (`<Kind>.cancel.retired.<hash8>@<program>`, `common::retired_orders`).
fn retired_units(name: &str, r: &CancelRetired) -> Vec<(String, u64)> {
    let keys = common::keys();
    let built = build_cancel_retired(r, &provisional).unwrap_or_else(|e| panic!("{name}: build: {e}"));
    let sigs = sign_locally(&built, &keys).unwrap_or_else(|e| panic!("{name}: sign: {e}"));
    let (tx, entries) = assemble(&built, &sigs).unwrap_or_else(|e| panic!("{name}: assemble: {e}"));
    let u = measure_units(&tx, &entries).unwrap_or_else(|e| panic!("{name}: engine: {e}"));
    built.roles.into_iter().zip(u).collect()
}

/// Largest `n` in `0..=MAX_PAD` for which `f(n)` builds (the builders' limits are monotone).
fn largest(f: impl Fn(usize) -> Option<Action>) -> usize {
    let mut n = 0;
    while n < MAX_PAD && f(n + 1).is_some_and(|a| builds(&a)) {
        n += 1;
    }
    n
}

/// Worker threads of the generator (the shapes are independent; the engine runs are the cost).
const THREADS: usize = 8;

/// `f` over `items` on up to [`THREADS`] threads, the results in the order of `items`.
fn par_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let next = AtomicUsize::new(0);
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).clamp(1, THREADS);
    let mut out: Vec<(usize, R)> = std::thread::scope(|s| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| {
                    let mut v = vec![];
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        if i >= items.len() {
                            break v;
                        }
                        v.push((i, f(&items[i])));
                    }
                })
            })
            .collect();
        workers.into_iter().flat_map(|w| w.join().expect("a worker panicked")).collect()
    });
    out.sort_by_key(|(i, _)| *i);
    out.into_iter().map(|(_, r)| r).collect()
}

/// Every measured shape (built once, shared by the tests of this file).
fn shapes() -> &'static [(String, Action)] {
    static SHAPES: OnceLock<Vec<(String, Action)>> = OnceLock::new();
    SHAPES.get_or_init(build_shapes)
}

/// Every measured shape: the scenarios on every program, their padded batches, and the grid.
fn build_shapes() -> Vec<(String, Action)> {
    let mut out: Vec<(String, Action)> = par_map(&common::PROGRAMS, |&p| {
        let mut out = vec![];
        // the golden scenarios and the branch fixtures (every builder role, C6: a role the table lacks is refused by `build`)
        for (n, a) in common::scenarios_on(p).into_iter().chain(common::branches::branch_shapes(p)) {
            let name = format!("{n}@{}", p.name());
            if matches!(a, Action::Batch(_)) {
                let pad = |asks: usize, bids: usize| common::pad_batch(p, &a, asks, bids);
                let asks = largest(|k| pad(k, 0));
                let mut combos = vec![(asks, 0), (0, largest(|m| pad(0, m)))];
                for k in asks.saturating_sub(1)..=asks {
                    combos.push((k, largest(|m| pad(k, m))));
                }
                combos.sort();
                combos.dedup();
                for (k, m) in combos.into_iter().filter(|c| *c != (0, 0)) {
                    out.push((format!("{name}+{k}asks+{m}bids"), pad(k, m).expect("a batch")));
                    // the same contexts with wide pad legs (amounts and prices of other byte widths): larger operands cost more
                    for (amount, price) in WIDE_PADS {
                        if let Some(w) = common::pad_batch_shaped(p, &a, k, m, amount, price) {
                            if builds(&w) {
                                out.push((format!("{name}+{k}asks+{m}bids+n{amount}p{price}"), w));
                            }
                        }
                    }
                }
            }
            out.push((name, a));
        }
        out
    })
    .into_iter()
    .flatten()
    .collect();
    out.extend(common::grid());
    out.extend(pair_shapes());
    out
}

/// Every ordered pair of programs (A, B).
fn program_pairs() -> Vec<(TemplateId, TemplateId)> {
    common::PROGRAMS.iter().flat_map(|a| common::PROGRAMS.iter().map(move |b| (*a, *b))).collect()
}

/// Is `e` a build failure on the token slots of a program (a shape the 3 / 3 or 4 / 5 programs cannot hold)?
fn slot_limited(e: &kob_protocol::Error) -> bool {
    let m = e.to_string();
    m.contains("token inputs /") && m.contains("token outputs per transaction")
}

/// The pair shapes of one program pair: every scenario (each must build) and every branch-grid shape that builds (the
/// others fail on the token slots only: `every_pair_role_is_present_and_sufficient_on_every_program_pair`).
fn pair_shapes_of(pa: TemplateId, pb: TemplateId) -> Vec<(String, Action)> {
    use common::pair::{pair_branch_shapes, pair_scenarios};
    let mut v = pair_scenarios(pa, pb);
    v.extend(pair_branch_shapes(pa, pb).into_iter().filter(|(_, a)| builds(a)));
    v
}

/// Every pair order shape (`common::pair::pair_scenarios` and the branch grid `common::pair::pair_branch_shapes`) on every
/// ordered pair of programs (A, B), each batch also padded with the most plain asks of A and then of B the token slots
/// allow: a pair covenant scans the inputs of BOTH its tokens (its stray guards), so its cost grows with every token input
/// of either; the pads add custody inputs of each token (their tokens go to the batch's taker).
fn pair_shapes() -> Vec<(String, Action)> {
    use common::pair::pair_name;
    par_map(&program_pairs(), |&(pa, pb)| {
        let mut out = vec![];
        let pn = pair_name(pa, pb);
        for (n, a) in pair_shapes_of(pa, pb) {
            if matches!(a, Action::Batch(_)) {
                let pad_a = |k: usize| common::pad_asks_of(pa, common::TOKEN_COV, 0xd1, &a, k);
                let ka = largest(pad_a);
                let a_full = pad_a(ka).expect("a batch");
                let pad_b = |k: usize| common::pad_asks_of(pb, common::TOKEN_B, 0xd5, &a_full, k);
                let kb = largest(pad_b);
                if ka + kb > 0 {
                    out.push((format!("{n}+{ka}a+{kb}b@{pn}"), pad_b(kb).expect("a batch")));
                }
            }
            out.push((format!("{n}@{pn}"), a));
        }
        out
    })
    .into_iter()
    .flatten()
    .collect()
}

/// Role -> (budget, the shape needing the most units, those units).
fn measure_all(all: &[(String, Action)]) -> BTreeMap<String, (u16, String, u64)> {
    let mut out: BTreeMap<String, (u16, String, u64)> = BTreeMap::new();
    let mut add = |name: &str, measured: Vec<(String, u64)>| {
        for (role, u) in measured {
            let e = out.entry(role).or_insert((0, String::new(), 0));
            if u > e.2 || e.1.is_empty() {
                *e = (budget_for_units(u), name.to_string(), u);
            }
        }
    };
    for (name, measured) in all.iter().map(|(n, _)| n).zip(par_map(all, |(name, action)| units(name, action))) {
        add(name, measured);
    }
    // the maker's cancel of every retired template (spend-only), on every program of its family
    for (name, r) in common::retired_orders::retired_cancel_shapes() {
        add(&name, retired_units(&name, &r));
    }
    out
}

#[test]
fn compute_budget_table_is_generated_exact_and_current() {
    let all = shapes();
    let measured = measure_all(all);
    let generated: BTreeMap<String, u16> = measured.iter().map(|(r, (b, _, _))| (r.clone(), *b)).collect();
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("data/compute_budgets.json");
    if std::env::var("KOB_REGEN").is_ok_and(|v| v == "1") {
        let mut json = serde_json::to_string_pretty(&generated).unwrap();
        json.push('\n');
        std::fs::write(&path, json).unwrap();
        println!("wrote {} roles to {}", generated.len(), path.display());
    } else {
        assert_eq!(
            table(),
            &generated,
            "compute budget table is stale: run `KOB_REGEN=1 cargo test -p kob-protocol --test budget_table`"
        );
    }

    // exactness: at each role's worst shape, one unit less is rejected by the engine
    let by_name: BTreeMap<&str, &Action> = all.iter().map(|(n, a)| (n.as_str(), a)).collect();
    let retired_by_name: BTreeMap<String, CancelRetired> = common::retired_orders::retired_cancel_shapes().into_iter().collect();
    let keys = common::keys();
    let worst: Vec<(&String, &(u16, String, u64))> = measured.iter().collect();
    let checked = par_map(&worst, |&(role, (budget, shape, used))| {
        if *budget == 0 {
            return false; // within the free allowance: nothing to lower
        }
        for (b, must_pass) in [(*budget, true), (*budget - 1, false)] {
            let with = |r: &str| if r == role { Ok(b) } else { lookup(r) };
            let built = match by_name.get(shape.as_str()) {
                Some(action) => build_with(action, &with),
                None => build_cancel_retired(&retired_by_name[shape], &with),
            }
            .unwrap_or_else(|e| panic!("{shape}: build: {e}"));
            let sigs = sign_locally(&built, &keys).unwrap();
            let (tx, entries) = assemble(&built, &sigs).unwrap();
            let res = execute(&tx, &entries, true).unwrap();
            let at: Vec<usize> = built.roles.iter().enumerate().filter(|(_, r)| *r == role).map(|(i, _)| i).collect();
            if must_pass {
                for (i, r) in res.iter().enumerate() {
                    assert!(r.is_ok(), "{role} at budget {b} ({shape}): input {i} fails: {r:?}");
                }
            } else {
                assert!(
                    at.iter().any(|i| matches!(&res[*i], Err(e) if e.contains("ExceededCommittedScriptUnits"))),
                    "{role}: budget {b} is enough for {shape} ({used} units): the table over-provisions"
                );
            }
        }
        true
    })
    .into_iter()
    .filter(|c| *c)
    .count();
    assert!(checked > 500, "{checked} roles checked");
}

/// The shape the KRON integration of the executor found one unit short before context padding: the
/// close of an if-done ask on the 2,433-byte KRON program after another ask in the same batch.
#[test]
fn an_ifd_ask_close_after_another_ask_fits_its_budget() {
    let p = TemplateId::KronToken2433;
    let (name, close) = common::scenarios_on(p).into_iter().find(|(n, _)| n == "ifd.ask.final").expect("the IfdAsk close scenario");
    let padded = common::pad_batch(p, &close, 1, 0).unwrap();
    let keys = common::keys();
    let built = build_with(&padded, &lookup).unwrap_or_else(|e| panic!("{name}: {e}"));
    let sigs = sign_locally(&built, &keys).unwrap();
    assert!(built.roles.iter().any(|r| r == "KobIfdAskKron.settle.close@KronToken2433"), "{:?}", built.roles);
    let (tx, entries) = assemble(&built, &sigs).unwrap();
    for (i, r) in execute(&tx, &entries, true).unwrap().into_iter().enumerate() {
        assert!(r.is_ok(), "{name} + 1 ask: input {i} ({}) fails: {r:?}", built.roles[i]);
    }
}

/// The generator measures wide pad legs (amounts and prices of other byte widths; the widest only where the program's output
/// cap allows) besides the one-token pads, on every program, so the table covers the operand magnitudes of real books (and
/// every role is provisioned exactly, see the exactness check above).
#[test]
fn wide_pad_shapes_are_measured_on_every_program() {
    let names: Vec<&str> = shapes().iter().map(|(n, _)| n.as_str()).collect();
    for p in common::PROGRAMS {
        for w in [format!("+n{}p{}", WIDE_PADS[0].0, WIDE_PADS[0].1), format!("+n{}p{}", WIDE_PADS[1].0, WIDE_PADS[1].1)] {
            assert!(
                names.iter().any(|n| n.contains(&w) && n.ends_with(&w) && n.contains(&format!("@{}", p.name()))),
                "{} {w}",
                p.name()
            );
        }
    }
}

/// The generator measures every pair shape on every program pair, padded to the token slots of both tokens.
#[test]
fn pair_shapes_are_measured_on_every_program_pair() {
    let names: Vec<&str> = shapes().iter().map(|(n, _)| n.as_str()).collect();
    for pa in common::PROGRAMS {
        for pb in common::PROGRAMS {
            let pn = format!("@{}", common::pair::pair_name(pa, pb));
            for (n, a) in common::pair::pair_scenarios(pa, pb) {
                assert!(names.contains(&format!("{n}{pn}").as_str()), "{n}{pn}");
                // the padded shapes where both programs have token slots to spare (8 / 8, 16 / 16)
                let roomy = |p: TemplateId| p.token_slots().unwrap().1 >= 8;
                if matches!(a, Action::Batch(_)) && roomy(pa) && roomy(pb) {
                    assert!(names.iter().any(|x| x.starts_with(&format!("{n}+")) && x.ends_with(&pn)), "{n}{pn} padded");
                }
            }
        }
    }
}

/// The pair role space of one program pair (without its `@A+B` suffix): every role name the pair builders can produce
/// (`build/pair.rs`: fills, updates, merges, refunds, cancels), each side, every branch, every trigger mode, every
/// evidence mode. Left out by construction: a take-profit (leg 0) never takes trigger evidence or an auction, a merge only
/// follows a take-profit, an if-done refund holds two custodies only when sell-first.
fn pair_role_space() -> BTreeSet<String> {
    let mut v = BTreeSet::new();
    v.insert("KobPair.cancel".to_string());
    v.insert("KobCondPair.cancel".to_string());
    v.insert("KobIfdPair.cancel".to_string());
    for side in ["ask", "bid"] {
        for branch in ["rest", "return", "close"] {
            for twap in ["", ".twap"] {
                for decay in ["", ".decay"] {
                    v.insert(format!("KobPair.settle.{side}.{branch}{twap}{decay}"));
                }
            }
            for merge in ["", ".merge"] {
                v.insert(format!("KobCondPair.settle.{side}.leg0{merge}.{branch}"));
            }
            for trigger in ["", ".arm0", ".arm1", ".auction"] {
                v.insert(format!("KobCondPair.settle.{side}.leg1{trigger}.{branch}"));
            }
        }
        for kill in ["", ".kill"] {
            v.insert(format!("KobPair.refund{kill}.{side}"));
        }
        v.insert(format!("KobCondPair.refund.{side}"));
        for ev in ["ev0", "ev1"] {
            for what in ["arm", "trail"] {
                v.insert(format!("KobCondPair.update.{side}.{what}.{ev}"));
            }
            v.insert(format!("KobIfdPair.update.{side}.arm.{ev}"));
        }
        for trigger in ["", ".arm0", ".arm1", ".auction"] {
            for book in ["", ".book"] {
                for kind in ["cont", "wait", "close"] {
                    v.insert(format!("KobIfdPair.fill.{side}{trigger}{book}.{kind}"));
                }
            }
        }
        for new in ["", ".new"] {
            for sellout in ["", ".sellout"] {
                v.insert(format!("KobIfdPair.fill.merge.{side}{new}{sellout}"));
            }
        }
    }
    v.insert("KobIfdPair.refund.bid".to_string());
    v.insert("KobIfdPair.refund.ask".to_string());
    v.insert("KobIfdPair.refund.ask.two".to_string());
    v
}

/// Roles of the space no transaction can have, and why.
fn impossible_pair_role(role: &str) -> Option<&'static str> {
    if role.starts_with("KobCondPair.settle.ask.") && role.ends_with(".return") {
        Some("a conditional ASK's custody is its amount left: a fill never leaves a rest to return")
    } else if role.contains(".merge.") && role.ends_with(".return") {
        Some("a re-arming take-profit hands its custody's rest to the entry (it rests or closes)")
    } else if role.starts_with("KobIfdPair.fill.") && role.ends_with(".book.close") {
        Some("a booking fill always continues: the entry waits for its exit's merge")
    } else {
        None
    }
}

/// Every role the builders produce for a pair order, on EVERY program pair, is in the committed table and sufficient:
/// every pair shape (the scenarios and the whole branch grid of `common::pair`: fills, updates in both evidence modes,
/// merges of every custody state) either fails to build on the token slots of that program pair only, or builds with the
/// committed budgets and passes the engine with those budgets enforced. The roles they produce cover the pair role space
/// ([`pair_role_space`]) on every program pair, except the impossible ones ([`impossible_pair_role`]) and the ones whose
/// every shape is a token-slot failure there (each produced by that shape on a program pair with more slots).
#[test]
fn every_pair_role_is_present_and_sufficient_on_every_program_pair() {
    use common::pair::{pair_branch_shapes, pair_name, pair_scenarios};
    let keys = common::keys();
    // per program pair: (shape -> roles it produced) and (the shapes that failed on the token slots)
    type PerPair = (BTreeMap<String, BTreeSet<String>>, BTreeSet<String>);
    let pairs = program_pairs();
    let results: Vec<PerPair> = par_map(&pairs, |&(pa, pb)| {
        let pn = pair_name(pa, pb);
        let mut produced: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut slot_failed = BTreeSet::new();
        let grid: BTreeSet<String> = pair_branch_shapes(pa, pb).into_iter().map(|(n, _)| n).collect();
        for (name, a) in pair_scenarios(pa, pb).into_iter().chain(pair_branch_shapes(pa, pb)) {
            if let Err(e) = build_with(&a, &provisional) {
                assert!(grid.contains(&name) && slot_limited(&e), "{name}@{pn}: {e}");
                slot_failed.insert(name);
                continue;
            }
            let built: BuiltTx = build_with(&a, &lookup).unwrap_or_else(|e| panic!("{name}@{pn}: the committed table: {e}"));
            let sigs = sign_locally(&built, &keys).unwrap();
            let (tx, entries) = assemble(&built, &sigs).unwrap();
            for (i, r) in execute(&tx, &entries, true).unwrap().into_iter().enumerate() {
                assert!(r.is_ok(), "{name}@{pn}: input {i} ({}) fails at its committed budget: {r:?}", built.roles[i]);
            }
            let roles = built.roles.iter().filter_map(|r| r.strip_suffix(&format!("@{pn}"))).map(str::to_string);
            produced.entry(name).or_default().extend(roles);
        }
        (produced, slot_failed)
    });
    // shape -> every pair role it produced anywhere
    let mut by_shape: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for (produced, _) in &results {
        for (shape, roles) in produced {
            by_shape.entry(shape).or_default().extend(roles.iter().map(String::as_str));
        }
    }
    let space = pair_role_space();
    let mut slot_limited_roles = BTreeSet::new();
    for ((pa, pb), (produced, slot_failed)) in pairs.iter().zip(&results) {
        let pn = pair_name(*pa, *pb);
        let here: BTreeSet<&str> = produced.values().flatten().map(String::as_str).collect();
        for role in &space {
            if here.contains(role.as_str()) {
                assert!(impossible_pair_role(role).is_none(), "{role}@{pn} is produced but listed impossible");
                continue;
            }
            if impossible_pair_role(role).is_some() {
                continue;
            }
            let why = slot_failed.iter().find(|s| by_shape.get(s.as_str()).is_some_and(|r| r.contains(role.as_str())));
            assert!(why.is_some(), "{role}@{pn}: no shape produces it (and none failing on the token slots would)");
            slot_limited_roles.insert(format!("{role}@{pn}"));
        }
        // every pair role produced is in the space (a new builder role must be added to the space and to the grid)
        for role in &here {
            let kind = role.split('.').next().unwrap_or("");
            if ["KobPair", "KobCondPair", "KobIfdPair"].contains(&kind) {
                assert!(space.contains(*role), "{role}@{pn} is not in the pair role space");
            }
        }
    }
    println!("{} pair roles limited by the token slots: {slot_limited_roles:?}", slot_limited_roles.len());
}

/// The merge the executor found short: a sell-first entry sold out with a prefund rest left (its B custody held, its A
/// custody new), re-armed by its exit's sell-out, on a KCC-20 / KRON pair both ways. Its input needs more than 259,999
/// units (the budget measured on entries holding nothing), and the committed table covers it.
#[test]
fn a_sold_out_entry_keeping_its_prefund_rest_re_arms_within_its_budget() {
    use common::pair::{pair_name, rearm_with, Rearm};
    let keys = common::keys();
    for (pa, pb) in [(TemplateId::Kcc20Ref, TemplateId::KronToken2433), (TemplateId::KronToken2433, TemplateId::Kcc20Ref)] {
        let role = format!("KobIfdPair.fill.merge.ask.new.sellout@{}", pair_name(pa, pb));
        let a = Action::Batch(rearm_with(false, pa, pb, Rearm { entry_left: 0, b_rest: 7, ..Rearm::default() }));
        let (_, used) = units(&role, &a).into_iter().find(|(r, _)| *r == role).expect("the merge role");
        assert!(used > 259_999, "{role}: {used} units");
        let built = build_with(&a, &lookup).unwrap_or_else(|e| panic!("{role}: {e}"));
        let sigs = sign_locally(&built, &keys).unwrap();
        let (tx, entries) = assemble(&built, &sigs).unwrap();
        for (i, r) in execute(&tx, &entries, true).unwrap().into_iter().enumerate() {
            assert!(r.is_ok(), "{role}: input {i} ({}) fails: {r:?}", built.roles[i]);
        }
    }
}
