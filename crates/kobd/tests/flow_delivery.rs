//! Bid delivery modes followed by the indexer (`docs/design/bid-delivery-spec.md`): a mode-1 bid's bought-custody is the order's
//! (never a stray), its fills, claim, refund and ending fill hand the tokens to the maker, a mode-2 top-up is the fill's delivery,
//! and a delivery's KAS is not taken for a carrier the escrow released. The transactions are the builders' mode-0 ones patched to
//! the delivery shapes (`common/delivery.rs`).

mod common;
#[path = "common/delivery.rs"]
mod delivery;

use common::*;
use delivery::*;
use kob_protocol::build::{Action, CancelOrder};
use kob_protocol::state::*;
use kob_protocol::tx::*;
use kobd::hex::Hash32;
use kobd::testkit::*;
use serde_json::Value;

fn order_id(cov: &Hash32) -> &[u8] {
    &cov.0[..]
}

/// The detail of the newest event of `kind` of order `cov`.
fn detail(c: &Ctx, cov: &Hash32, kind: &str) -> Value {
    let d: String = c.hs.query(
        "SELECT kob_dtext(detail) FROM order_events WHERE order_id = (SELECT id FROM orders WHERE covenant_id = ?1) AND kind = ?2 ORDER BY id DESC LIMIT 1",
        rusqlite::params![order_id(cov), kind],
    );
    serde_json::from_str(&d).unwrap()
}

/// `(role, amount, spent)` of every token row owned by order `cov`, oldest first.
fn token_rows(c: &Ctx, cov: &Hash32) -> Vec<(String, i64, bool)> {
    let g = c.hs.ingest.lock().unwrap();
    let mut st = g
        .conn()
        .prepare(
            "SELECT role, amount, spent_block IS NOT NULL FROM token_utxos WHERE order_id = (SELECT id FROM orders WHERE covenant_id = ?1) \
             ORDER BY created_block, idx",
        )
        .unwrap();
    st.query_map([order_id(cov)], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap().map(|r| r.unwrap()).collect()
}

/// The live holding of key `k` at output `j` of `t`: `(amount, role, borrow scheme)`.
fn holding(c: &Ctx, t: &SignedTx, j: usize, k: u8) -> (i64, String, i64) {
    let (amount, role, owner, state): (i64, String, Vec<u8>, Vec<u8>) = {
        let g = c.hs.ingest.lock().unwrap();
        g.conn()
            .query_row(
                "SELECT h.amount, h.role, w.h, kob_hstate(h.state, h.family, w.h, h.owner_kind, h.amount) FROM token_holdings h \
                 JOIN ids w ON w.id = h.owner_id WHERE h.txid = ?1 AND h.idx = ?2 AND h.spent_block IS NULL",
                rusqlite::params![&t.tx.id[..], j as i64],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap()
    };
    assert_eq!(owner, pk(k).to_vec(), "the maker's");
    let s = Kcc20State::decode(&state).unwrap();
    (amount, role, s.borrow_scheme as i64)
}

/// `(income, own_trade)` of the executor row of `t`.
fn executor(c: &Ctx, t: &SignedTx) -> (Option<i64>, i64) {
    c.hs.ingest
        .lock()
        .unwrap()
        .conn()
        .query_row("SELECT income, own_trade FROM executor_txs WHERE txid = ?1", [&t.tx.id[..]], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
}

fn mode1(b: BidState) -> BidState {
    BidState { mode: MODE_ACCUMULATE, ..b }
}

/// A mode-1 bid of MAKER_A placed with `escrow`, indexed.
async fn place(c: &Ctx, b: &BidState, escrow: u64) -> (SignedTx, Hash32) {
    let create = c.w.create_tx(AnyState::KobBid(b.clone()), escrow, MAKER_A, 0);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    assert_eq!(c.hs.status(&cov), "open");
    (create, cov)
}

#[tokio::test]
async fn a_mode_1_bid_keeps_what_it_buys_in_its_bought_custody_and_the_claim_and_the_refund_release_it() {
    let c = Ctx::new();
    let b0 = BidState { expiry_daa: c.daa() as i64 + 5_000, ..mode1(bid(MAKER_A, P250)) };
    let escrow = b0.escrow(10 * WHOLE, 4).unwrap() as u64;
    let (create, cov) = place(&c, &b0, escrow).await;
    let v = view(&c, &cov);
    assert_eq!((v.delivery, v.delivery_mode, v.bought.as_deref()), (Some("order"), Some(1), Some("0")));
    assert_eq!((v.topup_tip.as_deref(), v.recv_threshold.as_deref()), (Some("0"), Some("0")));
    assert!(v.custody.is_none() && v.bought_custody.as_ref().is_some_and(|b| b.ok && b.utxo.is_none()));

    // fill A: 4 whole tokens into a new bought-custody
    let fa = fill_first(&c, &c.w.order(&create, 0, b0.clone()), &cov, 4 * WHOLE);
    c.push(&[&fa]).await;
    let b4 = BidState { bought: 4 * WHOLE, ..b0.clone() };
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobBid(b4.clone())), "the continuation's `bought` is proven");
    assert_eq!(token_rows(&c, &cov), [("bought".to_string(), 4 * WHOLE, false)], "the order's, not a stray");
    let d = detail(&c, &cov, "fill");
    assert_eq!((d["delivery"].as_str(), d["delivery_mode"].as_i64(), d["bought"].as_str()), (Some("custody"), Some(1), Some("4000")));
    assert!(d.get("delivery_kas_in").is_none(), "the first fill takes no token input");
    let v = view(&c, &cov);
    assert_eq!((v.status.as_str(), v.bought.as_deref(), v.filled_amount.as_str()), ("partial", Some("4000"), "4000"));
    let bc = v.bought_custody.clone().expect("bought-custody view");
    assert!(bc.ok && bc.utxo.as_ref().is_some_and(|u| u.role == "bought" && u.amount == "4000"));
    assert_eq!(v.custody.as_ref().map(|c| c.ok), Some(true), "a bid's custody is its bought-custody");
    assert!(v.strays.as_ref().is_some_and(|s| s.is_empty()));
    assert!(c.check_income(&fa));

    // fill B: 3 more into it, from the custody (no carrier released: the delivery rides on the custody's KAS)
    let ca = find_custody(&fa, &cov, 4 * WHOLE).unwrap();
    let custody_a = c.w.token_at(&fa, ca, bought(4 * WHOLE, &cov));
    let oa = find_cov(&fa, &cov).unwrap();
    let fb = fill_into(&c, &c.w.order(&fa, oa, b4.clone()), &cov, &custody_a, 3 * WHOLE);
    c.push(&[&fb]).await;
    let b7 = BidState { bought: 7 * WHOLE, ..b0.clone() };
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobBid(b7.clone())));
    assert_eq!(token_rows(&c, &cov), [("bought".to_string(), 4 * WHOLE, true), ("bought".to_string(), 7 * WHOLE, false)]);
    let d = detail(&c, &cov, "fill");
    assert_eq!((d["delivery"].as_str(), d["bought"].as_str()), (Some("custody"), Some("7000")));
    assert_eq!(d["delivery_kas_in"].as_str(), Some(fa.tx.outputs[ca].value.to_string().as_str()));
    // the escrow put only the rounding on the delivery: what the buy drew is within its all-in limit, with no carrier
    let num = |k: &str| d[k].as_str().unwrap().parse::<i64>().unwrap();
    let put = num("delivery_value") - num("delivery_kas_in");
    assert!(num("escrow_before") - num("escrow_after") - put <= 3 * (b0.price + b0.tip));
    assert_eq!(num("escrow_before") - num("escrow_after"), b0.used(3 * WHOLE).unwrap(), "no carrier left the escrow");
    assert!(c.check_income(&fb), "the custody's KAS in and the delivery out are the order's");
    let rem: i64 = c.hs.query(
        "SELECT remaining_amount FROM order_state WHERE order_id = (SELECT id FROM orders WHERE covenant_id = ?1)",
        [order_id(&cov)],
    );
    let ob = find_cov(&fb, &cov).unwrap();
    assert_eq!(rem, b7.buying_power(fb.tx.outputs[ob].value as i64 + b7.delivery_carrier), "no carrier reserved for the next fill");

    // claim (CL1): the maker takes the 7 and the order goes on with bought = 0
    let cb = find_custody(&fb, &cov, 7 * WHOLE).unwrap();
    let custody_b = c.w.token_at(&fb, cb, bought(7 * WHOLE, &cov));
    let b_claimed = AnyState::KobBid(b0.clone());
    let claim =
        cancel_sweeping(&c, &c.w.order(&fb, ob, AnyState::KobBid(b7.clone())), &cov, Some(&custody_b), Some(b_claimed.clone()));
    c.push(&[&claim]).await;
    assert_eq!(c.hs.event_kinds(&cov).last().map(|s| s.as_str()), Some("claim"));
    assert_eq!(c.hs.tip_state(&cov), Some(b_claimed), "the claim's continuation is proven");
    let amount: i64 = c.hs.query(
        "SELECT amount FROM order_events WHERE order_id = (SELECT id FROM orders WHERE covenant_id = ?1) AND kind = 'claim'",
        [order_id(&cov)],
    );
    assert_eq!(amount, 7 * WHOLE);
    assert_eq!(detail(&c, &cov, "claim")["released"].as_str(), Some("7000"));
    let v = view(&c, &cov);
    assert_eq!((v.status.as_str(), v.bought.as_deref(), v.filled_amount.as_str()), ("partial", Some("0"), "7000"));
    assert!(v.custody.is_none() && v.bought_custody.as_ref().is_some_and(|b| b.ok));
    let rel = claim.tx.outputs.iter().position(|o| o.script_public_key == tok_spk(&plain(7 * WHOLE, MAKER_A))).expect("release");
    assert_eq!(holding(&c, &claim, rel, MAKER_A), (7 * WHOLE, "owned".to_string(), 0), "the maker's plain UTXO");
    let ev = {
        let g = c.hs.ingest.lock().unwrap();
        let ctx = kobd::indexer::reads::ReadCtx { node_daa: Some(c.daa()), settle_depth_daa: 100, now_unix: None };
        kobd::indexer::reads::order_events(g.conn(), &ctx, &cov, 100, None).unwrap()
    };
    let e = ev.items.iter().find(|e| e.kind == "claim").unwrap();
    assert_eq!((e.amount.as_deref(), e.txid.as_str(), e.closes), (Some("7000"), kobd::hex::encode(&claim.tx.id).as_str(), false));

    // fill C after the claim: a new bought-custody
    let oc = find_cov(&claim, &cov).unwrap();
    let fc = fill_first(&c, &c.w.order(&claim, oc, b0.clone()), &cov, 2 * WHOLE);
    c.push(&[&fc]).await;
    let b2 = BidState { bought: 2 * WHOLE, ..b0.clone() };
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobBid(b2.clone())));
    assert_eq!(token_rows(&c, &cov).last(), Some(&("bought".to_string(), 2 * WHOLE, false)));

    // a keeper's refund after the expiry releases the custody at its own index
    let o2 = find_cov(&fc, &cov).unwrap();
    let c2 = c.w.token_at(&fc, find_custody(&fc, &cov, 2 * WHOLE).unwrap(), bought(2 * WHOLE, &cov));
    let order = c.w.order(&fc, o2, AnyState::KobBid(b2.clone()));
    let due = AnyState::KobBid(b2).refund_due(order.utxo.block_daa_score as i64).unwrap() as u64;
    let refund = refund_releasing(&c, &order, &c2, due);
    c.push(&[&refund]).await;
    assert_eq!(c.hs.status(&cov), "refunded");
    let d = detail(&c, &cov, "refund");
    assert_eq!(d["released"].as_str(), Some("2000"));
    let at = d["release_output"].as_u64().unwrap() as usize;
    assert_eq!(holding(&c, &refund, at, MAKER_A).0, 2 * WHOLE);
    assert!(token_rows(&c, &cov).iter().all(|r| r.2), "nothing of the order is left live");
    assert!(c.check_income(&refund), "the release at the custody's index is pinned by the covenant, not the keeper's");
    assert_eq!(executor(&c, &refund).1, 0);
}

#[tokio::test]
async fn a_mode_1_bids_ending_fill_releases_bought_and_the_fill_to_the_maker_and_its_unspent_kas_to_the_refund_slot() {
    let c = Ctx::new();
    // the escrow buys 2 then 3 whole tokens; what is left after them (a carrier and half a KAS over the reserve of a carrier) is
    // below the next minimum fill, so the second fill ends the order. A mode-0 fill would release a carrier and leave half a KAS,
    // below REFUND_MIN, on the delivery; with nothing released the whole of it is the maker's refund
    let b0 = BidState { reserve: DC, ..mode1(bid(MAKER_A, P250)) };
    let x = DC + KAS as i64 / 2;
    let escrow = (b0.used(2 * WHOLE).unwrap() + DC + b0.used(3 * WHOLE).unwrap() + x) as u64;
    let (create, cov) = place(&c, &b0, escrow).await;
    let fa = fill_first(&c, &c.w.order(&create, 0, b0.clone()), &cov, 2 * WHOLE);
    c.push(&[&fa]).await;
    let b2 = BidState { bought: 2 * WHOLE, ..b0.clone() };
    let custody = c.w.token_at(&fa, find_custody(&fa, &cov, 2 * WHOLE).unwrap(), bought(2 * WHOLE, &cov));
    let fb = fill_into(&c, &c.w.order(&fa, find_cov(&fa, &cov).unwrap(), b2), &cov, &custody, 3 * WHOLE);
    assert!(find_cov(&fb, &cov).is_none(), "the fill ends the order");
    c.push(&[&fb]).await;
    assert_eq!(c.hs.status(&cov), "filled");
    let d = detail(&c, &cov, "fill");
    assert_eq!((d["delivery"].as_str(), d["released"].as_str()), (Some("custody"), Some("5000")));
    assert!(d.get("bought").is_none());
    // the delivery is the maker's plain UTXO of bought + n, carrying the custody's KAS
    let j = fb.tx.outputs.iter().position(|o| o.script_public_key == tok_spk(&plain(5 * WHOLE, MAKER_A))).unwrap();
    assert_eq!(j, 0, "at the order's input index");
    assert_eq!(holding(&c, &fb, j, MAKER_A), (5 * WHOLE, "owned".to_string(), 0));
    assert_eq!(d["delivery_kas_in"].as_str(), Some(custody.utxo.amount.to_string().as_str()));
    // no carrier was released, so all the unspent KAS, a carrier more than mode 0 leaves, is the maker's refund
    let refund: i64 = d["refund_value"].as_str().expect("the refund slot").parse().unwrap();
    assert_eq!(refund, fb.tx.outputs[fb.tx.outputs.len() - 1].value as i64);
    assert!(refund >= x, "{refund}");
    assert!(token_rows(&c, &cov).iter().all(|r| r.0 == "bought" && r.2));
    assert!(c.check_income(&fb));
}

#[tokio::test]
async fn a_mode_2_top_up_is_the_fills_delivery_and_the_receive_utxo_is_the_makers() {
    let c = Ctx::new();
    let (thr, tip) = (500, 3_000_000);
    let b0 = BidState { mode: MODE_TOPUP, recv_threshold: thr, topup_tip: tip, ..bid(MAKER_A, P250) };
    let escrow = b0.escrow(10 * WHOLE, 4).unwrap() as u64;
    let (create, cov) = place(&c, &b0, escrow).await;
    let v = view(&c, &cov);
    assert_eq!((v.delivery, v.topup_tip.as_deref(), v.recv_threshold.as_deref()), (Some("topup"), Some("3000000"), Some("500")));
    assert!(v.bought_custody.is_none(), "mode 2 holds nothing");
    // the maker's receive UTXO of 6 whole tokens
    let send = send_receive(&c, 6 * WHOLE, MAKER_A, thr);
    c.push(&[&send]).await;
    let r = send.tx.outputs.iter().position(|o| o.script_public_key == tok_spk(&receive(6 * WHOLE, MAKER_A, thr))).unwrap();
    assert_eq!(holding(&c, &send, r, MAKER_A), (6 * WHOLE, "owned".to_string(), 1));
    let recv = c.w.token_at(&send, r, receive(6 * WHOLE, MAKER_A, thr));

    let fill = fill_topup(&c, &c.w.order(&create, 0, b0.clone()), &cov, &recv, 4 * WHOLE);
    c.push(&[&fill]).await;
    assert_eq!(c.hs.status(&cov), "partial");
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobBid(b0.clone())));
    let d = detail(&c, &cov, "fill");
    assert_eq!((d["delivery"].as_str(), d["delivery_mode"].as_i64()), (Some("topup"), Some(2)));
    assert_eq!((d["topup_tip"].as_str(), d["receive_before"].as_str()), (Some("3000000"), Some("6000")));
    assert_eq!(d["delivery_kas_in"].as_str(), Some(recv.utxo.amount.to_string().as_str()));
    let num = |k: &str| d[k].as_str().unwrap().parse::<i64>().unwrap();
    assert_eq!(num("escrow_before") - num("escrow_after"), b0.used(4 * WHOLE).unwrap() + tip, "the escrow released topupTip");
    // the topped-up receive UTXO is the maker's, at the order's input index; no token row of the order
    assert_eq!(holding(&c, &fill, 0, MAKER_A), (10 * WHOLE, "owned".to_string(), 1));
    assert!(token_rows(&c, &cov).is_empty());
    // the receive UTXO in is the maker's, its KAS the order's delivery: the income is what the filler kept (the taker sells its own
    // tokens, an own trade as for any taker)
    let (income, own) = executor(&c, &fill);
    assert_eq!(own, 1);
    assert_eq!(income, Some(income_of(&fill, TAKER)));
}

#[tokio::test]
async fn a_look_alike_of_the_bought_custody_is_a_stray() {
    let c = Ctx::new();
    let b0 = mode1(bid(MAKER_A, P250));
    let escrow = b0.escrow(10 * WHOLE, 4).unwrap() as u64;
    let (create, cov) = place(&c, &b0, escrow).await;
    // a fill whose state says bought = 4 but whose outputs owned by the order hold 3 and 1: neither is the bought-custody
    let order = c.w.order(&create, 0, b0.clone());
    let mut built = bid_fill(&c, &order, 4 * WHOLE);
    retarget(&mut built, &plain(4 * WHOLE, MAKER_A), &bought(3 * WHOLE, &cov));
    add_token_output(&mut built, &bought(WHOLE, &cov), CARRIER);
    take_change(&mut built, CARRIER as i64 + KAS as i64);
    set_cont(&mut built, &cov, &AnyState::KobBid(BidState { bought: 4 * WHOLE, ..b0.clone() }), 0);
    let fill = c.w.resign(built, false);
    c.push(&[&fill]).await;
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobBid(BidState { bought: 4 * WHOLE, ..b0.clone() })));
    assert_eq!(token_rows(&c, &cov), [("stray".to_string(), 3 * WHOLE, false), ("stray".to_string(), WHOLE, false)]);
    let v = view(&c, &cov);
    assert_eq!(v.bought.as_deref(), Some("4000"));
    assert!(v.bought_custody.as_ref().is_some_and(|b| !b.ok && b.utxo.is_none()), "the check fails closed");
    assert_eq!(v.strays.as_ref().map(|s| s.len()), Some(2));

    // and a token sent to the order id with the exact amount later is a stray too: only the fill that writes `bought` makes one
    let c2 = Ctx::new();
    let (create2, cov2) = place(&c2, &b0, escrow).await;
    let fa = fill_first(&c2, &c2.w.order(&create2, 0, b0.clone()), &cov2, 4 * WHOLE);
    c2.push(&[&fa]).await;
    let stray = c2.w.stray(cov2, 4 * WHOLE, TAKER);
    c2.push(&[&stray]).await;
    assert_eq!(token_rows(&c2, &cov2), [("bought".to_string(), 4 * WHOLE, false), ("stray".to_string(), 4 * WHOLE, false)]);
    let v = view(&c2, &cov2);
    assert!(v.bought_custody.as_ref().is_some_and(|b| b.ok));
    assert_eq!(v.strays.as_ref().map(|s| s.len()), Some(1));
    // the matcher's book: the bought-custody is served apart, never with the strays
    let g = c2.hs.ingest.lock().unwrap();
    let book = kobd::indexer::book::listed_orders(g.conn()).unwrap();
    let o = book.iter().find(|o| o.order.utxo.covenant_id == Some(cov2.0)).unwrap();
    assert_eq!(o.strays.len(), 1);
    let bc = kobd::indexer::book::bought_custodies(g.conn()).unwrap();
    assert_eq!(bc.get(&cov2.0).map(|t| t.state.amount()), Some(4 * WHOLE));
}

#[tokio::test]
async fn a_cancel_that_leaves_the_bought_custody_is_no_claim() {
    let c = Ctx::new();
    let b0 = mode1(bid(MAKER_A, P250));
    let escrow = b0.escrow(10 * WHOLE, 4).unwrap() as u64;
    let (create, cov) = place(&c, &b0, escrow).await;
    let fa = fill_first(&c, &c.w.order(&create, 0, b0.clone()), &cov, 4 * WHOLE);
    c.push(&[&fa]).await;
    let b4 = BidState { bought: 4 * WHOLE, ..b0.clone() };
    // the maker's cancel continues the order with bought = 0 but does not spend the custody: not a claim, the state is not proven
    let oa = find_cov(&fa, &cov).unwrap();
    let cancel = cancel_sweeping(&c, &c.w.order(&fa, oa, AnyState::KobBid(b4)), &cov, None, Some(AnyState::KobBid(b0.clone())));
    c.push(&[&cancel]).await;
    assert_eq!(c.hs.event_kinds(&cov).last().map(|s| s.as_str()), Some("cancel"));
    assert_eq!(c.hs.tip_state(&cov), None, "an unproven continuation");
    assert_eq!(token_rows(&c, &cov), [("bought".to_string(), 4 * WHOLE, false)], "the custody stays the order's");
    // the maker's cancel that takes the custody with the escrow (CL4) releases it
    let c2 = Ctx::new();
    let (create2, cov2) = place(&c2, &b0, escrow).await;
    let fa = fill_first(&c2, &c2.w.order(&create2, 0, b0.clone()), &cov2, 4 * WHOLE);
    c2.push(&[&fa]).await;
    let custody = c2.w.token_at(&fa, find_custody(&fa, &cov2, 4 * WHOLE).unwrap(), bought(4 * WHOLE, &cov2));
    let b4 = BidState { bought: 4 * WHOLE, ..b0.clone() };
    let cl4 = cancel_sweeping(&c2, &c2.w.order(&fa, find_cov(&fa, &cov2).unwrap(), AnyState::KobBid(b4)), &cov2, Some(&custody), None);
    c2.push(&[&cl4]).await;
    assert_eq!(c2.hs.status(&cov2), "cancelled");
    assert_eq!(detail(&c2, &cov2, "cancel")["released"].as_str(), Some("4000"));
    assert!(token_rows(&c2, &cov2).iter().all(|r| r.2));
}

#[tokio::test]
async fn a_mode_0_bid_reads_wallet_and_holds_no_bought_custody() {
    let c = Ctx::new();
    let b0 = bid(MAKER_A, P250);
    let escrow = b0.escrow(10 * WHOLE, 4).unwrap() as u64;
    let (create, cov) = place(&c, &b0, escrow).await;
    let fill = c.w.resign(bid_fill(&c, &c.w.order(&create, 0, b0.clone()), 4 * WHOLE), false);
    c.push(&[&fill]).await;
    assert!(c.check_income(&fill));
    let d = detail(&c, &cov, "fill");
    assert_eq!((d["delivery"].as_str(), d["delivery_mode"].as_i64()), (Some("new"), Some(0)));
    assert!(d.get("bought").is_none() && d.get("released").is_none());
    let v = view(&c, &cov);
    assert_eq!((v.delivery, v.bought.as_deref()), (Some("wallet"), Some("0")));
    assert!(v.custody.is_none() && v.bought_custody.is_none());
    // an ask delivers nothing to its maker: no delivery fields
    let a = c.w.create_tx(AnyState::KobAsk(ask(MAKER_B, P260)), CARRIER, MAKER_B, 10 * WHOLE);
    c.push(&[&a]).await;
    let va = view(&c, &c.w.cov(&a, 0));
    assert!(va.delivery.is_none() && va.bought.is_none() && va.topup_tip.is_none());
    let json = serde_json::to_value(&va).unwrap();
    assert!(json.get("delivery").is_none() && json.get("bought_custody").is_none(), "absent, not null");
}

/// L2: a fill that names an exact look-alike of the bought-custody (a stray sent to the order: the contract takes either) in place
/// of the live one leaves that one unspent. The fill's new custody is the order's, the old one a stray: the order stays listed with
/// its bought-custody, a reorg gives the roles back, and the maker's cancel sweeps the stray with the custody.
#[tokio::test]
async fn a_fill_naming_a_look_alike_of_the_bought_custody_keeps_the_order_and_strays_the_old_one() {
    let c = Ctx::new();
    let b0 = mode1(bid(MAKER_A, P250));
    let escrow = b0.escrow(10 * WHOLE, 4).unwrap() as u64;
    let (create, cov) = place(&c, &b0, escrow).await;
    let fa = fill_first(&c, &c.w.order(&create, 0, b0.clone()), &cov, 4 * WHOLE);
    c.push(&[&fa]).await;
    let b4 = BidState { bought: 4 * WHOLE, ..b0.clone() };
    let stray = c.w.stray(cov, 4 * WHOLE, TAKER);
    c.push(&[&stray]).await;
    assert_eq!(token_rows(&c, &cov), [("bought".to_string(), 4 * WHOLE, false), ("stray".to_string(), 4 * WHOLE, false)]);
    let before = c.hs.snapshot();

    let look = c.w.token_at(&stray, find_custody(&stray, &cov, 4 * WHOLE).unwrap(), bought(4 * WHOLE, &cov));
    let fb = fill_into(&c, &c.w.order(&fa, find_cov(&fa, &cov).unwrap(), b4.clone()), &cov, &look, 3 * WHOLE);
    c.push(&[&fb]).await;
    let b7 = BidState { bought: 7 * WHOLE, ..b0.clone() };
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobBid(b7.clone())));
    let rows =
        [("stray".to_string(), 4 * WHOLE, false), ("stray".to_string(), 4 * WHOLE, true), ("bought".to_string(), 7 * WHOLE, false)];
    assert_eq!(token_rows(&c, &cov), rows, "the new custody is the order's, the one left behind a stray");
    let v = view(&c, &cov);
    assert_eq!((v.status.as_str(), v.bought.as_deref()), ("partial", Some("7000")));
    assert!(v.bought_custody.as_ref().is_some_and(|b| b.ok && b.utxo.as_ref().is_some_and(|u| u.amount == "7000")));
    assert_eq!(v.strays.as_ref().map(|s| s.len()), Some(1));
    {
        let g = c.hs.ingest.lock().unwrap();
        let book = kobd::indexer::book::listed_orders(g.conn()).unwrap();
        assert!(book.iter().any(|o| o.order.utxo.covenant_id == Some(cov.0)), "still listed: the matcher fills it");
        let bc = kobd::indexer::book::bought_custodies(g.conn()).unwrap();
        assert_eq!(bc.get(&cov.0).map(|t| t.state.amount()), Some(7 * WHOLE));
    }

    // a reorg of the fill's block gives the old custody its role back, every row as it was
    c.hs.node.reorg(1, vec![vec![]]);
    c.hs.sync().await;
    let rows_of = |s: &str| s.lines().filter(|l| !l.starts_with("state ")).map(str::to_string).collect::<Vec<_>>();
    assert_eq!(rows_of(&c.hs.snapshot()), rows_of(&before));
    assert_eq!(token_rows(&c, &cov), [("bought".to_string(), 4 * WHOLE, false), ("stray".to_string(), 4 * WHOLE, false)]);
    c.w.include(&c.hs.node, &[&fb]);
    c.hs.sync().await;
    assert_eq!(token_rows(&c, &cov), rows);

    // the maker's cancel takes the custody and the stray: nothing of the order is left
    let ob = find_cov(&fb, &cov).unwrap();
    let custody = c.w.token_at(&fb, find_custody(&fb, &cov, 7 * WHOLE).unwrap(), bought(7 * WHOLE, &cov));
    let old = c.w.token_at(&fa, find_custody(&fa, &cov, 4 * WHOLE).unwrap(), bought(4 * WHOLE, &cov));
    let req = CancelOrder {
        retired: None,
        prefund: None,
        order: c.w.order(&fb, ob, AnyState::KobBid(b7)),
        custody: None,
        foreign: vec![],
        strays: vec![custody, old],
        tokens: vec![],
        funding: vec![c.w.coin(MAKER_A, 10)],
        change: None,
        replace: None,
        lock_time: 0,
        records: vec![],
        fee: FeeOptions::default(),
    };
    let cancel = c.w.sign(&Action::CancelOrder(req));
    c.push(&[&cancel]).await;
    assert_eq!(c.hs.status(&cov), "cancelled");
    assert!(token_rows(&c, &cov).iter().all(|r| r.2), "nothing of the order is left live");
}
