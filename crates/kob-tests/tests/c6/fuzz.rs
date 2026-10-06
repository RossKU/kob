//! The fuzz loop: deterministic seeds, mutation chains replayable from (base seed, [(operator, operator seed)]),
//! a growing corpus of accepted mutants, and greedy minimisation of every finding to the shortest chain that still
//! shows it.

use std::collections::{BTreeMap, HashSet};
use std::time::{Duration, Instant};

use kob_protocol::tx::SigPlan;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

use super::mutate::{apply, balance, Cx, OPS, WEIGHTS};
use super::seeds::Seed;
use super::{accept_with, materialize, oracle, Finding, Keys, MTx, Reject, ABLATE, INVALID_TERMS, UNRESOLVED};

/// A replayable mutant.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Lineage {
    pub base: usize,
    pub chain: Vec<(usize, u64)>,
}

/// Replays a lineage: the mutant and the description of every operator that applied.
pub fn replay(seeds: &[Seed], cx: &Cx, l: &Lineage) -> (MTx, Vec<String>) {
    let mut m = seeds[l.base].m.clone();
    let mut d = vec![];
    for &(op, s) in &l.chain {
        let mut r = StdRng::seed_from_u64(s);
        if let Some(x) = apply(op, &mut m, &mut r, cx) {
            d.push(x);
        }
    }
    let mut r = StdRng::seed_from_u64(0xba1a_0000 ^ l.chain.len() as u64);
    balance(&mut m, &mut r);
    (m, d)
}

/// Outcome of one mutant.
pub enum Outcome {
    Rejected(Reject),
    Accepted(Vec<Finding>),
}

impl Outcome {
    /// `Ok(findings)` when the engine accepted the mutant, the rejection otherwise.
    pub fn is_accepted(&self) -> Result<&Vec<Finding>, &Reject> {
        match self {
            Outcome::Accepted(f) => Ok(f),
            Outcome::Rejected(r) => Err(r),
        }
    }
}

pub fn evaluate(m: &MTx, keys: &Keys) -> Outcome {
    match materialize(m, keys) {
        Err(e) => Outcome::Rejected(Reject::Assemble(e)),
        Ok((tx, en)) => match accept_with(&tx, &en, &ablated(m)) {
            Err(r) => Outcome::Rejected(r),
            Ok(()) => Outcome::Accepted(oracle(m)),
        },
    }
}

/// A minimised finding.
#[derive(Clone, Debug)]
pub struct Report {
    pub finding: Finding,
    pub seed: String,
    pub lineage: Lineage,
    pub steps: Vec<String>,
    pub first_seen: u64,
    pub hits: u64,
}

#[derive(Default, Debug)]
pub struct Stats {
    pub iterations: u64,
    pub accepted: u64,
    pub accepted_clean: u64,
    pub corpus: usize,
    pub seeds: usize,
    pub per_op: BTreeMap<&'static str, (u64, u64)>,
    pub rejects: BTreeMap<String, u64>,
    pub elapsed: Duration,
    /// Accepted mutants with a maker whose fill amounts were too many to search exactly (no value verdict for it).
    pub unresolved: u64,
    pub invalid_terms: u64,
}

pub struct Config {
    pub seed: u64,
    pub iterations: Option<u64>,
    pub time: Option<Duration>,
    pub max_chain: usize,
    pub corpus_cap: usize,
}

/// Greedy chain minimisation: drop operators while the finding kind persists.
pub fn minimize(seeds: &[Seed], cx: &Cx, keys: &Keys, l: &Lineage, kind: &str) -> Lineage {
    let shows = |l: &Lineage| {
        let (m, _) = replay(seeds, cx, l);
        matches!(evaluate(&m, keys), Outcome::Accepted(f) if f.iter().any(|x| x.kind == kind))
    };
    let mut cur = l.clone();
    loop {
        let mut improved = false;
        for i in 0..cur.chain.len() {
            let mut c = cur.clone();
            c.chain.remove(i);
            if shows(&c) {
                cur = c;
                improved = true;
                break;
            }
        }
        if !improved {
            return cur;
        }
    }
}

fn shape(m: &MTx) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    m.ins.len().hash(&mut h);
    m.outs.len().hash(&mut h);
    for i in &m.ins {
        match &i.plan {
            SigPlan::Entry { template, entry, args, .. } => {
                template.hash(&mut h);
                entry.hash(&mut h);
                format!("{args:?}").hash(&mut h);
            }
            p => std::mem::discriminant(p).hash(&mut h),
        }
    }
    for o in &m.outs {
        o.covenant.map(|b| b.authorizing_input).hash(&mut h);
        (o.value / 1_000_000).hash(&mut h);
    }
    h.finish()
}

/// Runs the fuzzer; returns the statistics and the minimised findings (one per finding kind and seed family).
pub fn run(seeds: &[Seed], keys: &Keys, cfg: &Config, mut log: impl FnMut(&str)) -> (Stats, Vec<Report>) {
    let cx = Cx { donors: seeds.iter().map(|s| s.m.clone()).collect() };
    let mut rng = StdRng::seed_from_u64(cfg.seed);
    let start = Instant::now();
    let unresolved0 = UNRESOLVED.with(|u| u.get());
    let invalid0 = INVALID_TERMS.with(|u| u.get());
    let mut stats = Stats { seeds: seeds.len(), ..Default::default() };
    let mut corpus: Vec<Lineage> = vec![];
    let mut shapes: HashSet<u64> = seeds.iter().map(|s| shape(&s.m)).collect();
    let mut reports: Vec<Report> = vec![];
    let mut seen: BTreeMap<(String, String), usize> = BTreeMap::new();
    let total_w: u32 = WEIGHTS.iter().sum();
    // seeds that spend an order without its maker's signature: where the covenants alone protect someone
    let keyless: Vec<usize> = (0..seeds.len()).filter(|&k| is_keyless(&seeds[k].m)).collect();
    log(&format!("{} seeds, {} keyless", seeds.len(), keyless.len()));
    let mut next_log = Duration::from_secs(60);
    loop {
        if cfg.iterations.is_some_and(|n| stats.iterations >= n) || cfg.time.is_some_and(|t| start.elapsed() >= t) {
            break;
        }
        stats.iterations += 1;
        let parent = if !corpus.is_empty() && rng.gen_bool(0.35) {
            corpus[rng.gen_range(0..corpus.len())].clone()
        } else if !keyless.is_empty() && rng.gen_bool(0.75) {
            Lineage { base: keyless[rng.gen_range(0..keyless.len())], chain: vec![] }
        } else {
            Lineage { base: rng.gen_range(0..seeds.len()), chain: vec![] }
        };
        let mut l = parent.clone();
        let n = 1 + (0..cfg.max_chain - 1).take_while(|_| rng.gen_bool(0.45)).count();
        let mut ops_used = vec![];
        for _ in 0..n {
            let mut x = rng.gen_range(0..total_w);
            let mut op = 0;
            while x >= WEIGHTS[op] {
                x -= WEIGHTS[op];
                op += 1;
            }
            l.chain.push((op, rng.gen()));
            ops_used.push(op);
        }
        let (m, _) = replay(seeds, &cx, &l);
        for &op in &ops_used {
            stats.per_op.entry(OPS[op]).or_default().0 += 1;
        }
        match evaluate(&m, keys) {
            Outcome::Rejected(r) => *stats.rejects.entry(r.class()).or_default() += 1,
            Outcome::Accepted(findings) => {
                stats.accepted += 1;
                for &op in &ops_used {
                    stats.per_op.entry(OPS[op]).or_default().1 += 1;
                }
                if findings.is_empty() {
                    stats.accepted_clean += 1;
                    if corpus.len() < cfg.corpus_cap && shapes.insert(shape(&m)) {
                        corpus.push(l.clone());
                    }
                }
                for f in findings {
                    let family = seeds[l.base].name.split(['@', '+']).next().unwrap_or("").to_string();
                    let key = (f.kind.clone(), family);
                    if let Some(&k) = seen.get(&key) {
                        reports[k].hits += 1;
                        continue;
                    }
                    let min = minimize(seeds, &cx, keys, &l, &f.kind);
                    let (mm, steps) = replay(seeds, &cx, &min);
                    let finding = match evaluate(&mm, keys) {
                        Outcome::Accepted(fs) => fs.into_iter().find(|x| x.kind == f.kind).unwrap_or(f.clone()),
                        _ => f.clone(),
                    };
                    log(&format!(
                        "FINDING {} on {} after {} iterations: {} | steps: {} | lineage {:?}",
                        finding.kind,
                        seeds[min.base].name,
                        stats.iterations,
                        finding.detail,
                        steps.join("; "),
                        min
                    ));
                    seen.insert(key, reports.len());
                    reports.push(Report {
                        finding,
                        seed: seeds[min.base].name.clone(),
                        lineage: min,
                        steps,
                        first_seen: stats.iterations,
                        hits: 1,
                    });
                }
            }
        }
        if start.elapsed() >= next_log {
            next_log += Duration::from_secs(60);
            log(&format!(
                "[{:>5}s] iterations {} accepted {} corpus {} findings {}",
                start.elapsed().as_secs(),
                stats.iterations,
                stats.accepted,
                corpus.len(),
                reports.len()
            ));
        }
    }
    stats.corpus = corpus.len();
    stats.elapsed = start.elapsed();
    stats.unresolved = UNRESOLVED.with(|u| u.get()) - unresolved0;
    stats.invalid_terms = INVALID_TERMS.with(|u| u.get()) - invalid0;
    (stats, reports)
}

/// True when the transaction spends an order or intent whose owner does not sign it.
pub fn is_keyless(m: &MTx) -> bool {
    let signers: HashSet<[u8; 32]> = m.ins.iter().filter_map(|i| i.plan.signer()).collect();
    m.ins.iter().any(|i| match &i.plan {
        SigPlan::Entry { template, state, .. } => {
            kob_protocol::state::AnyState::decode(*template, state).is_ok_and(|s| !signers.contains(&s.maker()))
        }
        SigPlan::Router { .. } => i.plan.signer().is_none(),
        _ => false,
    })
}

fn ablated(m: &MTx) -> Vec<bool> {
    match ABLATE.with(|a| a.get()) {
        None => vec![],
        Some(t) => m.ins.iter().map(|i| matches!(&i.plan, SigPlan::Entry { template, .. } if *template == t)).collect(),
    }
}
