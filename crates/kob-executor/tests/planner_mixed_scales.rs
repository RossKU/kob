//! Planner fuzz over books with mixed SCALES: orders of one token at scales 1, 100, 1,000, 1,000,000 and 10^9 base units per
//! whole token sit in separate books of one market (their prices are per whole token, comparable only at the same scale);
//! amounts, minimum fills and `maxFill` caps in base units (whole tokens or not, minimum fills from 1 base unit to 7 whole
//! tokens), a wide tip range, many equal prices, larger books (up to 40 orders), IOC / FOK and TWAP intervals, on every KCC-20
//! program. Every plan must be engine-valid and satisfy `check_invariants` (no partial FOK, no stray, one input per covenant
//! id, at most 8 token inputs, every fill within its quantity rules, profit >= 1), AND the compute-budget table must be exact
//! for every shape it produces: the one-unit slack safety net of the lowering never fires (it fired for 0.6-1% of the books
//! of the v2.6 lot-geometry variant of this fuzz on Kcc20Ref4x5 before the table was generated over mixed batches).
//!
//! `KOB_MIXED_ITERS` (default 200), `KOB_MIXED_SEED`.

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

const SCALES: [i64; 6] = [1, 100, 1_000, 1_000, 1_000_000, 1_000_000_000];

fn order(r: &mut Rng, id: u32, tpl: TemplateId, mid: i64) -> ListedOrder {
    let maker = r.range(1, 6) as u8;
    // prices tightly around one mid, many exact ties
    let price = mid + r.range(-6, 6) * 250_000;
    let tip = r.pick(&[0, 0, 100_000, 1_000_000, 5_000_000, 40_000_000]);
    let scale = r.pick(&SCALES);
    let mf = r.pick(&[1, (scale / 4).max(1), scale, scale * 3 / 2, 7 * scale]);
    let whole = r.range(1, 9) * scale;
    let odd = r.range(1, 9 * scale);
    let amount = r.pick(&[whole, odd]);
    let tif = r.pick(&[0, 0, 0, 0, TIF_IOC, TIF_FOK]);
    if r.chance(50) {
        let mut a = ask(maker, price, amount, tpl);
        a.scale = scale;
        a.tip = tip;
        a.min_fill = mf;
        a.tif = tif;
        if r.chance(15) {
            a.max_fill = r.range(1, 4 * scale);
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
        b.scale = scale;
        b.tip = tip;
        b.min_fill = mf;
        b.tif = tif;
        if r.chance(15) {
            b.max_fill = r.range(1, 4 * scale);
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
fn mixed_scale_books_keep_every_planner_invariant_and_the_budget_table_exact() {
    let iters: u64 = std::env::var("KOB_MIXED_ITERS").ok().and_then(|s| s.parse().ok()).unwrap_or(200);
    let seed: u64 = std::env::var("KOB_MIXED_SEED").ok().and_then(|s| s.parse().ok()).unwrap_or(0x05ee_dd15);
    let mut r = Rng(seed | 1);
    let (mut txs, mut fills, mut failures, mut slack_books) = (0usize, 0usize, vec![], vec![]);
    for it in 0..iters {
        let tpl = r.pick(&PROGRAMS);
        let n = r.range(6, 40) as u32;
        let mid = 250_000_000 + r.range(0, 4) * 10_000_000;
        let orders: Vec<ListedOrder> = (0..n).map(|i| order(&mut r, 1_000 * (it as u32 + 1) + i, tpl, mid)).collect();
        let b = MemoryBook { daa_score: NOW + 5, orders, wallet_tokens: vec![] };
        let inp = input(&b);
        let slack0 = kob_executor::matcher::lower::budget_slack_retries();
        let tk = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let rep =
                kob_executor::matcher::engine::tick(&inp, &cfg(), &kob_executor::matcher::family::Families::default(), &signer());
            for p in &rep.prepared {
                check_invariants(p, &inp);
            }
            rep
        }));
        let slack1 = kob_executor::matcher::lower::budget_slack_retries();
        if slack1 > slack0 {
            let feats: Vec<String> = b
                .orders
                .iter()
                .map(|o| format!("{}:{}", o.order.state.template_id().name(), o.order.state.amount_left().unwrap_or(-1)))
                .collect();
            slack_books.push(format!("book {it} ({tpl:?}): {} slack retries, orders {feats:?}", slack1 - slack0));
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
    assert!(slack_books.is_empty(), "the compute-budget table was one unit short for {} books (see SLACK lines)", slack_books.len());
    assert_no_budget_slack();
}
