//! An order the database recorded under a template this build does not pin is never listed nor offered to the matcher or
//! the keepers (`docs/spec/template-retirement.md`).
//!
//! A database carried over a template change (the TN10 soak keeps its indexer databases across builds) can hold live orders
//! of an older template under today's contract name; when that template had today's state layout, the state decodes as
//! today's kind. Every fill, kill or refund planned from today's template would spend a script that does not hash to the
//! order's output: such an order is unknown to this build, like an order of any other template it does not pin. No read
//! returns it: not the matcher's and keepers' live orders, not the public book, its counts and stats, not `/v1/orders`, and
//! not `/v1/orders/{id}` (not found: the web wallet then resolves the order from its own placement record and marks it
//! `old-template`).

mod common;

use common::*;
use kob_executor::hex::Hash32;
use kob_executor::indexer::book::{listed_orders, unlisted_orders};
use kob_executor::indexer::market;
use kob_executor::indexer::reads::{self, OrderFilter, ReadCtx};
use kob_executor::testkit::*;
use kob_protocol::state::*;

/// The database of an older build: the order recorded under another template hash (same layout, other code).
fn record_under_another_template(c: &Ctx, cov: &Hash32) -> [u8; 32] {
    let other = [0x5a; 32];
    let g = c.hs.ingest.lock().unwrap();
    g.conn()
        .execute("UPDATE orders SET template_hash = ?1 WHERE covenant_id = ?2", rusqlite::params![&other[..], &cov.0[..]])
        .unwrap();
    other
}

fn read_ctx(c: &Ctx) -> ReadCtx {
    ReadCtx { node_daa: Some(c.daa()), settle_depth_daa: 100, now_unix: None }
}

/// Where the order is seen: (the matcher's book, the keepers' unlisted orders, the public book, `/v1/orders` of its token,
/// `/v1/orders/{id}`, the token's open asks in the stats).
fn seen(c: &Ctx, cov: &Hash32) -> (bool, bool, bool, bool, bool, i64) {
    let g = c.hs.ingest.lock().unwrap();
    let conn = g.conn();
    let ctx = read_ctx(c);
    let has = |v: Vec<kob_executor::matcher::book::ListedOrder>| v.iter().any(|o| o.id() == cov.0);
    let book = reads::book(conn, &ctx, &Hash32(TOKEN_COV), 50, false).unwrap();
    let in_book = match book.asks {
        reads::BookSide::Orders(v) => v.iter().any(|o| o.covenant_id == cov.to_hex()),
        reads::BookSide::Levels(_) => unreachable!("not aggregated"),
    };
    let f = OrderFilter { maker: None, token: Some(Hash32(TOKEN_COV)), status: None };
    let listed = reads::orders(conn, &ctx, &f, 100, None).unwrap().items.iter().any(|o| o.covenant_id == cov.to_hex());
    let one = reads::order(conn, &ctx, cov).unwrap().is_some();
    let stats = market::stats(conn, &ctx, &Hash32(TOKEN_COV), None).unwrap();
    (has(listed_orders(conn).unwrap()), has(unlisted_orders(conn).unwrap()), in_book, listed, one, stats.open_asks)
}

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
    let todays = kob_protocol::artifacts::template(kob_protocol::artifacts::TemplateId::KobIfdAsk).hash;
    assert_eq!(view(&c, &cov).template_hash, kob_executor::hex::encode(&todays), "the view names today's template");

    record_under_another_template(&c, &cov);
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobIfdAsk(ia)), "the state still reads (the store keeps the row)");
    assert_eq!(offered(&c), (false, false), "neither listed nor offered to the keepers");
    let g = c.hs.ingest.lock().unwrap();
    assert!(reads::order(g.conn(), &read_ctx(&c), &cov).unwrap().is_none(), "the single-order view does not know it");
}

#[tokio::test]
async fn a_resting_ask_of_an_unpinned_template_leaves_every_book_list_and_count() {
    let c = Ctx::new();
    let a = ask(MAKER_A, 250_000_000);
    let create = c.w.create_tx(AnyState::KobAsk(a), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    assert_eq!(seen(&c, &cov), (true, false, true, true, true, 1), "today's template: listed everywhere");

    record_under_another_template(&c, &cov);
    assert_eq!(c.hs.status(&cov), "open", "still live on chain (its maker cancels it with the release it was placed with)");
    assert_eq!(seen(&c, &cov), (false, false, false, false, false, 0), "another template: listed nowhere, not found by id");
}
