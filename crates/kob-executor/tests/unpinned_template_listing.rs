//! An order the database recorded under a template this build does not pin is never offered to the matcher or the
//! keepers.
//!
//! A database carried over a template change (the TN10 soak keeps its indexer databases across builds) can hold live orders
//! of an older template under today's contract name; when that template had today's state layout, the state decodes as
//! today's kind. Every fill, kill or refund planned from today's template would spend a script that does not hash to the
//! order's output: such an order is unknown to this build, like an order of any other template it does not pin.

mod common;

use common::*;
use kob_executor::indexer::book::{listed_orders, unlisted_orders};
use kob_executor::testkit::*;
use kob_protocol::state::*;

#[tokio::test]
async fn an_order_of_an_unpinned_template_with_todays_layout_is_never_offered() {
    let c = Ctx::new();
    let ia = IfdAskState { min_fill: 3 * WHOLE, ..ifd_ask(MAKER_A) };
    let create = c.w.create_tx(AnyState::KobIfdAsk(ia.clone()), ia.escrow(CARRIER as i64).unwrap() as u64, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    let offered = |c: &Ctx| {
        let g = c.hs.ingest.lock().unwrap();
        let ids = |v: Vec<kob_executor::matcher::book::ListedOrder>| v.iter().any(|o| o.id() == cov.0);
        (ids(listed_orders(g.conn()).unwrap()), ids(unlisted_orders(g.conn()).unwrap()))
    };
    assert_eq!(offered(&c), (true, false), "an entry of today's template is in the matcher's book");

    // the database of an older build: the same order recorded under another template hash (same layout, other code)
    let other = [0x5a; 32];
    {
        let g = c.hs.ingest.lock().unwrap();
        g.conn()
            .execute("UPDATE orders SET template_hash = ?1 WHERE covenant_id = ?2", rusqlite::params![&other[..], &cov.0[..]])
            .unwrap();
    }
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobIfdAsk(ia)), "the state still reads (the indexer views)");
    assert_eq!(offered(&c), (false, false), "neither listed nor offered to the keepers");
    assert_eq!(view(&c, &cov).template_hash, kob_executor::hex::encode(&other), "the view names the order's own template");
}
