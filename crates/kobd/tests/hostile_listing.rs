//! Regression: an order whose numbers the covenant refuses is indexed (status, cancel) but never listed.
//!
//! `slope != 0` with `decayStep = 0` decodes and used to be listed; the matcher then divided by zero and the process died.

mod common;

use common::*;
use kob_protocol::artifacts::TemplateId;
use kob_protocol::state::*;
use kobd::hex::Hash32;
use kobd::indexer::book;
use kobd::indexer::recordlog::{ImportCustody, LogOp};
use kobd::script::spk_bytes;
use kobd::testkit::*;

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

/// Regression: a bid whose budget the covenant can never evaluate (a negative tip, or a scale above `10^9` where `quoteOf`
/// overflows) is indexed with no buying power, at once, and never listed.
///
/// The buying power of such a bid used to scan down one base unit at a time from about `10^18`: the indexer hung on the
/// placement (and again after every restart), and so did every reader of the bid.
#[tokio::test]
async fn a_bid_whose_budget_never_evaluates_is_indexed_at_once_and_not_listed() {
    let c = Ctx::new();
    // a block first: the indexer has a cursor, and an honest bid is listed
    let honest = bid(MAKER_B, P260);
    let honest_tx = c.w.create_tx(AnyState::KobBid(honest.clone()), honest.escrow(5 * WHOLE, 2).unwrap() as u64, MAKER_B, 0);
    c.push(&[&honest_tx]).await;
    let hostile = [
        (0x52, BidState { price: 2, tip: -1, scale: 1_000_000_000, ..bid(MAKER_B, P260) }),
        (0x62, BidState { tip: 0, scale: 1_000_000_000_000, ..bid(MAKER_B, P260) }),
    ];
    for (k, b) in hostile {
        // the budget rate is positive, but the covenant cannot evaluate the budget of floor(budget * scale / rate)
        let rate = b.budget_rate().filter(|r| *r > 0).expect("a positive budget rate");
        // 10 KAS of budget over the delivery carrier and the reserve
        let value = (b.delivery_carrier + b.reserve) as u64 + 10 * KAS;
        let naive = (10 * KAS) as i128 * b.scale as i128 / rate as i128;
        assert!(b.used(naive as i64).is_none(), "{naive}");
        let state = AnyState::KobBid(b);
        let cov = Hash32([k; 32]);
        let op = LogOp::Import {
            template: TemplateId::KobBid,
            state: state.encode(),
            covenant_id: cov,
            txid: Hash32([k + 1; 32]),
            index: 0,
            value,
            spk: spk_bytes(&state.spk()),
            daa: 900,
            custody: None,
            parent: None,
            prefund: None,
        };
        let applied = c.hs.ingest.lock().unwrap().apply_ops(vec![op], None).unwrap();
        assert_eq!(applied.ops.imported, 1, "{:?}", applied.ops.rejected);
        let g = c.hs.ingest.lock().unwrap();
        let reason: Option<String> =
            g.conn().query_row("SELECT unlisted_reason FROM orders WHERE covenant_id = ?1", [&cov.0[..]], |r| r.get(0)).unwrap();
        assert!(reason.as_deref().is_some_and(|r| r.starts_with("bad_state:")), "{reason:?}");
        let (status, remaining): (String, Option<i64>) = g
            .conn()
            .query_row(
                "SELECT status, remaining_amount FROM order_state WHERE order_id = (SELECT id FROM orders WHERE covenant_id = ?1)",
                [&cov.0[..]],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((status.as_str(), remaining), ("open", Some(0)));
        let listed = book::listed_orders(g.conn()).unwrap();
        assert_eq!(listed.len(), 1, "only the honest bid is in the matcher's book");
        assert!(listed.iter().all(|o| o.id() != cov.0));
    }
}
