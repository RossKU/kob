//! Security review 2026-11-19 K-L4: `GET /v1/stats/{token}` (`market::stats`) holds one order's
//! levels at a time, not every level of every curve of the book. The M4 bound kept the book views
//! to their best levels; the statistics read the whole side into one sorted list. Here a book of
//! long curves at distinct prices is read while a counting allocator records the peak.
//!
//! One test in this binary, so no other test allocates while the peak is measured.

mod common;

use common::*;
use kob_protocol::state::*;
use kobd::hex::Hash32;
use kobd::indexer::market;
use kobd::indexer::reads::ReadCtx;
use kobd::testkit::*;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};

struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            let now = LIVE.fetch_add(l.size(), SeqCst) + l.size();
            PEAK.fetch_max(now, SeqCst);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) };
        LIVE.fetch_sub(l.size(), SeqCst);
    }
}

#[global_allocator]
static A: Counting = Counting;

const LEVELS: usize = 1_000;
const CURVES: i64 = 12;

fn curve(maker: u8, k: i64) -> CurveAskState {
    let (h, p, s) = tok_fields(T3);
    // distinct prices for every level of every curve
    let table: Vec<i64> = (0..LEVELS as i64).map(|i| P250 + i * 1_000_000 + k * 7).collect();
    CurveAskState {
        maker: pk(maker),
        token_cov_id: TOKEN_COV,
        token_tpl_hash: h,
        tpl_prefix_len: p,
        tpl_suffix_len: s,
        scale: SCALE,
        min_fill: 1,
        tip: 0,
        active_from: 0,
        level_size: 1,
        total: LEVELS as i64,
        amount_left: LEVELS as i64,
        extension_commitment: EXT,
        table,
    }
}

#[tokio::test]
async fn the_stats_of_a_book_of_long_curves_hold_one_curve_at_a_time() {
    let c = Ctx::new();
    for k in 0..CURVES {
        let s = curve(MAKER_A, k);
        let create = c.w.create_tx(AnyState::KobCurveAsk(s.clone()), CARRIER, MAKER_A, s.amount_left);
        c.push(&[&create]).await;
    }
    let g = c.hs.ingest.lock().unwrap();
    let ctx = ReadCtx { node_daa: Some(c.daa()), settle_depth_daa: 100, now_unix: None };
    let warm = market::stats(g.conn(), &ctx, &Hash32(TOKEN_COV), Some(3)).unwrap();
    assert_eq!(warm.open_asks, CURVES * LEVELS as i64, "every remaining level of every curve is counted");
    assert_eq!(warm.best_ask.as_deref(), Some("250000000"), "the cheapest level of the cheapest curve");
    let base = LIVE.load(SeqCst);
    PEAK.store(base, SeqCst);
    let v = market::stats(g.conn(), &ctx, &Hash32(TOKEN_COV), Some(3)).unwrap();
    let peak = PEAK.load(SeqCst) - base;
    assert_eq!(v.open_asks, warm.open_asks);
    // 12,000 levels at distinct prices: a sorted list of the side is several hundred KiB; one curve's levels are 24 KiB
    assert!(peak < 200 * 1024, "the stats read held {peak} bytes at its peak");
}
