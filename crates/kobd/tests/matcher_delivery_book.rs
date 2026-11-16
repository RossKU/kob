//! The matcher's book from the indexer (`indexer::book::listed_orders`) carries what a delivering bid's fill spends: a
//! mode-1 bid's proven bought-custody (`ListedOrder::bought_custody`, exact, so the order is a candidate) and a mode-2
//! bid's top-up targets, the maker's live receive UTXOs of the token and commitment (`ListedOrder::receive`).

mod common;
#[path = "common/delivery.rs"]
mod delivery;

use common::*;
use delivery::*;
use kob_protocol::state::*;
use kobd::testkit::*;

#[tokio::test]
async fn the_book_lists_the_bought_custody_and_the_makers_receive_utxos() {
    let c = Ctx::new();
    // mode 1: after a first fill the book lists the order with its exact bought-custody
    let b1 = BidState { mode: MODE_ACCUMULATE, ..bid(MAKER_A, P250) };
    let create = c.w.create_tx(AnyState::KobBid(b1.clone()), b1.escrow(10 * WHOLE, 4).unwrap() as u64, MAKER_A, 0);
    c.push(&[&create]).await;
    let cov1 = c.w.cov(&create, 0);
    let fa = fill_first(&c, &c.w.order(&create, 0, b1.clone()), &cov1, 4 * WHOLE);
    c.push(&[&fa]).await;

    // mode 2: the maker's receive UTXO (threshold 500)
    let thr = 500;
    let b2 = BidState { mode: MODE_TOPUP, recv_threshold: thr, topup_tip: 3_000_000, ..bid(MAKER_A, P250) };
    let create2 = c.w.create_tx(AnyState::KobBid(b2.clone()), b2.escrow(10 * WHOLE, 4).unwrap() as u64, MAKER_A, 0);
    c.push(&[&create2]).await;
    let cov2 = c.w.cov(&create2, 0);
    let send = send_receive(&c, 6 * WHOLE, MAKER_A, thr);
    c.push(&[&send]).await;
    let r = send.tx.outputs.iter().position(|o| o.script_public_key == tok_spk(&receive(6 * WHOLE, MAKER_A, thr))).unwrap();

    let g = c.hs.ingest.lock().unwrap();
    let book = kobd::indexer::book::listed_orders(g.conn()).unwrap();
    let o1 = book.iter().find(|o| o.order.utxo.covenant_id == Some(cov1.0)).expect("the mode-1 bid is listed");
    let bc = o1.bought_custody.as_ref().expect("its bought-custody");
    assert_eq!((bc.state.amount(), bc.state.owner()), (4 * WHOLE, cov1.0));
    assert!(o1.custody_ok() && o1.strays.is_empty() && o1.receive.is_empty());
    let o2 = book.iter().find(|o| o.order.utxo.covenant_id == Some(cov2.0)).expect("the mode-2 bid is listed");
    assert!(o2.bought_custody.is_none() && o2.custody_ok());
    assert_eq!(o2.receive.len(), 1, "only the receive UTXO: {:?}", o2.receive);
    let rv = &o2.receive[0];
    assert_eq!((rv.utxo.transaction_id, rv.utxo.index), (send.tx.id, r as u32));
    assert_eq!(rv.state, receive(6 * WHOLE, MAKER_A, thr));
    assert!(kobd::matcher::book::topup_receive(o2, 2 * WHOLE).is_some());
}
