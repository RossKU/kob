//! C6 value-conservation fuzzer (script level): builder-produced transactions of every order kind and batch shape,
//! mutated structurally and run through the rusty-kaspa v2.1.0 script engine; every accepted mutant is checked by a
//! value oracle (token supply per covenant id, each maker's value at its own worst all-in price, pair order quotes in both tokens,
//! intent bounds, order continuations). See `c6/mod.rs`.
//!
//! * `c6_seeds_are_valid_and_clean`: every seed builds, validates and passes the oracle (the oracle has no false
//!   positive on the builders' own transactions).
//! * `c6_fuzz_ci`: a CI-sized deterministic run (`KOB_C6_ITERS`, default 400) that must find nothing.
//! * `c6_fuzz_long` (ignored): `KOB_C6_SECS` seconds (default 600) from `KOB_C6_SEED`, report to stdout and to
//!   `KOB_C6_OUT` if set. Run it in release with one thread:
//!   `cargo test --release -p kob-tests --test c6_fuzz -- --ignored --nocapture --test-threads 1`.

#[path = "../../kob-protocol/tests/common/mod.rs"]
#[allow(dead_code, unused_imports, clippy::all)]
mod common;

mod c6;

use std::time::Duration;

use c6::fuzz::{run, Config, Report, Stats};
use c6::seeds::build_seeds;
use c6::{oracle, Keys, ATTACKER};

fn keys() -> Keys {
    let mut k = common::keys();
    k.insert(common::pk(ATTACKER), common::sk(ATTACKER));
    k
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn summary(stats: &Stats, reports: &[Report]) -> String {
    let mut s = format!(
        "seeds {} | iterations {} | accepted {} (clean {}) | corpus {} | {:.1}s\n",
        stats.seeds,
        stats.iterations,
        stats.accepted,
        stats.accepted_clean,
        stats.corpus,
        stats.elapsed.as_secs_f64()
    );
    s += "operator: applied / in accepted mutants\n";
    for (op, (a, b)) in &stats.per_op {
        s += &format!("  {op:<18} {a:>8} {b:>8}\n");
    }
    s += "rejections by class\n";
    let mut rj: Vec<_> = stats.rejects.iter().collect();
    rj.sort_by(|a, b| b.1.cmp(a.1));
    for (c, n) in rj.iter().take(30) {
        s += &format!("  {c:<40} {n:>8}\n");
    }
    s += &format!("makers without an exact value verdict (too many free fill amounts): {}\n", stats.unresolved);
    s += &format!("makers not valued: a pair order with a non-positive scale (own terms, finding NP64): {}\n", stats.invalid_terms);
    s += &format!("findings: {}\n", reports.len());
    for r in reports {
        s += &format!(
            "- {} on {} (first at {}, {} hits): {}\n  steps: {}\n  lineage: {:?}\n",
            r.finding.kind,
            r.seed,
            r.first_seen,
            r.hits,
            r.finding.detail,
            r.steps.join("; "),
            r.lineage
        );
    }
    s
}

#[test]
fn c6_seeds_are_valid_and_clean() {
    let keys = keys();
    let (seeds, bad) = build_seeds(&keys, true);
    for (n, e) in &bad {
        println!("seed not built: {n}: {e}");
    }
    println!("{} seeds, {} not built", seeds.len(), bad.len());
    assert!(seeds.len() > 300, "too few seeds: {}", seeds.len());
    // every pair order shape builds and validates on every program pair of the corpus
    let pair_bad: Vec<&(String, String)> = bad.iter().filter(|(n, _)| n.starts_with("pair.")).collect();
    let pair_seeds = seeds.iter().filter(|s| s.name.starts_with("pair.")).count();
    println!("{pair_seeds} pair seeds");
    assert!(pair_bad.is_empty(), "pair seeds not built: {pair_bad:#?}");
    assert!(pair_seeds > 300, "too few pair seeds: {pair_seeds}");
    let mut dirty = vec![];
    for s in &seeds {
        let f = oracle(&s.m);
        if !f.is_empty() {
            dirty.push(format!("{}: {:?}", s.name, f));
        }
    }
    assert!(dirty.is_empty(), "the oracle flags builder transactions:\n{}", dirty.join("\n"));
}

#[test]
fn c6_fuzz_ci() {
    let keys = keys();
    let (seeds, _) = build_seeds(&keys, false);
    let cfg = Config {
        seed: env_u64("KOB_C6_SEED", 0xc6),
        iterations: Some(env_u64("KOB_C6_ITERS", 400)),
        time: None,
        max_chain: 4,
        corpus_cap: 2_000,
    };
    let (stats, reports) = run(&seeds, &keys, &cfg, |l| println!("{l}"));
    let s = summary(&stats, &reports);
    println!("{s}");
    assert!(stats.accepted > 0, "no mutant was accepted: the harness is not reaching the engine's accept path");
    assert!(reports.is_empty(), "the fuzzer found invariant violations:\n{s}");
}

#[test]
#[ignore]
fn c6_fuzz_long() {
    let keys = keys();
    let (seeds, _) = build_seeds(&keys, true);
    let cfg = Config {
        seed: env_u64("KOB_C6_SEED", 0xc6c6),
        iterations: None,
        time: Some(Duration::from_secs(env_u64("KOB_C6_SECS", 600))),
        max_chain: 6,
        corpus_cap: 20_000,
    };
    let (stats, reports) = run(&seeds, &keys, &cfg, |l| println!("{l}"));
    let s = summary(&stats, &reports);
    println!("{s}");
    if let Ok(p) = std::env::var("KOB_C6_OUT") {
        std::fs::write(p, &s).unwrap();
    }
}

/// The oracle sees theft: on every keyless batch seed, an output paid to a maker who does not sign the transaction,
/// redirected to the attacker, and a custody token output handed to the attacker, are each flagged (oracle only,
/// without the engine).
#[test]
fn c6_oracle_flags_redirected_maker_outputs() {
    use c6::{p2pk_of, MTx};
    use kob_protocol::script::p2pk_spk;
    use kob_protocol::tx::SigPlan;
    let keys = keys();
    let (seeds, _) = build_seeds(&keys, false);
    let (mut tried, mut missed) = (0, vec![]);
    for s in &seeds {
        let signers: std::collections::BTreeSet<[u8; 32]> = s.m.ins.iter().filter_map(|i| i.plan.signer()).collect();
        let makers: std::collections::BTreeSet<[u8; 32]> =
            s.m.ins
                .iter()
                .filter_map(|i| match &i.plan {
                    SigPlan::Entry { template, state, .. } => {
                        kob_protocol::state::AnyState::decode(*template, state).ok().map(|x| x.maker())
                    }
                    _ => None,
                })
                .filter(|k| !signers.contains(k))
                .collect();
        for j in 0..s.m.outs.len() {
            let to_maker = p2pk_of(&s.m.outs[j].script_public_key).is_some_and(|k| makers.contains(&k));
            let token_to_maker =
                kob_protocol::artifacts::spk_trace::lookup(&s.m.outs[j].script_public_key).is_some_and(|(o, st)| match o {
                    kob_protocol::artifacts::spk_trace::Origin::Template(t) if t.is_token() => {
                        let tt = kob_protocol::artifacts::token_template(t);
                        let ts = kob_protocol::state::TokenState::decode_with(tt, &st).unwrap();
                        makers.contains(&ts.owner()) && ts.amount() > 0
                    }
                    _ => false,
                });
            if !to_maker && !token_to_maker {
                continue;
            }
            tried += 1;
            let mut m: MTx = s.m.clone();
            m.outs[j].script_public_key = p2pk_spk(&common::pk(ATTACKER));
            if oracle(&m).is_empty() {
                missed.push(format!("{} output {j}", s.name));
            }
        }
    }
    println!("{tried} redirected maker outputs, {} missed", missed.len());
    assert!(tried > 100);
    assert!(missed.is_empty(), "the oracle missed:\n{}", missed.join("\n"));
}

/// The oracle values makers exactly (base units, the covenants' rounding): on every keyless seed, a KAS output paid to a
/// maker who does not sign is cut by ONE sompi; wherever the builder pays exactly the maker's boundary (ceil of what it
/// receives, floor of what it pays, at an amount not a multiple of the scale included) the oracle flags it, and it never
/// flags the uncut seed. Oracle only, without the engine.
#[test]
fn c6_oracle_flags_one_sompi_short() {
    use c6::{p2pk_of, MTx};
    use kob_protocol::tx::SigPlan;
    let keys = keys();
    let (seeds, _) = build_seeds(&keys, false);
    let (mut tried, mut flagged) = (0, 0);
    let mut kinds: std::collections::BTreeMap<String, (u64, u64)> = Default::default();
    for s in &seeds {
        let signers: std::collections::BTreeSet<[u8; 32]> = s.m.ins.iter().filter_map(|i| i.plan.signer()).collect();
        let makers: std::collections::BTreeSet<[u8; 32]> =
            s.m.ins
                .iter()
                .filter_map(|i| match &i.plan {
                    SigPlan::Entry { template, state, .. } => {
                        kob_protocol::state::AnyState::decode(*template, state).ok().map(|x| x.maker())
                    }
                    _ => None,
                })
                .filter(|k| !signers.contains(k))
                .collect();
        for j in 0..s.m.outs.len() {
            if !p2pk_of(&s.m.outs[j].script_public_key).is_some_and(|k| makers.contains(&k)) || s.m.outs[j].value < 2 {
                continue;
            }
            tried += 1;
            let mut m: MTx = s.m.clone();
            m.outs[j].value -= 1;
            let hit = !oracle(&m).is_empty();
            if !hit && std::env::var("KOB_C6_DEBUG").is_ok() {
                println!("not flagged: {} output {j}", s.name);
            }
            flagged += u64::from(hit);
            let family = s.name.split(['.', '@']).take(2).collect::<Vec<_>>().join(".");
            let e = kinds.entry(family).or_default();
            e.0 += 1;
            e.1 += u64::from(hit);
        }
    }
    for (k, (t, f)) in &kinds {
        println!("  {k:<28} {f:>5} of {t:>5} maker outputs one sompi short flagged");
    }
    println!("{flagged} of {tried} maker outputs one sompi short flagged");
    println!("makers without an exact verdict: {}", c6::UNRESOLVED.with(|u| u.get()));
    assert!(tried > 100 && flagged * 2 > tried, "the oracle is not exact: {flagged} of {tried}");
}

/// Canary: with the covenant of one order kind switched off (its input scripts ignored), the same fuzzer and oracle
/// find the theft within a CI-sized run. A fuzzer that finds nothing here would find nothing anywhere.
#[test]
fn c6_canary_finds_theft_when_a_covenant_is_disabled() {
    use kob_protocol::artifacts::TemplateId;
    let keys = keys();
    let (seeds, _) = build_seeds(&keys, false);
    for t in [
        TemplateId::KobAsk,
        TemplateId::KobBid,
        TemplateId::KobCondAsk,
        TemplateId::KobIfdBid,
        TemplateId::KobPair,
        TemplateId::KobCondPair,
        TemplateId::KobIfdPair,
    ] {
        c6::ABLATE.with(|a| a.set(Some(t)));
        let cfg = Config { seed: 7, iterations: Some(3_000), time: None, max_chain: 3, corpus_cap: 500 };
        let (stats, reports) = run(&seeds, &keys, &cfg, |_| {});
        c6::ABLATE.with(|a| a.set(None));
        let kinds: Vec<&str> = reports.iter().map(|r| r.finding.kind.as_str()).collect();
        println!("{} disabled: {} findings in {} iterations: {kinds:?}", t.name(), reports.len(), stats.iterations);
        for r in reports.iter().take(3) {
            println!("   {} on {}: {} | {}", r.finding.kind, r.seed, r.finding.detail, r.steps.join("; "));
        }
        assert!(!reports.is_empty(), "{}: the canary found nothing", t.name());
    }
}

/// Replays one lineage (`KOB_C6_REPLAY="<seed name>|op:seed,op:seed"`, with the same `KOB_C6_EXTRA_SEEDS` as the run that
/// found it) and prints the mutant, its engine verdict and the oracle.
#[test]
#[ignore]
fn c6_fuzz_replay() {
    let Ok(spec) = std::env::var("KOB_C6_REPLAY") else { return };
    let keys = keys();
    let (seeds, _) = build_seeds(&keys, true);
    let (name, chain) = spec.split_once('|').expect("<seed name>|op:seed,...");
    let base = seeds.iter().position(|s| s.name == name).expect("seed name");
    let chain: Vec<(usize, u64)> = chain
        .split(',')
        .filter(|s| !s.is_empty())
        .map(|p| {
            let (o, s) = p.split_once(':').unwrap();
            (o.parse().unwrap(), s.parse().unwrap())
        })
        .collect();
    let cx = c6::mutate::Cx { donors: seeds.iter().map(|s| s.m.clone()).collect() };
    let (m, steps) = c6::fuzz::replay(&seeds, &cx, &c6::fuzz::Lineage { base, chain });
    println!("steps: {steps:?}");
    for (i, x) in m.ins.iter().enumerate() {
        let what = match &x.plan {
            kob_protocol::tx::SigPlan::Entry { template, entry, args, .. } => format!("{} {entry} {args:?}", template.name()),
            kob_protocol::tx::SigPlan::TokenLeader { state, next_states, .. } => {
                format!(
                    "leader {} owner {:02x?} scheme {} next {:?}",
                    state.amount,
                    &state.owner[..4],
                    state.owner_scheme,
                    next_states.iter().map(|s| (s.amount, s.owner_scheme)).collect::<Vec<_>>()
                )
            }
            kob_protocol::tx::SigPlan::TokenDelegator { state, .. } => {
                format!("delegator {} owner {:02x?} scheme {}", state.amount, &state.owner[..4], state.owner_scheme)
            }
            p => format!("{p:?}").chars().take(80).collect(),
        };
        println!("in {i}: {} sompi cov {:02x?} {what}", x.entry.amount, x.entry.covenant_id.map(|h| h.as_bytes()[0]));
    }
    for (j, o) in m.outs.iter().enumerate() {
        let t = kob_protocol::artifacts::spk_trace::lookup(&o.script_public_key).map(|(o, s)| format!("{o:?} {}", s.len()));
        println!(
            "out {j}: {} sompi cov {:?} p2pk {:?} {t:?}",
            o.value,
            o.covenant.map(|b| (b.authorizing_input, b.covenant_id.as_bytes()[0])),
            c6::p2pk_of(&o.script_public_key).map(|k| k[0])
        );
    }
    println!("{:?}", c6::fuzz::evaluate(&m, &keys).is_accepted());
    println!("oracle: {:?}", oracle(&m));
}

/// Pair orders in both tokens: on every keyless pair seed, a token output paid to a maker who does not sign (an ask's B
/// delivery, a bid's A delivery, a return of the custody) gives ONE base unit to the attacker (a new token output of
/// the same token: the supply is unchanged, so only the maker's books can see it). Wherever the builder pays exactly
/// the maker's boundary (ceil of what an ask receives, exactly n of what a bid buys, the exact custody), the oracle
/// flags it; it never flags the uncut seed (`c6_seeds_are_valid_and_clean`). Oracle only, without the engine.
#[test]
fn c6_oracle_flags_pair_token_one_unit_short() {
    use c6::MTx;
    use kaspa_consensus_core::tx::TransactionOutput;
    use kob_protocol::artifacts::spk_trace::{lookup, Origin};
    use kob_protocol::artifacts::token_template;
    use kob_protocol::state::{AnyState, TokenState};
    use kob_protocol::tx::SigPlan;
    let keys = keys();
    let (seeds, _) = build_seeds(&keys, false);
    let (mut tried, mut flagged) = (0u64, 0u64);
    let mut kinds: std::collections::BTreeMap<String, (u64, u64)> = Default::default();
    for s in seeds.iter().filter(|s| s.name.starts_with("pair.")) {
        let signers: std::collections::BTreeSet<[u8; 32]> = s.m.ins.iter().filter_map(|i| i.plan.signer()).collect();
        let makers: std::collections::BTreeSet<[u8; 32]> =
            s.m.ins
                .iter()
                .filter_map(|i| match &i.plan {
                    SigPlan::Entry { template, state, .. } => {
                        AnyState::decode(*template, state).ok().filter(|x| x.is_pair()).map(|x| x.maker())
                    }
                    _ => None,
                })
                .filter(|k| !signers.contains(k))
                .collect();
        for j in 0..s.m.outs.len() {
            let Some((Origin::Template(t), st)) = lookup(&s.m.outs[j].script_public_key) else { continue };
            if !t.is_token() {
                continue;
            }
            let tt = token_template(t);
            let ts = TokenState::decode_with(tt, &st).unwrap();
            if !ts.is_user() || !makers.contains(&ts.owner()) || ts.amount() < 2 {
                continue;
            }
            let Some(bind) = s.m.outs[j].covenant else { continue };
            tried += 1;
            let mut m: MTx = s.m.clone();
            m.outs[j].script_public_key = ts.with_amount(ts.amount() - 1).spk_with(tt);
            let thief = TokenState::user(t.family(), 1, common::pk(ATTACKER), ts.extension());
            m.outs.push(TransactionOutput { value: s.m.outs[j].value, script_public_key: thief.spk_with(tt), covenant: Some(bind) });
            let hit = !oracle(&m).is_empty();
            if !hit && std::env::var("KOB_C6_DEBUG").is_ok() {
                println!("not flagged: {} output {j}", s.name);
            }
            flagged += u64::from(hit);
            let family = s.name.split(['@']).next().unwrap_or("").to_string();
            let e = kinds.entry(family).or_default();
            e.0 += 1;
            e.1 += u64::from(hit);
        }
    }
    for (k, (t, f)) in &kinds {
        println!("  {k:<34} {f:>5} of {t:>5} maker token outputs one base unit short flagged");
    }
    println!("{flagged} of {tried} pair maker token outputs one base unit short flagged");
    assert!(tried > 50 && flagged * 2 > tried, "the oracle is not exact on pair tokens: {flagged} of {tried}");
}

/// The twelve findings of the first pair-phase long run (seed 2026100601), replayed. All are "different world" mutants
/// of a pair order's own terms (`orderState`):
/// - a KobIfdPair sell-first entry whose limit is 0 or negative, auctioned from its stop: the covenant fills only at a
///   positive quote, so the maker's worst is 1 (the oracle had valued it as "no fill possible");
/// - a KobPair whose scale of A is negative: KobPair has no `scale > 0` check (finding NP64); only the maker can create
///   such terms (builders and indexer refuse them), so the maker is left unvalued and counted (`INVALID_TERMS`).
///
/// Each replay must leave the oracle clean.
#[test]
fn c6_pair_long_run_regressions() {
    let keys = keys();
    let (seeds, _) = build_seeds(&keys, true);
    let cx = c6::mutate::Cx { donors: seeds.iter().map(|s| s.m.clone()).collect() };
    let cases: [(&str, u64, bool); 12] = [
        ("pair.ask.ioc@KronToken2433>KCC20Ref+oneMaker", 7585978075027431233, true),
        ("pairgrid.ask.return.decayfalse.twap@KronToken2433>KCC20Ref_8x8+oneMaker", 16287918994125123015, true),
        ("pairgrid.ifd.ask.auction.close.tip@KronToken2433>KCC20Ref_8x8+oneMaker", 12382582100974033423, false),
        ("pairgrid.ifd.ask.arm1.close@KronToken2433>KCC20Ref_8x8", 13679103126665020201, false),
        ("pair.ask.close@KCC20KaspaCom_0_2_5>KCC20P2", 17997030425699801929, true),
        ("pair.net.2x2.close@KCC20Ref_8x8>KronToken2433", 6091004526121319853, true),
        ("pair.ask.fok@KronToken2433>KCC20Ref+oneMaker", 7709506244033634556, true),
        ("pairgrid.ask.return.decaytrue.twap@KCC20Ref_8x8>KCC20Ref_8x8", 10531623874732506812, true),
        ("pairgrid.ifd.ask.arm0.close@KronToken2433>KCC20Ref_8x8", 3736764389874600003, false),
        ("pairgrid.ifd.ask.auction.close@KCC20Ref_8x8>KCC20Ref_8x8", 405491482584364473, false),
        ("pairgrid.ifd.ask.arm1.close.tip@KronToken2433>KCC20Ref_8x8+oneMaker", 1288796983043464974, false),
        ("pair.ask.ioc@KCC20KaspaCom_0_2_5>KCC20P2", 4104355800711999445, true),
    ];
    for (name, op_seed, np64) in cases {
        let Some(base) = seeds.iter().position(|s| s.name == name) else { panic!("seed {name} not in the corpus") };
        let (m, steps) = c6::fuzz::replay(&seeds, &cx, &c6::fuzz::Lineage { base, chain: vec![(26, op_seed)] });
        let before = c6::INVALID_TERMS.with(|u| u.get());
        let accepted = c6::fuzz::evaluate(&m, &keys);
        let after = c6::INVALID_TERMS.with(|u| u.get());
        println!("{name}: {steps:?} accepted {:?} invalid-terms makers {}", accepted.is_accepted().is_ok(), after - before);
        if let Ok(f) = accepted.is_accepted() {
            assert!(f.is_empty(), "{name}: the oracle flags {f:?}");
            assert_eq!(after > before, np64, "{name}: NP64 accounting");
        }
    }
}
