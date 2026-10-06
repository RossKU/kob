//! C6 differential tests, Rust <-> covenant: for every arithmetic rule the builders mirror, random inputs
//! (boundaries and near-overflow included) are built by the production builders and the boundary the Rust code
//! computes is probed in the rusty-kaspa v2.1.0 engine: the covenant must accept exactly the Rust boundary value and
//! refuse one unit worse (for the maker). Every probe moves one number of the builder's own transaction:
//!
//! | rule | probe |
//! |---|---|
//! | ask proceeds ceil(n * (p(t) - tip) / scale) (decay, TWAP slice origin, tip, IOC / sold-out carriers) | the maker's KAS output |
//! | bid spend floor(n * (p(t) + tip) / scale) and budget ceil(n * (pMax + tip) / scale) (rising, reserve, delivery carrier, continuation) | the delivery's KAS; the continuation (exact) |
//! | stop band / take-profit leg at t (conditional sell / buy) | the maker's output; the conditional bid's continuation |
//! | trailing ratchet steps (sell and buy side) | the continuation's stop (k, k + 1, k - 1) |
//! | keeper tips (arm, trail) | the continuation's value |
//! | refund tips and refund times (every kind; expiry, 90-day idle, IOC/FOK kill) | the maker's output; the lock time |
//! | if-done amounts (buy-first spend, sell-first proceeds and prefund) | the entry's continuation; the delivery carrier; the exit |
//! | pair order quote at t: ask B delivery ceil(n * p(t) / scale(A)), bid release floor(n * p(t) / scale(A)), KAS tip | the maker's B delivery (exact, minus one); the builder's release |
//! | activation, trigger rest time (minRestDaa), TWAP interval (CSV) | the lock time; the input sequence |
//!
//! Amounts are base units at random scales (powers of ten); every rule mirrors the covenants' `quoteOf` exactly (split
//! multiplication, ceil for what a maker receives, floor for what it pays: [`quote`], cross-checked against
//! `kob_protocol::state::quote_of` and the per-kind helpers) and most fills are not a multiple of the scale (the rounding
//! paths; counted per rule).
//!
//! `KOB_C6_DIFF_CASES` (default 25) random cases per rule, `KOB_C6_SEED`.

#[path = "../../kob-protocol/tests/common/mod.rs"]
#[allow(dead_code, unused_imports, clippy::all)]
mod common;

mod c6;

use std::collections::BTreeMap;

use c6::mutate::balance;
use c6::{accept, materialize, p2pk_of, Keys, MTx, ATTACKER};
use common::*;
use kaspa_consensus_core::tx::{CovenantBinding, TransactionOutput};
use kob_protocol::artifacts::{token_template, TemplateId};
use kob_protocol::build::*;
use kob_protocol::state::*;
use kob_protocol::tx::{Arg, KeyUtxo, OrderUtxo, SigPlan, TokenUtxo};
use kob_protocol::Family;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

/// Least KAS on a token output that KaspaCom's KCC20 0.2.5 accepts (C6-7, measured).
const KASPACOM_MIN_CARRIER: u64 = 50_000_000;

/// A small carrier every token program takes (KaspaCom's floor; a 1-sompi carrier is probed too: the builders refuse it).
const SMALL: i64 = KASPACOM_MIN_CARRIER as i64;

const PROGS: [TemplateId; 6] = [
    TemplateId::Kcc20Ref,
    TemplateId::Kcc20Ref8x8,
    TemplateId::Kcc20P2,
    TemplateId::Kcc20KaspaCom025,
    TemplateId::KronToken2433,
    TemplateId::KronToken2732,
];

fn keys() -> Keys {
    let mut k = common::keys();
    k.insert(pk(ATTACKER), sk(ATTACKER));
    k
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

#[derive(Default)]
struct Stat {
    cases: u64,
    probes: u64,
    skipped: u64,
    /// Built cases whose fill is not a multiple of the scale.
    off_scale: u64,
}

struct D {
    keys: Keys,
    r: StdRng,
    stats: BTreeMap<&'static str, Stat>,
    fails: Vec<String>,
    notes: BTreeMap<String, u64>,
}

impl D {
    fn new() -> D {
        D {
            keys: keys(),
            r: StdRng::seed_from_u64(env_u64("KOB_C6_SEED", 0xd1ff)),
            stats: BTreeMap::new(),
            fails: vec![],
            notes: BTreeMap::new(),
        }
    }
    fn st(&mut self, rule: &'static str) -> &mut Stat {
        self.stats.entry(rule).or_default()
    }
    fn note(&mut self, s: impl Into<String>) {
        *self.notes.entry(s.into()).or_default() += 1;
    }
    fn valid(&self, m: &MTx) -> Result<(), String> {
        let mut m = m.clone();
        balance(&mut m, &mut StdRng::seed_from_u64(1));
        let (tx, en) = materialize(&m, &self.keys)?;
        accept(&tx, &en).map_err(|e| format!("{e:?}"))
    }
    /// Builds an action (KRON: converted) and checks the builder's transaction is valid.
    fn build(&mut self, rule: &'static str, a: Action, kron: bool, ctx: &str) -> Option<MTx> {
        let a = if kron { kron_action(a) } else { a };
        let a_dbg = a.clone();
        let built = match build(&a) {
            Ok(b) => b,
            Err(e) => {
                self.st(rule).skipped += 1;
                let e = e.to_string();
                let shape: String = e.chars().map(|c| if c.is_ascii_digit() { '#' } else { c }).collect();
                let shape = shape.split("#").filter(|s| !s.is_empty()).collect::<Vec<_>>().join("#");
                self.note(format!("{rule}: builder refused: {}", shape.chars().take(140).collect::<String>()));
                if std::env::var("KOB_C6_DEBUG").is_ok() {
                    println!("DEBUG {rule} refused [{ctx}]: {e}");
                }
                return None;
            }
        };
        let m = MTx::from_built(&built);
        if let Err(e) = self.valid(&m) {
            if e.starts_with("Consensus(\"mass:") {
                // KIP-9: a fill paying a dust output (a tiny maker payment or carrier) has a storage mass beyond the block limit
                self.st(rule).skipped += 1;
                self.note(format!("{rule}: builder output beyond the KIP-9 mass limit (dust output), not probed"));
                return None;
            }
            // C6-7: KaspaCom's KCC20 0.2.5 refuses a token output carrying less than 0.5 KAS (measured), a floor the builders
            // do not know: a fill whose delivery carries less (a tiny deliveryCarrier) is refused by the token program
            let kaspacom_floor = m.outs.iter().any(|o| {
                o.value < KASPACOM_MIN_CARRIER
                    && kob_protocol::artifacts::spk_trace::lookup(&o.script_public_key)
                        .is_some_and(|(t, _)| t == kob_protocol::artifacts::spk_trace::Origin::Template(TemplateId::Kcc20KaspaCom025))
            });
            if kaspacom_floor && e.contains("VerifyError") {
                self.st(rule).skipped += 1;
                self.note(format!("{rule}: a KaspaCom token output below 0.5 KAS (C6-7), not probed"));
                return None;
            }
            let prog: Vec<String> = built.roles.iter().map(|r| r.to_string()).collect();
            if std::env::var("KOB_C6_DEBUG").is_ok() {
                if let Ok((tx, _)) = materialize(&m, &self.keys) {
                    for (k, (inp, p)) in tx.inputs.iter().zip(&m.ins).enumerate() {
                        let args = match &p.plan {
                            SigPlan::Entry { args, entry, .. } => format!("{entry} {args:?}"),
                            _ => String::new(),
                        };
                        println!(
                            "DEBUG input {k}: sigscript {} bytes, cov {:?} {args}",
                            inp.signature_script.len(),
                            p.entry.covenant_id.map(|h| h.as_bytes()[0])
                        );
                    }
                    println!("DEBUG {}", serde_json::to_string(&a_dbg).unwrap());
                }
            }
            self.fails.push(format!("{rule} [{ctx}]: the builder's own transaction is invalid: {e} | roles {prog:?}"));
            return None;
        }
        self.st(rule).cases += 1;
        Some(m)
    }
    /// The mutant must be accepted (`ok`) or refused.
    fn expect(&mut self, rule: &'static str, m: &MTx, ok: bool, what: String) {
        self.st(rule).probes += 1;
        match (self.valid(m), ok) {
            (Ok(()), true) | (Err(_), false) => {}
            (Err(e), true) => self.fails.push(format!("{rule}: REFUSED at the Rust boundary: {what}: {e}")),
            (Ok(()), false) => self.fails.push(format!("{rule}: ACCEPTED one unit worse than the Rust boundary: {what}")),
        }
    }
    /// Output `j` exactly at `need` is accepted, `need - 1` refused; and the builder pays at least `need`.
    fn floor(&mut self, rule: &'static str, m: &MTx, j: usize, need: i64, what: &str) {
        if need < 10_000_000 {
            // below 0.1 KAS the probe output itself would be dust (KIP-9 storage mass): the engine verdict is about mass
            self.note(format!("{rule}: boundary below 0.1 KAS not probed"));
            return;
        }
        let v = m.outs[j].value as i64;
        if v < need {
            self.fails.push(format!("{rule}: the builder pays {v} < the Rust boundary {need} ({what})"));
            return;
        }
        if v > need {
            self.note(format!("{rule}: builder pays above the boundary"));
        }
        for (val, ok) in [(need, true), (need - 1, false)] {
            let mut x = m.clone();
            x.outs[j].value = val as u64;
            self.expect(rule, &x, ok, format!("{what}: output {j} = {val} (Rust boundary {need}, builder {v})"));
        }
    }
    fn prog(&mut self) -> (TemplateId, bool) {
        let p = PROGS[self.r.gen_range(0..PROGS.len())];
        (p, p.family() == Family::Kron)
    }
}

fn input_of(m: &MTx, c: [u8; 32]) -> Option<usize> {
    m.ins.iter().position(|i| i.entry.covenant_id.map(|h| h.as_bytes()) == Some(c) && matches!(i.plan, SigPlan::Entry { .. }))
}

fn cont_of(m: &MTx, c: [u8; 32]) -> Option<usize> {
    m.outs.iter().position(|o| o.covenant.is_some_and(|b| b.covenant_id.as_bytes() == c))
}

fn batch(lock: u64, legs: Vec<Leg>) -> Batch {
    Batch {
        lock_time: lock,
        legs,
        updates: vec![],
        taker_tokens: vec![],
        taker: None,
        taker_token_carrier: CARRIER,
        keep_surplus: vec![],
        keep_carrier: None,
        receivers: vec![],
        payments: vec![],
        funding: vec![],
        change: None,
        records: vec![],
        fee: fee(),
    }
}

fn tok_custody(tag: u8, amount: i64, c: [u8; 32], daa: u64) -> TokenUtxo {
    tok_of(tag, amount, c, SCHEME_COVID, daa, TOKEN_COV)
}

fn funding(amount: i64) -> Vec<KeyUtxo> {
    vec![key_utxo(12, TAKER, (amount.max(0) as u64).saturating_add(1_000 * KAS))]
}

/// The covenants' `quoteOf(n, rate, scale)` exactly as the scripts compute it (split multiplication, `c = scale - 1`
/// for the ceil a maker receives, `c = 0` for the floor it pays, every step checked), cross-checked against
/// `kob_protocol::state::quote_of` on every call.
fn quote(n: i64, rate: i64, scale: i64, up: bool) -> Option<i64> {
    let c = if up { scale - 1 } else { 0 };
    let local = (|| {
        if n < 0 || rate < 0 || scale <= 0 {
            return None;
        }
        let (q, m) = (n / scale, n % scale);
        q.checked_mul(rate)?
            .checked_add(m.checked_mul(rate / scale)?)?
            .checked_add(m.checked_mul(rate % scale)?.checked_add(c)? / scale)
    })();
    let lib = quote_of(n, rate, scale, if up { Round::Up } else { Round::Down });
    assert_eq!(
        local,
        lib,
        "quoteOf({n}, {rate}, {scale}, {}) local split vs kob_protocol::state::quote_of",
        if up { "ceil" } else { "floor" }
    );
    local
}

/// Notes a case whose fill is not a multiple of the scale (the rounding paths).
fn off_scale(d: &mut D, rule: &'static str, n: i64, scale: i64) {
    if scale > 1 && n % scale != 0 {
        d.st(rule).off_scale += 1;
    }
}

/// A random order geometry: a scale (a power of ten), an amount in base units (mostly not a multiple of the scale) and a
/// price (quote units per whole token) keeping the full fill far below 2^62. A KRON token UTXO holds at most
/// `KRON_MAX_OUTPUT_AMOUNT` base units (the KRON programs refuse more), so a KRON amount stays within it.
fn geometry(r: &mut StdRng, kron: bool) -> (i64, i64, i64) {
    let scale = [1, 10, 1_000, 1_000, 100_000, 100_000_000][r.gen_range(0..6)];
    let amount = if r.gen_bool(0.25) { r.gen_range(1..=30) * scale } else { r.gen_range(1..=30 * scale) };
    let amount = if kron { amount.min(kob_protocol::registry::KRON_MAX_OUTPUT_AMOUNT) } else { amount };
    let cap = (4_000_000_000_000_000i128 * scale as i128 / amount as i128).clamp(2, 4_000_000_000_000_000) as i64;
    let price = match r.gen_range(0..4) {
        0 => r.gen_range(1..=1_000),
        1 => r.gen_range(1..=1_000_000_000),
        2 => r.gen_range(1..=cap.min(10_000_000_000_000)),
        _ => (cap - r.gen_range(0..1_000)).max(1),
    };
    (scale, amount, price.min(cap))
}

/// The decayed (or risen) quote `price -+ slope * ((t - origin) / step)` fits an i64 (the covenants' checked arithmetic;
/// `kob_protocol::state::decay_down` / `rise_up` compute it unchecked).
fn decay_fits(price: i64, slope: i64, step: i64, origin: i64, t: i64, up: bool) -> bool {
    if slope == 0 || step <= 0 {
        return true;
    }
    let k = slope as i128 * ((t - origin) / step) as i128;
    let v = if up { price as i128 + k } else { price as i128 - k };
    k <= i64::MAX as i128 && (i64::MIN as i128..=i64::MAX as i128).contains(&v)
}

/// A fill amount of an order of `amount` with minimum fill `min_fill`: everything, or at least the minimum.
fn fill_of(r: &mut StdRng, amount: i64, min_fill: i64) -> i64 {
    if r.gen_bool(0.25) || min_fill >= amount {
        amount
    } else {
        r.gen_range(min_fill.max(1)..=amount)
    }
}

/// A minimum fill for an order of `amount` that keeps it to at most `fills` fills.
fn min_fill_of(r: &mut StdRng, amount: i64, fills: i64) -> i64 {
    let least = (amount + fills - 1) / fills;
    if r.gen_bool(0.3) {
        least.max(1)
    } else {
        r.gen_range(least.max(1)..=amount)
    }
}

// ------------------------------------------------------------------------------------------------ asks

fn rule_ask(d: &mut D) {
    const R: &str = "ask.proceeds";
    let (tpl, kron) = d.prog();
    let (scale, amount, price) = geometry(&mut d.r, kron);
    let r = &mut d.r;
    let min_fill = if r.gen_bool(0.5) { 1 } else { r.gen_range(1..=amount) };
    let n = fill_of(r, amount, min_fill);
    let mut a = AskState { scale, min_fill, price, amount_left: amount, tip: 0, ..ask(MAKER_A, price, tpl) };
    let lock = NOW as i64;
    if r.gen_bool(0.5) {
        a.slope = r.gen_range(1..=(price / 20).max(1));
        a.price_end = r.gen_range(1..=price);
        a.decay_step = if r.gen_bool(0.3) { 1 } else { r.gen_range(1..=2_000) };
        a.active_from = r.gen_range(0..lock - 10);
    }
    let floor_price = if a.slope > 0 { a.price_end } else { price };
    if floor_price > 1 && r.gen_bool(0.6) {
        a.tip = r.gen_range(0..floor_price);
    }
    a.tif = [0, 0, 0, TIF_IOC, TIF_FOK][r.gen_range(0..5)];
    let n = if a.tif == TIF_FOK { amount } else { n };
    if r.gen_bool(0.2) {
        a.interval = r.gen_range(1..=2_000);
        if r.gen_bool(0.5) {
            a.max_fill = r.gen_range(n..=amount);
        }
    }
    let daa = r.gen_range(1_000..(lock - a.interval - 1).max(1_001)) as u64;
    let Some(origin) = a.origin(daa as i64).filter(|o| *o <= lock) else {
        d.st(R).skipped += 1;
        return;
    };
    if origin > lock {
        d.st(R).skipped += 1;
        return;
    }
    let t = (a.slope > 0).then(|| r.gen_range(origin..=lock));
    if !decay_fits(a.price, a.slope, a.decay_step, origin, t.unwrap_or(lock), false) {
        d.st(R).skipped += 1;
        d.note(format!("{R}: price - slope * steps does not fit an i64 (the covenant fails the script), not probed"));
        return;
    }
    let c = cov(0xa1);
    let Some(p) = a.price_at(t.unwrap_or(lock), daa as i64) else {
        d.st(R).skipped += 1;
        return;
    };
    let Some(need) = quote(n, p - a.tip, scale, true) else {
        d.st(R).skipped += 1;
        return;
    };
    if a.proceeds(n, t.unwrap_or(lock), daa as i64) != Some(need) {
        d.fails.push(format!("{R}: AskState::proceeds != ceil(n * (p - tip) / scale) = {need} ({a:?} n {n})"));
    }
    let mut b = batch(
        lock as u64,
        vec![Leg::Ask { order: order(10, CARRIER, c, daa, a.clone()), custody: tok_custody(11, amount, c, daa), amount: n, t }],
    );
    b.funding = funding(need);
    let ctx = format!("{a:?} n {n} t {t:?}");
    let Some(m) = d.build(R, Action::Batch(b), kron, &ctx) else { return };
    off_scale(d, R, n, scale);
    let i = input_of(&m, c).unwrap();
    let carriers = (CARRIER
        + m.ins
            .iter()
            .find(|x| x.entry.covenant_id.is_some() && !matches!(x.plan, SigPlan::Entry { .. }))
            .map(|x| x.entry.amount)
            .unwrap_or(0)) as i64;
    let extra = if n == amount {
        carriers
    } else if a.tif != TIF_GTC {
        CARRIER as i64
    } else {
        0
    };
    d.floor(R, &m, i, need + extra, &ctx);
    // TWAP: the slice interval is proven by the input's relative lock (CSV): one DAA less is refused
    if a.interval > 1 {
        let mut x = m.clone();
        x.ins[i].seq = (a.interval - 1) as u64;
        d.expect(R, &x, false, format!("{ctx}: sequence interval - 1"));
    }
    // activation: one DAA before activeFrom is refused
    if a.active_from > 0 && a.slope == 0 {
        let mut x = m.clone();
        x.lock_time = (a.active_from - 1) as u64;
        d.expect(R, &x, false, format!("{ctx}: lock activeFrom - 1"));
    }
}

// ------------------------------------------------------------------------------------------------ bids

fn rule_bid(d: &mut D) {
    const R: &str = "bid.spend+budget";
    let (tpl, kron) = d.prog();
    let (scale, amount, price) = geometry(&mut d.r, kron);
    let r = &mut d.r;
    let lock = NOW as i64;
    let mut b0 = BidState { scale, price, tip: 0, ..bid(MAKER_B, price, tpl) };
    if r.gen_bool(0.5) {
        b0.slope = r.gen_range(1..=(price / 20).max(1));
        b0.price_end = price + r.gen_range(0..=price.min(1_000_000_000_000));
        b0.decay_step = r.gen_range(1..=2_000);
        b0.active_from = r.gen_range(0..lock - 10);
    }
    if r.gen_bool(0.6) {
        b0.tip = r.gen_range(0..=price);
    }
    b0.delivery_carrier = [1, SMALL, KAS as i64, DC][r.gen_range(0..4)];
    b0.reserve = if r.gen_bool(0.3) { r.gen_range(0..=10 * KAS as i64) } else { 0 };
    b0.tif = [0, 0, 0, TIF_IOC, TIF_FOK][r.gen_range(0..5)];
    let n = r.gen_range(1..=amount);
    b0.min_fill = if r.gen_bool(0.5) { 1 } else { r.gen_range(1..=n) };
    let (Some(used_n), Some(used_min)) = (b0.used(n), b0.used(b0.min_fill)) else {
        d.st(R).skipped += 1;
        return;
    };
    // the bid continues when it can still afford one minimum fill, else it ends (FOK: always)
    let continues = b0.tif != TIF_FOK && r.gen_bool(0.6);
    let value = if continues {
        let funded = n + r.gen_range(b0.min_fill..=amount.max(b0.min_fill) + 3 * scale);
        match b0.used(funded) {
            Some(u) => u + 2 * b0.delivery_carrier + b0.reserve + r.gen_range(0..=used_min),
            None => {
                d.st(R).skipped += 1;
                return;
            }
        }
    } else {
        used_n + b0.delivery_carrier + b0.reserve + r.gen_range(0..used_min.max(1))
    };
    let daa = r.gen_range(1_000..lock - 1) as u64;
    let Some(origin) = b0.origin(daa as i64).filter(|o| *o <= lock) else {
        d.st(R).skipped += 1;
        return;
    };
    if origin > lock {
        d.st(R).skipped += 1;
        return;
    }
    let t = (b0.slope > 0).then(|| r.gen_range(origin..=lock));
    if !decay_fits(b0.price, b0.slope, b0.decay_step, origin, t.unwrap_or(lock), true) {
        d.st(R).skipped += 1;
        d.note(format!("{R}: price + slope * steps does not fit an i64 (the covenant fails the script), not probed"));
        return;
    }
    let c = cov(0xb1);
    let mut b = batch(lock as u64, vec![Leg::Bid { order: order(20, value as u64, c, daa, b0.clone()), amount: n, t }]);
    b.taker_tokens = vec![tok(21, n, pk(TAKER), SCHEME_P2PK, 1_000)];
    b.change = Some(pk(TAKER));
    let ctx = format!("{b0:?} value {value} n {n} t {t:?}");
    // the budget a fill consumes (ceil at pMax + tip) and the spend (floor at p(t) + tip), as the covenant rounds
    let Some(p) = b0.price_at(t.unwrap_or(lock), daa as i64) else {
        d.st(R).skipped += 1;
        return;
    };
    let (Some(used), Some(all_in)) = (quote(n, b0.price_max() + b0.tip, scale, true), quote(n, p + b0.tip, scale, false)) else {
        d.st(R).skipped += 1;
        return;
    };
    if b0.used(n) != Some(used) || b0.spend(n, t.unwrap_or(lock), daa as i64) != Some(all_in) {
        d.fails.push(format!("{R}: BidState::used / spend disagree with the covenant rounding ({ctx})"));
    }
    let Some(m) = d.build(R, Action::Batch(b), kron, &ctx) else { return };
    off_scale(d, R, n, scale);
    let i = input_of(&m, c).unwrap();
    let left = value - used;
    let can_continue = left - b0.delivery_carrier - b0.reserve >= used_min;
    if can_continue != b0.can_continue(left) {
        d.fails.push(format!("{R}: BidState::can_continue disagrees ({ctx})"));
    }
    if b0.tif == TIF_GTC && can_continue {
        d.floor(R, &m, i, b0.delivery_carrier + used - all_in, &ctx);
        // the continuation keeps exactly left - deliveryCarrier
        let k = cont_of(&m, c).expect("continuation");
        let exact = left - b0.delivery_carrier;
        if m.outs[k].value as i64 != exact {
            d.fails.push(format!("{R}: continuation {} != Rust {exact} ({ctx})", m.outs[k].value));
        }
        for (v, ok) in [(exact, true), (exact - 1, false), (exact + 1, false)] {
            let mut x = m.clone();
            x.outs[k].value = v as u64;
            d.expect(R, &x, ok, format!("{ctx}: continuation {v} (Rust {exact})"));
        }
    } else {
        d.floor(R, &m, i, value - all_in, &ctx);
    }
}

// ------------------------------------------------------------------------------------------------ conditional orders

fn rule_cond_ask(d: &mut D) {
    const R: &str = "condAsk.leg";
    let (tpl, kron) = d.prog();
    let (scale, amount, _) = geometry(&mut d.r, kron);
    let r = &mut d.r;
    let lock = NOW as i64;
    let cap = (4_000_000_000_000_000i128 * scale as i128 / amount as i128).min(MAX_STOP_PRICE as i128) as i64;
    let stop = r.gen_range(2..=cap.max(3));
    let min_fill = if r.gen_bool(0.5) { 1 } else { r.gen_range(1..=amount) };
    let mut s = CondAskState { scale, min_fill, amount_left: amount, stop_price: stop, tip: 0, ..cond_ask(MAKER_A, tpl) };
    s.tp_price = if r.gen_bool(0.7) { r.gen_range(stop..=stop.saturating_mul(2).min(cap).max(stop)) } else { 0 };
    s.slip_bps = [0, 1, 300, 9_999, 10_000, r.gen_range(0..=10_000)][r.gen_range(0..6)];
    s.band_daa = if r.gen_bool(0.3) { 0 } else { r.gen_range(1..=2_000) };
    let daa = r.gen_range(1_000..lock - 1) as u64;
    s.armed = if r.gen_bool(0.5) { 1 } else { r.gen_range(daa as i64..=lock) };
    let leg: u8 = if s.tp_price > 0 && r.gen_bool(0.3) { 0 } else { 1 };
    let floor = if leg == 0 { s.tp_price } else { s.stop_floor() };
    if floor > 1 && r.gen_bool(0.5) {
        s.tip = r.gen_range(0..floor);
    }
    let n = fill_of(r, amount, min_fill);
    let origin = armed_origin(s.armed, daa as i64).unwrap();
    let t = r.gen_range(origin.min(lock)..=lock);
    let c = cov(0xc1);
    let Some(leg_price) = s.leg_price(leg as i64, false, t, daa as i64) else {
        d.st(R).skipped += 1;
        return;
    };
    let Some(need) = quote(n, leg_price - s.tip, scale, true) else {
        d.st(R).skipped += 1;
        return;
    };
    if s.proceeds(n, leg_price) != Some(need) {
        d.fails.push(format!("{R}: CondAskState::proceeds disagrees with ceil(n * (legPrice - tip) / scale) = {need}"));
    }
    let mut b = batch(
        lock as u64,
        vec![Leg::CondAsk {
            order: order(30, CARRIER, c, daa, s.clone()),
            custody: tok_custody(31, amount, c, daa),
            amount: n,
            leg,
            evidence: None,
            t: Some(t),
            merge: None,
        }],
    );
    b.funding = funding(need);
    let ctx = format!("{s:?} n {n} leg {leg} t {t}");
    let Some(m) = d.build(R, Action::Batch(b), kron, &ctx) else { return };
    off_scale(d, R, n, scale);
    let i = input_of(&m, c).unwrap();
    let custody_v = m
        .ins
        .iter()
        .find(|x| x.entry.covenant_id.is_some() && !matches!(x.plan, SigPlan::Entry { .. }))
        .map(|x| x.entry.amount)
        .unwrap_or(0);
    let extra = if n == amount { (CARRIER + custody_v) as i64 } else { 0 };
    d.floor(R, &m, i, need + extra, &ctx);
}

fn rule_cond_bid(d: &mut D) {
    const R: &str = "condBid.leg";
    let (tpl, kron) = d.prog();
    let (scale, amount, _) = geometry(&mut d.r, kron);
    let r = &mut d.r;
    let lock = NOW as i64;
    let cap = (1_000_000_000_000_000i128 * scale as i128 / amount as i128).min(MAX_STOP_PRICE as i128 / 2) as i64;
    let stop = r.gen_range(2..=cap.max(3));
    let min_fill = min_fill_of(r, amount, 30);
    let mut s = CondBidState { scale, min_fill, amount_left: amount, stop_price: stop, tip: 0, ..cond_bid(MAKER_B, tpl) };
    s.tp_price = if r.gen_bool(0.7) { r.gen_range(1..=stop) } else { 0 };
    s.slip_bps = [0, 1, 300, 9_999, 10_000, r.gen_range(0..=10_000)][r.gen_range(0..6)];
    s.band_daa = if r.gen_bool(0.3) { 0 } else { r.gen_range(1..=2_000) };
    if r.gen_bool(0.5) {
        s.tip = r.gen_range(0..=stop);
    }
    s.delivery_carrier = [1, SMALL, KAS as i64, DC][r.gen_range(0..4)];
    let daa = r.gen_range(1_000..lock - 1) as u64;
    s.armed = if r.gen_bool(0.5) { 1 } else { r.gen_range(daa as i64..=lock) };
    let leg: u8 = if s.tp_price > 0 && r.gen_bool(0.3) { 0 } else { 1 };
    let n = fill_of(r, amount, min_fill);
    let origin = armed_origin(s.armed, daa as i64).unwrap();
    let t = r.gen_range(origin.min(lock)..=lock);
    let c = cov(0xe1);
    let Some(escrow) = s.escrow(s.max_fills()) else {
        d.st(R).skipped += 1;
        return;
    };
    let value = escrow + r.gen_range(0..KAS as i64);
    let Some(leg_price) = s.leg_price(leg as i64, false, t, daa as i64) else {
        d.st(R).skipped += 1;
        return;
    };
    let Some(spend) = quote(n, leg_price + s.tip, scale, false) else {
        d.st(R).skipped += 1;
        return;
    };
    if s.spend(n, leg_price) != Some(spend) {
        d.fails.push(format!("{R}: CondBidState::spend disagrees with floor(n * (legPrice + tip) / scale) = {spend}"));
    }
    let mut b = batch(
        lock as u64,
        vec![Leg::CondBid {
            order: order(40, value as u64, c, daa, s.clone()),
            amount: n,
            leg,
            evidence: None,
            t: Some(t),
            merge: None,
        }],
    );
    b.taker_tokens = vec![tok(41, n, pk(TAKER), SCHEME_P2PK, 1_000)];
    b.change = Some(pk(TAKER));
    let ctx = format!("{s:?} value {value} n {n} leg {leg} t {t}");
    let Some(m) = d.build(R, Action::Batch(b), kron, &ctx) else { return };
    off_scale(d, R, n, scale);
    let i = input_of(&m, c).unwrap();
    let left = value - spend;
    if n < amount {
        let k = cont_of(&m, c).expect("continuation");
        d.floor(R, &m, k, left - s.delivery_carrier, &format!("{ctx} (continuation keeps the rest)"));
        d.floor(R, &m, i, s.delivery_carrier, &format!("{ctx} (delivery carrier)"));
    } else {
        d.floor(R, &m, i, left, &ctx);
    }
}

/// An update batch: the evidence leg (taker side) and the update of `o` next to it.
fn update_batch(lock: u64, o: OrderUtxo<AnyState>, ev: Leg) -> Batch {
    let mut b = batch(lock, vec![]);
    match &ev {
        Leg::Ask { .. } => {
            b.funding = vec![key_utxo(94, TAKER, 1_000 * KAS)];
        }
        Leg::Bid { amount, .. } => {
            b.taker_tokens = vec![tok(93, *amount, pk(TAKER), SCHEME_P2PK, 1_000)];
            if std::env::var("KOB_C6_EVFUND").is_ok() {
                b.funding = vec![key_utxo(94, TAKER, 10 * KAS)];
            }
        }
        _ => {}
    }
    b.legs.push(ev);
    b.updates = vec![BatchUpdate { order: o, evidence: 0, evidence_b: None, take: None }];
    b.change = Some(pk(MATCHER));
    b
}

fn rule_trail(d: &mut D) {
    const R: &str = "trail.steps+keeperTip";
    let (tpl, kron) = d.prog();
    let r = &mut d.r;
    let sell = r.gen_bool(0.5);
    let step = [1, 7, 5_000_000, r.gen_range(1..=50_000_000)][r.gen_range(0..4)];
    let gap = r.gen_range(0..=20_000_000);
    let keeper_tip = [0, 1, ktip(tpl), r.gen_range(0..=KAS as i64)][r.gen_range(0..4)];
    let c = cov(0x46);
    let (state, ev, rp, k): (AnyState, Leg, i64, i64);
    if sell {
        let stop = r.gen_range(100_000_000..=200_000_000);
        let s = CondAskState {
            stop_price: stop,
            trail_step: step,
            trail_gap: gap,
            trail_wait: 0,
            keeper_tip,
            tp_price: if r.gen_bool(0.5) { stop + r.gen_range(1..=200_000_000) } else { 0 },
            ..cond_ask(MAKER_A, tpl)
        };
        rp = stop + gap + r.gen_range(0..=10 * step.min(30_000_000));
        k = s.trail_steps(rp);
        ev = ev_bid(rp, 1, tpl);
        state = AnyState::KobCondAsk(s);
    } else {
        let stop = r.gen_range(250_000_000..=400_000_000);
        let s = CondBidState {
            stop_price: stop,
            trail_step: step,
            trail_gap: gap,
            trail_wait: 0,
            keeper_tip,
            tp_price: if r.gen_bool(0.5) { r.gen_range(1..stop) } else { 0 },
            ..cond_bid(MAKER_B, tpl)
        };
        rp = (stop - gap - r.gen_range(0..=10 * step.min(20_000_000))).max(1);
        k = s.trail_steps(rp);
        ev = ev_ask(rp, 1, tpl);
        state = AnyState::KobCondBid(s);
    }
    let value = match &state {
        AnyState::KobCondBid(s) => s.escrow(2).unwrap() as u64,
        _ => CARRIER,
    };
    let ctx = format!(
        "{} amountLeft {:?} step {step} gap {gap} rp {rp} k {k} keeperTip {keeper_tip}",
        state.template_id().name(),
        state.amount_left()
    );
    let st = state.clone();
    let Some(m) = d.build(R, Action::Batch(update_batch(NOW, order(70, value, c, 2_000, state), ev)), kron, &ctx) else {
        if k >= 1 {
            d.note(format!("{R}: the builder refused a ratchet of k = {k} >= 1"));
        }
        return;
    };
    if k < 1 {
        d.fails.push(format!("{R}: the builder trailed although Rust k = {k} ({ctx})"));
        return;
    }
    let j = cont_of(&m, c).expect("continuation");
    let i = input_of(&m, c).unwrap();
    // the continuation carries exactly stop +- k steps; k + 1 and k - 1 are refused
    let st = if kron { st.into_family(Family::Kron) } else { st };
    let moved = |kk: i64| -> AnyState {
        let mut s2 = st.clone();
        match &mut s2 {
            AnyState::KobCondAsk(x) | AnyState::KobCondAskKron(x) => x.stop_price += kk * x.trail_step,
            AnyState::KobCondBid(x) | AnyState::KobCondBidKron(x) => x.stop_price -= kk * x.trail_step,
            _ => {}
        }
        s2
    };
    if m.outs[j].script_public_key != moved(k).spk() {
        d.fails.push(format!("{R}: the builder's continuation is not stop moved by Rust k = {k} ({ctx})"));
        return;
    }
    for kk in [k + 1, k - 1] {
        if kk < 1 {
            continue;
        }
        let mut x = m.clone();
        x.outs[j].script_public_key = moved(kk).spk();
        d.expect(R, &x, false, format!("{ctx}: continuation with {kk} steps"));
    }
    d.floor(R, &m, j, m.ins[i].entry.amount as i64 - keeper_tip, &format!("{ctx}: keeper tip"));
}

fn rule_arm(d: &mut D) {
    const R: &str = "arm.restTime+keeperTip";
    let (tpl, kron) = d.prog();
    let r = &mut d.r;
    let keeper_tip = [0, 1, ktip(tpl), r.gen_range(0..=KAS as i64)][r.gen_range(0..4)];
    let min_rest = [0, 1, 50, r.gen_range(0..=100_000)][r.gen_range(0..4)];
    // the evidence ask / bid of the fixtures rests since DAA 1000 (UTXO and custody)
    let exposed = 1_000i64;
    let lock = (exposed + min_rest) as u64;
    let c = cov(0x47);
    let kind = r.gen_range(0..4);
    let (state, value, ev): (AnyState, u64, Leg) = match kind {
        0 => (
            AnyState::KobCondAsk(CondAskState { keeper_tip, min_rest_daa: min_rest, ..cond_ask(MAKER_A, tpl) }),
            CARRIER,
            ev_ask(198_000_000, 1, tpl),
        ),
        1 => {
            let s = CondBidState { keeper_tip, min_rest_daa: min_rest, ..cond_bid(MAKER_B, tpl) };
            let v = s.escrow(2).unwrap() as u64;
            (AnyState::KobCondBid(s), v, ev_bid(302_000_000, 1, tpl))
        }
        2 => {
            let s = IfdBidState {
                entry_stop: 255_000_000,
                min_fill: 3 * WHOLE,
                keeper_tip,
                min_rest_daa: min_rest,
                ..ifd_bid(MAKER_A, 10, tpl)
            };
            let v = s.escrow().unwrap() as u64;
            (AnyState::KobIfdBid(s), v, ev_bid(256_000_000, 1, tpl))
        }
        _ => {
            let s = IfdAskState {
                entry_stop: 255_000_000,
                min_fill: 3 * WHOLE,
                keeper_tip,
                min_rest_daa: min_rest,
                ..ifd_ask(MAKER_A, tpl)
            };
            let v = s.escrow(CARRIER as i64).unwrap() as u64;
            (AnyState::KobIfdAsk(s), v, ev_ask(254_000_000, 1, tpl))
        }
    };
    let ctx = format!("{} minRestDaa {min_rest} keeperTip {keeper_tip}", state.template_id().name());
    let Some(m) = d.build(R, Action::Batch(update_batch(lock, order(70, value, c, 500, state), ev)), kron, &ctx) else { return };
    let j = cont_of(&m, c).expect("continuation");
    let i = input_of(&m, c).unwrap();
    d.floor(R, &m, j, m.ins[i].entry.amount as i64 - keeper_tip, &format!("{ctx}: keeper tip"));
    if lock > 0 {
        let mut x = m.clone();
        x.lock_time = lock - 1;
        d.expect(R, &x, false, format!("{ctx}: lock exposed + minRestDaa - 1"));
    }
}

// ------------------------------------------------------------------------------------------------ refunds

fn rule_refund(d: &mut D) {
    const R: &str = "refund.tip+due";
    let (tpl, kron) = d.prog();
    let r = &mut d.r;
    let refund_tip = [0, 1, rtip(tpl), r.gen_range(0..=CARRIER as i64 - 1)][r.gen_range(0..4)];
    let daa = r.gen_range(1_000..=2_000_000) as i64;
    let expiry = match r.gen_range(0..4) {
        0 => daa + r.gen_range(0..=1_000),
        1 => NO_EXPIRY,
        2 => daa + MAX_IDLE + r.gen_range(-5..=5),
        _ => r.gen_range(0..=daa + 2 * MAX_IDLE),
    };
    let tif = [0, 0, TIF_IOC, TIF_FOK][r.gen_range(0..4)];
    let active_from = if r.gen_bool(0.3) { daa + r.gen_range(-100..=1_000) } else { 0 };
    // an amount of base units that is not a multiple of the scale (the custody holds exactly amountLeft)
    let amount = r.gen_range(1..=10 * WHOLE);
    let c = cov(0x80);
    let kind = r.gen_range(0..7);
    let (state, value, holds): (AnyState, u64, bool) = match kind {
        0 => (
            AnyState::KobAsk(AskState {
                refund_tip,
                expiry_daa: expiry,
                tif,
                active_from,
                amount_left: amount,
                ..ask(MAKER_A, P250, tpl)
            }),
            CARRIER,
            true,
        ),
        1 => {
            let b = BidState { refund_tip, expiry_daa: expiry, tif, active_from, ..bid(MAKER_B, P245, tpl) };
            (AnyState::KobBid(b), 25 * KAS, false)
        }
        2 => (
            AnyState::KobCondAsk(CondAskState { refund_tip, expiry_daa: expiry, amount_left: amount, ..cond_ask(MAKER_A, tpl) }),
            CARRIER,
            true,
        ),
        3 => {
            let s = CondBidState { refund_tip, expiry_daa: expiry, ..cond_bid(MAKER_B, tpl) };
            let v = s.escrow(2).unwrap() as u64;
            (AnyState::KobCondBid(s), v, false)
        }
        4 => {
            let s = IfdBidState { refund_tip, expiry_daa: expiry, ..ifd_bid(MAKER_A, 10, tpl) };
            let v = s.escrow().unwrap() as u64;
            (AnyState::KobIfdBid(s), v, false)
        }
        5 => {
            let s = IfdAskState { refund_tip, expiry_daa: expiry, amount_left: amount, ..ifd_ask(MAKER_A, tpl) };
            let v = s.escrow(CARRIER as i64).unwrap() as u64;
            (AnyState::KobIfdAsk(s), v, true)
        }
        _ => {
            // a pair ask (A on this program, B a KCC-20 token): the custody of A holds exactly amountLeft
            let x = PairState {
                refund_tip,
                expiry_daa: expiry,
                tif,
                active_from,
                amount_left: amount,
                custody: amount,
                ..pair::pair(MAKER_A, true, tpl, TemplateId::Kcc20Ref8x8, pair::RATE)
            };
            (AnyState::KobPair(x), pair::PV, true)
        }
    };
    let state = if kron { state.into_family(Family::Kron) } else { state };
    let due = state.refund_due(daa).expect("refund due");
    if !(0..=400_000_000_000).contains(&due) {
        d.st(R).skipped += 1;
        return;
    }
    let custody = holds.then(|| TokenUtxo {
        utxo: utxo(81, CARRIER, daa as u64, Some(TOKEN_COV)),
        state: TokenState::custody(tpl.family(), state.custody_amount().unwrap_or(10 * WHOLE), c, ext_for(tpl)),
    });
    let ctx = format!(
        "{} refundTip {refund_tip} expiry {expiry} tif {tif} activeFrom {active_from} utxoDaa {daa} due {due}",
        state.template_id().name()
    );
    let req = RefundOrder {
        order: order(80, value, c, daa as u64, state),
        foreign: vec![],
        custody,
        prefund: None,
        lock_time: due as u64,
        funding: vec![],
        change: Some(pk(KEEPER)),
        fee: fee(),
    };
    let Some(m) = d.build(R, Action::RefundOrder(req), false, &ctx) else { return };
    let i = input_of(&m, c).unwrap();
    let carriers: i64 = m
        .ins
        .iter()
        .filter(|x| {
            x.entry.covenant_id.map(|h| h.as_bytes()) == Some(c)
                || (holds && x.entry.covenant_id.map(|h| h.as_bytes()) == Some(TOKEN_COV))
        })
        .map(|x| x.entry.amount as i64)
        .sum();
    d.floor(R, &m, i, carriers - refund_tip, &ctx);
    if due > 0 {
        let mut x = m.clone();
        x.lock_time = (due - 1) as u64;
        d.expect(R, &x, false, format!("{ctx}: lock due - 1"));
    }
}

// ------------------------------------------------------------------------------------------------ if-done entries

fn rule_ifd_bid(d: &mut D) {
    const R: &str = "ifdBid.spend";
    let (tpl, kron) = d.prog();
    let r = &mut d.r;
    let amount = r.gen_range(1..=20 * WHOLE);
    let price = if r.gen_bool(0.3) { r.gen_range(1..=1_000_000_000_000) } else { r.gen_range(100_000_000..=1_000_000_000_000) };
    let tip = if r.gen_bool(0.5) { r.gen_range(0..=price) } else { 0 };
    let dc = [1, SMALL, KAS as i64, DC][r.gen_range(0..4)];
    let ec = [1, SMALL, KAS as i64, EC][r.gen_range(0..4)];
    let min_fill = min_fill_of(r, amount, 20);
    let rpt = if r.gen_bool(0.3) { 1 + r.gen_range(0..=3 * amount) } else { 0 };
    let s = IfdBidState {
        amount_left: amount,
        price,
        tip,
        delivery_carrier: dc,
        exit_carrier: ec,
        min_fill,
        rpt_amount: rpt,
        ..ifd_bid(MAKER_A, 1, tpl)
    };
    let n = fill_of(r, amount, min_fill);
    let Some(escrow) = s.escrow() else {
        d.st(R).skipped += 1;
        return;
    };
    let value = escrow + r.gen_range(0..=KAS as i64);
    let c = cov(0xd1);
    let mut b =
        batch(NOW, vec![Leg::IfdBid { order: order(50, value as u64, c, 1_000, s.clone()), amount: n, evidence: None, t: None }]);
    b.taker_tokens = vec![tok(51, n, pk(TAKER), SCHEME_P2PK, 1_000)];
    b.change = Some(pk(TAKER));
    let ctx = format!("price {price} tip {tip} amount {amount} minFill {min_fill} n {n} dc {dc} ec {ec} rpt {rpt} value {value}");
    let Some(p) = s.price_at(false, NOW as i64, 1_000) else {
        d.st(R).skipped += 1;
        return;
    };
    let Some(spend) = quote(n, p + tip, SCALE, false) else {
        d.st(R).skipped += 1;
        return;
    };
    if s.spend(n, p) != Some(spend) {
        d.fails.push(format!("{R}: IfdBidState::spend disagrees with floor(n * (p + tip) / scale) = {spend} ({ctx})"));
    }
    let Some(m) = d.build(R, Action::Batch(b), kron, &ctx) else { return };
    off_scale(d, R, n, SCALE);
    let i = input_of(&m, c).unwrap();
    let left = value - spend - dc;
    // the delivery (tokens to the exit) carries at least deliveryCarrier
    d.floor(R, &m, i, dc, &format!("{ctx}: delivery carrier"));
    if amount - n > 0 || rpt > 0 {
        let k = cont_of(&m, c).expect("continuation");
        d.floor(R, &m, k, left - ec, &format!("{ctx}: entry continuation"));
    }
}

fn rule_ifd_ask(d: &mut D) {
    const R: &str = "ifdAsk.proceeds+prefund";
    let (tpl, kron) = d.prog();
    let r = &mut d.r;
    let amount = r.gen_range(1..=20 * WHOLE);
    let price = r.gen_range(2..=1_000_000_000_000);
    let tip = if r.gen_bool(0.5) { r.gen_range(0..price) } else { 0 };
    let ec = [1, SMALL, KAS as i64, EC][r.gen_range(0..4)];
    let prefund = [0, 1, KAS as i64 / 2, r.gen_range(0..=10 * KAS as i64)][r.gen_range(0..4)];
    let min_fill = min_fill_of(r, amount, 20);
    let s = IfdAskState { amount_left: amount, price, tip, exit_carrier: ec, prefund, min_fill, ..ifd_ask(MAKER_A, tpl) };
    let n = fill_of(r, amount, min_fill);
    let Some(value) = s.escrow(CARRIER as i64) else {
        d.st(R).skipped += 1;
        return;
    };
    let c = cov(0xf1);
    let Some(p) = s.price_at(false, NOW as i64, 1_000) else {
        d.st(R).skipped += 1;
        return;
    };
    let (Some(proceeds), Some(pre)) = (quote(n, p - tip, SCALE, true), quote(n, prefund, SCALE, true)) else {
        d.st(R).skipped += 1;
        return;
    };
    if s.proceeds(n, p) != Some(proceeds) || s.prefund_of(n) != Some(pre) {
        d.fails.push(format!("{R}: IfdAskState::proceeds / prefund_of disagree with the covenant rounding"));
    }
    let mut b = batch(
        NOW,
        vec![Leg::IfdAsk {
            order: order(60, value as u64, c, 1_000, s.clone()),
            custody: tok_custody(61, amount, c, 1_000),
            amount: n,
            evidence: None,
            t: None,
        }],
    );
    b.funding = funding(proceeds);
    let ctx = format!("price {price} tip {tip} amount {amount} minFill {min_fill} n {n} ec {ec} prefund {prefund}");
    let Some(m) = d.build(R, Action::Batch(b), kron, &ctx) else { return };
    off_scale(d, R, n, SCALE);
    let i = input_of(&m, c).unwrap();
    if n < amount {
        let k = cont_of(&m, c).expect("continuation");
        d.floor(R, &m, k, m.ins[i].entry.amount as i64 - pre - ec, &format!("{ctx}: entry continuation"));
    }
    // the exit's value (its covenant id commits to it: recompute the binding of the probe)
    let carriers = (m.ins[i].entry.amount
        + m.ins
            .iter()
            .find(|x| {
                x.entry.covenant_id.map(|h| h.as_bytes()) == Some(TOKEN_COV)
                    && matches!(x.plan, SigPlan::TokenLeader { .. } | SigPlan::TokenDelegator { .. } | SigPlan::KronToken { .. })
            })
            .map(|x| x.entry.amount)
            .unwrap_or(0)) as i64;
    let need = if n < amount { proceeds + pre + ec } else { proceeds + carriers };
    let Some(x_out) =
        m.outs.iter().position(|o| o.covenant.is_some_and(|b| b.authorizing_input as usize == i && b.covenant_id.as_bytes() != c))
    else {
        d.fails.push(format!("{R}: no exit output ({ctx})"));
        return;
    };
    let v = m.outs[x_out].value as i64;
    if v < need {
        d.fails.push(format!("{R}: the builder's exit holds {v} < Rust {need} ({ctx})"));
        return;
    }
    for (val, ok) in [(need, true), (need - 1, false)] {
        let mut x = m.clone();
        x.outs[x_out].value = val as u64;
        let id = kaspa_consensus_core::hashing::covenant_id::covenant_id(x.ins[i].op, std::iter::once((x_out as u32, &x.outs[x_out])));
        x.outs[x_out].covenant = Some(CovenantBinding { authorizing_input: i as u16, covenant_id: id });
        d.expect(R, &x, ok, format!("{ctx}: exit value {val} (Rust {need}, builder {v})"));
    }
}

// ------------------------------------------------------------------------------------------------ pair orders

/// A plain pair order (`KobPair`) on a random program pair: an ASK's B delivery is at least `ceil(n * p(t) / scale(A))`
/// (probed in the engine: exactly the Rust value accepted, one base unit less refused), a BID releases exactly
/// `floor(n * p(t) / scale(A))` of B (the builder's sOut equals the Rust value), the KAS tip is `floor(n * tip / scale(A))`;
/// decay (ask down, bid up) at a random t, IOC, tips. The counterparty side is settled exactly through the KAS books.
fn rule_pair(d: &mut D) {
    const R: &str = "pair.quote";
    let pairs = [
        (TemplateId::Kcc20Ref, TemplateId::Kcc20Ref8x8),
        (TemplateId::Kcc20Ref8x8, TemplateId::KronToken2433),
        (TemplateId::KronToken2433, TemplateId::Kcc20Ref),
        (TemplateId::KronToken2732, TemplateId::KronToken2433),
    ];
    let (pa, pb) = pairs[d.r.gen_range(0..pairs.len())];
    let r = &mut d.r;
    let ask = r.gen_bool(0.5);
    let amount = r.gen_range(1..=10 * WHOLE);
    let min_fill = if r.gen_bool(0.5) { 1 } else { r.gen_range(1..=amount) };
    let n = fill_of(r, amount, min_fill);
    let mut x = PairState { amount_left: amount, min_fill, ..pair::pair(MAKER_A, ask, pa, pb, pair::RATE) };
    x.price = r.gen_range(1..=3 * pair::RATE);
    let lock = NOW as i64;
    if r.gen_bool(0.4) {
        x.slope = r.gen_range(1..=(x.price / 20).max(1));
        x.price_end = if ask { r.gen_range(1..=x.price) } else { r.gen_range(x.price..=3 * pair::RATE) };
        x.decay_step = if r.gen_bool(0.3) { 1 } else { r.gen_range(1..=500) };
        x.active_from = lock - r.gen_range(0..=2_000);
    }
    if r.gen_bool(0.3) {
        x.tif = TIF_IOC;
        x.expiry_daa = lock + 200;
    }
    if r.gen_bool(0.4) {
        x.tip = r.gen_range(0..=100_000);
    }
    // an ask holds its whole amount; a bid an escrow for its amount at its highest quote plus one unit per fill
    x.custody = if ask { amount } else { x.bid_escrow(amount, x.max_fills()).unwrap_or(0) };
    let daa = 1_000i64;
    let Some(origin) = x.origin(daa).filter(|o| *o <= lock) else {
        d.st(R).skipped += 1;
        return;
    };
    let t = (x.slope > 0).then(|| r.gen_range(origin..=lock));
    let Some(p) = x.price_at(t.unwrap_or(lock), daa) else {
        d.st(R).skipped += 1;
        return;
    };
    let sc = x.a_scale();
    let (Some(quote_b), Some(tip_kas)) = (quote(n, p, sc, ask), quote(n, x.tip, sc, false)) else {
        d.st(R).skipped += 1;
        return;
    };
    let (s_out, t_out) = if ask { (n, quote_b) } else { (quote_b, n) };
    if x.s_out(n, p) != Some(s_out) || x.t_out_min(n, p) != Some(t_out) || x.tip_kas(n) != Some(tip_kas) {
        d.fails.push(format!("{R}: PairState::s_out / t_out_min / tip_kas disagree with the covenant rounding ({x:?} n {n} p {p})"));
    }
    if quote_b <= 0 {
        d.st(R).skipped += 1;
        d.note(format!("{R}: a fill worth no B base unit, not probed"));
        return;
    }
    let value = x.kas_value(x.max_fills()).unwrap_or(0).max(0) as u64 + CARRIER;
    // the counterparty side through the KAS books, exactly: an ASK's A to a bid of A and its B from an ask of B; a BID's
    // B to a bid of B and its A from an ask of A (each KAS-book maker paid above dust)
    let px = |units: i64| P250.max(20_000_000_000 / units.max(1) + 1);
    let (a_leg, b_leg) = if ask {
        (
            pair::kbid_units(TOKEN_COV, pa, MAKER_B, px(n), cov(0x91), 72, n),
            pair::kask_units(TOKEN_B, pb, MAKER_C, px(quote_b), cov(0x92), 74, quote_b),
        )
    } else {
        (
            pair::kask_units(TOKEN_COV, pa, MAKER_C, px(n), cov(0x92), 74, n),
            pair::kbid_units(TOKEN_B, pb, MAKER_B, px(quote_b), cov(0x91), 72, quote_b),
        )
    };
    let (stok, sprog) = pair::s_of(ask, pa, pb);
    let leg = Leg::Pair {
        order: order(70, value, pair::X_ID, daa as u64, x.clone()),
        custody: pair::tutxo(sprog, stok, 71, x.custody, pair::X_ID, true),
        amount: n,
        t,
    };
    let bt = pair::route(vec![leg, a_leg, b_leg]);
    let ctx = format!(
        "{} / {}: {} price {} priceEnd {} slope {} step {} activeFrom {} tip {} tif {} amount {amount} minFill {min_fill} n {n} t {t:?}",
        pa.name(),
        pb.name(),
        if ask { "ASK" } else { "BID" },
        x.price,
        x.price_end,
        x.slope,
        x.decay_step,
        x.active_from,
        x.tip,
        x.tif
    );
    let Some(m) = d.build(R, Action::Batch(bt), false, &ctx) else { return };
    off_scale(d, R, n, sc);
    let i = input_of(&m, pair::X_ID).unwrap();
    // settle(nb, custIn, tTplIn, t, sOut, tOut)
    let SigPlan::Entry { args, .. } = &m.ins[i].plan else { unreachable!() };
    let (Some(Arg::Int(b_s)), Some(Arg::Int(b_t))) = (args.get(4).cloned(), args.get(5).cloned()) else {
        d.fails.push(format!("{R}: no sOut / tOut argument ({ctx})"));
        return;
    };
    if b_s != s_out || b_t < t_out || (!ask && b_t != t_out) {
        d.fails.push(format!("{R}: the builder's sOut {b_s} / tOut {b_t} != Rust {s_out} / {t_out} ({ctx})"));
        return;
    }
    if ask {
        // the maker's B delivery at output i: exactly the ceil accepted, one base unit less refused
        for (amount, ok) in [(t_out, true), (t_out - 1, false)] {
            match deliver_b(&m, i, amount, pb, 5) {
                Some(y) => d.expect(R, &y, ok, format!("{ctx}: B delivery {amount} (Rust {t_out}, builder {b_t})")),
                None => d.fails.push(format!("{R}: could not re-layout the B delivery ({ctx})")),
            }
        }
    } else {
        d.expect(R, &m, true, format!("{ctx}: the builder's bid fill"));
    }
}

/// The order at input `i` delivers exactly `amount` of B (its argument `arg`); the rest of the builder's delivery goes to a
/// new token output of the matcher (B leader's next states / every KRON B input's next states follow).
fn deliver_b(m: &MTx, i: usize, amount: i64, pb: TemplateId, arg: usize) -> Option<MTx> {
    let mut x = m.clone();
    let tt = token_template(pb);
    let b_cov = x.outs[i].covenant?.covenant_id;
    let (o, st) = match kob_protocol::artifacts::spk_trace::lookup(&x.outs[i].script_public_key)? {
        (kob_protocol::artifacts::spk_trace::Origin::Template(t), st) if t == pb => (t, st),
        _ => return None,
    };
    let _ = o;
    let cur = TokenState::decode_with(tt, &st).ok()?;
    let surplus = cur.amount() - amount;
    if surplus < 0 {
        return None;
    }
    let pos =
        x.outs.iter().enumerate().filter(|(_, q)| q.covenant.is_some_and(|b| b.covenant_id == b_cov)).position(|(k, _)| k == i)?;
    let new_state = cur.with_amount(amount);
    x.outs[i].script_public_key = new_state.spk_with(tt);
    let extra = (surplus > 0).then(|| TokenState::user(pb.family(), surplus, pk(MATCHER), ext_for(pb)));
    let auth = x.outs[i].covenant?.authorizing_input;
    if let Some(e) = &extra {
        x.outs.push(TransactionOutput {
            value: CARRIER,
            script_public_key: e.spk_with(tt),
            covenant: Some(CovenantBinding { authorizing_input: auth, covenant_id: b_cov }),
        });
    }
    for inp in &mut x.ins {
        if inp.entry.covenant_id != Some(b_cov) {
            continue;
        }
        match (&mut inp.plan, &new_state) {
            (SigPlan::TokenLeader { next_states, .. }, TokenState::Kcc20(s)) => {
                next_states[pos] = s.clone();
                if let Some(TokenState::Kcc20(e)) = &extra {
                    next_states.push(e.clone());
                }
            }
            (SigPlan::KronToken { next_states, .. }, TokenState::Kron(s)) => {
                next_states[pos] = s.clone();
                if let Some(TokenState::Kron(e)) = &extra {
                    next_states.push(e.clone());
                }
            }
            _ => {}
        }
    }
    if let SigPlan::Entry { args, .. } = &mut x.ins[i].plan {
        args[arg] = Arg::Int(amount);
    }
    // KRON presence for the matcher's new key-held output needs no input; KCC-20 needs nothing either
    let _ = p2pk_of;
    Some(x)
}

// ------------------------------------------------------------------------------------------------ repeat until

fn rule_rpt_until(d: &mut D) {
    const R: &str = "rpt.until";
    let (tpl, kron) = d.prog();
    let r = &mut d.r;
    let until = r.gen_range(10_000..=4_000_000);
    let amount = r.gen_range(1..=6 * WHOLE);
    let exit = CondAskState {
        amount_left: amount,
        min_fill: 1,
        rpt_until: until,
        parent: cov(0xd1),
        rpt_price: P260 + TIP,
        ..ifd_exit(MAKER_A, tpl)
    };
    let n = r.gen_range(1..=amount);
    let c = cov(0xc1);
    let mut b = batch(
        until as u64,
        vec![Leg::CondAsk {
            order: order(30, CARRIER, c, 2_000, exit.clone()),
            custody: tok_custody(31, amount, c, 2_000),
            amount: n,
            leg: 0,
            evidence: None,
            t: None,
            merge: None,
        }],
    );
    b.funding = vec![key_utxo(33, TAKER, 1_000 * KAS)];
    let ctx = format!("rptUntil {until} amount {amount} n {n}");
    let Some(m) = d.build(R, Action::Batch(b), kron, &ctx) else { return };
    let mut x = m.clone();
    x.lock_time = (until - 1) as u64;
    d.expect(R, &x, false, format!("{ctx}: take-profit without its entry at rptUntil - 1"));
}

// ------------------------------------------------------------------------------------------------ driver

#[test]
fn c6_differential_rust_vs_covenant() {
    let cases = env_u64("KOB_C6_DIFF_CASES", 25);
    let mut d = D::new();
    type Rule = (&'static str, fn(&mut D));
    let rules: [Rule; 11] = [
        ("ask.proceeds", rule_ask),
        ("bid.spend+budget", rule_bid),
        ("condAsk.leg", rule_cond_ask),
        ("condBid.leg", rule_cond_bid),
        ("trail.steps+keeperTip", rule_trail),
        ("arm.restTime+keeperTip", rule_arm),
        ("refund.tip+due", rule_refund),
        ("ifdBid.spend", rule_ifd_bid),
        ("ifdAsk.proceeds+prefund", rule_ifd_ask),
        ("pair.quote", rule_pair),
        ("rpt.until", rule_rpt_until),
    ];
    for (_, f) in rules.iter() {
        for _ in 0..cases {
            f(&mut d);
        }
    }
    println!("rule: built cases / engine probes / skipped / cases with a fill not a multiple of the scale");
    for (k, s) in &d.stats {
        println!("  {k:<26} {:>6} {:>6} {:>6} {:>6}", s.cases, s.probes, s.skipped, s.off_scale);
    }
    let total = |f: fn(&Stat) -> u64| d.stats.values().map(f).sum::<u64>();
    println!(
        "TOTAL cases {} probes {} skipped {} off-scale {}",
        total(|s| s.cases),
        total(|s| s.probes),
        total(|s| s.skipped),
        total(|s| s.off_scale)
    );
    println!("notes:");
    for (k, n) in &d.notes {
        println!("  {n:>6}  {k}");
    }
    for f in d.fails.iter().take(40) {
        println!("FAIL {f}");
    }
    assert!(d.fails.is_empty(), "{} Rust <-> covenant disagreements", d.fails.len());
    assert!(d.stats.values().map(|s| s.probes).sum::<u64>() > 0);
    // the rounding paths are probed: fills that are not a multiple of the scale, on every amount-based rule
    for rule in
        ["ask.proceeds", "bid.spend+budget", "condAsk.leg", "condBid.leg", "ifdBid.spend", "ifdAsk.proceeds+prefund", "pair.quote"]
    {
        let s = d.stats.get(rule).map(|s| s.off_scale).unwrap_or(0);
        assert!(cases < 10 || s > 0, "{rule}: no case with a fill that is not a multiple of the scale");
    }
}

// ------------------------------------------------------------------------------------------------ budget coverage

/// The matcher's fallback for a role the committed table has not measured (`kob_executor::matcher::lower::budgets`):
/// the largest budget of the same template entry and token program, plus one unit.
fn matcher_fallback(role: &str) -> kob_protocol::Result<u16> {
    if let Ok(b) = kob_protocol::budget::lookup(role) {
        return Ok(b);
    }
    let table = kob_protocol::budget::table();
    let (lhs, program) = match role.split_once('@') {
        Some((l, p)) => (l, Some(p)),
        None => (role, None),
    };
    let parts: Vec<&str> = lhs.split('.').collect();
    let prefix = parts.iter().take(if program.is_some() { 2 } else { 3 }).copied().collect::<Vec<_>>().join(".");
    table
        .iter()
        .filter(|(k, _)| {
            let (kl, kp) = match k.split_once('@') {
                Some((l, p)) => (l, Some(p)),
                None => (k.as_str(), None),
            };
            kp == program && (kl == prefix || kl.starts_with(&format!("{prefix}.")))
        })
        .map(|(_, v)| *v)
        .max()
        .map(|v| v.saturating_add(1))
        .ok_or_else(|| kob_protocol::Error::MissingBudget(role.to_string()))
}

/// C6-2: every role the builders produce for a valid fill is in the committed compute-budget table (before the fix 152 roles
/// were missing and the library's own `build` refused them with `MissingBudget`; the matcher survived on its fallback,
/// which is also checked to be sufficient for whatever the table lacks).
#[test]
fn c6_budget_table_covers_every_fill_branch() {
    let keys = keys();
    let (mut covered, mut missing, mut refused) = (0, BTreeMap::<String, String>::new(), BTreeMap::<String, u64>::new());
    let mut fallback_short = vec![];
    let mut invalid = vec![];
    for p in common::PROGRAMS {
        for (name, a) in common::branches::branch_shapes(p) {
            // the shape itself must be a valid transaction (scripts pass on an unlimited meter), whatever its budgets
            if let Ok(built) = build_with(&a, &matcher_fallback) {
                let (tx, en) = materialize(&MTx::from_built(&built), &keys).expect("assemble");
                if let Err(e) = kob_protocol::verify::measure_units(&tx, &en) {
                    invalid.push(format!("{name}@{}: {e}", p.name()));
                    continue;
                }
            }
            match build(&a) {
                Ok(_) => covered += 1,
                Err(kob_protocol::Error::MissingBudget(role)) => {
                    let verdict = match build_with(&a, &matcher_fallback) {
                        Ok(built) => {
                            let m = MTx::from_built(&built);
                            let (tx, en) = materialize(&m, &keys).expect("assemble");
                            match kob_protocol::verify::execute(&tx, &en, true) {
                                Ok(r) if r.iter().all(|x| x.is_ok()) => "fallback ok".to_string(),
                                Ok(r) => {
                                    let units = kob_protocol::verify::measure_units(&tx, &en).unwrap_or_default();
                                    let bad: Vec<String> = r
                                        .iter()
                                        .enumerate()
                                        .filter(|(_, x)| x.is_err())
                                        .map(|(i, _)| {
                                            let need = kob_protocol::budget::budget_for_units(units.get(i).copied().unwrap_or(0));
                                            format!("{} needs {need} > {}", built.roles[i], m.ins[i].budget)
                                        })
                                        .collect();
                                    fallback_short.push(format!("{name}@{}: {}", p.name(), bad.join(", ")));
                                    "FALLBACK SHORT".to_string()
                                }
                                Err(e) => format!("engine error {e}"),
                            }
                        }
                        Err(e) => format!("fallback build: {e}"),
                    };
                    missing.insert(role, format!("{name} -> {verdict}"));
                }
                Err(e) => *refused.entry(format!("{name}: {}", e.to_string().split(':').next().unwrap_or(""))).or_default() += 1,
            }
        }
    }
    println!("{covered} shapes build with the committed table; {} roles missing:", missing.len());
    for (r, v) in &missing {
        println!("  MISSING {r}  ({v})");
    }
    for (r, n) in &refused {
        println!("  refused x{n}: {r}");
    }
    for f in &fallback_short {
        println!("  SHORT {f}");
    }
    for f in &invalid {
        println!("  INVALID SHAPE {f}");
    }
    assert!(invalid.is_empty(), "{} fixture shapes are not valid transactions", invalid.len());
    assert!(fallback_short.is_empty(), "the matcher's fallback budget is short for {} shapes", fallback_short.len());
    // C6-2: every role the builders produce for a valid fill is measured (the library's `build` refused 152 of them)
    assert!(missing.is_empty(), "{} builder roles are missing from the compute-budget table", missing.len());
}

// ------------------------------------------------------------------------------------------------ wallet vectors (web TS <-> Rust)

/// Largest `bps` in `0..=10_000` whose stop band does not pass `limit` (sell: `stop - band >= limit`, buy:
/// `stop + band <= limit`), by the covenants' own band (`kob_protocol::state::band`); 0 when none (or no stop).
fn rust_slip_for_limit(sell: bool, stop: i64, limit: i64) -> i64 {
    if stop <= 0 {
        return 0;
    }
    let ok = |bps: i64| {
        let b = band(stop, bps);
        if sell {
            stop - b >= limit
        } else {
            stop.checked_add(b).is_some_and(|w| w <= limit)
        }
    };
    if !ok(0) || (sell && stop <= limit) || (!sell && limit <= stop) {
        return 0;
    }
    let (mut lo, mut hi) = (0i64, 10_000i64);
    while lo < hi {
        let mid = (lo + hi + 1) / 2;
        if ok(mid) {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    lo
}

/// The wallet arithmetic the web app mirrors from `kob_protocol::state` (`web/src/kob/orders/common.ts`, `cond-legs.ts`),
/// as random vectors (boundaries and near-overflow included) with the Rust results, for the vitest suite
/// `web/src/kob/orders/c6-economics.test.ts`. Amounts in base units, prices and tips in sompi per whole token (`scale`
/// base units); `null` where the covenant arithmetic fails (the result does not fit an i64). Protocol v3 keys: `quoteOf`,
/// `bidUsed`, `bidEscrow`, `bidBuyingPower` (they replace the retired v2.6 budget keys).
fn wallet_vectors() -> serde_json::Value {
    use serde_json::json;
    let mut r = StdRng::seed_from_u64(0xc6_7e57);
    let s = |v: i64| v.to_string();
    let mut quotes = vec![];
    let mut used = vec![];
    let mut escrow = vec![];
    let mut power = vec![];
    let scales = [1i64, 10, 1_000, 100_000, 100_000_000, 1_000_000_000];
    for k in 0..300 {
        // quoteOf: the exact rounding in both directions, near-overflow included (null where the covenant fails)
        let scale = scales[r.gen_range(0..scales.len())];
        let n = match k % 3 {
            0 => r.gen_range(0..=10 * scale),
            1 => r.gen_range(0..=i64::MAX / 2),
            _ => r.gen_range(0..scale.max(2)).min(scale),
        };
        let rate = match k % 4 {
            0 => r.gen_range(0..=1_000_000_000),
            1 => r.gen_range(0..=i64::MAX / 4),
            2 => scale * r.gen_range(0..=1_000) + r.gen_range(0..scale),
            _ => r.gen_range(0..=1_000),
        };
        for up in [false, true] {
            let v = quote_of(n, rate, scale, if up { Round::Up } else { Round::Down });
            quotes.push(
                json!({ "n": s(n), "rate": s(rate), "scale": s(scale), "round": if up { "up" } else { "down" }, "value": v.map(s) }),
            );
        }
        // a bid's budget, escrow and buying power (rising bids: the cap pMax)
        let scale = scales[r.gen_range(0..4)];
        let cap = 4_000_000_000_000_000i64 / 1_000;
        let price = match k % 4 {
            0 => r.gen_range(1..=1_000),
            1 => r.gen_range(1..=1_000_000_000),
            2 => r.gen_range(1..=cap),
            _ => cap - r.gen_range(0..100),
        };
        let slope = [0, 0, 1, r.gen_range(1..=1_000_000)][r.gen_range(0..4)];
        let price_end = [0, price, price - 1, price + 1, r.gen_range(1..=cap)][r.gen_range(0..5)].max(0);
        let tip = [0, 1, r.gen_range(0..=price.min(1_000_000_000))][r.gen_range(0..3)];
        let b = BidState { scale, price, tip, slope, price_end, ..bid(MAKER_B, P245, TemplateId::Kcc20Ref) };
        let rate = b.price_max() + tip;
        let amount = r.gen_range(1..=((1i128 << 62) * scale as i128 / rate.max(1) as i128).clamp(1, 1_000_000_000) as i64);
        used.push(json!({
            "price": s(price), "tip": s(tip), "slope": s(slope), "priceEnd": s(price_end), "scale": s(scale),
            "amount": s(amount), "used": b.used(amount).map(s),
        }));
        let fills = r.gen_range(1..=50);
        let dc = [0, 1, KAS as i64, DC][r.gen_range(0..4)];
        let reserve = [0, r.gen_range(0..=10 * KAS as i64)][r.gen_range(0..2)];
        let b = BidState { delivery_carrier: dc, reserve, ..b };
        escrow.push(json!({
            "price": s(price), "tip": s(tip), "slope": s(slope), "priceEnd": s(price_end), "scale": s(scale),
            "amount": s(amount), "fills": s(fills), "deliveryCarrier": s(dc), "reserve": s(reserve),
            "escrow": b.escrow(amount, fills).map(s),
        }));
        let value = match b.escrow(amount, fills) {
            Some(v) if r.gen_bool(0.5) => v - r.gen_range(0..=v.min(1_000_000)),
            _ => r.gen_range(0..=1_000_000_000_000),
        };
        power.push(json!({
            "price": s(price), "tip": s(tip), "slope": s(slope), "priceEnd": s(price_end), "scale": s(scale),
            "deliveryCarrier": s(dc), "reserve": s(reserve), "value": s(value), "buyingPower": s(b.buying_power(value)),
        }));
    }
    let mut worst = vec![];
    let mut slip = vec![];
    let mut legs = vec![];
    for k in 0..300 {
        let stop = match k % 5 {
            0 => r.gen_range(1..=10_000),
            1 => r.gen_range(1..=1_000_000_000),
            2 => r.gen_range(1..=MAX_STOP_PRICE),
            3 => MAX_STOP_PRICE - r.gen_range(0..1_000),
            _ => [1, 2, 9_999, 10_000, 10_001][r.gen_range(0..5)],
        };
        let bps = [0, 1, 300, 9_999, 10_000, r.gen_range(0..=10_000)][r.gen_range(0..6)];
        let floor = CondAskState { stop_price: stop, slip_bps: bps, ..cond_ask(MAKER_A, TemplateId::Kcc20Ref) }.stop_floor();
        let ceiling = CondBidState { stop_price: stop, slip_bps: bps, ..cond_bid(MAKER_B, TemplateId::Kcc20Ref) }.stop_ceiling();
        worst.push(json!({ "side": "sell", "stop": s(stop), "slipBps": s(bps), "worst": s(floor) }));
        worst.push(json!({ "side": "buy", "stop": s(stop), "slipBps": s(bps), "worst": s(ceiling) }));
        let d = [0, 1, band(stop, bps), band(stop, bps) + 1, r.gen_range(0..=stop.min(1 << 40))][r.gen_range(0..5)];
        slip.push(
            json!({ "side": "sell", "stop": s(stop), "limit": s(stop - d), "slip": s(rust_slip_for_limit(true, stop, stop - d)) }),
        );
        let lim = stop.saturating_add(d);
        slip.push(json!({ "side": "buy", "stop": s(stop), "limit": s(lim), "slip": s(rust_slip_for_limit(false, stop, lim)) }));
        let tp = [0, 1, stop, stop / 2, stop.saturating_mul(2).min(MAX_STOP_PRICE)][r.gen_range(0..5)];
        let has_stop = r.gen_bool(0.8);
        if tp == 0 && !has_stop {
            continue;
        }
        let sp = if has_stop { stop } else { 0 };
        let ca = CondAskState { tp_price: tp, stop_price: sp, slip_bps: bps, ..cond_ask(MAKER_A, TemplateId::Kcc20Ref) };
        let cb = CondBidState { tp_price: tp, stop_price: sp, slip_bps: bps, ..cond_bid(MAKER_B, TemplateId::Kcc20Ref) };
        let mut ask_worst = i64::MAX;
        if tp > 0 {
            ask_worst = ask_worst.min(tp);
        }
        if sp > 0 {
            ask_worst = ask_worst.min(ca.stop_floor());
        }
        legs.push(json!({
            "tpPrice": s(tp), "stopPrice": s(sp),
            "askStopWorst": s(if sp > 0 { ca.stop_floor() } else { 0 }), "bidStopWorst": s(if sp > 0 { cb.stop_ceiling() } else { 0 }),
            "askWorst": s(ask_worst), "bidWorst": s(cb.worst()),
        }));
    }
    json!({
        "generatedBy": "crates/kob-tests/tests/c6_diff.rs wallet_vectors (KOB_REGEN=1 rewrites); Rust: kob_protocol::state",
        "quoteOf": quotes,
        "bidUsed": used,
        "bidEscrow": escrow,
        "bidBuyingPower": power,
        "stopWorstPrice": worst,
        "slipForLimit": slip,
        "legsWorst": legs,
    })
}

/// The vectors are well formed: every rule has more than 100 rows, the two roundings of one quote differ by at most one,
/// and `quote_of` equals the exact 128-bit value wherever the covenant's arithmetic holds.
#[test]
fn c6_wallet_vectors_are_consistent() {
    let v = wallet_vectors();
    for k in ["quoteOf", "bidUsed", "bidEscrow", "bidBuyingPower", "stopWorstPrice", "slipForLimit", "legsWorst"] {
        assert!(v[k].as_array().map(|a| a.len()).unwrap_or(0) > 100, "{k}");
    }
    let num = |x: &serde_json::Value| x.as_str().map(|t| t.parse::<i64>().unwrap());
    let rows = v["quoteOf"].as_array().unwrap();
    let mut off = 0;
    for pair in rows.chunks(2) {
        let (n, rate, scale) = (num(&pair[0]["n"]).unwrap(), num(&pair[0]["rate"]).unwrap(), num(&pair[0]["scale"]).unwrap());
        for row in pair {
            let up = row["round"] == "up";
            let exact = quote_exact(n, rate, scale, if up { Round::Up } else { Round::Down }).unwrap();
            let want = (exact <= i64::MAX as i128).then_some(exact as i64);
            // the covenant's split multiplication may fail before the result does (an intermediate beyond i64)
            assert!(num(&row["value"]) == want || num(&row["value"]).is_none(), "{row}");
        }
        if let (Some(d), Some(u)) = (num(&pair[0]["value"]), num(&pair[1]["value"])) {
            assert!(u >= d && u - d <= 1, "{pair:?}");
            off += i64::from(u != d);
        }
    }
    assert!(off > 50, "too few vectors exercise the rounding ({off})");
}

/// The committed vectors of the web suite are the Rust results (regenerate with `KOB_REGEN=1`).
#[test]
#[ignore = "web wallet vectors not regenerated for v3 yet: the web port must make c6-economics.test.ts read the v3 keys (quoteOf, bidUsed, bidEscrow, bidBuyingPower), then run KOB_REGEN=1 once"]
fn c6_wallet_vectors_are_current() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/src/kob/orders/c6-economics.vectors.json");
    let mut json = serde_json::to_string_pretty(&wallet_vectors()).unwrap();
    json.push('\n');
    if std::env::var("KOB_REGEN").is_ok_and(|v| v == "1") {
        std::fs::write(&path, &json).unwrap();
        println!("wrote {}", path.display());
    } else {
        let have = std::fs::read_to_string(&path).unwrap_or_default().replace("\r\n", "\n");
        assert!(
            have == json,
            "{} is stale: run `KOB_REGEN=1 cargo test -p kob-tests --test c6_diff c6_wallet_vectors`",
            path.display()
        );
    }
}

// ------------------------------------------------------------------------------------------------ C6-5 / C6-6

/// C6-5 / C6-6 (fixed 2026-10-02): `update` of an if-done stop entry requires an amount to fill (a repeating entry whose
/// amount all sits in its exits used to be armable for the keeper's tip, although the merge bringing it back resets `armed`) and the
/// stop on the limit's side (an entry whose stop is beyond its limit used to be armable although `fill` / `settle` can
/// never fill it). The builder refuses both arms; a transaction made anyway (the builder's own arm of a one-token entry,
/// re-pointed at the empty or inverted entry and its armed continuation) is refused by the covenant, while the same
/// re-pointing with the original state is accepted.
#[test]
fn c6_5_an_empty_or_inverted_stop_entry_cannot_be_armed() {
    let keys = keys();
    let c = cov(0x46);
    let armed = |s: &AnyState| match s {
        AnyState::KobIfdBid(x) | AnyState::KobIfdBidKron(x) => {
            AnyState::KobIfdBid(IfdBidState { armed: 1, ..x.clone() }).into_family(s.family())
        }
        AnyState::KobIfdAsk(x) | AnyState::KobIfdAskKron(x) => {
            AnyState::KobIfdAsk(IfdAskState { armed: 1, ..x.clone() }).into_family(s.family())
        }
        _ => unreachable!(),
    };
    for tpl in [TemplateId::Kcc20Ref, TemplateId::KronToken2433] {
        let fam = tpl.family();
        let ib = IfdBidState {
            entry_stop: P255,
            amount_left: WHOLE,
            rpt_amount: 1 + 8 * WHOLE,
            min_fill: WHOLE,
            ..ifd_bid(MAKER_A, 10, tpl)
        };
        let ia =
            IfdAskState { entry_stop: P255, amount_left: WHOLE, rpt_amount: 1 + 8 * WHOLE, min_fill: WHOLE, ..ifd_ask(MAKER_A, tpl) };
        let cases = [
            (
                AnyState::KobIfdBid(ib.clone()),
                ev_bid(256_000_000, 1, tpl),
                [
                    AnyState::KobIfdBid(IfdBidState { amount_left: 0, ..ib.clone() }),
                    AnyState::KobIfdBid(IfdBidState { price: P255 - 1, ..ib.clone() }),
                ],
            ),
            (
                AnyState::KobIfdAsk(ia.clone()),
                ev_ask(254_000_000, 1, tpl),
                [
                    AnyState::KobIfdAsk(IfdAskState { amount_left: 0, ..ia.clone() }),
                    AnyState::KobIfdAsk(IfdAskState { price: P255 + 1, ..ia.clone() }),
                ],
            ),
        ];
        for (good, ev, bad) in cases {
            let name = format!("{}@{}", good.template_id().name(), tpl.name());
            let a = |s: &AnyState| {
                let a = Action::Batch(update_batch(NOW, order(70, 5 * KAS, c, 1_000, s.clone()), ev.clone()));
                if fam == Family::Kron {
                    kron_action(a)
                } else {
                    a
                }
            };
            let built = build(&a(&good)).unwrap_or_else(|e| panic!("{name}: the honest arm: {e}"));
            let m = MTx::from_built(&built);
            let repoint = |s: &AnyState| {
                let s = s.clone().into_family(fam);
                let mut x = m.clone();
                let i = input_of(&x, c).expect("the entry input");
                let SigPlan::Entry { state, .. } = &mut x.ins[i].plan else { unreachable!() };
                *state = s.encode();
                x.ins[i].entry.script_public_key = s.spk();
                let o = cont_of(&x, c).expect("the armed continuation");
                x.outs[o].script_public_key = armed(&s).spk();
                materialize(&x, &keys).unwrap()
            };
            let (tx, en) = repoint(&good);
            assert!(accept(&tx, &en).is_ok(), "{name}: the control (same state re-pointed) is accepted");
            for (what, s) in ["nothing left", "stop beyond the limit"].into_iter().zip(bad) {
                let err = build(&a(&s)).expect_err(&format!("{name}: the builder arms an entry with {what}"));
                assert!(err.to_string().contains("cannot be armed"), "{name} {what}: {err}");
                let (tx, en) = repoint(&s);
                assert!(accept(&tx, &en).is_err(), "{name}: the covenant arms an entry with {what}");
            }
        }
    }
}

// ------------------------------------------------------------------------------------------------ C6-7

/// C6-7: the smallest KAS value each supported token program accepts on a token output it authorises, measured in the
/// script engine by bisection over the recipient output of a plain token send (only the token program judges it), equals
/// `TemplateId::min_token_output`: KaspaCom's KCC20 0.2.5 refuses less than 0.5 KAS, every other program takes any value.
#[test]
fn c6_7_token_output_floors_are_measured() {
    let keys = keys();
    for p in TemplateId::ALL.into_iter().filter(|t| t.is_token()) {
        let send = Action::SendTokens(SendTokens {
            token: TokenRef { covenant_id: TOKEN_COV, program: p },
            tokens: vec![tok(1, 7 * WHOLE, pk(TAKER), SCHEME_P2PK, 500)],
            recipients: vec![TokenRecipient { pubkey: pk(MAKER_C), amount: 4 * WHOLE, carrier: CARRIER }],
            token_change: None,
            token_change_carrier: CARRIER,
            funding: vec![key_utxo(3, TAKER, 100 * KAS)],
            change: None,
            records: vec![],
            fee: fee(),
        });
        let a = if p.family() == Family::Kron { kron_action(send) } else { send };
        let built = build(&a).unwrap_or_else(|e| panic!("{}: {e}", p.name()));
        let m = MTx::from_built(&built);
        let o = m.outs.iter().position(|o| o.covenant.is_some()).expect("a token output");
        let scripts_ok = |v: u64| {
            let mut x = m.clone();
            x.outs[o].value = v;
            let (tx, en) = materialize(&x, &keys).unwrap();
            kob_protocol::verify::execute(&tx, &en, true).is_ok_and(|r| r.iter().all(|i| i.is_ok()))
        };
        assert!(scripts_ok(CARRIER), "{}: the send itself", p.name());
        let floor = if scripts_ok(1) {
            1
        } else {
            let (mut lo, mut hi) = (1u64, CARRIER); // refused at lo, accepted at hi
            while hi - lo > 1 {
                let mid = lo + (hi - lo) / 2;
                if scripts_ok(mid) {
                    hi = mid;
                } else {
                    lo = mid;
                }
            }
            hi
        };
        println!("{}: smallest token output {floor} sompi", p.name());
        assert_eq!(Some(floor), p.min_token_output(), "{}: measured floor", p.name());
    }
}

/// C6-7 / dust: a new order whose token-output carrier is below its program's floor (or the dust bound) is refused by the
/// order check, and a transaction with a token output below its program's floor is refused by every builder.
#[test]
fn c6_7_builders_refuse_token_outputs_below_the_program_floor() {
    let kc = TemplateId::Kcc20KaspaCom025;
    let low = BidState { delivery_carrier: KASPACOM_MIN_CARRIER as i64 - 1, ..bid(MAKER_B, P245, kc) };
    let e = check_new_order(&AnyState::KobBid(low.clone())).unwrap_err().to_string();
    assert!(e.contains("deliveryCarrier") && e.contains("50000000"), "{e}");
    assert!(check_new_order(&AnyState::KobBid(BidState { delivery_carrier: KASPACOM_MIN_CARRIER as i64, ..low.clone() })).is_ok());
    let dust = BidState { delivery_carrier: kob_protocol::tx::DUST_OUTPUT_MIN as i64 - 1, ..bid(MAKER_B, P245, TemplateId::Kcc20Ref) };
    assert!(check_new_order(&AnyState::KobBid(dust)).unwrap_err().to_string().contains("deliveryCarrier"));
    // a send whose recipient carries less than the KaspaCom floor
    let send = |carrier: u64| {
        Action::SendTokens(SendTokens {
            token: TokenRef { covenant_id: TOKEN_COV, program: kc },
            tokens: vec![tok(1, 7 * WHOLE, pk(TAKER), SCHEME_P2PK, 500)],
            recipients: vec![TokenRecipient { pubkey: pk(MAKER_C), amount: 4 * WHOLE, carrier }],
            token_change: None,
            token_change_carrier: CARRIER,
            funding: vec![key_utxo(3, TAKER, 100 * KAS)],
            change: None,
            records: vec![],
            fee: fee(),
        })
    };
    let e = build(&send(KASPACOM_MIN_CARRIER - 1)).unwrap_err().to_string();
    assert!(e.contains("refuses a token output below 50000000"), "{e}");
    assert!(build(&send(KASPACOM_MIN_CARRIER)).is_ok());
}

/// Dust (KIP-9): a fill whose maker payout is tiny (one token of an ask paying a few thousand sompi) has a storage mass beyond
/// the block limit; the builder refuses it with a "dust payout" error naming the output instead of building an unminable
/// transaction.
#[test]
fn c6_builders_refuse_dust_payouts() {
    let a = AskState { price: 3_000, tip: 0, ..ask(MAKER_A, P250, TemplateId::Kcc20Ref) };
    let mut b = batch(
        NOW,
        vec![Leg::Ask {
            order: order(10, CARRIER, cov(0xa1), 1_000, a.clone()),
            custody: custody(11, 10, cov(0xa1), 1_000),
            amount: WHOLE,
            t: None,
        }],
    );
    b.funding = vec![key_utxo(94, TAKER, 1_000 * KAS)];
    b.change = Some(pk(TAKER));
    let e = build(&Action::Batch(b)).unwrap_err().to_string();
    assert!(e.contains("dust payout"), "{e}");
}
