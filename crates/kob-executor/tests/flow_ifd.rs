//! If-done orders (IFD / IFO) on both sides, exits booked without a new placement record, the repeat cycles
//! (`rptAmount`, `rptUntil`, entry <-> exit merges, empty repeating entries, close) and a stop entry triggered by the resting
//! fill of its own transaction.

mod common;

use common::*;
use kob_executor::hex::Hash32;
use kob_executor::testkit::*;
use kob_protocol::build::*;
use kob_protocol::state::*;
use kob_protocol::tx::*;

fn cancel_all(c: &Ctx, orders: Vec<CancelItem>) -> SignedTx {
    c.w.sign(&Action::CancelPosition(CancelPosition {
        orders,
        funding: vec![c.w.coin(MAKER_A, 10)],
        change: None,
        lock_time: 0,
        token_carrier: None,
        records: vec![],
        fee: FeeOptions::default(),
    }))
}

#[tokio::test]
async fn ifd_buy_first_entry_creates_exits_that_take_profit() {
    let c = Ctx::new();
    let ib = IfdBidState { min_fill: 3 * WHOLE, ..ifd_bid(MAKER_A, 10) };
    let create = c.w.create_tx(AnyState::KobIfdBid(ib.clone()), ib.escrow().unwrap() as u64, MAKER_A, 0);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    assert_eq!(c.hs.status(&cov), "open");

    // a taker sells 4 whole tokens into the entry: the bought tokens go into a fresh exit's custody
    let mut b = batch(
        &c.w,
        c.daa(),
        vec![Leg::IfdBid { order: c.w.order(&create, 0, ib.clone()), amount: 4 * WHOLE, evidence: None, t: None }],
    );
    b.taker_tokens = vec![c.w.token(TAKER, 4 * WHOLE)];
    let fill = c.w.sign(&Action::Batch(b));
    c.push(&[&fill]).await;
    let entry6 = IfdBidState { amount_left: 6 * WHOLE, ..ib.clone() };
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobIfdBid(entry6.clone())));
    assert_eq!(c.hs.status(&cov), "partial");
    let (xi, exit) = find_fresh(&fill, &[cov]).expect("the exit output");
    let exit_state = ib.exit_for(4 * WHOLE, None).unwrap();
    assert_eq!(c.hs.tip_state(&exit), Some(AnyState::KobCondAsk(exit_state.clone())), "derived from the entry, no placement record");
    let parent: Vec<u8> = c.hs.query("SELECT parent FROM orders WHERE covenant_id = ?1", [&exit.0[..]]);
    assert_eq!(parent, cov.0.to_vec());
    let v = view(&c, &cov);
    assert_eq!(v.children.as_deref(), Some(&[exit.to_hex()][..]));
    assert_eq!(c.hs.event_kinds(&exit), ["create"]);
    // the exit's custody is exactly the bought amount
    let amount: i64 = c.hs.query("SELECT amount FROM token_utxos WHERE owner = ?1 AND role = 'custody'", [&exit.0[..]]);
    assert_eq!(amount, 4 * WHOLE);
    let cv = view(&c, &exit).custody.expect("ask-side exit has a custody check");
    assert!(cv.ok, "{cv:?}");
    let entry_fill_detail: String =
        c.hs.query("SELECT detail FROM order_events WHERE covenant_id = ?1 AND kind = 'fill'", [&cov.0[..]]);
    assert!(entry_fill_detail.contains(&exit.to_hex()), "the fill event names its exit");

    // the exit takes profit on all 4 whole tokens (leg 0): it closes; the entry is untouched
    let ci = find_custody(&fill, &exit, 4 * WHOLE).expect("exit custody output");
    let leg = Leg::CondAsk {
        order: c.w.order(&fill, xi, exit_state.clone()),
        custody: c.w.token_at(&fill, ci, Kcc20State::custody(4 * WHOLE, exit.0, EXT)),
        amount: 4 * WHOLE,
        leg: 0,
        evidence: None,
        t: None,
        merge: None,
    };
    let tp = c.w.sign(&Action::Batch(batch(&c.w, c.daa(), vec![leg])));
    c.push(&[&tp]).await;
    assert_eq!(c.hs.status(&exit), "filled");
    assert_eq!(c.hs.event_kinds(&exit), ["create", "fill"]);
    assert_eq!(c.hs.status(&cov), "partial");

    // the remaining 6 whole tokens (>= minFill): the entry fills completely and closes
    let oi = find_cov(&fill, &cov).unwrap();
    let mut b =
        batch(&c.w, c.daa(), vec![Leg::IfdBid { order: c.w.order(&fill, oi, entry6), amount: 6 * WHOLE, evidence: None, t: None }]);
    b.taker_tokens = vec![c.w.token(TAKER, 6 * WHOLE)];
    let last = c.w.sign(&Action::Batch(b));
    assert!(find_cov(&last, &cov).is_none());
    c.push(&[&last]).await;
    assert_eq!(c.hs.status(&cov), "filled");
    assert_eq!(c.hs.event_kinds(&cov), ["create", "fill", "fill"]);
    let exits: i64 = c.hs.query("SELECT COUNT(*) FROM orders WHERE parent = ?1", [&cov.0[..]]);
    assert_eq!(exits, 2, "one exit per entry fill");
}

#[tokio::test]
async fn ifd_sell_first_entry_holds_custody_and_creates_bid_exits() {
    let c = Ctx::new();
    let ia = IfdAskState { min_fill: 3 * WHOLE, ..ifd_ask(MAKER_A) };
    let create = c.w.create_tx(AnyState::KobIfdAsk(ia.clone()), ia.escrow(CARRIER as i64).unwrap() as u64, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    assert!(view(&c, &cov).custody.unwrap().ok);
    let custody = c.w.token_at(&create, 1, Kcc20State::custody(10 * WHOLE, cov.0, EXT));
    let leg = Leg::IfdAsk { order: c.w.order(&create, 0, ia.clone()), custody, amount: 4 * WHOLE, evidence: None, t: None };
    let fill = c.w.sign(&Action::Batch(batch(&c.w, c.daa(), vec![leg])));
    c.push(&[&fill]).await;
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobIfdAsk(IfdAskState { amount_left: 6 * WHOLE, ..ia.clone() })));
    let (_, exit) = find_fresh(&fill, &[cov]).expect("the exit");
    assert_eq!(c.hs.tip_state(&exit), Some(AnyState::KobCondBid(ia.exit_for(4 * WHOLE, None).unwrap())));
    // the entry's custody now holds exactly the remaining 6 whole tokens
    let cv = view(&c, &cov).custody.unwrap();
    assert!(cv.ok, "{cv:?}");
    assert_eq!(cv.expected_amount.as_deref(), Some("6000"));
    // the bid-side exit owns no tokens
    assert_eq!(c.hs.query::<i64>("SELECT COUNT(*) FROM token_utxos WHERE owner = ?1", [&exit.0[..]]), 0);
}

#[tokio::test]
async fn repeat_buy_first_cycle_merges_take_profits_into_the_entry() {
    let c = Ctx::new();
    let ib = IfdBidState { rpt_amount: 1 + 20 * WHOLE, min_fill: 3 * WHOLE, ..ifd_bid(MAKER_A, 10) };
    let create = c.w.create_tx(AnyState::KobIfdBid(ib.clone()), ib.escrow().unwrap() as u64, MAKER_A, 0);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    assert_eq!(
        view(&c, &cov).repeat.as_ref().map(|r| (r.role, r.rpt_amount.clone(), r.rearm_amount.clone())),
        Some(("entry", Some((1 + 20 * WHOLE).to_string()), Some((20 * WHOLE).to_string())))
    );

    // fill 4 whole tokens: a BOOKED exit (parent, rptPrice, rptUntil) and the entry continues with rptAmount - 4 whole tokens
    let entry_daa = c.w.utxo(&create, 0).block_daa_score as i64;
    let lock = c.daa();
    let mut b =
        batch(&c.w, lock, vec![Leg::IfdBid { order: c.w.order(&create, 0, ib.clone()), amount: 4 * WHOLE, evidence: None, t: None }]);
    b.taker_tokens = vec![c.w.token(TAKER, 4 * WHOLE)];
    let fill = c.w.sign(&Action::Batch(b));
    c.push(&[&fill]).await;
    let entry6 = IfdBidState { amount_left: 6 * WHOLE, rpt_amount: 1 + 16 * WHOLE, ..ib.clone() };
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobIfdBid(entry6.clone())));
    let (xi, exit) = find_fresh(&fill, &[cov]).unwrap();
    let booking = Booking { parent: cov.0, until: rpt_until(ib.expiry_daa, entry_daa).unwrap() };
    let exit4 = ib.exit_for(4 * WHOLE, Some(booking)).unwrap();
    assert_eq!(c.hs.tip_state(&exit), Some(AnyState::KobCondAsk(exit4.clone())));
    let rv = view(&c, &exit).repeat.expect("a booked exit");
    assert_eq!((rv.role, rv.parent.as_deref()), ("exit", Some(cov.to_hex().as_str())));
    assert_eq!(rv.rpt_until.as_deref(), Some(booking.until.to_string().as_str()));
    assert!(c.hs.event_kinds(&cov).contains(&"fill".to_string()));

    // the exit takes profit on 3 whole tokens: it MUST merge its entry, which re-arms them at the same terms
    let entry_utxo = c.w.order(&fill, find_cov(&fill, &cov).unwrap(), entry6.clone());
    let ci = find_custody(&fill, &exit, 4 * WHOLE).unwrap();
    let leg = Leg::CondAsk {
        order: c.w.order(&fill, xi, exit4.clone()),
        custody: c.w.token_at(&fill, ci, Kcc20State::custody(4 * WHOLE, exit.0, EXT)),
        amount: 3 * WHOLE,
        leg: 0,
        evidence: None,
        t: None,
        merge: Some(entry_utxo),
    };
    let tp = c.w.sign(&Action::Batch(batch(&c.w, c.daa(), vec![leg])));
    c.push(&[&tp]).await;
    let entry9 = IfdBidState { amount_left: 9 * WHOLE, rpt_amount: 1 + 16 * WHOLE, ..ib.clone() };
    assert_eq!(
        c.hs.tip_state(&cov),
        Some(AnyState::KobIfdBid(entry9.clone())),
        "the merge re-arms 3 whole tokens, rptAmount is unchanged"
    );
    let exit1 = CondAskState { amount_left: WHOLE, ..exit4.clone() };
    assert_eq!(c.hs.tip_state(&exit), Some(AnyState::KobCondAsk(exit1.clone())));
    assert_eq!(c.hs.event_kinds(&cov), ["create", "fill", "rearm"]);
    assert_eq!(c.hs.event_kinds(&exit), ["create", "fill"]);
    let d: String = c.hs.query("SELECT detail FROM order_events WHERE covenant_id = ?1 AND kind = 'fill'", [&exit.0[..]]);
    assert!(d.contains("merged_into") && d.contains(&cov.to_hex()), "{d}");
    let d: String = c.hs.query("SELECT detail FROM order_events WHERE covenant_id = ?1 AND kind = 'rearm'", [&cov.0[..]]);
    assert!(d.contains(&exit.to_hex()), "the re-arm names the exit that took profit: {d}");
    let (amount, filled): (i64, i64) = {
        let g = c.hs.ingest.lock().unwrap();
        g.conn()
            .query_row("SELECT amount FROM order_events WHERE covenant_id = ?1 AND kind = 'rearm'", [&cov.0[..]], |r| {
                Ok((r.get(0)?, 0))
            })
            .unwrap()
    };
    assert_eq!((amount, filled), (3 * WHOLE, 0));
    // the entry's own fills exclude the re-armed amount
    assert_eq!(c.hs.query::<i64>("SELECT filled_amount FROM order_state WHERE covenant_id = ?1", [&cov.0[..]]), 4 * WHOLE);

    // the exit's last whole token sells out and merges again: the entry gets its carriers back and 10 whole tokens
    let entry_utxo = c.w.order(&tp, find_cov(&tp, &cov).unwrap(), entry9.clone());
    let xi = find_cov(&tp, &exit).unwrap();
    let ci = find_custody(&tp, &exit, WHOLE).unwrap();
    let leg = Leg::CondAsk {
        order: c.w.order(&tp, xi, exit1.clone()),
        custody: c.w.token_at(&tp, ci, Kcc20State::custody(WHOLE, exit.0, EXT)),
        amount: WHOLE,
        leg: 0,
        evidence: None,
        t: None,
        merge: Some(entry_utxo),
    };
    let tp2 = c.w.sign(&Action::Batch(batch(&c.w, c.daa(), vec![leg])));
    c.push(&[&tp2]).await;
    assert_eq!(c.hs.status(&exit), "filled");
    assert_eq!(
        c.hs.tip_state(&cov),
        Some(AnyState::KobIfdBid(IfdBidState { amount_left: 10 * WHOLE, rpt_amount: 1 + 16 * WHOLE, ..ib.clone() }))
    );
    assert_eq!(c.hs.event_kinds(&cov), ["create", "fill", "rearm", "rearm"]);
}

#[tokio::test]
async fn cancel_all_of_a_repeat_position_cancels_entry_and_exits_in_one_transaction() {
    let c = Ctx::new();
    let ib = IfdBidState { rpt_amount: 1 + 20 * WHOLE, min_fill: 3 * WHOLE, ..ifd_bid(MAKER_A, 10) };
    let create = c.w.create_tx(AnyState::KobIfdBid(ib.clone()), ib.escrow().unwrap() as u64, MAKER_A, 0);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    let entry_daa = c.w.utxo(&create, 0).block_daa_score as i64;
    let lock = c.daa();
    let mut b =
        batch(&c.w, lock, vec![Leg::IfdBid { order: c.w.order(&create, 0, ib.clone()), amount: 4 * WHOLE, evidence: None, t: None }]);
    b.taker_tokens = vec![c.w.token(TAKER, 4 * WHOLE)];
    let fill = c.w.sign(&Action::Batch(b));
    c.push(&[&fill]).await;
    let (xi, exit) = find_fresh(&fill, &[cov]).unwrap();
    let entry6 = IfdBidState { amount_left: 6 * WHOLE, rpt_amount: 1 + 16 * WHOLE, ..ib.clone() };
    let exit4 = ib.exit_for(4 * WHOLE, Some(Booking { parent: cov.0, until: rpt_until(ib.expiry_daa, entry_daa).unwrap() })).unwrap();
    let ci = find_custody(&fill, &exit, 4 * WHOLE).unwrap();
    let cancel = cancel_all(
        &c,
        vec![
            CancelItem {
                prefund: None,
                order: c.w.order(&fill, find_cov(&fill, &cov).unwrap(), AnyState::KobIfdBid(entry6)),
                custody: None,
                strays: vec![],
            },
            CancelItem {
                prefund: None,
                order: c.w.order(&fill, xi, AnyState::KobCondAsk(exit4)),
                custody: Some(c.w.token_at(&fill, ci, Kcc20State::custody(4 * WHOLE, exit.0, EXT))),
                strays: vec![],
            },
        ],
    );
    c.push(&[&cancel]).await;
    assert_eq!(c.hs.status(&cov), "cancelled");
    assert_eq!(c.hs.status(&exit), "cancelled");
    assert_eq!(
        c.hs.query::<i64>("SELECT COUNT(*) FROM token_utxos WHERE spent_block IS NULL", []),
        0,
        "the exit's custody was spent by the cancel"
    );
}

#[tokio::test]
async fn repeat_sell_first_cycle_merges_the_bought_back_amount_into_the_entry_custody() {
    let c = Ctx::new();
    let ia = IfdAskState { rpt_amount: 1 + 20 * WHOLE, min_fill: 3 * WHOLE, ..ifd_ask(MAKER_A) };
    let create = c.w.create_tx(AnyState::KobIfdAsk(ia.clone()), ia.escrow(CARRIER as i64).unwrap() as u64, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    let entry_daa = c.w.utxo(&create, 0).block_daa_score as i64;
    let lock = c.daa();
    let custody = c.w.token_at(&create, 1, Kcc20State::custody(10 * WHOLE, cov.0, EXT));
    let leg = Leg::IfdAsk { order: c.w.order(&create, 0, ia.clone()), custody, amount: 4 * WHOLE, evidence: None, t: None };
    let fill = c.w.sign(&Action::Batch(batch(&c.w, lock, vec![leg])));
    c.push(&[&fill]).await;
    let entry6 = IfdAskState { amount_left: 6 * WHOLE, rpt_amount: 1 + 16 * WHOLE, ..ia.clone() };
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobIfdAsk(entry6.clone())));
    let (xi, exit) = find_fresh(&fill, &[cov]).unwrap();
    let exit4 = ia.exit_for(4 * WHOLE, Some(Booking { parent: cov.0, until: rpt_until(ia.expiry_daa, entry_daa).unwrap() })).unwrap();
    assert_eq!(c.hs.tip_state(&exit), Some(AnyState::KobCondBid(exit4.clone())));

    // the exit buys back 3 whole tokens at its limit: they go into the entry's custody (6 -> 9 whole tokens)
    let entry_utxo = SellFirstEntry {
        entry: c.w.order(&fill, find_cov(&fill, &cov).unwrap(), entry6.clone()),
        custody: Some(c.w.token_at(&fill, find_custody(&fill, &cov, 6 * WHOLE).unwrap(), Kcc20State::custody(6 * WHOLE, cov.0, EXT))),
    };
    let leg = Leg::CondBid {
        order: c.w.order(&fill, xi, exit4.clone()),
        amount: 3 * WHOLE,
        leg: 0,
        evidence: None,
        t: None,
        merge: Some(entry_utxo),
    };
    let mut b = batch(&c.w, c.daa(), vec![leg]);
    b.taker_tokens = vec![c.w.token(TAKER, 3 * WHOLE)];
    let tp = c.w.sign(&Action::Batch(b));
    c.push(&[&tp]).await;
    assert_eq!(
        c.hs.tip_state(&cov),
        Some(AnyState::KobIfdAsk(IfdAskState { amount_left: 9 * WHOLE, rpt_amount: 1 + 16 * WHOLE, ..ia.clone() }))
    );
    assert_eq!(c.hs.tip_state(&exit), Some(AnyState::KobCondBid(CondBidState { amount_left: WHOLE, ..exit4 })));
    let cv = view(&c, &cov).custody.unwrap();
    assert!(cv.ok, "the entry's custody holds exactly amountLeft after the merge: {cv:?}");
    assert_eq!(cv.expected_amount.as_deref(), Some("9000"));
    assert_eq!(c.hs.event_kinds(&cov), ["create", "fill", "rearm"]);
}

#[tokio::test]
async fn empty_repeating_entry_waits_without_custody_gets_a_new_one_on_merge_and_closes() {
    let c = Ctx::new();
    let ia = IfdAskState { rpt_amount: 1 + 20 * WHOLE, min_fill: 3 * WHOLE, expiry_daa: 1_050_000, ..ifd_ask(MAKER_A) };
    let create = c.w.create_tx(AnyState::KobIfdAsk(ia.clone()), ia.escrow(CARRIER as i64).unwrap() as u64, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    let entry_daa = c.w.utxo(&create, 0).block_daa_score as i64;
    let lock = c.daa();
    let custody = c.w.token_at(&create, 1, Kcc20State::custody(10 * WHOLE, cov.0, EXT));
    // all 10 whole tokens at once: the repeating entry does not terminate, it waits for its exit
    let leg = Leg::IfdAsk { order: c.w.order(&create, 0, ia.clone()), custody, amount: 10 * WHOLE, evidence: None, t: None };
    let fill = c.w.sign(&Action::Batch(batch(&c.w, lock, vec![leg])));
    c.push(&[&fill]).await;
    let waiting = IfdAskState { amount_left: 0, rpt_amount: 1 + 10 * WHOLE, ..ia.clone() };
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobIfdAsk(waiting.clone())));
    assert_ne!(c.hs.status(&cov), "filled", "a repeating entry outlives its last fill");
    assert_eq!(
        c.hs.query::<i64>("SELECT COUNT(*) FROM token_utxos WHERE owner = ?1 AND spent_block IS NULL", [&cov.0[..]]),
        0,
        "no custody while empty"
    );
    let cv = view(&c, &cov).custody.unwrap();
    assert!(cv.ok && cv.utxo.is_none(), "an empty repeating entry needs no custody: {cv:?}");
    // not liquidity: an empty entry is not in the book
    {
        let g = c.hs.ingest.lock().unwrap();
        let ctx = kob_executor::indexer::reads::ReadCtx { node_daa: Some(c.daa()), settle_depth_daa: 100, now_unix: None };
        let bk = kob_executor::indexer::reads::book(g.conn(), &ctx, &Hash32(TOKEN_COV), 10, false).unwrap();
        let kob_executor::indexer::reads::BookSide::Orders(asks) = bk.asks else { panic!() };
        assert!(asks.is_empty());
    }

    // the exit buys back everything: a NEW custody is created for the entry
    let (xi, exit) = find_fresh(&fill, &[cov]).unwrap();
    let exit10 =
        ia.exit_for(10 * WHOLE, Some(Booking { parent: cov.0, until: rpt_until(ia.expiry_daa, entry_daa).unwrap() })).unwrap();
    let merge = SellFirstEntry { entry: c.w.order(&fill, find_cov(&fill, &cov).unwrap(), waiting.clone()), custody: None };
    let leg =
        Leg::CondBid { order: c.w.order(&fill, xi, exit10), amount: 10 * WHOLE, leg: 0, evidence: None, t: None, merge: Some(merge) };
    let mut b = batch(&c.w, c.daa(), vec![leg]);
    b.taker_tokens = vec![c.w.token(TAKER, 10 * WHOLE)];
    let tp = c.w.sign(&Action::Batch(b));
    c.push(&[&tp]).await;
    assert_eq!(
        c.hs.tip_state(&cov),
        Some(AnyState::KobIfdAsk(IfdAskState { amount_left: 10 * WHOLE, rpt_amount: 1 + 10 * WHOLE, ..ia.clone() }))
    );
    assert_eq!(c.hs.status(&exit), "filled");
    let cv = view(&c, &cov).custody.unwrap();
    assert!(cv.ok && cv.utxo.is_some(), "the merge created the entry's custody: {cv:?}");

    // a second cycle empties it again, then anyone may close the empty entry after its expiry
    let lock2 = c.daa();
    let custody = c.w.token_at(&tp, find_custody(&tp, &cov, 10 * WHOLE).unwrap(), Kcc20State::custody(10 * WHOLE, cov.0, EXT));
    let st = IfdAskState { amount_left: 10 * WHOLE, rpt_amount: 1 + 10 * WHOLE, ..ia.clone() };
    let leg = Leg::IfdAsk {
        order: c.w.order(&tp, find_cov(&tp, &cov).unwrap(), st.clone()),
        custody,
        amount: 10 * WHOLE,
        evidence: None,
        t: None,
    };
    let fill2 = c.w.sign(&Action::Batch(batch(&c.w, lock2, vec![leg])));
    c.push(&[&fill2]).await;
    let empty = IfdAskState { amount_left: 0, rpt_amount: 1, ..ia.clone() };
    assert_eq!(
        c.hs.tip_state(&cov),
        Some(AnyState::KobIfdAsk(empty.clone())),
        "rptAmount 1 = no re-arms left (rptAmount - 1 base units)"
    );
    let due = AnyState::KobIfdAsk(empty.clone())
        .refund_due(c.w.utxo(&fill2, find_cov(&fill2, &cov).unwrap()).block_daa_score as i64)
        .unwrap();
    let close = c.w.sign(&Action::RefundOrder(RefundOrder {
        prefund: None,
        order: c.w.order(&fill2, find_cov(&fill2, &cov).unwrap(), AnyState::KobIfdAsk(empty)),
        foreign: vec![],
        custody: None,
        lock_time: due as u64,
        funding: vec![c.w.coin(KEEPER, 10)],
        change: Some(pk(KEEPER)),
        fee: FeeOptions::default(),
    }));
    c.push(&[&close]).await;
    assert_eq!(c.hs.status(&cov), "refunded");
    let d: String = c.hs.query("SELECT detail FROM order_events WHERE covenant_id = ?1 AND kind = 'refund'", [&cov.0[..]]);
    assert!(d.contains("\"close\""), "the empty entry was closed with `close()`: {d}");
}

#[tokio::test]
async fn a_buy_stop_entry_triggers_in_the_batch_of_its_evidence_and_creates_its_exit() {
    let c = Ctx::new();
    // a buy-first stop entry at 2.55 (limit 2.60): it buys when a resting bid trades at or above 2.55
    let ib = IfdBidState { entry_stop: 255_000_000, ..ifd_bid(MAKER_A, 10) };
    let create = c.w.create_tx(AnyState::KobIfdBid(ib.clone()), ib.escrow().unwrap() as u64, MAKER_A, 0);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    // unarmed, a stop entry is a stop order: in no book (its 2.60 limit is not resting liquidity before its trigger)
    assert_eq!(bids_in_book(&c), Vec::<String>::new());
    // a database an earlier build wrote listed it: the start-up migration takes it out
    {
        let g = c.hs.ingest.lock().unwrap();
        g.conn().execute("UPDATE orders SET in_book = 1 WHERE covenant_id = ?1", [&cov.0[..]]).unwrap();
        assert_eq!(kob_executor::indexer::db::backfill_stop_entry_books(g.conn()).unwrap(), 1);
        assert_eq!(kob_executor::indexer::db::backfill_stop_entry_books(g.conn()).unwrap(), 0, "idempotent");
    }
    assert_eq!(bids_in_book(&c), Vec::<String>::new());
    let r = resting(&c, SIDE_BID, 256_000_000).await;
    let entry = Leg::IfdBid { order: c.w.order(&create, 0, ib.clone()), amount: 4 * WHOLE, evidence: None, t: None };
    let mut b = batch(&c.w, rested(&c, &r, ib.min_rest_daa), vec![entry]);
    b.taker_tokens = vec![c.w.token(TAKER, 4 * WHOLE)];
    let k = r.add_to(&c, &mut b, 2 * WHOLE);
    if let Leg::IfdBid { evidence, .. } = &mut b.legs[0] {
        *evidence = Some(k);
    }
    let fill = c.w.sign(&Action::Batch(b));
    c.push(&[&fill]).await;
    // the entry bought 4 whole tokens at its trigger and continues armed; the exit holds them
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobIfdBid(IfdBidState { amount_left: 6 * WHOLE, armed: 1, ..ib.clone() })));
    // armed, it auctions in its book
    assert!(bids_in_book(&c).contains(&cov.to_hex()), "{:?}", bids_in_book(&c));
    assert_eq!(c.hs.event_kinds(&cov), ["create", "fill"]);
    let d = c.hs.event_detail(&cov, "fill");
    assert_eq!(d["evidence"]["input"], k);
    assert_eq!(d["evidence"]["order"], r.cov(&c).to_hex());
    assert_eq!(d["evidence"]["side"], SIDE_BID);
    assert_eq!(d["evidence"]["price"], "256000000");
    let price: i64 = c.hs.query("SELECT price FROM order_events WHERE covenant_id = ?1 AND kind = 'fill'", [&cov.0[..]]);
    assert_eq!(price, ib.price_at(true, 0, 0).unwrap());
    assert_eq!(price, 255_000_000, "a banded stop entry fills at its trigger");
    let (_, exit) = find_fresh(&fill, &[cov, r.cov(&c)]).expect("the exit");
    assert_eq!(c.hs.tip_state(&exit), Some(AnyState::KobCondAsk(ib.exit_for(4 * WHOLE, None).unwrap())));
    assert_eq!(c.hs.event_kinds(&r.cov(&c)), ["create", "fill"]);
}

/// Covenant ids (hex) of the bids in the token's book.
fn bids_in_book(c: &Ctx) -> Vec<String> {
    let g = c.hs.ingest.lock().unwrap();
    let ctx = kob_executor::indexer::reads::ReadCtx { node_daa: Some(c.daa()), settle_depth_daa: 100, now_unix: None };
    let bk = kob_executor::indexer::reads::book(g.conn(), &ctx, &Hash32(TOKEN_COV), 50, false).unwrap();
    let kob_executor::indexer::reads::BookSide::Orders(bids) = bk.bids else { panic!("per-order book") };
    bids.into_iter().map(|o| o.covenant_id).collect()
}
