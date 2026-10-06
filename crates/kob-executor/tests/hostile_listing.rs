//! Regression: an order whose numbers the covenant refuses is indexed (status, cancel) but never listed.
//!
//! `slope != 0` with `decayStep = 0` decodes and used to be listed; the matcher then divided by zero and the process died.

mod common;

use common::*;
use kob_executor::hex::Hash32;
use kob_executor::indexer::book;
use kob_executor::indexer::recordlog::{ImportCustody, LogOp};
use kob_executor::script::spk_bytes;
use kob_executor::testkit::*;
use kob_protocol::artifacts::TemplateId;
use kob_protocol::state::*;

#[tokio::test]
async fn a_decay_step_zero_ask_is_indexed_but_not_listed() {
    let c = Ctx::new();
    let honest_bid = bid(MAKER_B, P260);
    let bid_tx = c.w.create_tx(AnyState::KobBid(honest_bid.clone()), honest_bid.escrow(5 * WHOLE, 2).unwrap() as u64, MAKER_B, 0);
    c.push(&[&bid_tx]).await;

    let hostile = AskState { slope: 1, price_end: 1, decay_step: 0, ..ask(MAKER_A, P250) };
    let state = AnyState::KobAsk(hostile);
    let bytes = state.encode();
    assert!(AnyState::decode(TemplateId::KobAsk, &bytes).is_ok(), "the codec still accepts decayStep = 0 (the chain does)");
    let cov = Hash32([0x42; 32]);
    let op = LogOp::Import {
        template: TemplateId::KobAsk,
        state: bytes,
        covenant_id: cov,
        txid: Hash32([0x43; 32]),
        index: 0,
        value: CARRIER,
        spk: spk_bytes(&state.spk()),
        daa: 900,
        custody: Some(ImportCustody { txid: Hash32([0x44; 32]), index: 1, value: CARRIER, amount: 10 * WHOLE, ext: Hash32(EXT) }),
        parent: None,
        prefund: None,
    };
    let applied = c.hs.ingest.lock().unwrap().apply_ops(vec![op], None).unwrap();
    assert_eq!(applied.ops.imported, 1, "{:?}", applied.ops.rejected);

    let reason: Option<String> = {
        let g = c.hs.ingest.lock().unwrap();
        g.conn().query_row("SELECT unlisted_reason FROM orders WHERE covenant_id = ?1", [&cov.0[..]], |r| r.get(0)).unwrap()
    };
    assert!(reason.as_deref().is_some_and(|r| r.starts_with("bad_state:decayStep")), "{reason:?}");
    let listed = { book::listed_orders(c.hs.ingest.lock().unwrap().conn()).unwrap() };
    assert_eq!(listed.len(), 1, "only the honest bid is in the matcher's book");
    assert!(listed.iter().all(|o| o.id() != cov.0));
}
