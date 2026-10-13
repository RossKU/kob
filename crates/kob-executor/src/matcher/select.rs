//! Weighted work selection among competing executors (`docs/spec/matcher.md` §3.6).
//!
//! The reference planner is deterministic: two executors with the same view build the same batch, and with several of them
//! running (the long-term plan: miners run executors) all but one transaction of every race are refused (`MissingInput` /
//! `DoubleSpend`). Kaspa's node avoids the same waste for block templates: when the mempool frontier holds more than a block,
//! each miner samples its transactions with probability proportional to `feerate^ALPHA` without replacement, so parallel
//! blocks of the DAG carry different transactions; when the frontier fits one block it takes everything
//! (rusty-kaspa v2.1.0 `mining/src/mempool/model/frontier.rs:379-390` `Frontier::build_selector`: `TakeAllSelector`, else
//! `sample_inplace` or the `MutatingTreeSelector` of `frontier/selectors.rs:284-376`, which draws (`:349-350`) `rng.gen::<f64>() ·
//! total_weight` and removes the drawn key; the weight `(fee / mass)^ALPHA` is `frontier/feerate_key.rs:31`, `ALPHA = 3`
//! `frontier/selectors.rs:18`).
//!
//! The executor mirrors that at the level of the work units it can take independently, the components of the view (the
//! books of one token, joined by the pair orders between tokens, §3.1):
//!
//! * a unit's weight is its standalone profit per byte (the executor's "fee rate") to the power `alpha` (1 by default, see
//!   [`DEFAULT_ALPHA`] for why not Kaspa's 3);
//! * per tick the executor takes every *mandatory* unit (it holds class 1 or class 2 work, or a crossing visible for at least
//!   `max_defer_daa`: the starvation bound) and samples a share of the others without replacement, proportionally to their
//!   weight ([`sample`]); the units it leaves wait for the next tick, when another executor has usually taken them;
//! * the share is the executor's fair share `1 / n̂` of the work, `n̂` the number of executors racing for it, estimated
//!   from the share of its own transactions that lose races ([`Selector::observe`], [`fair_share`]). It starts at 1 (take
//!   everything, as Kaspa's `TakeAllSelector` when the frontier fits: a lone executor plans exactly as the reference) and
//!   returns to 1 once the losses stop; it never falls below `min_share`.
//!
//! Nothing inside a unit is random: its priority classes, price → tip → age, netting and routing are the reference planner's
//! (a selected unit is planned exactly as the reference planner plans it alone). The keepers sample their kills, refunds and
//! closes the same way (`crate::keepers`), weighted by their tip per byte. Selection is not consensus: the covenants accept any
//! valid fill, and no user's terms depend on it.

use std::collections::{BTreeMap, BTreeSet};

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use serde::{Deserialize, Serialize};

/// The weighted selection of one tick (matcher or keepers), set by the runner on the engine's and the keepers' configuration
/// every step. Integers so that the configurations stay `Eq`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TickSelect {
    /// Seed of the tick's random source (drawn by the runner from its own, OS-seeded or `--select-seed`).
    pub seed: u64,
    /// The share of the optional units to take, parts per million (1,000,000 or more: every unit, no sampling).
    pub share_ppm: u32,
    /// Weight exponent, thousandths (1,000: proportional to the profit per byte; Kaspa's `ALPHA = 3` is 3,000; 0: uniform among
    /// the optional units).
    pub alpha_milli: u32,
    /// A crossing (a refund) visible for this many DAA is taken by every executor (the starvation bound).
    pub max_defer_daa: u64,
}

impl Default for TickSelect {
    fn default() -> Self {
        TickSelect { seed: 0, share_ppm: PPM, alpha_milli: (DEFAULT_ALPHA * 1_000.0) as u32, max_defer_daa: DEFAULT_MAX_DEFER_DAA }
    }
}

/// One whole share, parts per million.
pub const PPM: u32 = 1_000_000;

/// Default starvation bound: 30 DAA (3 s at 10 blocks per second).
pub const DEFAULT_MAX_DEFER_DAA: u64 = 30;

/// Default weight exponent: 1, the probability proportional to the profit per byte. Kaspa weighs `feerate^3`
/// (`frontier/selectors.rs:18`) because a miner wants the fees and a transaction two parallel blocks carry costs nothing; a
/// work unit two executors take costs one of them its transaction, and an exponent of 3 sends every executor to the same
/// best units. Measured on the race of `tests/matcher_select.rs` (`explore_selection_parameters`): five executors lose 41 %
/// of their transactions at 1, 61 % at 3 (the reference: 80 %).
pub const DEFAULT_ALPHA: f64 = 1.0;

impl TickSelect {
    /// The share as a fraction in `[0, 1]`.
    pub fn share(&self) -> f64 {
        (self.share_ppm.min(PPM) as f64) / PPM as f64
    }
    /// Whether the tick samples at all (a share below one).
    pub fn samples(&self) -> bool {
        self.share_ppm < PPM
    }
    pub fn alpha(&self) -> f64 {
        self.alpha_milli as f64 / 1_000.0
    }
    /// The tick's random source.
    pub fn rng(&self) -> StdRng {
        StdRng::seed_from_u64(self.seed)
    }
}

/// Runner settings (`--no-weighted-select`, `--select-alpha`, `--select-max-defer-daa`, `--select-min-share`,
/// `--select-seed`).
#[derive(Clone, Debug, PartialEq)]
pub struct SelectConfig {
    /// Off: the reference planner and keepers, deterministic (the library default; the live commands turn it on).
    pub enabled: bool,
    /// Weight exponent ([`DEFAULT_ALPHA`]; Kaspa: 3).
    pub alpha: f64,
    /// The starvation bound, DAA.
    pub max_defer_daa: u64,
    /// The lowest share the races may push the executor to (the most executors it makes room for: `1 / min_share`).
    pub min_share: f64,
    /// Weight of a step in the moving average of the lost fraction, in (0, 1].
    pub smoothing: f64,
    /// Seed of the runner's random source (tests); None: OS randomness.
    pub seed: Option<u64>,
}

/// Default [`SelectConfig::min_share`]: room for ten executors.
pub const DEFAULT_MIN_SHARE: f64 = 0.1;

impl Default for SelectConfig {
    fn default() -> Self {
        SelectConfig {
            enabled: false,
            alpha: DEFAULT_ALPHA,
            max_defer_daa: DEFAULT_MAX_DEFER_DAA,
            min_share: DEFAULT_MIN_SHARE,
            smoothing: 0.25,
            seed: None,
        }
    }
}

impl SelectConfig {
    /// The checks of the command line: a finite exponent in `[0, 16]`, a minimum share in `(0, 1]`.
    pub fn check(&self) -> Result<(), String> {
        if !self.alpha.is_finite() || !(0.0..=16.0).contains(&self.alpha) {
            return Err(format!("--select-alpha {} outside [0, 16]", self.alpha));
        }
        if !self.min_share.is_finite() || self.min_share <= 0.0 || self.min_share > 1.0 {
            return Err(format!("--select-min-share {} outside (0, 1]", self.min_share));
        }
        if !self.smoothing.is_finite() || self.smoothing <= 0.0 || self.smoothing > 1.0 {
            return Err("the smoothing of the lost fraction must be in (0, 1]".into());
        }
        Ok(())
    }
}

/// Command-line options of the weighted selection (`kob-executor run`, `match`, `keep`; on by default).
#[derive(clap::Args, Debug, Clone)]
pub struct SelectArgs {
    /// Plan deterministically, as the reference planner and keepers: every executor with the same view builds the same
    /// transactions (`docs/spec/matcher.md` §3.6).
    #[arg(long)]
    pub no_weighted_select: bool,
    /// Weight exponent of the selection: a work unit (a due keeper job) is drawn with probability proportional to its
    /// profit (tip) per byte to this power (1: proportional; Kaspa's template selection uses 3; 0: uniform).
    #[arg(long, default_value_t = DEFAULT_ALPHA)]
    pub select_alpha: f64,
    /// A crossing (a due keeper job) visible for this many DAA is taken by every executor, deterministically: the bound on
    /// how long the selection may leave work for others (10 DAA per second).
    #[arg(long, default_value_t = DEFAULT_MAX_DEFER_DAA)]
    pub select_max_defer_daa: u64,
    /// The lowest share of the optional work the executor takes per tick. The share is the executor's estimate of its fair
    /// share (one over the executors racing for the work, from the races it loses): 1 for a lone executor.
    #[arg(long, default_value_t = DEFAULT_MIN_SHARE)]
    pub select_min_share: f64,
    /// Seed of the selection's random source (reproducible runs). Default: OS randomness.
    #[arg(long)]
    pub select_seed: Option<u64>,
}

impl SelectArgs {
    /// The runner's selection settings, checked.
    pub fn config(&self) -> Result<SelectConfig, String> {
        let c = SelectConfig {
            enabled: !self.no_weighted_select,
            alpha: self.select_alpha,
            max_defer_daa: self.select_max_defer_daa,
            min_share: self.select_min_share,
            seed: self.select_seed,
            ..SelectConfig::default()
        };
        c.check()?;
        Ok(c)
    }
}

/// Who selects: the matcher's batches or the keepers' jobs (each has its own share: their races are separate).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Match,
    Keep,
}

/// The runner's selection state: its random source and the share of each role, and what the selection did (metrics).
#[derive(Debug)]
pub struct Selector {
    pub cfg: SelectConfig,
    rng: StdRng,
    pub share_match: f64,
    pub share_keep: f64,
    /// Moving averages of the fraction of each role's transactions that lose races.
    pub lost_match: f64,
    pub lost_keep: f64,
    /// Units (matcher) and jobs (keepers) the selection judged, took, left for later, and took because they were overdue.
    pub stats: SelectStats,
    /// The DAA score from which each order has been crossable, as the matcher's sampling ticks saw it (in a work unit's
    /// standalone allocation, taken or left): the starvation bound counts from it (`EngineConfig::select_since`).
    pub history: BTreeMap<[u8; 32], u64>,
}

/// Counters of the selection (cumulative, `kob_select_*` metrics).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SelectStats {
    /// Ticks that sampled (a share below one).
    pub sampled_ticks: u64,
    /// Matcher units judged in sampling ticks, mandatory ones included.
    pub units: u64,
    /// Matcher units taken because they were overdue (the starvation bound) and not otherwise mandatory.
    pub overdue: u64,
    /// Optional matcher units taken by the sample.
    pub taken: u64,
    /// Optional matcher units left for later.
    pub deferred: u64,
    /// Keeper jobs taken by the sample, left for later, and taken because they were overdue.
    pub keeper_taken: u64,
    pub keeper_deferred: u64,
    pub keeper_overdue: u64,
    /// Orders this executor left for later that another transaction took before it did (the races it did not run: a proxy
    /// of the collisions avoided).
    pub ceded: u64,
    /// Steps that lost a race (matcher, keepers).
    pub lost_steps_match: u64,
    pub lost_steps_keep: u64,
}

impl Selector {
    pub fn new(cfg: SelectConfig) -> Self {
        let rng = match cfg.seed {
            Some(s) => StdRng::seed_from_u64(s),
            None => StdRng::from_entropy(),
        };
        Selector {
            cfg,
            rng,
            share_match: 1.0,
            share_keep: 1.0,
            lost_match: 0.0,
            lost_keep: 0.0,
            stats: SelectStats::default(),
            history: BTreeMap::new(),
        }
    }

    pub fn share(&self, role: Role) -> f64 {
        match role {
            Role::Match => self.share_match,
            Role::Keep => self.share_keep,
        }
    }

    /// The selection of the next tick of `role` (None when the selection is off).
    pub fn tick(&mut self, role: Role) -> Option<TickSelect> {
        if !self.cfg.enabled {
            return None;
        }
        let share = self.share(role).clamp(0.0, 1.0);
        Some(TickSelect {
            seed: self.rng.gen(),
            share_ppm: (share * PPM as f64).round() as u32,
            alpha_milli: (self.cfg.alpha * 1_000.0).round().clamp(0.0, u32::MAX as f64) as u32,
            max_defer_daa: self.cfg.max_defer_daa,
        })
    }

    /// Records the orders a matcher tick found crossable at `daa` (every order of its work units, [`TickSelect`] ticks only;
    /// none for a tick that did not sample): each keeps the score it first became crossable at, the others leave the history
    /// (an order that stops crossing starts again when it crosses again).
    pub fn record(&mut self, crossable: &[[u8; 32]], daa: u64) {
        let now: BTreeSet<&[u8; 32]> = crossable.iter().collect();
        self.history.retain(|id, _| now.contains(id));
        for id in crossable {
            self.history.entry(*id).or_insert(daa);
        }
    }

    /// What a step of `role` saw: `lost` transactions that lost a race (refused as a double spend or missing input, or dropped
    /// because another transaction spent an input), `sent` taken into the mempool. The step's lost fraction enters the
    /// role's moving average, and the share becomes the fair share that average implies ([`fair_share`]), never below
    /// `min_share`. A step that submitted nothing changes nothing.
    pub fn observe(&mut self, role: Role, lost: usize, sent: usize) {
        if !self.cfg.enabled || lost + sent == 0 {
            return;
        }
        let w = self.cfg.smoothing.clamp(f64::MIN_POSITIVE, 1.0);
        let min = self.cfg.min_share.clamp(f64::MIN_POSITIVE, 1.0);
        let (s, l) = match role {
            Role::Match => (&mut self.share_match, &mut self.lost_match),
            Role::Keep => (&mut self.share_keep, &mut self.lost_keep),
        };
        *l = (1.0 - w) * *l + w * (lost as f64 / (lost + sent) as f64);
        *s = fair_share(*s, *l).max(min);
        if lost > 0 {
            match role {
                Role::Match => self.stats.lost_steps_match += 1,
                Role::Keep => self.stats.lost_steps_keep += 1,
            }
        }
    }
}

/// Below this average lost fraction the executor counts as alone: share 1, the reference planner.
pub const ALONE_LOST_FRACTION: f64 = 0.02;

/// The fair share of an executor that took a share `s` of the optional work and lost a fraction `lost` of its transactions.
///
/// A unit the executor takes is also taken by `K` others, `K` about Poisson with mean `λ = (n − 1)·s` when the `n − 1` other
/// executors take the same share; the transactions reach the node in a random order, so the executor loses a unit with
/// probability `E[K / (K + 1)] = 1 − (1 − e^−λ) / λ`. The observed fraction gives `λ`, hence `n = 1 + λ / s` and the fair
/// share `1 / n = s / (s + λ)`. Two executors settle at one half each, three at one third; a lone executor (no losses) at
/// one. Against an executor that takes everything the loss stays at one half whatever the share, and the share falls to
/// `min_share`.
pub fn fair_share(s: f64, lost: f64) -> f64 {
    if !lost.is_finite() || lost < ALONE_LOST_FRACTION {
        return 1.0;
    }
    let lost = lost.min(0.999);
    let f = |lam: f64| 1.0 - (1.0 - (-lam).exp()) / lam;
    let (mut lo, mut hi) = (1e-9f64, 1e4f64);
    for _ in 0..100 {
        let mid = (lo * hi).sqrt();
        if f(mid) < lost {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let lam = (lo * hi).sqrt();
    let s = s.clamp(1e-6, 1.0);
    (s / (s + lam)).clamp(0.0, 1.0)
}

/// The weight of a unit (a job): its profit per byte relative to the best of its tick, to the power `alpha`. Relative so that
/// the floating point never overflows (Kaspa keeps `feerate^3` in f64 for the same range, `frontier.rs:241-248`); every weight
/// stays positive, so every optional unit keeps a chance.
pub fn weights(rates: &[f64], alpha: f64) -> Vec<f64> {
    let top = rates.iter().copied().filter(|r| r.is_finite()).fold(0.0f64, f64::max);
    rates
        .iter()
        .map(|&r| {
            let rel = if top > 0.0 && r.is_finite() && r > 0.0 { r / top } else { 0.0 };
            let w = rel.powf(alpha.max(0.0));
            if w.is_finite() && w > 0.0 {
                w
            } else {
                f64::MIN_POSITIVE
            }
        })
        .collect()
}

/// Profit per byte of a unit or job (a profit below one sompi counts as one: still a candidate).
pub fn rate(profit: i64, bytes: u64) -> f64 {
    profit.max(1) as f64 / bytes.max(1) as f64
}

/// How many of `m` optional units a tick takes at `share`: `share · m` rounded at random (its expectation is exactly
/// `share · m`), at most `m`.
pub fn take_count(share: f64, m: usize, rng: &mut impl Rng) -> usize {
    let x = share.clamp(0.0, 1.0) * m as f64;
    let base = x.floor();
    let extra = if rng.gen::<f64>() < x - base { 1 } else { 0 };
    ((base as usize) + extra).min(m)
}

/// Weighted sampling without replacement: `k` indices of `weights`, each draw proportional to the weights of the indices not
/// drawn yet (Kaspa's `MutatingTreeSelector::select_transactions`: a uniform query in `[0, total weight)` picks the key whose
/// prefix weight covers it, and the key leaves the tree). The order of the result is the draw order. The units of a tick are
/// few (one per token component), so a linear scan replaces Kaspa's search tree.
pub fn sample(weights: &[f64], k: usize, rng: &mut impl Rng) -> Vec<usize> {
    let mut left: Vec<usize> = (0..weights.len()).collect();
    let mut out = Vec::with_capacity(k.min(weights.len()));
    while out.len() < k && !left.is_empty() {
        let total: f64 = left.iter().map(|&i| weights[i]).sum();
        let pick = if total.is_finite() && total > 0.0 {
            let q = rng.gen::<f64>() * total;
            let mut acc = 0.0;
            let mut at = left.len() - 1;
            for (j, &i) in left.iter().enumerate() {
                acc += weights[i];
                if q < acc {
                    at = j;
                    break;
                }
            }
            at
        } else {
            rng.gen_range(0..left.len())
        };
        out.push(left.remove(pick));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The draws follow the weights: the first draw of a heavy unit is about as frequent as its weight's share, and every
    /// unit, the lightest included, is drawn sometimes.
    #[test]
    fn draws_follow_the_weights_and_every_unit_keeps_a_chance() {
        let w = [8.0, 4.0, 2.0, 1.0, 1.0];
        let mut rng = StdRng::seed_from_u64(7);
        let mut first = [0u32; 5];
        let mut any = [0u32; 5];
        let n = 40_000;
        for _ in 0..n {
            let s = sample(&w, 2, &mut rng);
            assert_eq!(s.len(), 2);
            assert_ne!(s[0], s[1], "without replacement");
            first[s[0]] += 1;
            for i in s {
                any[i] += 1;
            }
        }
        let total: f64 = w.iter().sum();
        for i in 0..5 {
            let want = w[i] / total;
            let got = first[i] as f64 / n as f64;
            assert!((got - want).abs() < 0.01, "unit {i}: first draw {got:.3}, weight share {want:.3}");
            assert!(any[i] > 0);
        }
        // the heaviest unit is in the sample at least as often as the share of the units the sample takes (2 of 5)
        assert!(any[0] as f64 / n as f64 >= 0.4);
    }

    /// The take count is `share · m` in expectation, between its floor and its ceiling.
    #[test]
    fn the_take_count_rounds_at_random() {
        let mut rng = StdRng::seed_from_u64(1);
        let mut sum = 0usize;
        for _ in 0..10_000 {
            let k = take_count(0.3, 5, &mut rng);
            assert!(k == 1 || k == 2);
            sum += k;
        }
        assert!((sum as f64 / 10_000.0 - 1.5).abs() < 0.05);
        assert_eq!(take_count(1.0, 5, &mut rng), 5);
        assert_eq!(take_count(0.0, 5, &mut rng), 0);
        assert_eq!(take_count(0.5, 0, &mut rng), 0);
    }

    /// Weights are relative to the best rate, positive and finite at any exponent; exponent 0 is uniform.
    #[test]
    fn weights_are_relative_positive_and_finite() {
        let w = weights(&[1e12, 1.0, 0.0, f64::NAN], 3.0);
        assert_eq!(w[0], 1.0);
        assert!(w.iter().all(|x| x.is_finite() && *x > 0.0));
        assert_eq!(weights(&[5.0, 1.0], 0.0), vec![1.0, 1.0]);
        assert!((weights(&[4.0, 2.0], 1.0)[1] - 0.5).abs() < 1e-12);
    }

    /// The command line: on by default with the documented defaults; `--no-weighted-select` turns it off; out-of-range values
    /// are refused.
    #[test]
    fn the_command_line_turns_it_on_by_default() {
        use clap::Parser;
        #[derive(clap::Parser)]
        struct Cli {
            #[command(flatten)]
            select: SelectArgs,
        }
        let c = Cli::try_parse_from(["x"]).unwrap().select.config().unwrap();
        assert_eq!(c, SelectConfig { enabled: true, ..SelectConfig::default() });
        assert_eq!((c.alpha, c.max_defer_daa, c.min_share), (DEFAULT_ALPHA, DEFAULT_MAX_DEFER_DAA, DEFAULT_MIN_SHARE));
        let off = Cli::try_parse_from(["x", "--no-weighted-select", "--select-seed", "7"]).unwrap().select.config().unwrap();
        assert!(!off.enabled);
        assert_eq!(off.seed, Some(7));
        for bad in [["x", "--select-alpha", "17"], ["x", "--select-min-share", "0"], ["x", "--select-min-share", "1.5"]] {
            assert!(Cli::try_parse_from(bad).unwrap().select.config().is_err(), "{bad:?}");
        }
    }

    /// The fair share: one alone, one half against one other executor taking the same share, one third against two.
    #[test]
    fn the_fair_share_counts_the_executors() {
        let poisson_loss = |lam: f64| 1.0 - (1.0 - (-lam).exp()) / lam;
        assert_eq!(fair_share(0.7, 0.0), 1.0);
        assert_eq!(fair_share(0.7, 0.01), 1.0, "a stray loss (a maker's cancel) is no competitor");
        for (n, s) in [(2.0, 0.5), (3.0, 1.0 / 3.0), (5.0, 0.2)] {
            let lost = poisson_loss((n - 1.0) * s);
            assert!((fair_share(s, lost) - 1.0 / n).abs() < 1e-6, "{n} executors at their fair share stay there");
        }
        // from everything (one transaction against another executor's identical one: half lost) to below a half
        let s1 = fair_share(1.0, 0.5);
        assert!(s1 > 0.3 && s1 < 0.5, "{s1}");
        assert!(fair_share(0.1, 0.5) < 0.1, "an executor that takes everything pushes the share down");
    }

    /// The share: one at the start, lowered by the races the executor loses, back to one once they stop; never below the
    /// minimum; the same seed gives the same ticks.
    #[test]
    fn the_share_follows_the_races_and_the_seed_reproduces_the_ticks() {
        let cfg = SelectConfig { enabled: true, seed: Some(42), ..SelectConfig::default() };
        let mut s = Selector::new(cfg.clone());
        assert_eq!(s.tick(Role::Match).map(|t| t.share_ppm), Some(PPM));
        s.observe(Role::Match, 1, 1);
        assert!(s.share_match < 1.0);
        for _ in 0..40 {
            s.observe(Role::Match, 1, 0);
        }
        assert_eq!(s.share_match, DEFAULT_MIN_SHARE, "never below the minimum share");
        assert_eq!(s.share_keep, 1.0, "the roles keep their own shares");
        let before = s.share_match;
        s.observe(Role::Match, 0, 0);
        assert_eq!(s.share_match, before, "a step that sent nothing changes nothing");
        for _ in 0..40 {
            s.observe(Role::Match, 0, 2);
        }
        assert_eq!(s.share_match, 1.0, "alone again: everything");
        let a: Vec<u64> = {
            let mut x = Selector::new(cfg.clone());
            (0..5).map(|_| x.tick(Role::Match).unwrap().seed).collect()
        };
        let b: Vec<u64> = {
            let mut x = Selector::new(cfg.clone());
            (0..5).map(|_| x.tick(Role::Match).unwrap().seed).collect()
        };
        assert_eq!(a, b);
        assert!(Selector::new(SelectConfig::default()).tick(Role::Match).is_none(), "off by default in the library");
    }
}
