//! Regression: planner fuzz with SANE-looking but differently distributed books than `matcher_fuzz`:
//! mixed minimum fills (1 base unit up to 7 whole tokens) and amounts that are not multiples of a whole token (the
//! minimum-fill rule and the maker-favour rounding), one token at two scales (two books of one market), wide tip range
//! (tips larger than the spread), many equal prices, larger books (up to 40 orders), more IOC / FOK, `maxFill` caps and TWAP
//! intervals. Every plan must be engine-valid and satisfy `check_invariants` (no partial FOK, no stray, one
//! input per covenant id, at most 8 token inputs, profit >= 1). A panic or a violated invariant is reported
//! with its seed.
//!
//! `KOB_AUDIT_ITERS` (default 60), `KOB_AUDIT_SEED`.

#[path = "matcher_common/mod.rs"]
mod common;

use common::*;
use kob_executor::matcher::book::{ListedOrder, MemoryBook};
use kob_protocol::artifacts::TemplateId;
use kob_protocol::state::*;

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

const PROGRAMS: [TemplateId; 4] = [TemplateId::Kcc20Ref, TemplateId::Kcc20Ref4x5, TemplateId::Kcc20Ref8x8, TemplateId::Kcc20Ref16x16];

fn order(r: &mut Rng, id: u32, tpl: TemplateId, mid: i64) -> ListedOrder {
    let maker = r.range(1, 6) as u8;
    // prices tightly around one mid, many exact ties
    let price = mid + r.range(-6, 6) * 250_000;
    let tip = r.pick(&[0, 0, 100_000, 1_000_000, 5_000_000, 40_000_000]);
    // one token at two scales (1,000 and 1,000,000 base units per whole token): two books of one market; amounts, minimum
    // fills and caps in base units, whole tokens or not
    let k = if r.chance(20) { 1_000 } else { 1 };
    let mf = r.pick(&[1, 1, 250, WHOLE, 1_500, 3_000, 7_000]) * k;
    let whole = r.range(1, 9) * WHOLE;
    let odd = r.range(1, 9 * WHOLE);
    let amount = r.pick(&[whole, odd]) * k;
    let tif = r.pick(&[0, 0, 0, 0, TIF_IOC, TIF_FOK]);
    if r.chance(50) {
        let mut a = ask(maker, price, amount, tpl);
        a.scale = SCALE * k;
        a.tip = tip;
        a.min_fill = mf;
        a.tif = tif;
        if r.chance(15) {
            a.max_fill = r.range(1, 4 * WHOLE) * k;
        }
        if r.chance(10) {
            a.interval = 600;
        }
        let o = l_ask(id, a);
        if tif != 0 {
            fresh(o)
        } else {
            o
        }
    } else {
        let mut b = bid(maker, price, tpl);
        b.scale = SCALE * k;
        b.tip = tip;
        b.min_fill = mf;
        b.tif = tif;
        if r.chance(15) {
            b.max_fill = r.range(1, 4 * WHOLE) * k;
        }
        let v = (b.used(amount).expect("budget") + r.range(1, 4) * DC) as u64;
        let o = listed(cid(id), AnyState::KobBid(b), v, 1_000);
        if tif != 0 {
            fresh(o)
        } else {
            o
        }
    }
}

#[test]
fn different_distributions_keep_every_planner_invariant() {
    let iters: u64 = std::env::var("KOB_AUDIT_ITERS").ok().and_then(|s| s.parse().ok()).unwrap_or(60);
    let seed: u64 = std::env::var("KOB_AUDIT_SEED").ok().and_then(|s| s.parse().ok()).unwrap_or(0x05ee_dd15);
    let mut r = Rng(seed | 1);
    let (mut txs, mut fills, mut failures, mut slack_books) = (0usize, 0usize, vec![], vec![]);
    for it in 0..iters {
        let tpl = r.pick(&PROGRAMS);
        let n = r.range(6, 40) as u32;
        let mid = 250_000_000 + r.range(0, 4) * 10_000_000;
        let orders: Vec<ListedOrder> = (0..n).map(|i| order(&mut r, 1_000 * (it as u32 + 1) + i, tpl, mid)).collect();
        let b = MemoryBook { daa_score: NOW + 5, orders, wallet_tokens: vec![] };
        let inp = input(&b);
        let tk = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let rep =
                kob_executor::matcher::engine::tick(&inp, &cfg(), &kob_executor::matcher::family::Families::default(), &signer());
            for p in &rep.prepared {
                check_invariants(p, &inp);
            }
            rep
        }));
        let slack_now = tk.as_ref().map(|r| r.slack_retries).unwrap_or(0);
        if slack_now > 0 {
            let feats: Vec<String> = b
                .orders
                .iter()
                .map(|o| format!("{}:{}", o.order.state.template_id().name(), o.order.state.amount_left().unwrap_or(-1)))
                .collect();
            slack_books.push(format!("book {it} ({tpl:?}): {} slack retries, orders {feats:?}", slack_now));
        }
        match tk {
            Ok(rep) => {
                for p in &rep.prepared {
                    txs += 1;
                    fills += p.plan.fills.len();
                }
            }
            Err(e) => {
                let msg = e
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_default();
                failures.push(format!("seed {seed:#x} book {it}: {msg}"));
            }
        }
    }
    println!("fuzz2: {iters} books, {txs} transactions, {fills} fills, {} failures", failures.len());
    println!("{} books needed the one-unit budget slack safety net", slack_books.len());
    for s in slack_books.iter().take(5) {
        println!("SLACK {s}");
    }
    for f in failures.iter().take(10) {
        println!("FAILURE {f}");
    }
    assert!(failures.is_empty(), "the planner violated an invariant or produced an invalid plan (see output)");
}
