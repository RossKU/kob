//! Regression: an order the database recorded under a template this build no longer pins is never offered to the matcher or
//! the keepers.
//!
//! The protocol v3 sell-first entries `KobIfdAsk` 189b9c32 / `KobIfdAskKron` 85d87838 were retired on 2026-10-06 with today's
//! state layout (only their code changed). A database carried over that change (the TN10 soak keeps its indexer databases
//! across builds) holds live entries of the old template under the contract name `KobIfdAsk`: their state decodes as today's
//! kind, so the book used to list them, and every fill, kill or refund planned from today's template would spend a script
//! that does not hash to the order's output. Such an order is spend-only through its maker's retired cancel.

mod common;

use common::*;
use kob_executor::hex;
use kob_executor::indexer::book::{listed_orders, unlisted_orders};
use kob_executor::testkit::*;
use kob_protocol::artifacts::{template, TemplateId};
use kob_protocol::state::*;

#[tokio::test]
async fn an_order_of_a_retired_template_with_todays_layout_is_never_offered() {
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

    // the database of an older build: the same order recorded under the retired entry (same layout, other code)
    let old = kob_protocol::retired::retired()
        .iter()
        .find(|r| r.kind == TemplateId::KobIfdAsk && r.is_current_layout())
        .expect("KobIfdAsk 189b9c32 is retired with today's layout");
    assert_eq!(hex::encode(&old.template.hash[..4]), "189b9c32");
    assert_ne!(old.template.hash, template(TemplateId::KobIfdAsk).hash);
    {
        let g = c.hs.ingest.lock().unwrap();
        g.conn()
            .execute(
                "UPDATE orders SET template_hash = ?1 WHERE covenant_id = ?2",
                rusqlite::params![&old.template.hash[..], &cov.0[..]],
            )
            .unwrap();
    }
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobIfdAsk(ia)), "the state still reads (the indexer views and the cancel)");
    assert_eq!(offered(&c), (false, false), "neither listed nor offered to the keepers");
    let v = view(&c, &cov);
    assert_eq!(v.template_hash, old.hash_hex(), "the view names the retired template, so a wallet builds the retired cancel");
}
