//! The seed corpus: every builder shape of the golden-vector fixtures on every token program, every pair order shape
//! (plain, conditional, if-done, netting, both evidence modes, repeat merges) on mixed program pairs and every
//! budget branch shape of the pair inputs, wide batches, and "one maker" variants of the multi-order batches (every order
//! of the batch made by the same key, so a single output serving two orders shows up in that maker's books).

use kob_protocol::artifacts::TemplateId;
use kob_protocol::build::{build_with, Action};

use super::{accept, materialize, Keys, MTx};
use crate::common::{self, pair, pk, MAKER_A, MAKER_B};

pub struct Seed {
    pub name: String,
    pub action: Option<Action>,
    pub m: MTx,
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// The action with every order's maker replaced by maker A (JSON field `maker`; payout keys follow the state).
pub fn one_maker(a: &Action) -> Option<Action> {
    let mut v = serde_json::to_value(a).ok()?;
    let target = serde_json::Value::String(hex(&pk(MAKER_A)));
    let mut changed = 0;
    fn walk(v: &mut serde_json::Value, target: &serde_json::Value, changed: &mut usize) {
        match v {
            serde_json::Value::Object(m) => {
                for (k, x) in m.iter_mut() {
                    if k == "maker" && x.is_string() && x != target {
                        *x = target.clone();
                        *changed += 1;
                    } else {
                        walk(x, target, changed);
                    }
                }
            }
            serde_json::Value::Array(a) => a.iter_mut().for_each(|x| walk(x, target, changed)),
            _ => {}
        }
    }
    walk(&mut v, &target, &mut changed);
    (changed > 0).then(|| serde_json::from_value(v).ok()).flatten()
}

/// Terminal bid fills around `REFUND_MIN` (KobBid, KobBidKron, KobCondBid, KobCondBidKron): the order holds the spend, the
/// delivery carrier and a remainder of REFUND_MIN - 1 (stays on the delivery), REFUND_MIN, REFUND_MIN + 1 and 7 KAS (returns to
/// the maker in an output of its own at output M - 1 - i), plus a batch of two such bids (both refunds; one refund and one
/// remainder that stays). The fuzzer mutates these layouts: refund missing, redirected, short, in the wrong slot.
pub fn refund_actions() -> Vec<(String, Action)> {
    use crate::common::{bid, cond_bid, cov, dc, order, taker_carrier, tok, MAKER_C, NOW, P250, SCALE, TAKER, WHOLE};
    use kob_protocol::build::{Batch, Leg};
    use kob_protocol::state::{quote_of, BidState, Round, TIF_IOC};
    const MIN: i64 = 100_000_000;
    let batch = |legs: Vec<Leg>| Batch {
        lock_time: NOW,
        legs,
        updates: vec![],
        taker_tokens: vec![],
        taker: None,
        taker_token_carrier: taker_carrier(),
        keep_surplus: vec![],
        keep_carrier: None,
        receivers: vec![],
        payments: vec![],
        funding: vec![],
        change: None,
        records: vec![],
        fee: crate::common::fee(),
    };
    let rems = [("below", MIN - 1), ("min", MIN), ("above", MIN + 1), ("big", 700_000_000)];
    let mut v = vec![];
    for p in [TemplateId::Kcc20Ref8x8, TemplateId::KronToken2433] {
        let conv = |a: Action| if p.family() == kob_protocol::Family::Kron { crate::common::kron_action_on(p, a) } else { a };
        let n = 3 * WHOLE;
        for (tag, rem) in rems {
            // KobBid / KobBidKron: an IOC bid fills and ends
            let b0 = BidState { tif: TIF_IOC, ..bid(MAKER_B, P250, p) };
            let spend = b0.spend(n, NOW as i64, 1_000).unwrap();
            let mut b =
                batch(vec![Leg::Bid { order: order(20, (spend + dc() + rem) as u64, cov(0xb1), 1_000, b0), amount: n, t: None }]);
            b.taker_tokens = vec![tok(21, n, pk(TAKER), kob_protocol::state::SCHEME_P2PK, 1_000)];
            b.change = Some(pk(TAKER));
            v.push((format!("refund.bid.{tag}@{}", p.name()), conv(Action::Batch(b))));
            // KobCondBid / KobCondBidKron: the stop leg takes everything left
            let mut c0 = cond_bid(MAKER_B, p);
            c0.armed = 1;
            let leg_price = c0.leg_price(1, false, NOW as i64, 2_000).unwrap();
            let spend = c0.spend(c0.amount_left, leg_price).unwrap();
            let amount = c0.amount_left;
            let mut b = batch(vec![Leg::CondBid {
                order: order(40, (spend + dc() + rem) as u64, cov(0xe1), 2_000, c0),
                amount,
                leg: 1,
                evidence: None,
                t: Some(NOW as i64),
                merge: None,
            }]);
            b.taker_tokens = vec![tok(41, amount, pk(TAKER), kob_protocol::state::SCHEME_P2PK, 1_000)];
            b.change = Some(pk(TAKER));
            v.push((format!("refund.condbid.{tag}@{}", p.name()), conv(Action::Batch(b))));
        }
        // two bids of one batch: both refund (MIN, big), or one refunds and one stays (big, below)
        for (tag, r0, r1) in [("both", MIN, 700_000_000), ("refund+stay", 700_000_000, MIN - 1), ("stay+refund", MIN - 1, 700_000_000)]
        {
            let mut legs = vec![];
            for (k, (mk, rem)) in [(MAKER_B, r0), (MAKER_C, r1)].into_iter().enumerate() {
                let b0 = BidState { tif: TIF_IOC, ..bid(mk, P250, p) };
                let spend = quote_of(n, b0.price + b0.tip, SCALE, Round::Down).unwrap();
                legs.push(Leg::Bid {
                    order: order(20 + k as u8, (spend + dc() + rem) as u64, cov(0xb1 + k as u8), 1_000, b0),
                    amount: n,
                    t: None,
                });
            }
            let mut b = batch(legs);
            b.taker_tokens = vec![tok(31, 2 * n, pk(TAKER), kob_protocol::state::SCHEME_P2PK, 1_000)];
            b.change = Some(pk(TAKER));
            v.push((format!("refund.bid2.{tag}@{}", p.name()), conv(Action::Batch(b))));
        }
    }
    v
}

/// Every seed action (name, action).
pub fn actions(wide: bool) -> Vec<(String, Action)> {
    let mut v = vec![];
    for p in common::PROGRAMS {
        for (n, a) in common::scenarios_on(p) {
            v.push((format!("{n}@{}", p.name()), a));
        }
    }
    let pairs = [
        (TemplateId::Kcc20Ref, TemplateId::Kcc20Ref8x8),
        (TemplateId::Kcc20Ref8x8, TemplateId::KronToken2433),
        (TemplateId::KronToken2433, TemplateId::Kcc20Ref),
        (TemplateId::KronToken2732, TemplateId::KronToken2433),
        (TemplateId::Kcc20KaspaCom025, TemplateId::Kcc20P2),
    ];
    for (pa, pb) in pairs {
        for (n, a) in pair::pair_scenarios(pa, pb) {
            v.push((format!("{n}@{}>{}", pa.name(), pb.name()), a));
        }
    }
    v.extend(refund_actions());
    if wide {
        for p in [TemplateId::Kcc20Ref8x8, TemplateId::KronToken2433] {
            let base: Vec<(String, Action)> = common::scenarios_on(p)
                .into_iter()
                .filter(|(n, _)| n.starts_with("take.") || n.starts_with("cond.") || n.starts_with("ifd.") || n.starts_with("rpt."))
                .collect();
            for (n, a) in base {
                if let Some(w) = common::pad_batch(p, &a, 1, 1) {
                    v.push((format!("{n}+pad1x1@{}", p.name()), w));
                }
            }
        }
        for (pa, pb) in [(TemplateId::Kcc20Ref8x8, TemplateId::Kcc20Ref8x8), (TemplateId::KronToken2433, TemplateId::Kcc20Ref8x8)] {
            v.push((format!("pair.net.3x1@{}>{}", pa.name(), pb.name()), Action::Batch(pair::netting(pa, pb, 3, 1, false))));
            v.push((format!("pair.net.1x3@{}>{}", pa.name(), pb.name()), Action::Batch(pair::netting(pa, pb, 1, 3, false))));
            for (n, a) in pair::pair_branch_shapes(pa, pb) {
                v.push((format!("{n}@{}>{}", pa.name(), pb.name()), a));
            }
        }
    }
    // one-maker variants of every batch with more than one order
    let extra: Vec<(String, Action)> = v
        .iter()
        .filter(|(_, a)| matches!(a, Action::Batch(b) if b.legs.len() + b.updates.len() > 1))
        .filter_map(|(n, a)| one_maker(a).map(|a| (format!("{n}+oneMaker"), a)))
        .collect();
    v.extend(extra);
    v
}

/// Builds every seed; returns the valid ones and the names that did not build or validate (with why).
/// Extra seed actions: every `*.json` file (a builder `Action`) of the directory `KOB_C6_EXTRA_SEEDS` (e.g. the requests
/// the planner property tests of `kobd` dump with `KOB_C6_DUMP`: planner-shaped global batches).
pub fn extra_actions() -> Vec<(String, Action)> {
    let Some(dir) = std::env::var_os("KOB_C6_EXTRA_SEEDS") else { return vec![] };
    let mut files: Vec<_> = std::fs::read_dir(dir).map(|d| d.filter_map(Result::ok).map(|e| e.path()).collect()).unwrap_or_default();
    files.sort();
    files
        .into_iter()
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .filter_map(|p| {
            let a: Action = serde_json::from_str(&std::fs::read_to_string(&p).ok()?).ok()?;
            Some((format!("extra:{}", p.file_name()?.to_string_lossy()), a))
        })
        .collect()
}

pub fn build_seeds(keys: &Keys, wide: bool) -> (Vec<Seed>, Vec<(String, String)>) {
    let mut ok = vec![];
    let mut bad = vec![];
    // a role the committed budget table has not measured gets a provisional budget (budgets are not enforced here)
    let budgets = |role: &str| kob_protocol::budget::lookup(role).or(Ok(100));
    for (name, a) in actions(wide).into_iter().chain(extra_actions()) {
        let built = match build_with(&a, &budgets) {
            Ok(b) => b,
            Err(e) => {
                bad.push((name, format!("build: {e}")));
                continue;
            }
        };
        let m = MTx::from_built(&built);
        match materialize(&m, keys)
            .map_err(|e| format!("assemble: {e}"))
            .and_then(|(tx, en)| accept(&tx, &en).map_err(|e| format!("{e:?}")))
        {
            Ok(()) => ok.push(Seed { name, action: Some(a), m }),
            Err(e) => bad.push((name, e)),
        }
    }
    // router intents: executed by a keeper (no signature) and expired by anyone
    for (name, built) in super::intents::intent_seeds() {
        let m = MTx::from_built(&built);
        match materialize(&m, keys)
            .map_err(|e| format!("assemble: {e}"))
            .and_then(|(tx, en)| accept(&tx, &en).map_err(|e| format!("{e:?}")))
        {
            Ok(()) => ok.push(Seed { name, action: None, m }),
            Err(e) => bad.push((name, e)),
        }
    }
    (ok, bad)
}
