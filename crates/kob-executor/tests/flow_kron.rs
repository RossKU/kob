//! The KRON family through the indexer: every order kind created, filled, merged, cancelled and refunded
//! with the `kob-protocol` builders (protocol v3 transactions with the 46-byte KRON tokens, validated by the script
//! engine) and followed end to end: placement records with the 2-byte custody part, exact custody
//! (`id_type` 2), strays, booked exits and repeat merges, stops armed and triggered by a resting fill of the same
//! transaction, reorgs and the record log.
//! The KCC-20 twins of these flows are `flow_kinds`, `flow_ifd`, `flow_strays` and `flow_cond`.

mod common;

use common::*;
use kob_executor::hex::Hash32;
use kob_executor::indexer::reads;
use kob_executor::testkit::*;
use kob_protocol::build::*;
use kob_protocol::family::Family;
use kob_protocol::state::*;
use kob_protocol::tx::*;

// KRON fixtures: the KCC-20 fixtures with the KRON token and program, no extension commitment, KRON tips.
fn ka(a: AskState) -> AskState {
    match kron(AnyState::KobAsk(a)) {
        AnyState::KobAskKron(x) => x,
        _ => unreachable!(),
    }
}
fn kb(b: BidState) -> BidState {
    match kron(AnyState::KobBid(b)) {
        AnyState::KobBidKron(x) => x,
        _ => unreachable!(),
    }
}
fn kca(c: CondAskState) -> CondAskState {
    match kron(AnyState::KobCondAsk(c)) {
        AnyState::KobCondAskKron(x) => x,
        _ => unreachable!(),
    }
}
fn kib(i: IfdBidState) -> IfdBidState {
    match kron(AnyState::KobIfdBid(i)) {
        AnyState::KobIfdBidKron(x) => x,
        _ => unreachable!(),
    }
}
fn kia(i: IfdAskState) -> IfdAskState {
    match kron(AnyState::KobIfdAsk(i)) {
        AnyState::KobIfdAskKron(x) => x,
        _ => unreachable!(),
    }
}

/// The KRON custody state of `cov`.
fn cust(amount: i64, cov: &Hash32) -> TokenState {
    TokenState::custody(Family::Kron, amount, cov.0, EXT)
}

fn taker(c: &Ctx, units: i64) -> TokenUtxo {
    c.w.token_for(Family::Kron, TAKER, units)
}

fn find_cust(t: &SignedTx, owner: &Hash32, amount: i64) -> Option<usize> {
    find_custody_for(Family::Kron, t, owner, amount)
}

fn cancel_req(c: &Ctx, order: OrderUtxo<AnyState>, custody: Option<TokenUtxo>, strays: Vec<TokenUtxo>) -> SignedTx {
    c.w.sign(&Action::CancelOrder(CancelOrder {
        prefund: None,
        order,
        custody,
        foreign: vec![],
        strays,
        tokens: vec![],
        funding: vec![c.w.coin(MAKER_A, 10)],
        change: None,
        replace: None,
        lock_time: 0,
        records: vec![],
        fee: FeeOptions::default(),
    }))
}

fn ctx_of(c: &Ctx) -> reads::ReadCtx {
    reads::ReadCtx { node_daa: Some(c.daa()), settle_depth_daa: 100, now_unix: None }
}

#[tokio::test]
async fn kron_ask_create_fill_fill_closes_with_exact_custody() {
    let c = Ctx::new();
    let a = ka(ask(MAKER_A, P250));
    let create = c.w.create_tx(AnyState::KobAskKron(a.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    assert_eq!(c.hs.status(&cov), "open");
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobAskKron(a.clone())));
    let v = view(&c, &cov);
    assert_eq!((v.contract.as_str(), v.family), ("KobAskKron", 2));
    assert_eq!(v.token.as_deref(), Some(kob_executor::hex::encode(&TOKEN_COV_KRON).as_str()));
    assert!(v.custody.as_ref().unwrap().ok, "one live custody of amountLeft (id_type 2)");
    // the token registry saw the KRON token (no extension commitment)
    assert_eq!(c.hs.query::<i64>("SELECT COUNT(*) FROM token_events WHERE token_cov_id = ?1", [&TOKEN_COV_KRON[..]]), 1);

    // 4 of 10 whole tokens: the taker's KRON tokens are authorised by address presence (its funding input)
    let custody = c.w.token_at(&create, 1, cust(10 * WHOLE, &cov));
    let leg = Leg::Ask { order: c.w.order(&create, 0, a.clone()), custody, amount: 4 * WHOLE, t: None };
    let fill1 = c.w.sign(&Action::Batch(batch(&c.w, c.daa(), vec![leg])));
    c.push(&[&fill1]).await;
    assert_eq!(c.hs.status(&cov), "partial");
    let a6 = AskState { amount_left: 6 * WHOLE, ..a.clone() };
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobAskKron(a6.clone())));
    let cv = view(&c, &cov).custody.unwrap();
    assert!(cv.ok, "{cv:?}");
    assert_eq!(cv.expected_amount.as_deref(), Some("6000"));

    // the remaining 6 whole tokens: the order closes
    let oi = find_cov(&fill1, &cov).expect("continuation");
    let ci = find_cust(&fill1, &cov, 6 * WHOLE).expect("custody after the fill");
    let leg = Leg::Ask {
        order: c.w.order(&fill1, oi, a6),
        custody: c.w.token_at(&fill1, ci, cust(6 * WHOLE, &cov)),
        amount: 6 * WHOLE,
        t: None,
    };
    let fill2 = c.w.sign(&Action::Batch(batch(&c.w, c.daa(), vec![leg])));
    assert!(find_cov(&fill2, &cov).is_none());
    c.push(&[&fill2]).await;
    assert_eq!(c.hs.status(&cov), "filled");
    assert_eq!(c.hs.event_kinds(&cov), ["create", "fill", "fill"]);
    assert_eq!(c.hs.query::<i64>("SELECT filled_amount FROM order_state WHERE covenant_id = ?1", [&cov.0[..]]), 10 * WHOLE);
    assert_eq!(c.hs.query::<i64>("SELECT COUNT(*) FROM token_utxos WHERE owner = ?1 AND spent_block IS NULL", [&cov.0[..]]), 0);
    assert_eq!(c.hs.query::<i64>("SELECT COUNT(*) FROM token_utxos WHERE owner = ?1 AND role = 'custody'", [&cov.0[..]]), 2);
}

#[tokio::test]
async fn kron_ask_cancel_and_keeper_refund() {
    let c = Ctx::new();
    let a = ka(ask(MAKER_A, P250));
    let create = c.w.create_tx(AnyState::KobAskKron(a.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    let custody = c.w.token_at(&create, 1, cust(10 * WHOLE, &cov));
    let cancel = cancel_req(&c, c.w.order(&create, 0, AnyState::KobAskKron(a)), Some(custody), vec![]);
    c.push(&[&cancel]).await;
    assert_eq!(c.hs.status(&cov), "cancelled");
    assert_eq!(c.hs.event_kinds(&cov), ["create", "cancel"]);
    assert_eq!(c.hs.query::<i64>("SELECT COUNT(*) FROM token_utxos WHERE owner = ?1 AND spent_block IS NULL", [&cov.0[..]]), 0);

    // a keeper refunds an expired ask at its refund time
    let a = AskState { expiry_daa: c.daa() as i64 + 2_000, ..ka(ask(MAKER_A, P250 + 1)) };
    let create = c.w.create_tx(AnyState::KobAskKron(a.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    let state = AnyState::KobAskKron(a.clone());
    let due = state.refund_due(c.w.utxo(&create, 0).block_daa_score as i64).unwrap();
    let refund = c.w.sign(&Action::RefundOrder(RefundOrder {
        prefund: None,
        order: c.w.order(&create, 0, state),
        foreign: vec![],
        custody: Some(c.w.token_at(&create, 1, cust(10 * WHOLE, &cov))),
        lock_time: due as u64,
        funding: vec![c.w.coin(KEEPER, 10)],
        change: Some(pk(KEEPER)),
        fee: FeeOptions::default(),
    }));
    c.push(&[&refund]).await;
    assert_eq!(c.hs.status(&cov), "refunded");
    assert_eq!(c.hs.event_kinds(&cov), ["create", "refund"]);
}

#[tokio::test]
async fn kron_bid_escrow_fills_and_the_maker_is_delivered_tokens() {
    let c = Ctx::new();
    let b0 = kb(bid(MAKER_B, P245));
    let create = c.w.create_tx(AnyState::KobBidKron(b0.clone()), b0.escrow(10 * WHOLE, 3).unwrap() as u64, MAKER_B, 0);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobBidKron(b0.clone())));
    assert_eq!(view(&c, &cov).extension_commitment.as_deref(), Some(kob_executor::hex::encode(&[0u8; 32]).as_str()));
    let mut b = batch(&c.w, c.daa(), vec![Leg::Bid { order: c.w.order(&create, 0, b0.clone()), amount: 4 * WHOLE, t: None }]);
    b.taker_tokens = vec![taker(&c, 4 * WHOLE)];
    let fill = c.w.sign(&Action::Batch(b));
    c.push(&[&fill]).await;
    assert_eq!(c.hs.event_kinds(&cov), ["create", "fill"]);
    assert_eq!(c.hs.status(&cov), "partial");
    assert!(c.hs.tip_state(&cov).is_some(), "the bid continues with the rest of its escrow");
    let n: i64 = c.hs.query("SELECT amount FROM order_events WHERE covenant_id = ?1 AND kind = 'fill'", [&cov.0[..]]);
    assert_eq!(n, 4 * WHOLE);
}

#[tokio::test]
async fn kron_ioc_orders_are_killed_and_day_orders_record_their_deadline() {
    let c = Ctx::new();
    let now = c.daa() as i64;
    let ioc = AskState { tif: TIF_IOC, active_from: now, expiry_daa: now + 10_000, ..ka(ask(MAKER_A, P250)) };
    let create = c.w.create_tx(AnyState::KobAskKron(ioc.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    let state = AnyState::KobAskKron(ioc);
    let due = state.refund_due(c.w.utxo(&create, 0).block_daa_score as i64).unwrap();
    let kill = c.w.sign(&Action::RefundOrder(RefundOrder {
        prefund: None,
        order: c.w.order(&create, 0, state),
        foreign: vec![],
        custody: Some(c.w.token_at(&create, 1, cust(10 * WHOLE, &cov))),
        lock_time: due as u64,
        funding: vec![c.w.coin(KEEPER, 10)],
        change: Some(pk(KEEPER)),
        fee: FeeOptions::default(),
    }));
    c.push(&[&kill]).await;
    assert_eq!(c.hs.status(&cov), "killed");
    assert_eq!(c.hs.query::<i64>("SELECT remaining_amount FROM order_state WHERE covenant_id = ?1", [&cov.0[..]]), 10 * WHOLE);

    let d = kob_protocol::defaults::day_order(c.daa(), 1_790_694_000, Some(10_020));
    let a = AskState { expiry_daa: d.expiry_daa as i64, ..ka(ask(MAKER_A, P250 + 2)) };
    let mut req = c.w.create(AnyState::KobAskKron(a), CARRIER, MAKER_A, 10 * WHOLE);
    req.deadline = Some(d.deadline);
    let create = c.w.sign(&Action::CreateOrder(req));
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    assert_eq!(view_at(&c, &cov, d.deadline - 10).deadline, Some(d.deadline as i64));
    assert!(view_at(&c, &cov, d.deadline).deadline_passed);
}

#[tokio::test]
async fn kron_strays_are_flagged_never_counted_and_swept_by_the_makers_cancel() {
    let c = Ctx::new();
    let a = ka(ask(MAKER_A, P250));
    let create = c.w.create_tx(AnyState::KobAskKron(a.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    // exactly the custody amount, to be nasty
    let stray = c.w.stray_for(Family::Kron, cov, 10 * WHOLE, TAKER);
    c.push(&[&stray]).await;
    let roles: Vec<(String, i64)> = {
        let g = c.hs.ingest.lock().unwrap();
        let mut st = g.conn().prepare("SELECT role, amount FROM token_utxos WHERE owner = ?1 ORDER BY role").unwrap();
        st.query_map([&cov.0[..]], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(|r| r.unwrap()).collect()
    };
    assert_eq!(roles, [("custody".to_string(), 10 * WHOLE), ("stray".to_string(), 10 * WHOLE)]);
    let v = view(&c, &cov);
    assert!(v.custody.as_ref().unwrap().ok, "one live custody of the exact amount: the stray does not count");
    assert_eq!(v.strays.as_ref().unwrap().len(), 1);
    {
        let g = c.hs.ingest.lock().unwrap();
        let bk = reads::book(g.conn(), &ctx_of(&c), &Hash32(TOKEN_COV_KRON), 10, true).unwrap();
        let reads::BookSide::Levels(asks) = bk.asks else { panic!() };
        assert_eq!(asks[0].amount, (10 * WHOLE).to_string(), "a stray adds no liquidity");
    }
    // the maker's cancel sweeps custody and stray together
    let custody = c.w.token_at(&create, 1, cust(10 * WHOLE, &cov));
    let stray_utxo = c.w.token_at(&stray, find_cust(&stray, &cov, 10 * WHOLE).unwrap(), cust(10 * WHOLE, &cov));
    let sweep = cancel_req(&c, c.w.order(&create, 0, AnyState::KobAskKron(a)), Some(custody), vec![stray_utxo]);
    c.push(&[&sweep]).await;
    assert_eq!(c.hs.status(&cov), "cancelled");
    assert_eq!(c.hs.query::<i64>("SELECT COUNT(*) FROM token_utxos WHERE spent_block IS NULL", []), 0);
}

#[tokio::test]
async fn kron_if_done_entries_create_exits_and_repeat_cycles_merge() {
    let c = Ctx::new();
    // buy-first, repeating: fill 4 whole tokens, the exit takes profit on 3 and merges its entry, again on 1
    let ib = kib(IfdBidState { rpt_amount: 1 + 20 * WHOLE, min_fill: 3 * WHOLE, ..ifd_bid(MAKER_A, 10) });
    let create = c.w.create_tx(AnyState::KobIfdBidKron(ib.clone()), ib.escrow().unwrap() as u64, MAKER_A, 0);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    assert_eq!(
        view(&c, &cov).repeat.as_ref().map(|r| (r.role, r.rpt_amount.clone(), r.rearm_amount.clone())),
        Some(("entry", Some((1 + 20 * WHOLE).to_string()), Some((20 * WHOLE).to_string())))
    );
    let entry_daa = c.w.utxo(&create, 0).block_daa_score as i64;
    let lock = c.daa();
    let mut b =
        batch(&c.w, lock, vec![Leg::IfdBid { order: c.w.order(&create, 0, ib.clone()), amount: 4 * WHOLE, evidence: None, t: None }]);
    b.taker_tokens = vec![taker(&c, 4 * WHOLE)];
    let fill = c.w.sign(&Action::Batch(b));
    c.push(&[&fill]).await;
    let entry6 = IfdBidState { amount_left: 6 * WHOLE, rpt_amount: 1 + 16 * WHOLE, ..ib.clone() };
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobIfdBidKron(entry6.clone())));
    let (xi, exit) = find_fresh(&fill, &[cov]).unwrap();
    let booking = Booking { parent: cov.0, until: rpt_until(ib.expiry_daa, lock as i64, entry_daa).unwrap() };
    let exit4 = ib.exit_for(4 * WHOLE, Some(booking)).unwrap();
    assert_eq!(
        c.hs.tip_state(&exit),
        Some(AnyState::KobCondAskKron(exit4.clone())),
        "the exit is a KRON kind, derived from the entry, no placement record"
    );
    let rv = view(&c, &exit).repeat.expect("a booked exit");
    assert_eq!((rv.role, rv.parent.as_deref()), ("exit", Some(cov.to_hex().as_str())));
    assert_eq!(c.hs.query::<i64>("SELECT amount FROM token_utxos WHERE owner = ?1 AND role = 'custody'", [&exit.0[..]]), 4 * WHOLE);
    assert!(view(&c, &exit).custody.unwrap().ok);

    let entry_utxo = c.w.order(&fill, find_cov(&fill, &cov).unwrap(), entry6.clone());
    let ci = find_cust(&fill, &exit, 4 * WHOLE).unwrap();
    let leg = Leg::CondAsk {
        order: c.w.order(&fill, xi, exit4.clone()),
        custody: c.w.token_at(&fill, ci, cust(4 * WHOLE, &exit)),
        amount: 3 * WHOLE,
        leg: 0,
        evidence: None,
        t: None,
        merge: Some(entry_utxo),
    };
    let tp = c.w.sign(&Action::Batch(batch(&c.w, c.daa(), vec![leg])));
    c.push(&[&tp]).await;
    let entry9 = IfdBidState { amount_left: 9 * WHOLE, rpt_amount: 1 + 16 * WHOLE, ..ib.clone() };
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobIfdBidKron(entry9.clone())), "the merge re-arms 3 whole tokens");
    let exit1 = CondAskState { amount_left: WHOLE, ..exit4.clone() };
    assert_eq!(c.hs.tip_state(&exit), Some(AnyState::KobCondAskKron(exit1.clone())));
    assert_eq!(c.hs.event_kinds(&cov), ["create", "fill", "rearm"]);
    assert_eq!(c.hs.event_kinds(&exit), ["create", "fill"]);

    let entry_utxo = c.w.order(&tp, find_cov(&tp, &cov).unwrap(), entry9);
    let xi = find_cov(&tp, &exit).unwrap();
    let ci = find_cust(&tp, &exit, WHOLE).unwrap();
    let leg = Leg::CondAsk {
        order: c.w.order(&tp, xi, exit1),
        custody: c.w.token_at(&tp, ci, cust(WHOLE, &exit)),
        amount: WHOLE,
        leg: 0,
        evidence: None,
        t: None,
        merge: Some(entry_utxo),
    };
    let tp2 = c.w.sign(&Action::Batch(batch(&c.w, c.daa(), vec![leg])));
    c.push(&[&tp2]).await;
    assert_eq!(c.hs.status(&exit), "filled");
    assert_eq!(c.hs.event_kinds(&cov), ["create", "fill", "rearm", "rearm"]);
    assert_eq!(
        c.hs.tip_state(&cov),
        Some(AnyState::KobIfdBidKron(IfdBidState { amount_left: 10 * WHOLE, rpt_amount: 1 + 16 * WHOLE, ..ib }))
    );
}

#[tokio::test]
async fn kron_sell_first_entry_holds_custody_and_its_bid_exit_merges_back() {
    let c = Ctx::new();
    let ia = kia(IfdAskState { rpt_amount: 1 + 20 * WHOLE, min_fill: 3 * WHOLE, ..ifd_ask(MAKER_A) });
    let create = c.w.create_tx(AnyState::KobIfdAskKron(ia.clone()), ia.escrow(CARRIER as i64).unwrap() as u64, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    assert!(view(&c, &cov).custody.unwrap().ok);
    let entry_daa = c.w.utxo(&create, 0).block_daa_score as i64;
    let lock = c.daa();
    let custody = c.w.token_at(&create, 1, cust(10 * WHOLE, &cov));
    let leg = Leg::IfdAsk { order: c.w.order(&create, 0, ia.clone()), custody, amount: 4 * WHOLE, evidence: None, t: None };
    let fill = c.w.sign(&Action::Batch(batch(&c.w, lock, vec![leg])));
    c.push(&[&fill]).await;
    let entry6 = IfdAskState { amount_left: 6 * WHOLE, rpt_amount: 1 + 16 * WHOLE, ..ia.clone() };
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobIfdAskKron(entry6.clone())));
    let (xi, exit) = find_fresh(&fill, &[cov]).unwrap();
    let exit4 = ia
        .exit_for(4 * WHOLE, Some(Booking { parent: cov.0, until: rpt_until(ia.expiry_daa, lock as i64, entry_daa).unwrap() }))
        .unwrap();
    assert_eq!(c.hs.tip_state(&exit), Some(AnyState::KobCondBidKron(exit4.clone())));
    assert_eq!(view(&c, &cov).custody.unwrap().expected_amount.as_deref(), Some("6000"));

    // the exit buys back 3 whole tokens: they go into the entry's custody (6 -> 9 whole tokens)
    let merge = SellFirstEntry {
        entry: c.w.order(&fill, find_cov(&fill, &cov).unwrap(), entry6),
        custody: Some(c.w.token_at(&fill, find_cust(&fill, &cov, 6 * WHOLE).unwrap(), cust(6 * WHOLE, &cov))),
    };
    let leg = Leg::CondBid {
        order: c.w.order(&fill, xi, exit4.clone()),
        amount: 3 * WHOLE,
        leg: 0,
        evidence: None,
        t: None,
        merge: Some(merge),
    };
    let mut b = batch(&c.w, c.daa(), vec![leg]);
    b.taker_tokens = vec![taker(&c, 3 * WHOLE)];
    let tp = c.w.sign(&Action::Batch(b));
    c.push(&[&tp]).await;
    assert_eq!(
        c.hs.tip_state(&cov),
        Some(AnyState::KobIfdAskKron(IfdAskState { amount_left: 9 * WHOLE, rpt_amount: 1 + 16 * WHOLE, ..ia.clone() }))
    );
    let cv = view(&c, &cov).custody.unwrap();
    assert!(cv.ok, "the entry's custody holds exactly amountLeft after the merge: {cv:?}");
    assert_eq!(cv.expected_amount.as_deref(), Some("9000"));
    assert_eq!(c.hs.event_kinds(&cov), ["create", "fill", "rearm"]);
}

#[tokio::test]
async fn kron_stop_is_armed_by_a_resting_fill_of_its_own_family_then_fills_in_a_band() {
    let c = Ctx::new();
    let co = kca(cond_ask(MAKER_A)); // take-profit 3.00, stop 2.00, band 300 DAA
    let create = c.w.create_tx(AnyState::KobCondAskKron(co.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);

    // a resting KRON ask at 1.90 (at or below the stop) is filled: the batch arms the stop next to it
    let r = resting_for(&c, Family::Kron, SIDE_ASK, 190_000_000).await;
    assert_eq!(r.state.token_cov_id(), TOKEN_COV_KRON);
    let mut b = batch(&c.w, rested(&c, &r, co.min_rest_daa), vec![]);
    let k = r.add_to(&c, &mut b, 5 * WHOLE);
    b.updates = vec![update(c.w.order(&create, 0, AnyState::KobCondAskKron(co.clone())), k)];
    let arm = c.w.sign(&Action::Batch(b));
    c.push(&[&arm]).await;
    let armed = CondAskState { armed: 1, ..co.clone() };
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobCondAskKron(armed.clone())));
    assert_eq!(c.hs.event_kinds(&cov), ["create", "arm"]);
    assert_eq!(c.hs.event_detail(&cov, "arm")["evidence"]["order"], r.cov(&c).to_hex());
    let ai = find_cov(&arm, &cov).expect("the armed continuation");
    let arm_daa = c.w.utxo(&arm, ai).block_daa_score;

    // the armed stop leg fills 4 whole tokens inside its band: the continuation records the auction origin
    let custody = c.w.token_at(&create, 1, cust(10 * WHOLE, &cov));
    let t = arm_daa as i64 + 100;
    let leg = Leg::CondAsk {
        order: c.w.order(&arm, ai, armed.clone()),
        custody,
        amount: 4 * WHOLE,
        leg: 1,
        evidence: None,
        t: Some(t),
        merge: None,
    };
    let fill = c.w.sign(&Action::Batch(batch(&c.w, t as u64, vec![leg])));
    c.push(&[&fill]).await;
    assert_eq!(c.hs.status(&cov), "partial");
    let expect = CondAskState { armed: arm_daa as i64, amount_left: 6 * WHOLE, ..co.clone() };
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobCondAskKron(expect)));
    let au = view(&c, &cov).auction.expect("an armed stop leg is an auction");
    assert_eq!((au.kind, au.origin_daa), ("stop", arm_daa as i64));
}

#[tokio::test]
async fn kron_stop_entry_triggers_in_the_batch_of_its_evidence_and_creates_its_exit() {
    let c = Ctx::new();
    // a sell-first stop entry at 2.60 (limit 2.50: sells when a resting ask trades at or below 2.60)
    let ia = kia(IfdAskState { entry_stop: 260_000_000, ..ifd_ask(MAKER_A) });
    let create = c.w.create_tx(AnyState::KobIfdAskKron(ia.clone()), ia.escrow(CARRIER as i64).unwrap() as u64, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    let r = resting_for(&c, Family::Kron, SIDE_ASK, 255_000_000).await;
    let custody = c.w.token_at(&create, 1, cust(10 * WHOLE, &cov));
    let entry = Leg::IfdAsk { order: c.w.order(&create, 0, ia.clone()), custody, amount: 4 * WHOLE, evidence: None, t: None };
    let mut b = batch(&c.w, rested(&c, &r, ia.min_rest_daa), vec![entry]);
    let k = r.add_to(&c, &mut b, 2 * WHOLE);
    if let Leg::IfdAsk { evidence, .. } = &mut b.legs[0] {
        *evidence = Some(k);
    }
    let fill = c.w.sign(&Action::Batch(b));
    c.push(&[&fill]).await;
    // the entry continues armed with 6 whole tokens; the fill created its buy-back exit
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobIfdAskKron(IfdAskState { amount_left: 6 * WHOLE, armed: 1, ..ia.clone() })));
    assert_eq!(c.hs.event_kinds(&cov), ["create", "fill"]);
    let d = c.hs.event_detail(&cov, "fill");
    assert_eq!(d["evidence"]["input"], k);
    assert_eq!(d["evidence"]["order"], r.cov(&c).to_hex());
    assert_eq!(d["evidence"]["side"], SIDE_ASK);
    let (_, exit) = find_fresh(&fill, &[cov, r.cov(&c)]).expect("the exit");
    assert_eq!(c.hs.tip_state(&exit), Some(AnyState::KobCondBidKron(ia.exit_for(4 * WHOLE, None).unwrap())));
    assert_eq!(d["exit"], exit.to_hex());
}

#[tokio::test]
async fn kron_reorgs_revert_fills_creates_and_custody_exactly_and_the_log_replays() {
    let c = Ctx::new();
    let a = ka(ask(MAKER_A, P250));
    let create = c.w.create_tx(AnyState::KobAskKron(a.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    let before = c.hs.snapshot();
    let custody = c.w.token_at(&create, 1, cust(10 * WHOLE, &cov));
    let leg = Leg::Ask { order: c.w.order(&create, 0, a.clone()), custody, amount: 4 * WHOLE, t: None };
    let fill = c.w.sign(&Action::Batch(batch(&c.w, c.daa(), vec![leg])));
    c.push(&[&fill]).await;
    assert_eq!(c.hs.status(&cov), "partial");
    // the fill's block is replaced by an empty one: the state returns exactly to before the fill
    c.hs.node.reorg(1, vec![vec![]]);
    c.hs.sync().await;
    assert_eq!(c.hs.status(&cov), "open");
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobAskKron(a)));
    let g = c.hs.ingest.lock().unwrap();
    let live = g
        .conn()
        .query_row("SELECT COUNT(*) FROM token_utxos WHERE owner = ?1 AND spent_block IS NULL", [&cov.0[..]], |r| r.get::<_, i64>(0));
    assert_eq!(live.unwrap(), 1, "the original custody is live again");
    drop(g);
    assert_eq!(
        c.hs.snapshot().replace("cursor", ""),
        fresh_snapshot(&c.hs.node.chain()).replace("cursor", ""),
        "the same rows as a fresh build"
    );
    assert_ne!(before, "");
}
