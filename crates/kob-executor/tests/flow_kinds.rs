//! Flow tests on real artifacts: every order kind created, filled, cancelled and refunded with the
//! `kob-protocol` builders (v2.4 transactions validated by the script engine) and followed by the indexer.

mod common;

use common::*;
use kob_executor::hex::Hash32;
use kob_executor::testkit::*;
use kob_protocol::build::*;
use kob_protocol::state::*;
use kob_protocol::tx::*;

fn ask_leg(w: &World, create: &SignedTx, st: &AskState, custody: &TokenUtxo, amount: i64) -> Leg {
    Leg::Ask { order: w.order(create, 0, st.clone()), custody: custody.clone(), amount, t: None }
}

#[tokio::test]
async fn ask_create_fill_fill_closes() {
    let c = Ctx::new();
    let a = ask(MAKER_A, P250);
    let create = c.w.create_tx(AnyState::KobAsk(a.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    assert_eq!(c.hs.status(&cov), "open");
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobAsk(a.clone())));

    // 4 of 10 whole tokens
    let custody = c.w.token_at(&create, 1, Kcc20State::custody(10 * WHOLE, cov.0, EXT));
    let b = batch(&c.w, c.daa(), vec![ask_leg(&c.w, &create, &a, &custody, 4 * WHOLE)]);
    let fill1 = c.w.sign(&Action::Batch(b));
    c.push(&[&fill1]).await;
    assert_eq!(c.hs.status(&cov), "partial");
    let a6 = AskState { amount_left: 6 * WHOLE, ..a.clone() };
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobAsk(a6.clone())));

    // the remaining 6 whole tokens: the order closes
    let oi = find_cov(&fill1, &cov).expect("continuation");
    let ci = find_custody(&fill1, &cov, 6 * WHOLE).expect("custody after the fill");
    let order = c.w.order(&fill1, oi, a6.clone());
    let custody = c.w.token_at(&fill1, ci, Kcc20State::custody(6 * WHOLE, cov.0, EXT));
    let b = batch(&c.w, c.daa(), vec![Leg::Ask { order, custody, amount: 6 * WHOLE, t: None }]);
    let fill2 = c.w.sign(&Action::Batch(b));
    assert!(find_cov(&fill2, &cov).is_none());
    c.push(&[&fill2]).await;
    assert_eq!(c.hs.status(&cov), "filled");
    assert_eq!(c.hs.event_kinds(&cov), ["create", "fill", "fill"]);
    let filled: i64 = c.hs.query("SELECT filled_amount FROM order_state WHERE covenant_id = ?1", [&cov.0[..]]);
    assert_eq!(filled, 10 * WHOLE);
    // custody rows: created by the create, replaced by the first fill, spent by the second
    let live: i64 = c.hs.query("SELECT COUNT(*) FROM token_utxos WHERE owner = ?1 AND spent_block IS NULL", [&cov.0[..]]);
    assert_eq!(live, 0);
    let total: i64 = c.hs.query("SELECT COUNT(*) FROM token_utxos WHERE owner = ?1 AND role = 'custody'", [&cov.0[..]]);
    assert_eq!(total, 2);
    // the maker's payout is the value of the output at the input's position
    let payout: i64 = c.hs.query(
        "SELECT payout FROM order_events WHERE covenant_id = ?1 AND txid = ?2",
        rusqlite::params![&cov.0[..], &fill1.tx.id[..]],
    );
    assert_eq!(payout as u64, fill1.tx.outputs[0].value);
}

#[tokio::test]
async fn ask_cancel_returns_tokens_and_closes() {
    let c = Ctx::new();
    let a = ask(MAKER_A, P250);
    let create = c.w.create_tx(AnyState::KobAsk(a.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    let custody = c.w.token_at(&create, 1, Kcc20State::custody(10 * WHOLE, cov.0, EXT));
    let req = CancelOrder {
        prefund: None,
        order: c.w.order(&create, 0, AnyState::KobAsk(a)),
        custody: Some(custody),
        foreign: vec![],
        strays: vec![],
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
    assert_eq!(c.hs.event_kinds(&cov), ["create", "cancel"]);
    let live: i64 = c.hs.query("SELECT COUNT(*) FROM token_utxos WHERE owner = ?1 AND spent_block IS NULL", [&cov.0[..]]);
    assert_eq!(live, 0, "the custody was spent by the cancel");
}

#[tokio::test]
async fn ask_refund_after_expiry_by_a_keeper() {
    let c = Ctx::new();
    let a = AskState { expiry_daa: c.daa() as i64 + 2_000, ..ask(MAKER_A, P250) };
    let create = c.w.create_tx(AnyState::KobAsk(a.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    let custody = c.w.token_at(&create, 1, Kcc20State::custody(10 * WHOLE, cov.0, EXT));
    let state = AnyState::KobAsk(a.clone());
    let due = state.refund_due(c.w.utxo(&create, 0).block_daa_score as i64).unwrap();
    let req = RefundOrder {
        prefund: None,
        order: c.w.order(&create, 0, state),
        foreign: vec![],
        custody: Some(custody),
        lock_time: due as u64,
        funding: vec![c.w.coin(KEEPER, 10)],
        change: Some(pk(KEEPER)),
        fee: FeeOptions::default(),
    };
    let refund = c.w.sign(&Action::RefundOrder(req));
    c.push(&[&refund]).await;
    assert_eq!(c.hs.status(&cov), "refunded");
    assert_eq!(c.hs.event_kinds(&cov), ["create", "refund"]);
}

#[tokio::test]
async fn bid_escrow_fills_and_reports_estimated_amount() {
    let c = Ctx::new();
    let b0 = bid(MAKER_B, P245);
    let escrow = b0.escrow(10 * WHOLE, 3).unwrap() as u64;
    let create = c.w.create_tx(AnyState::KobBid(b0.clone()), escrow, MAKER_B, 0);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    assert_eq!(c.hs.status(&cov), "open");
    let remaining: i64 = c.hs.query("SELECT remaining_amount FROM order_state WHERE covenant_id = ?1", [&cov.0[..]]);
    assert_eq!(
        remaining,
        ((escrow as i64 - DC) as i128 * SCALE as i128 / b0.budget_rate().unwrap() as i128) as i64,
        "an upper bound: the escrow less one delivery carrier, in base units at the budget rate"
    );
    let exact: i64 = c.hs.query("SELECT amount_exact FROM order_state WHERE covenant_id = ?1", [&cov.0[..]]);
    assert_eq!(exact, 0, "a bid's quantity is its escrow, not an exact count");
    // a taker sells 4 whole tokens into it
    let mut b = batch(&c.w, c.daa(), vec![Leg::Bid { order: c.w.order(&create, 0, b0.clone()), amount: 4 * WHOLE, t: None }]);
    b.taker_tokens = vec![c.w.token(TAKER, 4 * WHOLE)];
    let fill = c.w.sign(&Action::Batch(b));
    c.push(&[&fill]).await;
    assert_eq!(c.hs.status(&cov), "partial");
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobBid(b0.clone())), "a bid's script never changes");
    let remaining: i64 = c.hs.query("SELECT remaining_amount FROM order_state WHERE covenant_id = ?1", [&cov.0[..]]);
    assert!(
        remaining < ((escrow as i64 - DC) as i128 * SCALE as i128 / b0.budget_rate().unwrap() as i128) as i64,
        "the estimate falls with the escrow"
    );
    // the buyer's tokens were delivered to the maker, not to the order id: no custody, no strays
    let n: i64 = c.hs.query("SELECT COUNT(*) FROM token_utxos", []);
    assert_eq!(n, 0);
    // the fill event reports the escrow before and after: what the buy drew never exceeds its all-in limit plus the delivery carrier
    let detail: String = c.hs.query("SELECT detail FROM order_events WHERE covenant_id = ?1 AND kind = 'fill'", [&cov.0[..]]);
    let d: serde_json::Value = serde_json::from_str(&detail).unwrap();
    let before: i64 = d["escrow_before"].as_str().unwrap().parse().unwrap();
    let after: i64 = d["escrow_after"].as_str().unwrap().parse().unwrap();
    assert_eq!(before as u64, escrow);
    let oi = find_cov(&fill, &cov).unwrap();
    assert_eq!(after as u64, fill.tx.outputs[oi].value);
    assert!(before - after <= 4 * (b0.price + b0.tip) + DC, "a buy draws at most its all-in budget plus one delivery carrier");
    // cancel the rest
    let oi = find_cov(&fill, &cov).unwrap();
    let cancel = c.w.sign(&Action::CancelOrder(CancelOrder {
        prefund: None,
        order: c.w.order(&fill, oi, AnyState::KobBid(b0)),
        custody: None,
        foreign: vec![],
        strays: vec![],
        tokens: vec![],
        funding: vec![c.w.coin(MAKER_B, 10)],
        change: None,
        replace: None,
        lock_time: 0,
        records: vec![],
        fee: FeeOptions::default(),
    }));
    c.push(&[&cancel]).await;
    assert_eq!(c.hs.status(&cov), "cancelled");
    let _ = Hash32::ZERO;
}

fn market_ask(now: i64) -> AskState {
    AskState {
        tif: TIF_IOC,
        price: P260,
        price_end: P260 - P260 / 10_000 * 300,
        slope: (P260 / 10_000 * 300 + 199) / 200,
        decay_step: 1,
        active_from: now - 100,
        expiry_daa: now + 200,
        ..ask(MAKER_A, P260)
    }
}

#[tokio::test]
async fn market_ioc_auction_partial_fill_returns_the_rest_as_a_kill() {
    let c = Ctx::new();
    let now = c.daa() as i64;
    let a = market_ask(now);
    let create = c.w.create_tx(AnyState::KobAsk(a.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    // the auction view: decaying from the touch toward the 3% bound
    let v = view(&c, &cov);
    let au = v.auction.clone().expect("an IOC market order is an auction");
    assert_eq!(au.kind, "decay");
    assert_eq!(au.start_price, P260.to_string());
    assert_eq!(au.end_price, a.price_end.to_string());
    let t = c.daa() as i64;
    let daa = c.w.utxo(&create, 0).block_daa_score as i64;
    assert_eq!(au.current_price, a.price_at(t, daa).unwrap().to_string());
    assert!(au.current_price.parse::<i64>().unwrap() < P260, "the price has started to decay");
    assert_eq!(v.kill_daa.unwrap(), AnyState::KobAsk(a.clone()).refund_due(daa).unwrap());
    assert_eq!(v.tif, Some(1));

    // a matcher fills 4 whole tokens at auction time t; the remainder returns to the maker at once
    let custody = c.w.token_at(&create, 1, Kcc20State::custody(10 * WHOLE, cov.0, EXT));
    let leg = Leg::Ask { order: c.w.order(&create, 0, a.clone()), custody, amount: 4 * WHOLE, t: Some(t) };
    let b = batch(&c.w, c.daa(), vec![leg]);
    let fill = c.w.sign(&Action::Batch(b));
    assert!(find_cov(&fill, &cov).is_none(), "an IOC order never continues");
    c.push(&[&fill]).await;
    assert_eq!(c.hs.status(&cov), "killed");
    assert_eq!(c.hs.event_kinds(&cov), ["create", "fill", "kill"]);
    let (amount, closes): (i64, i64) = {
        let g = c.hs.ingest.lock().unwrap();
        g.conn()
            .query_row("SELECT amount, closes FROM order_events WHERE covenant_id = ?1 AND kind = 'kill'", [&cov.0[..]], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap()
    };
    assert_eq!((amount, closes), (6 * WHOLE, 1), "6 unfilled whole tokens were returned");
    let filled: i64 = c.hs.query("SELECT filled_amount FROM order_state WHERE covenant_id = ?1", [&cov.0[..]]);
    assert_eq!(filled, 4 * WHOLE);
    // the fill quote is the auction bound at t
    let price: i64 = c.hs.query("SELECT price FROM order_events WHERE covenant_id = ?1 AND kind = 'fill'", [&cov.0[..]]);
    assert_eq!(price, a.price_at(t, daa).unwrap());
}

#[tokio::test]
async fn ioc_and_fok_are_killed_by_a_keeper_after_the_window() {
    let c = Ctx::new();
    let now = c.daa() as i64;
    for (tif, salt) in [(TIF_IOC, 1), (TIF_FOK, 2)] {
        let a = AskState { tif, expiry_daa: now + 10_000, active_from: now, ..ask(MAKER_A, P250 + salt) };
        let create = c.w.create_tx(AnyState::KobAsk(a.clone()), CARRIER, MAKER_A, 10 * WHOLE);
        c.push(&[&create]).await;
        let cov = c.w.cov(&create, 0);
        let daa = c.w.utxo(&create, 0).block_daa_score as i64;
        let due = AnyState::KobAsk(a.clone()).refund_due(daa).unwrap();
        assert_eq!(due, now.max(daa) + 600, "the kill window is 600 DAA");
        assert_eq!(view(&c, &cov).kill_daa, Some(due));
        let custody = c.w.token_at(&create, 1, Kcc20State::custody(10 * WHOLE, cov.0, EXT));
        let refund = c.w.sign(&Action::RefundOrder(RefundOrder {
            prefund: None,
            order: c.w.order(&create, 0, AnyState::KobAsk(a)),
            foreign: vec![],
            custody: Some(custody),
            lock_time: due as u64,
            funding: vec![c.w.coin(KEEPER, 10)],
            change: Some(pk(KEEPER)),
            fee: FeeOptions::default(),
        }));
        c.push(&[&refund]).await;
        assert_eq!(c.hs.status(&cov), "killed", "tif {tif}");
        assert_eq!(c.hs.event_kinds(&cov), ["create", "kill"]);
        let left: i64 = c.hs.query("SELECT remaining_amount FROM order_state WHERE covenant_id = ?1", [&cov.0[..]]);
        assert_eq!(left, 10 * WHOLE, "the whole amount came back");
    }
}

#[tokio::test]
async fn fok_fill_of_the_whole_order_closes_it_as_filled() {
    let c = Ctx::new();
    let now = c.daa() as i64;
    let a = AskState { tif: TIF_FOK, active_from: now, expiry_daa: now + 10_000, ..ask(MAKER_A, P250) };
    let create = c.w.create_tx(AnyState::KobAsk(a.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    let custody = c.w.token_at(&create, 1, Kcc20State::custody(10 * WHOLE, cov.0, EXT));
    let leg = Leg::Ask { order: c.w.order(&create, 0, a), custody, amount: 10 * WHOLE, t: None };
    let b = batch(&c.w, c.daa(), vec![leg]);
    let fill = c.w.sign(&Action::Batch(b));
    c.push(&[&fill]).await;
    assert_eq!(c.hs.status(&cov), "filled");
    assert_eq!(c.hs.event_kinds(&cov), ["create", "fill"]);
}

#[tokio::test]
async fn dutch_ask_and_rising_bid_expose_start_and_current_price() {
    let c = Ctx::new();
    let now = c.daa() as i64;
    let du = AskState {
        price: 300_000_000,
        slope: 1_000_000,
        price_end: 200_000_000,
        active_from: now - 500,
        decay_step: 100,
        ..ask(MAKER_A, 300_000_000)
    };
    let create = c.w.create_tx(AnyState::KobAsk(du.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    let daa = c.w.utxo(&create, 0).block_daa_score as i64;
    let t = c.daa() as i64;
    let au = view(&c, &cov).auction.expect("a decaying ask is an auction");
    assert_eq!((au.kind, au.origin_daa), ("decay", now - 500));
    assert_eq!(au.current_price, du.price_at(t, daa).unwrap().to_string());
    assert_eq!(au.current_price, (300_000_000 - 1_000_000 * ((t - (now - 500)) / 100)).to_string());
    assert!(!au.complete);
    // the book quotes the current price and keeps the limit as `price`
    {
        let g = c.hs.ingest.lock().unwrap();
        let ctx = kob_executor::indexer::reads::ReadCtx { node_daa: Some(c.daa()), settle_depth_daa: 100, now_unix: None };
        let bk = kob_executor::indexer::reads::book(g.conn(), &ctx, &Hash32(TOKEN_COV), 10, false).unwrap();
        let kob_executor::indexer::reads::BookSide::Orders(asks) = bk.asks else { panic!() };
        assert_eq!(asks.len(), 1);
        assert_eq!(asks[0].price, "300000000");
        assert_eq!(asks[0].quote.as_deref(), Some(au.current_price.as_str()));
    }
    // a rising bid
    let ri = BidState {
        price: 200_000_000,
        slope: 500_000,
        price_end: 250_000_000,
        active_from: now - 300,
        decay_step: 100,
        ..bid(MAKER_B, 200_000_000)
    };
    let value = ri.escrow(4 * WHOLE, 2).unwrap() as u64;
    let create_b = c.w.create_tx(AnyState::KobBid(ri.clone()), value, MAKER_B, 0);
    c.push(&[&create_b]).await;
    let covb = c.w.cov(&create_b, 0);
    let au = view(&c, &covb).auction.expect("a rising bid is an auction");
    assert_eq!(au.kind, "rise");
    let daab = c.w.utxo(&create_b, 0).block_daa_score as i64;
    assert_eq!(au.current_price, ri.price_at(c.daa() as i64, daab).unwrap().to_string());
    assert!(au.current_price.parse::<i64>().unwrap() > 200_000_000);
}

#[tokio::test]
async fn day_order_deadline_is_recorded_and_flagged() {
    let c = Ctx::new();
    let d = kob_protocol::defaults::day_order(c.daa(), 1_790_694_000, Some(10_020));
    let a = AskState { expiry_daa: d.expiry_daa as i64, ..ask(MAKER_A, P250) };
    let mut req = c.w.create(AnyState::KobAsk(a), CARRIER, MAKER_A, 10 * WHOLE);
    req.deadline = Some(d.deadline);
    let create = c.w.sign(&Action::CreateOrder(req));
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    let v = view_at(&c, &cov, d.deadline - 10);
    assert_eq!(v.deadline, Some(d.deadline as i64));
    assert!(!v.deadline_passed);
    let v = view_at(&c, &cov, d.deadline);
    assert!(v.deadline_passed, "conforming matchers stop at the deadline (UTC clock)");
    assert_eq!(v.status, "open", "on chain the order lives until its expiry");
    let ev: String = c.hs.query("SELECT detail FROM order_events WHERE covenant_id = ?1 AND kind = 'create'", [&cov.0[..]]);
    assert!(ev.contains(&d.deadline.to_string()));
}
