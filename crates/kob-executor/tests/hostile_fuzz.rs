//! Regression: adversarial-value fuzz of the matcher and keeper ticks.
//!
//! The existing `matcher_fuzz` draws sane parameters (prices 2.40..2.60, tips, amounts and minimum fills). This one
//! takes valid orders of every kind (the three pair kinds included) and overwrites random numeric state fields with extremes (0, +-1,
//! huge, `i64::MIN/MAX`), the way a hostile maker can: `AnyState::decode` accepts any `i64`. Every
//! tick runs under `catch_unwind`; panic sites, engine-invalid plans and unprofitable transactions are
//! collected. Environment: `KOB_AUDIT_ITERS` (default 150), `KOB_AUDIT_SEED`.
//!
//! The state numbers are gated by `kob_executor::sanity` before the planner sees them, so no panic site may remain (a panic caught
//! by the tick's own last-line `catch_unwind` still counts as a site here) and every transaction is profitable and engine-valid.

#[path = "matcher_common/mod.rs"]
mod common;

use common::pair::*;
use common::*;
use kob_executor::keepers::{tick as keeper_tick, KeeperConfig, KeeperInput};
use kob_executor::matcher::book::{ListedOrder, MemoryBook};
use kob_executor::matcher::engine::{tick, EngineConfig};
use kob_executor::matcher::family::Families;
use kob_protocol::state::*;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::Mutex;

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

const EXTREMES: [i64; 22] = [
    0,
    1,
    2,
    -1,
    -2,
    3,
    7,
    1_000,
    1_000_000,
    100_000_000,
    1_000_000_000,
    1_000_000_000_000,
    1_000_000_000_000_000,
    1_000_000_000_000_000_000,
    i64::MAX / 2,
    i64::MAX,
    i64::MIN,
    i64::MIN + 1,
    u32::MAX as i64,
    1 << 32,
    -1_000_000_000,
    250_000_001,
];

const SKIP: [&str; 14] = [
    "tplPrefixLen",
    "tplSuffixLen",
    "tokenTplHash",
    "tokenCovId",
    "maker",
    "exitState",
    "sPre",
    "sSuf",
    "tPre",
    "tSuf",
    "aPre",
    "aSuf",
    "bPre",
    "bSuf",
];

/// Overwrite numeric fields of a state with extremes (JSON round trip through the state's own serde).
fn mutate<S: Serialize + DeserializeOwned>(s: &S, r: &mut Rng, pct: u64) -> Option<S> {
    let mut v = serde_json::to_value(s).ok()?;
    let obj = v.as_object_mut()?;
    let keys: Vec<String> = obj.keys().cloned().collect();
    for k in keys {
        if SKIP.contains(&k.as_str()) {
            continue;
        }
        let is_int = obj[&k].as_str().is_some_and(|s| s.len() <= 20 && s.parse::<i64>().is_ok());
        if is_int && r.chance(pct) {
            // every field, `amountLeft` included: a huge one used to make candidate generation linear in it
            let v = r.pick(&EXTREMES);
            obj.insert(k, serde_json::Value::String(v.to_string()));
        }
    }
    serde_json::from_value(v).ok()
}

/// A state through the protocol codec, as a placement record would carry it.
fn through_codec(state: AnyState) -> Option<AnyState> {
    let bytes = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| state.encode())).ok()?;
    AnyState::decode(state.template_id(), &bytes).ok()
}

/// A hostile pair order (kind 0 `KobPair`, 1 `KobCondPair`, 2 `KobIfdPair`; an ask or a bid of token `tpl` against a KCC-20 or KRON
/// token B): valid fields overwritten with extremes, its custodies listed as the fixtures build them.
#[allow(clippy::too_many_arguments)]
fn hostile_pair(
    r: &mut Rng,
    id: u32,
    pa: kob_protocol::artifacts::TemplateId,
    kind: i64,
    maker: u8,
    amount: i64,
    min_fill: i64,
    pct: u64,
    daa: u64,
) -> Option<ListedOrder> {
    let pb = r.pick(&[T3, kob_protocol::artifacts::TemplateId::KronToken2433]);
    let ask = r.chance(50);
    let rate = if ask { WHOLE * r.range(90, 125) / 100 } else { WHOLE * r.range(115, 170) / 100 };
    let (tip, tif) = (r.pick(&[0, 10, PTIP]), r.pick(&[0, 0, 1, 2]));
    let listed = |f: &dyn Fn() -> ListedOrder| std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).ok();
    match kind {
        0 => {
            let x = PairState { min_fill, ..pair_state(maker, ask, pa, pb, amount, rate, tip, tif) };
            let x = mutate(&x, r, pct)?;
            let AnyState::KobPair(x) = through_codec(AnyState::KobPair(x))? else { return None };
            listed(&|| l_pair(id, pa, pb, x.clone(), daa))
        }
        1 => {
            let tp = if ask { rate + 100 } else { rate - 100 };
            let c = CondPairState { min_fill, tip, armed: r.pick(&[0, 1]), ..cond_pair(maker, ask, pa, pb, amount, tp, rate) };
            let c = mutate(&c, r, pct)?;
            let AnyState::KobCondPair(c) = through_codec(AnyState::KobCondPair(c))? else { return None };
            listed(&|| l_cond_pair(id, pa, pb, c.clone(), daa))
        }
        _ => {
            let e = IfdPairState { min_fill, tip, ..ifd_pair(maker, ask, pa, pb, amount, rate) };
            let e = mutate(&e, r, pct)?;
            let AnyState::KobIfdPair(e) = through_codec(AnyState::KobIfdPair(e))? else { return None };
            listed(&|| l_ifd_pair(id, pa, pb, e.clone(), daa))
        }
    }
}

fn hostile(r: &mut Rng, id: u32, tpl: kob_protocol::artifacts::TemplateId) -> Option<ListedOrder> {
    let maker = r.range(1, 4) as u8;
    // amounts in base units, whole tokens or not, and minimum fills other than one whole token (before the mutation)
    let whole = r.range(1, 8) * WHOLE;
    let odd = r.range(1, 8_000);
    let amount = r.pick(&[whole, odd]);
    let min_fill = r.pick(&[1, 250, WHOLE, 1_500]);
    let pct = r.pick(&[3, 8, 20]);
    let price = 240_000_000 + r.range(0, 40) * 500_000;
    let daa = r.pick(&[1_000, 2_000, NOW - 10]);
    let value_of = |s: &AnyState| -> Option<u64> {
        // an order the indexer would list carries a plausible escrow; a bogus one is as good for the fuzz
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match s {
            AnyState::KobBid(b) => (b.used(amount).unwrap_or(1) + b.delivery_carrier + b.reserve).max(1) as u64,
            AnyState::KobCondBid(c) => c.escrow(c.max_fills()).unwrap_or(1).max(1) as u64,
            AnyState::KobIfdBid(e) => e.escrow().unwrap_or(1).max(1) as u64,
            AnyState::KobIfdAsk(e) => e.escrow(CARRIER as i64).unwrap_or(1).max(1) as u64,
            _ => CARRIER,
        }))
        .ok()
    };
    let kind = r.range(0, 8);
    if kind >= 6 {
        // a pair order of token `tpl` against a KCC-20 or KRON token B
        return hostile_pair(r, id, tpl, kind - 6, maker, amount, min_fill, pct, daa);
    }
    let state = match kind {
        0 => AnyState::KobAsk(mutate(&AskState { min_fill, ..ask(maker, price, amount, tpl) }, r, pct)?),
        1 => AnyState::KobBid(mutate(&BidState { min_fill, ..bid(maker, price, tpl) }, r, pct)?),
        2 => {
            let c = CondAskState { min_fill, ..cond_ask(maker, price + 10_000_000, price - 5_000_000, amount, tpl) };
            AnyState::KobCondAsk(mutate(&c, r, pct)?)
        }
        3 => {
            let c = CondBidState { min_fill, ..cond_bid(maker, price - 10_000_000, price + 5_000_000, amount, tpl) };
            AnyState::KobCondBid(mutate(&c, r, pct)?)
        }
        4 => AnyState::KobIfdBid(mutate(&IfdBidState { min_fill, ..ifd_bid(maker, price, amount, tpl) }, r, pct)?),
        _ => AnyState::KobIfdAsk(mutate(&IfdAskState { min_fill, ..ifd_ask(maker, price, amount, tpl) }, r, pct)?),
    };
    // it must survive the protocol codec, as a placement record would
    let bytes = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| state.encode())).ok()?;
    let state = AnyState::decode(state.template_id(), &bytes).ok()?;
    let value = value_of(&state)?;
    let o = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| listed(cid(id), state, value, daa))).ok()?;
    Some(o)
}

static GEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static SITES: Mutex<BTreeMap<String, (usize, String)>> = Mutex::new(BTreeMap::new());

#[test]
fn hostile_values_never_panic_the_matcher_or_keepers() {
    let iters: u64 = std::env::var("KOB_AUDIT_ITERS").ok().and_then(|s| s.parse().ok()).unwrap_or(150);
    let seed: u64 = std::env::var("KOB_AUDIT_SEED").ok().and_then(|s| s.parse().ok()).unwrap_or(0x00a0_d17d);
    std::panic::set_hook(Box::new(|info| {
        if GEN.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        let loc = info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_default();
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_default();
        let mut g = SITES.lock().unwrap();
        let e = g.entry(loc).or_insert((0, msg));
        e.0 += 1;
    }));
    let mut r = Rng(seed | 1);
    let (mut ticks, mut txs, mut slow, mut pair_fills) = (0usize, 0usize, 0usize, 0usize);
    let mut bad_invariants: Vec<String> = vec![];
    for it in 0..iters {
        let tpl = r.pick(&[T3, T8, kob_protocol::artifacts::TemplateId::Kcc20Ref4x5]);
        let n = r.range(3, 14) as u32;
        let mut orders: Vec<ListedOrder> = vec![];
        for i in 0..n {
            GEN.store(true, std::sync::atomic::Ordering::SeqCst);
            let o = hostile(&mut r, 1_000 * (it as u32 + 1) + i, tpl);
            GEN.store(false, std::sync::atomic::Ordering::SeqCst);
            if let Some(o) = o {
                orders.push(o);
            }
        }
        // a sane crossing pair so that the planner has something to plan with
        if r.chance(60) {
            orders.push(l_bid(900_000 + it as u32, bid(1, P260, tpl), 5 * WHOLE));
            orders.push(l_ask(950_000 + it as u32, ask(2, P250, 5 * WHOLE, tpl)));
            // and the books of token B the pair orders route through (asks to buy B, bids to sell it), asks of A for the pair bids
            let k3 = kob_protocol::artifacts::TemplateId::KronToken2433;
            orders.push(l_ask_b(970_000 + it as u32, tpl, T3, 3, P200, 5 * WHOLE));
            orders.push(l_ask_b(980_000 + it as u32, tpl, k3, 3, P200, 5 * WHOLE));
            orders.push(l_bid_b(990_000 + it as u32, tpl, T3, 3, P200, 5 * WHOLE));
            orders.push(l_bid_b(995_000 + it as u32, tpl, k3, 3, P200, 5 * WHOLE));
            orders.push(l_ask_a(998_000 + it as u32, tpl, 3, P250, 5 * WHOLE));
            // and two sane pair orders (an ask selling A for B, a bid buying A with B) next to the hostile ones
            orders.push(l_pair(960_000 + it as u32, tpl, T3, pask(1, tpl, T3, 4 * WHOLE, 1_200, PTIP), 1_000));
            orders.push(l_pair(965_000 + it as u32, tpl, T3, pbid(2, tpl, T3, 4 * WHOLE, 1_400, PTIP), 1_000));
        }
        let b = MemoryBook { daa_score: NOW + 5, orders, wallet_tokens: vec![] };
        let mut k = EngineConfig { token_carrier: OP_TOKEN_CARRIER, ..cfg() };
        k.planner.arm = !r.chance(30);
        let inp = input(&b);
        let t0 = std::time::Instant::now();
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| tick(&inp, &k, &Families::default(), &signer())));
        if t0.elapsed().as_secs() >= 2 {
            slow += 1;
        }
        ticks += 1;
        if let Ok(rep) = res {
            // a hostile pair order is quarantined or planned, never panics; a transaction filling one is engine-validated,
            // profitable and leaves the operator no token
            check_pair(&rep, &inp);
            for p in &rep.prepared {
                txs += 1;
                pair_fills += p.plan.fills.iter().filter(|f| f.cand.pair.is_some()).count();
                if p.accounting.profit < 0 {
                    bad_invariants.push(format!("book {it}: unprofitable batch {}", p.accounting.profit));
                }
                if p.validation.is_none() {
                    bad_invariants.push(format!("book {it}: unvalidated transaction"));
                }
            }
        }
        let kin = KeeperInput {
            orders: b.orders.clone(),
            clock: inp.clock,
            funding: vec![funding(50 * KAS)],
            excluded: Default::default(),
            excluded_outpoints: Default::default(),
            known_unprofitable: Default::default(),
        };
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| keeper_tick(&kin, &KeeperConfig::default(), &signer())));
    }
    let _ = std::panic::take_hook();
    let sites = SITES.lock().unwrap();
    println!(
        "hostile fuzz: {iters} books, {ticks} ticks, {txs} transactions ({pair_fills} pair fills), {slow} ticks >= 2 s, {} panic sites",
        sites.len()
    );
    for (loc, (n, msg)) in sites.iter() {
        println!("PANIC SITE x{n}: {loc}: {msg}");
    }
    for m in bad_invariants.iter().take(20) {
        println!("INVARIANT: {m}");
    }
    assert!(sites.is_empty() && bad_invariants.is_empty(), "panic sites / invariant violations found (see output)");
}
