//! Pair orders (`KobPair`, `KobCondPair`, `KobIfdPair`; one template each for both sides and both token families) end to
//! end through the indexer: created with the `kob-protocol` builders, indexed (placement record, exact custodies of either
//! token, lineage, strays of both tokens), shown in the pair book, filled with `build_batch` legs (every transaction
//! validated in the rusty-kaspa engine) through the KAS books (route), against opposite pair orders (netting) or from the
//! filler's inventory, and followed through partial fills, IOC returns, closes, refunds, cancels, if-done exits, repeat merges
//! and arms in both evidence modes. The founder price rule: a pair fill records volume (`pair_fills`, its event without a
//! price) and never a KAS trade, candle or last price; its KAS-book counterparties record their own trades.

mod common;

use common::pair::*;
use common::*;
use kob_executor::hex::Hash32;
use kob_executor::indexer::book::snapshot;
use kob_executor::indexer::{market, pairs, reads};
use kob_executor::matcher::book::ListedOrder;
use kob_executor::testkit::*;
use kob_protocol::build::*;
use kob_protocol::family::Family;
use kob_protocol::state::*;
use kob_protocol::tx::*;

/// The KAS ask price of the route's token B and the evidence (sompi per whole token).
const P200: i64 = 200_000_000;

fn rctx(c: &Ctx) -> reads::ReadCtx {
    reads::ReadCtx { node_daa: Some(c.daa()), settle_depth_daa: 100, now_unix: Some(1_790_000_000) }
}

fn book(c: &Ctx) -> Vec<ListedOrder> {
    let g = c.hs.ingest.lock().unwrap();
    snapshot(g.conn(), Some(pk(MATCHER))).unwrap().orders
}

/// Let the orders age past every freshness rule.
async fn age(c: &Ctx, daa: usize) {
    c.hs.node.push_empty(daa);
    c.hs.sync().await;
}

/// Index of the input spending outpoint `op`.
fn input_of(t: &SignedTx, op: (&[u8; 32], u32)) -> usize {
    t.tx.inputs.iter().position(|i| (&i.transaction_id, i.index) == op).expect("input")
}

fn events(c: &Ctx, cov: &Hash32) -> Vec<(String, Option<i64>, Option<i64>, serde_json::Value)> {
    let g = c.hs.ingest.lock().unwrap();
    let mut st = g.conn().prepare("SELECT kind, amount, price, detail FROM order_events WHERE covenant_id = ?1 ORDER BY id").unwrap();
    st.query_map([&cov.0[..]], |r| {
        let d: Option<String> = r.get(3)?;
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, d.map(|d| serde_json::from_str(&d).unwrap()).unwrap_or_default()))
    })
    .unwrap()
    .map(|r| r.unwrap())
    .collect()
}

/// The last fill event of an order: (amount, price column, `detail.pair`).
fn last_fill(c: &Ctx, cov: &Hash32) -> (Option<i64>, Option<i64>, serde_json::Value) {
    let e = events(c, cov).into_iter().rev().find(|e| e.0 == "fill").expect("a fill event");
    (e.1, e.2, e.3["pair"].clone())
}

/// `(counterparty, price_source)` of the `pair_fills` rows of an order, oldest first.
fn pair_fill_rows(c: &Ctx, cov: &Hash32) -> Vec<(String, String, i64, Option<i64>)> {
    let g = c.hs.ingest.lock().unwrap();
    let mut st = g
        .conn()
        .prepare("SELECT counterparty, price_source, amount_a, amount_b FROM pair_fills WHERE covenant_id = ?1 ORDER BY id")
        .unwrap();
    st.query_map([&cov.0[..]], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap().map(|r| r.unwrap()).collect()
}

/// KAS trades of a token (the market views).
fn kas_trades(c: &Ctx, token: &Hash32) -> usize {
    let g = c.hs.ingest.lock().unwrap();
    market::trades(g.conn(), &rctx(c), token, None, None, 50).unwrap().items.len()
}

fn live_custodies(c: &Ctx, cov: &Hash32) -> Vec<(Hash32, i64)> {
    let g = c.hs.ingest.lock().unwrap();
    let mut st = g
        .conn()
        .prepare("SELECT token_cov_id, amount FROM token_utxos WHERE owner = ?1 AND role = 'custody' AND spent_block IS NULL ORDER BY amount")
        .unwrap();
    st.query_map([&cov.0[..]], |r| Ok((Hash32::from_slice(&r.get::<_, Vec<u8>>(0)?).unwrap(), r.get(1)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
}

/// Build, sign and engine-validate a batch; an engine refusal names the compute-budget role of every input.
fn sign_batch(c: &Ctx, b: Batch) -> SignedTx {
    let built = build(&Action::Batch(b)).unwrap_or_else(|e| panic!("build: {e}"));
    let sigs = sign_locally(&built, &keys()).unwrap();
    let signed = finalize(&built, &sigs, FinalizeOptions::default()).unwrap();
    if let Err(e) = kob_protocol::verify::validate_signed(&signed) {
        panic!("engine rejected the transaction: {e}; input roles: {:?}", built.roles);
    }
    c.w.finish(&built)
}

#[tokio::test]
async fn a_pair_ask_is_listed_quoted_routed_through_the_kas_books_and_sold_out_from_inventory() {
    for p in [AB, BA] {
        let c = Ctx::new();
        let (ta, tb) = (p.a(), p.b());
        let x = pair_order(p, MAKER_A, true, RATE, 10);
        let mut feed = c.hs.events.subscribe();
        let placed = place_pair(&c, p, AnyState::KobPair(x.clone()), PV, MAKER_A).await;
        let cov = placed.cov;
        // the change feed names both tokens: `book:<A>` and `book:<B>` subscribers refetch the pair book
        let (mut toks, mut ords) = (vec![], vec![]);
        while let Ok(ev) = feed.try_recv() {
            toks.extend(ev.tokens.iter().copied());
            ords.extend(ev.orders.iter().copied());
        }
        assert!(toks.contains(&ta) && toks.contains(&tb), "{p:?}: {toks:?}");
        assert!(ords.contains(&cov));

        // listed with exact custody; the order view shows the pair, the side and the price in B
        assert_eq!(c.hs.status(&cov), "open");
        let v = view(&c, &cov);
        assert!(v.listed, "{p:?}: {:?}", v.unlisted_reason);
        assert_eq!((v.side, v.in_book, v.price.clone(), v.quote.clone()), (1, false, None, None));
        assert!(v.custody.as_ref().unwrap().ok);
        let pv = v.pair.clone().expect("pair view");
        assert_eq!((pv.base.clone(), pv.quote.clone(), pv.side), (ta.to_hex(), tb.to_hex(), "ask"));
        assert_eq!((pv.price.as_deref(), pv.price_num.as_deref(), pv.price_den.as_deref()), (Some("1000"), Some("1"), Some("1")));
        assert_eq!(pv.custodies.len(), 1);
        assert_eq!(
            (pv.custodies[0].token.clone(), pv.custodies[0].expected_amount.as_str(), pv.custodies[0].ok),
            (ta.to_hex(), "10000", Some(true))
        );
        // no KAS book holds it; the orders list finds it under either token
        {
            let g = c.hs.ingest.lock().unwrap();
            let kas = reads::book(g.conn(), &rctx(&c), &ta, 50, false).unwrap();
            assert!(!serde_json::to_string(&kas).unwrap().contains(&cov.to_hex()));
            for t in [ta, tb] {
                let f = reads::OrderFilter { token: Some(t), ..Default::default() };
                let page = reads::orders(g.conn(), &rctx(&c), &f, 50, None).unwrap();
                assert!(page.items.iter().any(|o| o.covenant_id == cov.to_hex()));
            }
            assert_eq!(
                pairs::pairs(g.conn(), &rctx(&c), Some(&tb)).unwrap(),
                vec![pairs::PairSummary {
                    base: ta.to_hex(),
                    quote: tb.to_hex(),
                    direct_asks: 1,
                    direct_bids: 0,
                    entry_asks: 0,
                    entry_bids: 0,
                    conditionals: 0
                }]
            );
        }

        // the KAS orders of the route: a bid of A at 2.60, an ask of B at 2.00
        let bid_a = resting_for(&c, p.fa, SIDE_BID, P260).await;
        let ask_b = resting_for(&c, p.fb, SIDE_ASK, P200).await;
        age(&c, 20).await;
        {
            let g = c.hs.ingest.lock().unwrap();
            let pb = pairs::pair_book(g.conn(), &rctx(&c), &ta, &tb, 10).unwrap();
            assert_eq!(
                pb.asks,
                vec![pairs::PairLevel {
                    source: "direct",
                    price_num: "1".into(),
                    price_den: "1".into(),
                    amount: "10000".into(),
                    orders: 1
                }]
            );
            assert_eq!(pb.bids.len(), 1, "{:?}", pb.bids);
            assert_eq!(pb.bids[0].source, "route");
        }

        // first fill: 4 whole A through the bid of A and the ask of B (GTC: the rest continues)
        let leg = pair_leg(&c, p, &placed.create, 0, &x, &placed.create, 4 * WHOLE);
        let Leg::Pair { custody, .. } = &leg else { unreachable!() };
        let custody_op = (custody.utxo.transaction_id, custody.utxo.index);
        let b = batch(&c.w, c.daa(), vec![leg, bid_a.leg(&c, 4 * WHOLE), ask_b.leg(&c, 4 * WHOLE)]);
        let fill1 = sign_batch(&c, b);
        c.push(&[&fill1]).await;
        assert_eq!(c.hs.status(&cov), "partial");
        let x6 = PairState { amount_left: 6 * WHOLE, custody: 6 * WHOLE, ..x.clone() };
        assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobPair(x6.clone())));
        assert_eq!(
            custody_index(p, &fill1, &cov, ta.0, 6 * WHOLE),
            Some(input_of(&fill1, (&custody_op.0, custody_op.1))),
            "the custody rest at the custody input index"
        );
        assert_eq!(live_custodies(&c, &cov), vec![(ta, 6 * WHOLE)]);
        let v = view(&c, &cov);
        assert!(v.custody.as_ref().unwrap().ok);
        // the pair fill: no KAS price; the pair fields, counterparty `route` (KAS-book fills of A and B in the transaction)
        let (n, price, d) = last_fill(&c, &cov);
        assert_eq!((n, price), (Some(4 * WHOLE), None));
        assert_eq!(
            (d["side"].as_str(), d["price"].as_str(), d["amount_a"].as_str(), d["amount_b"].as_str()),
            (Some("ask"), Some("1000"), Some("4000"), Some("4000"))
        );
        assert_eq!(
            (d["tip_kas"].as_str(), d["counterparty"].as_str(), d["price_source"].as_str()),
            (Some("40000"), Some("route"), Some("none"))
        );
        assert_eq!((d["base"].as_str(), d["quote"].as_str()), (Some(ta.to_hex().as_str()), Some(tb.to_hex().as_str())));
        assert_eq!(pair_fill_rows(&c, &cov), vec![("route".to_string(), "none".to_string(), 4 * WHOLE, Some(4 * WHOLE))]);
        // the KAS-book counterparties made their own KAS trades (one per token); the pair fill made none
        assert_eq!((kas_trades(&c, &ta), kas_trades(&c, &tb)), (1, 1));
        assert_eq!(c.hs.event_kinds(&bid_a.cov(&c)), ["create", "fill"]);

        // second fill: the remaining 6 whole A from the taker's own B (inventory): the order sells out
        let leg = pair_leg(&c, p, &fill1, find_cov(&fill1, &cov).unwrap(), &x6, &fill1, 6 * WHOLE);
        let mut b = batch(&c.w, c.daa(), vec![leg]);
        b.taker_tokens = vec![taker_coin(&c, p.fb, 6 * WHOLE)];
        let fill2 = sign_batch(&c, b);
        assert!(find_cov(&fill2, &cov).is_none());
        c.push(&[&fill2]).await;
        assert_eq!(c.hs.status(&cov), "filled");
        assert_eq!(c.hs.event_kinds(&cov), ["create", "fill", "fill"]);
        assert_eq!(last_fill(&c, &cov).2["counterparty"], "inventory");
        assert_eq!(pair_fill_rows(&c, &cov).len(), 2);
        assert!(live_custodies(&c, &cov).is_empty());
        assert_eq!((kas_trades(&c, &ta), kas_trades(&c, &tb)), (1, 1), "an inventory fill is no KAS trade");
        let g = c.hs.ingest.lock().unwrap();
        assert!(pairs::pairs(g.conn(), &rctx(&c), None).unwrap().is_empty(), "nothing left to quote");
    }
}

#[tokio::test]
async fn opposite_pair_orders_net_against_each_other_and_set_no_price() {
    let p = AB;
    let c = Ctx::new();
    let (ta, tb) = (p.a(), p.b());
    let x = pair_order(p, MAKER_A, true, RATE, 10);
    let y = pair_order(p, MAKER_B, false, RATE + 10, 10);
    let px = place_pair(&c, p, AnyState::KobPair(x.clone()), PV, MAKER_A).await;
    let py = place_pair(&c, p, AnyState::KobPair(y.clone()), PV, MAKER_B).await;
    // the pair book: the ask at 1 B per A, the bid at 1.01
    {
        let g = c.hs.ingest.lock().unwrap();
        let pb = pairs::pair_book(g.conn(), &rctx(&c), &ta, &tb, 10).unwrap();
        assert_eq!((pb.asks.len(), pb.bids.len()), (1, 1));
        assert_eq!((pb.bids[0].source, pb.bids[0].price_num.as_str(), pb.bids[0].price_den.as_str()), ("direct", "101", "100"));
        assert_eq!(pb.bids[0].amount, "10000");
    }
    // 4 whole A from the ask to the bid; the bid's B (floor(4 000 x 1 010 / 1 000) = 4 040) to the ask (at least 4 000)
    let b = batch(
        &c.w,
        c.daa(),
        vec![pair_leg(&c, p, &px.create, 0, &x, &px.create, 4 * WHOLE), pair_leg(&c, p, &py.create, 0, &y, &py.create, 4 * WHOLE)],
    );
    let net = sign_batch(&c, b);
    c.push(&[&net]).await;
    assert_eq!((c.hs.status(&px.cov), c.hs.status(&py.cov)), ("partial".to_string(), "partial".to_string()));
    let paid = y.s_out(4 * WHOLE, y.price).unwrap();
    assert_eq!(paid, 4_040);
    assert_eq!(
        c.hs.tip_state(&py.cov),
        Some(AnyState::KobPair(PairState { amount_left: 6 * WHOLE, custody: y.custody - paid, ..y.clone() }))
    );
    assert_eq!(live_custodies(&c, &py.cov), vec![(tb, y.custody - paid)]);
    for (cov, side) in [(px.cov, "ask"), (py.cov, "bid")] {
        let (n, price, d) = last_fill(&c, &cov);
        assert_eq!((n, price), (Some(4 * WHOLE), None), "a netting fill sets no price");
        assert_eq!(
            (d["side"].as_str(), d["counterparty"].as_str(), d["price_source"].as_str()),
            (Some(side), Some("netting"), Some("none"))
        );
        assert_eq!(pair_fill_rows(&c, &cov)[0].0, "netting");
    }
    assert_eq!(last_fill(&c, &py.cov).2["amount_b"], "4040", "the bid paid exactly its floor");
    // no KAS trade, candle or last price of either token
    for t in [ta, tb] {
        assert_eq!(kas_trades(&c, &t), 0);
        let g = c.hs.ingest.lock().unwrap();
        let s = market::stats(g.conn(), &rctx(&c), &t, None).unwrap();
        assert!(s.last.is_none() && s.trades_24h == 0, "{s:?}");
        let cs = market::candles(g.conn(), &t, None, "5m", 300_000, None, None, 100).unwrap();
        assert!(cs.items.is_empty());
    }
    // the pair book follows the fills
    {
        let g = c.hs.ingest.lock().unwrap();
        let pb = pairs::pair_book(g.conn(), &rctx(&c), &ta, &tb, 10).unwrap();
        assert_eq!((pb.asks[0].amount.as_str(), pb.bids[0].amount.as_str()), ("6000", "6000"));
    }
    // a reorg that drops the netting block reverts both fills and their pair volume exactly
    c.hs.node.reorg(1, vec![vec![]]);
    c.hs.sync().await;
    assert_eq!((c.hs.status(&px.cov), c.hs.status(&py.cov)), ("open".to_string(), "open".to_string()));
    assert!(pair_fill_rows(&c, &px.cov).is_empty() && pair_fill_rows(&c, &py.cov).is_empty());
    assert_eq!(c.hs.tip_state(&py.cov), Some(AnyState::KobPair(y.clone())));
    assert_eq!(live_custodies(&c, &py.cov), vec![(tb, y.custody)]);
}

#[tokio::test]
async fn an_ioc_pair_ask_returns_the_rest_and_a_pair_bid_closes_returning_its_escrow_rest() {
    let p = AB;
    let c = Ctx::new();
    let (ta, tb) = (p.a(), p.b());
    // IOC: 3 of 10 whole A from inventory, the other 7 go back to the maker at the custody input index
    let x = PairState { tif: TIF_IOC, ..pair_order(p, MAKER_A, true, RATE, 10) };
    let px = place_pair(&c, p, AnyState::KobPair(x.clone()), PV, MAKER_A).await;
    let leg = pair_leg(&c, p, &px.create, 0, &x, &px.create, 3 * WHOLE);
    let Leg::Pair { custody, .. } = &leg else { unreachable!() };
    let at_op = (custody.utxo.transaction_id, custody.utxo.index);
    let mut b = batch(&c.w, c.daa(), vec![leg]);
    b.taker_tokens = vec![taker_coin(&c, p.fb, 3 * WHOLE)];
    let t = sign_batch(&c, b);
    let at = input_of(&t, (&at_op.0, at_op.1));
    let (pa, _) = prog(p.fa);
    let back = spk_to_string(
        &TokenState::user(p.fa, 7 * WHOLE, pk(MAKER_A), ext_of(p.fa)).spk_with(kob_protocol::artifacts::token_template(pa)),
    );
    assert_eq!(t.tx.outputs[at].script_public_key, back, "the IOC return at the custody input index");
    c.push(&[&t]).await;
    assert_eq!(c.hs.status(&px.cov), "killed");
    assert_eq!(c.hs.event_kinds(&px.cov), ["create", "fill", "kill"]);
    assert_eq!(c.hs.query::<i64>("SELECT remaining_amount FROM order_state WHERE covenant_id = ?1", [&px.cov.0[..]]), 7 * WHOLE);
    assert!(live_custodies(&c, &px.cov).is_empty());

    // a pair BID filled completely from inventory: pays floor(10 000) of its 10 004 escrow, the 4 left return to the maker
    let y = pair_order(p, MAKER_B, false, RATE, 10);
    let py = place_pair(&c, p, AnyState::KobPair(y.clone()), PV, MAKER_B).await;
    let mut b = batch(&c.w, c.daa(), vec![pair_leg(&c, p, &py.create, 0, &y, &py.create, 10 * WHOLE)]);
    b.taker_tokens = vec![taker_coin(&c, p.fa, 10 * WHOLE)];
    let t = sign_batch(&c, b);
    c.push(&[&t]).await;
    assert_eq!(c.hs.status(&py.cov), "filled");
    assert_eq!(c.hs.event_kinds(&py.cov), ["create", "fill"]);
    let (_, _, d) = last_fill(&c, &py.cov);
    assert_eq!(
        (d["side"].as_str(), d["amount_b"].as_str(), d["counterparty"].as_str()),
        (Some("bid"), Some("10000"), Some("inventory"))
    );
    assert!(live_custodies(&c, &py.cov).is_empty());
    let _ = tb;
    let _ = ta;
}

#[tokio::test]
async fn a_pair_order_is_refunded_by_anyone_after_expiry_and_cancelled_with_strays_of_both_tokens() {
    let p = AB;
    let c = Ctx::new();
    let (ta, tb) = (p.a(), p.b());
    // refund after expiry: the custody back to the maker; a stray of B stays (only the maker's cancel moves it)
    let now = c.daa() as i64;
    let x = PairState { expiry_daa: now + 30, ..pair_order(p, MAKER_A, true, RATE, 10) };
    let px = place_pair(&c, p, AnyState::KobPair(x.clone()), PV, MAKER_A).await;
    let stray = c.w.stray_for(p.fb, px.cov, 5, TAKER);
    c.push(&[&stray]).await;
    let v = view(&c, &px.cov);
    let strays = v.strays.clone().unwrap();
    assert_eq!(strays.len(), 1);
    assert!(!strays[0].foreign, "a stray of token B is the pair order's own");
    age(&c, 40).await;
    let refund = c.w.sign(&Action::RefundOrder(RefundOrder {
        order: c.w.order(&px.create, 0, AnyState::KobPair(x.clone())),
        custody: custody_at(&c, p, &px.create, &px.cov, ta.0, 10 * WHOLE),
        prefund: None,
        foreign: vec![],
        lock_time: c.daa(),
        funding: vec![c.w.coin(KEEPER, 10)],
        change: Some(pk(KEEPER)),
        fee: FeeOptions::default(),
    }));
    c.push(&[&refund]).await;
    assert_eq!(c.hs.status(&px.cov), "refunded");
    assert_eq!(c.hs.event_kinds(&px.cov), ["create", "refund"]);
    assert_eq!(view(&c, &px.cov).strays.map(|s| s.len()), Some(1));

    // the maker's cancel of a pair BID with strays of A and of B: all of them (and the escrow) back to the maker
    let y = pair_order(p, MAKER_B, false, RATE, 10);
    let py = place_pair(&c, p, AnyState::KobPair(y.clone()), PV, MAKER_B).await;
    let sa = c.w.stray_for(p.fa, py.cov, 3, TAKER);
    let sb = c.w.stray_for(p.fb, py.cov, 5, TAKER);
    c.push(&[&sa, &sb]).await;
    let o = book(&c).into_iter().find(|o| o.id() == py.cov.0).expect("still listed");
    assert!(o.custody_ok(), "the strays are not custody");
    assert_eq!(o.custody.as_ref().map(|c| c.utxo.covenant_id), Some(Some(tb.0)), "a pair bid's custody is its B escrow");
    assert_eq!(o.strays.len(), 2, "strays of both tokens are the order's own");
    assert!(o.foreign.is_empty());
    assert!(o.strays.iter().any(|s| s.utxo.covenant_id == Some(ta.0) && s.state.amount() == 3 && s.state.family() == p.fa));
    assert!(o.strays.iter().any(|s| s.utxo.covenant_id == Some(tb.0) && s.state.amount() == 5 && s.state.family() == p.fb));
    let v = view(&c, &py.cov);
    assert!(v.strays.as_ref().unwrap().iter().all(|s| !s.foreign));
    let cancel = c.w.sign(&Action::CancelOrder(CancelOrder {
        order: c.w.order(&py.create, 0, AnyState::KobPair(y.clone())),
        custody: o.custody.clone(),
        prefund: None,
        foreign: vec![],
        strays: o.strays.clone(),
        tokens: vec![],
        funding: vec![c.w.coin(MAKER_B, 10)],
        change: None,
        replace: None,
        lock_time: 0,
        records: vec![],
        fee: FeeOptions::default(),
    }));
    c.push(&[&cancel]).await;
    assert_eq!(c.hs.status(&py.cov), "cancelled");
    assert_eq!(c.hs.event_kinds(&py.cov), ["create", "cancel"]);
    assert_eq!(c.hs.query::<i64>("SELECT COUNT(*) FROM token_utxos WHERE owner = ?1 AND spent_block IS NULL", [&py.cov.0[..]]), 0);
}

#[tokio::test]
async fn a_sell_first_pair_entry_holds_two_custodies_and_each_fill_creates_a_bid_exit() {
    for p in [AB, BA] {
        let c = Ctx::new();
        let (ta, tb) = (p.a(), p.b());
        let e = ifd_pair(p, MAKER_A, false);
        let prefund = e.custody;
        assert_eq!(prefund, 2_009, "the prefund of 10 whole A (2 000) plus one unit per fill but the last");
        let pe = place_pair(&c, p, AnyState::KobIfdPair(e.clone()), ifd_value(&e), MAKER_A).await;
        let cov = pe.cov;
        // both custodies indexed as custody, each of its own token; the order view and the matcher's book have both
        let mut want = vec![(tb, prefund), (ta, 10 * WHOLE)];
        want.sort_by_key(|w| w.1);
        assert_eq!(live_custodies(&c, &cov), want, "{p:?}");
        let v = view(&c, &cov);
        assert!(v.listed, "{p:?}: {:?}", v.unlisted_reason);
        assert!(v.custody.as_ref().unwrap().ok);
        let pv = v.pair.clone().unwrap();
        assert_eq!((pv.side, pv.prefund.as_deref()), ("ask", Some("200")));
        assert_eq!(pv.custodies.len(), 2);
        assert_eq!(
            (pv.custodies[0].role, pv.custodies[0].expected_amount.as_str(), pv.custodies[0].ok),
            ("base", "10000", Some(true))
        );
        assert_eq!(
            (pv.custodies[1].role, pv.custodies[1].expected_amount.as_str(), pv.custodies[1].ok),
            ("quote", "2009", Some(true))
        );
        let o = book(&c).into_iter().find(|o| o.id() == cov.0).expect("listed");
        assert!(o.custody_ok());
        assert_eq!(o.custody.as_ref().and_then(|c| c.utxo.covenant_id), Some(ta.0));
        assert_eq!(o.custody_b.as_ref().and_then(|c| c.utxo.covenant_id), Some(tb.0));
        // the entry rests at its limit: an `entry` ask of the pair book
        {
            let g = c.hs.ingest.lock().unwrap();
            let pb = pairs::pair_book(g.conn(), &rctx(&c), &ta, &tb, 10).unwrap();
            assert_eq!(
                pb.asks,
                vec![pairs::PairLevel {
                    source: "entry",
                    price_num: "1".into(),
                    price_den: "1".into(),
                    amount: "10000".into(),
                    orders: 1
                }]
            );
            let s = pairs::pairs(g.conn(), &rctx(&c), None).unwrap();
            assert_eq!((s.len(), s[0].entry_asks), (1, 1));
        }

        // a fill of 4 whole A from inventory: the exit (a fresh KobCondPair BID) holds the proceeds plus the prefund of 4
        let leg = Leg::IfdPair {
            order: c.w.order(&pe.create, 0, e.clone()),
            a_custody: custody_at(&c, p, &pe.create, &cov, ta.0, 10 * WHOLE),
            b_custody: custody_at(&c, p, &pe.create, &cov, tb.0, prefund),
            amount: 4 * WHOLE,
            evidence: None,
            evidence_b: None,
            t: None,
        };
        let mut b = batch(&c.w, c.daa(), vec![leg]);
        b.taker_tokens = vec![taker_coin(&c, p.fb, 4 * WHOLE)];
        let fill = sign_batch(&c, b);
        c.push(&[&fill]).await;
        assert_eq!(c.hs.status(&cov), "partial");
        let pre4 = e.pre_of(4 * WHOLE).unwrap();
        assert_eq!(pre4, 800);
        let e6 = IfdPairState { amount_left: 6 * WHOLE, custody: prefund - pre4, ..e.clone() };
        assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobIfdPair(e6.clone())));
        let mut want = vec![(tb, prefund - pre4), (ta, 6 * WHOLE)];
        want.sort_by_key(|w| w.1);
        assert_eq!(live_custodies(&c, &cov), want, "both rests at their custody input indices");
        let (xi, exit) = find_fresh(&fill, &[cov]).expect("the exit");
        let x = e.exit_for(4 * WHOLE, 4 * WHOLE + pre4, None).unwrap();
        assert_eq!(c.hs.tip_state(&exit), Some(AnyState::KobCondPair(x.clone())));
        assert_eq!(live_custodies(&c, &exit), vec![(tb, 4 * WHOLE + pre4)], "the exit's B custody");
        assert_eq!(c.hs.query::<Vec<u8>>("SELECT parent FROM orders WHERE covenant_id = ?1", [&exit.0[..]]), cov.0.to_vec());
        let ev = events(&c, &cov);
        let fill_ev = ev.iter().rev().find(|e| e.0 == "fill").unwrap();
        assert_eq!(fill_ev.3["exit"], exit.to_hex());
        assert_eq!(
            (fill_ev.3["pair"]["amount_b"].as_str(), fill_ev.3["pair"]["counterparty"].as_str()),
            (Some("4000"), Some("inventory"))
        );
        let xv = view(&c, &exit);
        assert!(xv.listed && xv.custody.as_ref().unwrap().ok, "{p:?}: {:?}", xv.unlisted_reason);
        assert_eq!(xv.pair.as_ref().unwrap().side, "bid");

        // the exit buys the 4 whole A back at its limit (0.80 B): pays 3 200 B, the rest of its custody returns to the maker
        let leg = Leg::CondPair {
            order: c.w.order(&fill, xi, x.clone()),
            custody: custody_at(&c, p, &fill, &exit, tb.0, 4 * WHOLE + pre4).expect("the exit's custody"),
            amount: 4 * WHOLE,
            leg: 0,
            evidence: None,
            evidence_b: None,
            t: None,
            merge: None,
        };
        let mut b = batch(&c.w, c.daa(), vec![leg]);
        b.taker_tokens = vec![taker_coin(&c, p.fa, 4 * WHOLE)];
        let back = sign_batch(&c, b);
        c.push(&[&back]).await;
        assert_eq!(c.hs.status(&exit), "filled");
        let (_, price, d) = last_fill(&c, &exit);
        assert_eq!(price, None);
        assert_eq!((d["side"].as_str(), d["price"].as_str(), d["amount_b"].as_str()), (Some("bid"), Some("800"), Some("3200")));
        assert!(live_custodies(&c, &exit).is_empty());
    }
}

#[tokio::test]
async fn a_repeating_buy_first_pair_entry_books_its_exit_and_the_take_profit_merges_it_back() {
    let p = AB;
    let c = Ctx::new();
    let (ta, tb) = (p.a(), p.b());
    let mut e = IfdPairState { rpt_amount: 1 + 20 * WHOLE, ..ifd_pair(p, MAKER_A, true) };
    e.custody = e.b_custody_needed().unwrap();
    assert_eq!(e.custody, 10 * WHOLE, "the spend of the whole amount at the limit");
    let pe = place_pair(&c, p, AnyState::KobIfdPair(e.clone()), ifd_value(&e), MAKER_A).await;
    let cov = pe.cov;
    assert_eq!(live_custodies(&c, &cov), vec![(tb, 10 * WHOLE)]);
    assert_eq!(view(&c, &cov).pair.unwrap().side, "bid");

    // the entry buys 4 whole A from inventory: its exit (a KobCondPair ASK holding the 4 A) is booked
    let lock = c.daa();
    let leg = Leg::IfdPair {
        order: c.w.order(&pe.create, 0, e.clone()),
        a_custody: None,
        b_custody: custody_at(&c, p, &pe.create, &cov, tb.0, 10 * WHOLE),
        amount: 4 * WHOLE,
        evidence: None,
        evidence_b: None,
        t: None,
    };
    let mut b = batch(&c.w, lock, vec![leg]);
    b.taker_tokens = vec![taker_coin(&c, p.fa, 4 * WHOLE)];
    let fill = sign_batch(&c, b);
    c.push(&[&fill]).await;
    let entry_daa = c.w.utxo(&pe.create, 0).block_daa_score as i64;
    let e6 = IfdPairState { amount_left: 6 * WHOLE, custody: 6 * WHOLE, rpt_amount: e.rpt_amount - 4 * WHOLE, ..e.clone() };
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobIfdPair(e6.clone())));
    assert_eq!(live_custodies(&c, &cov), vec![(tb, 6 * WHOLE)]);
    let (xi, exit) = find_fresh(&fill, &[cov]).expect("the exit");
    let booking = Booking { parent: cov.0, until: rpt_until(e.expiry_daa, lock as i64, entry_daa).unwrap() };
    let x = e.exit_for(4 * WHOLE, 4 * WHOLE, Some(booking)).unwrap();
    assert_eq!(c.hs.tip_state(&exit), Some(AnyState::KobCondPair(x.clone())), "the booked exit");
    assert_eq!(live_custodies(&c, &exit), vec![(ta, 4 * WHOLE)]);
    let (_, _, d) = last_fill(&c, &cov);
    assert_eq!(
        (d["side"].as_str(), d["amount_b"].as_str(), d["counterparty"].as_str()),
        (Some("bid"), Some("4000"), Some("inventory"))
    );

    // the exit's take-profit (1.20 B per A) sells the 4 A and re-arms the entry: the entry's B custody grows by the budget
    let fi = find_cov(&fill, &cov).unwrap();
    let merge = PairEntryMerge {
        entry: c.w.order(&fill, fi, e6.clone()),
        a_custody: None,
        b_custody: custody_at(&c, p, &fill, &cov, tb.0, 6 * WHOLE),
    };
    let leg = Leg::CondPair {
        order: c.w.order(&fill, xi, x.clone()),
        custody: custody_at(&c, p, &fill, &exit, ta.0, 4 * WHOLE).expect("the exit's custody"),
        amount: 4 * WHOLE,
        leg: 0,
        evidence: None,
        evidence_b: None,
        t: None,
        merge: Some(merge),
    };
    let t_out = x.t_out_min(4 * WHOLE, x.tp_price).unwrap();
    assert_eq!(t_out, 4_800);
    let mut b = batch(&c.w, c.daa(), vec![leg]);
    b.taker_tokens = vec![taker_coin(&c, p.fb, t_out)];
    let rearm = sign_batch(&c, b);
    c.push(&[&rearm]).await;
    let budget = e.merge_budget(4 * WHOLE).unwrap();
    assert_eq!(budget, 4 * WHOLE);
    let e10 = IfdPairState { amount_left: 10 * WHOLE, custody: 6 * WHOLE + budget, ..e6.clone() };
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobIfdPair(e10)), "re-armed at the original parameters");
    assert_eq!(c.hs.event_kinds(&cov), ["create", "fill", "rearm"]);
    let rearm_ev = events(&c, &cov).into_iter().find(|e| e.0 == "rearm").unwrap();
    assert_eq!((rearm_ev.1, rearm_ev.3["exit"].as_str()), (Some(4 * WHOLE), Some(exit.to_hex().as_str())));
    assert_eq!(live_custodies(&c, &cov), vec![(tb, 10 * WHOLE)]);
    assert_eq!(c.hs.status(&exit), "filled");
    let (_, price, d) = events(&c, &exit).into_iter().find(|e| e.0 == "fill").map(|e| (e.1, e.2, e.3)).unwrap();
    assert_eq!(price, None);
    assert_eq!(d["merged_into"], cov.to_hex());
    assert_eq!(
        (d["pair"]["side"].as_str(), d["pair"]["amount_b"].as_str(), d["pair"]["price"].as_str()),
        (Some("ask"), Some("4800"), Some("1200"))
    );
    assert!(live_custodies(&c, &exit).is_empty());
}

#[tokio::test]
async fn pair_stops_arm_from_two_kas_book_fills_and_from_a_resting_pair_order() {
    let p = AB;
    let c = Ctx::new();
    let (ta, tb) = (p.a(), p.b());

    // mode 0: a sell stop at 1 B per A arms on a resting ask of A at 2.00 and a resting bid of B at 2.60 (rate 0.77 B per A)
    let x = cond_pair_ask(p, MAKER_A);
    let px = place_pair(&c, p, AnyState::KobCondPair(x.clone()), PV, MAKER_A).await;
    assert_eq!(view(&c, &px.cov).pair.unwrap().stop_price.as_deref(), Some("1000"));
    let ra = resting_for(&c, p.fa, SIDE_ASK, P200).await;
    let rb = resting_for(&c, p.fb, SIDE_BID, P260).await;
    let lock = rested(&c, &ra, x.min_rest_daa).max(rested(&c, &rb, x.min_rest_daa));
    let mut b = batch(&c.w, lock, vec![]);
    let ka = ra.add_to(&c, &mut b, WHOLE);
    let kb = rb.add_to(&c, &mut b, WHOLE);
    b.updates = vec![BatchUpdate {
        order: c.w.order(&px.create, 0, AnyState::KobCondPair(x.clone())),
        evidence: ka,
        evidence_b: Some(kb),
        take: None,
    }];
    let arm = sign_batch(&c, b);
    c.push(&[&arm]).await;
    assert_eq!(c.hs.tip_state(&px.cov), Some(AnyState::KobCondPair(CondPairState { armed: 1, ..x.clone() })));
    assert_eq!(c.hs.event_kinds(&px.cov), ["create", "arm"]);
    let ev = c.hs.event_detail(&px.cov, "arm")["evidence"].clone();
    assert_eq!(ev["mode"], 0);
    assert_eq!(ev["inputs"], serde_json::json!([ka, kb]));
    assert_eq!(ev["orders"], serde_json::json!([ra.cov(&c).to_hex(), rb.cov(&c).to_hex()]));
    assert_eq!((ev["a"].as_str(), ev["b"].as_str()), (Some("200000000"), Some("260000000")));
    // the evidence legs are ordinary KAS fills: their trades are recorded
    assert_eq!((kas_trades(&c, &ta), kas_trades(&c, &tb)), (1, 1));
    assert!(live_custodies(&c, &px.cov) == vec![(ta, 10 * WHOLE)], "an update never moves the custody");

    // mode 1: another sell stop at 1 B per A arms on the fill of a resting pair ASK of the pair quoting 0.90
    let y = cond_pair_ask(p, MAKER_A);
    let py = place_pair(&c, p, AnyState::KobCondPair(y.clone()), PV, MAKER_A).await;
    let z = pair_order(p, MAKER_B, true, RATE - 100, 10);
    let pz = place_pair(&c, p, AnyState::KobPair(z.clone()), PV, MAKER_B).await;
    // a trailing sell stop (step 50, gap 20, take-profit 1.20) ratchets up on the fill of a resting pair BID quoting 1.10
    let tr = CondPairState { trail_step: 50, trail_gap: 20, ..cond_pair_ask(p, MAKER_A) };
    let ptr = place_pair(&c, p, AnyState::KobCondPair(tr.clone()), PV, MAKER_A).await;
    let w = pair_order(p, MAKER_B, false, RATE + 100, 10);
    let pw = place_pair(&c, p, AnyState::KobPair(w.clone()), PV, MAKER_B).await;
    age(&c, 80).await;
    let mut b = batch(&c.w, c.daa(), vec![pair_leg(&c, p, &pz.create, 0, &z, &pz.create, WHOLE)]);
    b.taker_tokens = vec![taker_coin(&c, p.fb, z.t_out_min(WHOLE, z.price).unwrap())];
    b.updates = vec![BatchUpdate {
        order: c.w.order(&py.create, 0, AnyState::KobCondPair(y.clone())),
        evidence: 0,
        evidence_b: None,
        take: None,
    }];
    let arm = sign_batch(&c, b);
    c.push(&[&arm]).await;
    assert_eq!(c.hs.tip_state(&py.cov), Some(AnyState::KobCondPair(CondPairState { armed: 1, ..y.clone() })));
    assert_eq!(c.hs.event_kinds(&py.cov), ["create", "arm"]);
    let ev = c.hs.event_detail(&py.cov, "arm")["evidence"].clone();
    assert_eq!(ev["mode"], 1);
    assert_eq!(ev["inputs"], serde_json::json!([0]));
    assert_eq!(ev["orders"], serde_json::json!([pz.cov.to_hex()]));
    assert_eq!(ev["price"], "900");
    // the evidence pair order's own fill: inventory, no price
    let (n, price, d) = last_fill(&c, &pz.cov);
    assert_eq!((n, price, d["counterparty"].as_str()), (Some(WHOLE), None, Some("inventory")));
    assert_eq!((kas_trades(&c, &ta), kas_trades(&c, &tb)), (1, 1), "a pair-order evidence fill makes no KAS trade");

    // the trail: k = 1 (1 000 + 50 + 20 <= 1 100 < 1 000 + 100 + 20), the new stop 1 050 proven by the indexer
    assert_eq!(tr.trail_k(PairEvidence::Pair { price: w.price }), Some(1));
    let mut b = batch(&c.w, c.daa(), vec![pair_leg(&c, p, &pw.create, 0, &w, &pw.create, WHOLE)]);
    b.taker_tokens = vec![taker_coin(&c, p.fa, WHOLE)];
    b.updates = vec![BatchUpdate {
        order: c.w.order(&ptr.create, 0, AnyState::KobCondPair(tr.clone())),
        evidence: 0,
        evidence_b: None,
        take: None,
    }];
    let trail = sign_batch(&c, b);
    c.push(&[&trail]).await;
    assert_eq!(c.hs.tip_state(&ptr.cov), Some(AnyState::KobCondPair(CondPairState { stop_price: RATE + 50, ..tr.clone() })));
    assert_eq!(c.hs.event_kinds(&ptr.cov), ["create", "trail"]);
    let ev = c.hs.event_detail(&ptr.cov, "trail")["evidence"].clone();
    assert_eq!((ev["mode"].as_i64(), ev["price"].as_str()), (Some(1), Some("1100")));
    assert_eq!(live_custodies(&c, &ptr.cov), vec![(ta, 10 * WHOLE)]);
    let _ = Family::Kcc20;
}

#[tokio::test]
async fn a_pair_stop_entry_is_out_of_the_pair_book_until_a_resting_pair_order_arms_it() {
    let p = AB;
    let c = Ctx::new();
    let (ta, tb) = (p.a(), p.b());
    // a buy-stop entry: buys A at up to 1 B per A once the rate rose to 0.95
    let e = IfdPairState { entry_stop: RATE - 50, ..ifd_pair(p, MAKER_A, true) };
    let pe = place_pair(&c, p, AnyState::KobIfdPair(e.clone()), ifd_value(&e), MAKER_A).await;
    let z = pair_order(p, MAKER_B, false, RATE + 100, 10);
    let pz = place_pair(&c, p, AnyState::KobPair(z.clone()), PV, MAKER_B).await;
    age(&c, 80).await;
    let entry_levels = |c: &Ctx| {
        let g = c.hs.ingest.lock().unwrap();
        let pb = pairs::pair_book(g.conn(), &rctx(c), &ta, &tb, 10).unwrap();
        pb.bids.iter().filter(|l| l.source == "entry").count()
    };
    assert_eq!(entry_levels(&c), 0, "an unarmed stop entry is no resting liquidity");
    let mut b = batch(&c.w, c.daa(), vec![pair_leg(&c, p, &pz.create, 0, &z, &pz.create, WHOLE)]);
    b.taker_tokens = vec![taker_coin(&c, p.fa, WHOLE)];
    b.updates = vec![BatchUpdate {
        order: c.w.order(&pe.create, 0, AnyState::KobIfdPair(e.clone())),
        evidence: 0,
        evidence_b: None,
        take: None,
    }];
    let arm = sign_batch(&c, b);
    c.push(&[&arm]).await;
    assert_eq!(c.hs.tip_state(&pe.cov), Some(AnyState::KobIfdPair(IfdPairState { armed: 1, ..e.clone() })));
    assert_eq!(c.hs.event_kinds(&pe.cov), ["create", "arm"]);
    let ev = c.hs.event_detail(&pe.cov, "arm")["evidence"].clone();
    assert_eq!((ev["mode"].as_i64(), ev["price"].as_str()), (Some(1), Some("1100")));
    assert_eq!(ev["orders"], serde_json::json!([pz.cov.to_hex()]));
    assert_eq!(live_custodies(&c, &pe.cov), vec![(tb, e.custody)], "an update never moves the custody");
    assert_eq!(entry_levels(&c), 1, "armed, the entry rests in the pair book");
}

#[tokio::test]
async fn a_repeating_sell_first_entry_is_re_armed_by_its_exits_take_profit_into_both_custodies() {
    let p = AB;
    let c = Ctx::new();
    let (ta, tb) = (p.a(), p.b());
    let mut e = IfdPairState { rpt_amount: 1 + 20 * WHOLE, ..ifd_pair(p, MAKER_A, false) };
    e.custody = e.b_custody_needed().unwrap();
    let pe = place_pair(&c, p, AnyState::KobIfdPair(e.clone()), ifd_value(&e), MAKER_A).await;
    let cov = pe.cov;

    // the entry sells 4 whole A from inventory and books its BID exit (proceeds plus the prefund of 4)
    let lock = c.daa();
    let leg = Leg::IfdPair {
        order: c.w.order(&pe.create, 0, e.clone()),
        a_custody: custody_at(&c, p, &pe.create, &cov, ta.0, 10 * WHOLE),
        b_custody: custody_at(&c, p, &pe.create, &cov, tb.0, e.custody),
        amount: 4 * WHOLE,
        evidence: None,
        evidence_b: None,
        t: None,
    };
    let mut b = batch(&c.w, lock, vec![leg]);
    b.taker_tokens = vec![taker_coin(&c, p.fb, 4 * WHOLE)];
    let fill = sign_batch(&c, b);
    c.push(&[&fill]).await;
    let pre = e.pre_of(4 * WHOLE).unwrap();
    let e6 = IfdPairState { amount_left: 6 * WHOLE, custody: e.custody - pre, rpt_amount: e.rpt_amount - 4 * WHOLE, ..e.clone() };
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobIfdPair(e6.clone())));
    let entry_daa = c.w.utxo(&pe.create, 0).block_daa_score as i64;
    let (xi, exit) = find_fresh(&fill, &[cov]).expect("the exit");
    let booking = Booking { parent: cov.0, until: rpt_until(e.expiry_daa, lock as i64, entry_daa).unwrap() };
    let x = e.exit_for(4 * WHOLE, 4 * WHOLE + pre, Some(booking)).unwrap();
    assert_eq!(c.hs.tip_state(&exit), Some(AnyState::KobCondPair(x.clone())), "the booked BID exit");

    // the exit buys the 4 A back at its limit (sells out): the A join the entry's A custody (6 -> 10), the prefund of 4 returns
    // into its prefund custody
    let fi = find_cov(&fill, &cov).unwrap();
    let merge = PairEntryMerge {
        entry: c.w.order(&fill, fi, e6.clone()),
        a_custody: custody_at(&c, p, &fill, &cov, ta.0, 6 * WHOLE),
        b_custody: custody_at(&c, p, &fill, &cov, tb.0, e6.custody),
    };
    let leg = Leg::CondPair {
        order: c.w.order(&fill, xi, x.clone()),
        custody: custody_at(&c, p, &fill, &exit, tb.0, 4 * WHOLE + pre).expect("the exit's custody"),
        amount: 4 * WHOLE,
        leg: 0,
        evidence: None,
        evidence_b: None,
        t: None,
        merge: Some(merge),
    };
    let mut b = batch(&c.w, c.daa(), vec![leg]);
    b.taker_tokens = vec![taker_coin(&c, p.fa, 4 * WHOLE)];
    let rearm = sign_batch(&c, b);
    c.push(&[&rearm]).await;
    let rearmed = IfdPairState { amount_left: 10 * WHOLE, custody: e6.custody + pre, ..e6.clone() };
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobIfdPair(rearmed)));
    assert_eq!(c.hs.event_kinds(&cov), ["create", "fill", "rearm"]);
    let mut want = vec![(tb, e.custody), (ta, 10 * WHOLE)];
    want.sort_by_key(|w| w.1);
    assert_eq!(live_custodies(&c, &cov), want, "both custodies grown at their indices");
    let v = view(&c, &cov);
    assert!(v.custody.as_ref().unwrap().ok && v.pair.as_ref().unwrap().custodies.iter().all(|c| c.ok == Some(true)));
    assert_eq!(c.hs.status(&exit), "filled");
    let d = c.hs.event_detail(&exit, "fill");
    assert_eq!(d["merged_into"], cov.to_hex());
    assert_eq!((d["pair"]["side"].as_str(), d["pair"]["price"].as_str()), (Some("bid"), Some("800")));
    assert!(live_custodies(&c, &exit).is_empty());
}

#[tokio::test]
async fn a_waiting_buy_first_entry_without_escrow_gets_a_new_b_custody_from_its_exits_merge() {
    let p = AB;
    let c = Ctx::new();
    let (ta, tb) = (p.a(), p.b());
    let mut e = IfdPairState { rpt_amount: 1 + 20 * WHOLE, ..ifd_pair(p, MAKER_A, true) };
    e.custody = e.b_custody_needed().unwrap();
    let pe = place_pair(&c, p, AnyState::KobIfdPair(e.clone()), ifd_value(&e), MAKER_A).await;
    let cov = pe.cov;

    // the entry buys all 10 whole A: its escrow is used up, it waits (it repeats) with no custody at all
    let lock = c.daa();
    let leg = Leg::IfdPair {
        order: c.w.order(&pe.create, 0, e.clone()),
        a_custody: None,
        b_custody: custody_at(&c, p, &pe.create, &cov, tb.0, e.custody),
        amount: 10 * WHOLE,
        evidence: None,
        evidence_b: None,
        t: None,
    };
    let mut b = batch(&c.w, lock, vec![leg]);
    b.taker_tokens = vec![taker_coin(&c, p.fa, 10 * WHOLE)];
    let fill = sign_batch(&c, b);
    c.push(&[&fill]).await;
    let waiting = IfdPairState { amount_left: 0, custody: 0, rpt_amount: e.rpt_amount - 10 * WHOLE, ..e.clone() };
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobIfdPair(waiting.clone())));
    assert_eq!(c.hs.status(&cov), "partial", "a waiting repeating entry stays live");
    assert!(live_custodies(&c, &cov).is_empty());
    let entry_daa = c.w.utxo(&pe.create, 0).block_daa_score as i64;
    let (xi, exit) = find_fresh(&fill, &[cov]).expect("the exit");
    let booking = Booking { parent: cov.0, until: rpt_until(e.expiry_daa, lock as i64, entry_daa).unwrap() };
    let x = e.exit_for(10 * WHOLE, 10 * WHOLE, Some(booking)).unwrap();
    assert_eq!(c.hs.tip_state(&exit), Some(AnyState::KobCondPair(x.clone())));

    // the exit's take-profit sells the 10 A out: the budget of 10 goes into a NEW B custody of the entry (the covenant's bOut)
    let fi = find_cov(&fill, &cov).unwrap();
    let merge = PairEntryMerge { entry: c.w.order(&fill, fi, waiting.clone()), a_custody: None, b_custody: None };
    let leg = Leg::CondPair {
        order: c.w.order(&fill, xi, x.clone()),
        custody: custody_at(&c, p, &fill, &exit, ta.0, 10 * WHOLE).expect("the exit's custody"),
        amount: 10 * WHOLE,
        leg: 0,
        evidence: None,
        evidence_b: None,
        t: None,
        merge: Some(merge),
    };
    let t_out = x.t_out_min(10 * WHOLE, x.tp_price).unwrap();
    let mut b = batch(&c.w, c.daa(), vec![leg]);
    b.taker_tokens = vec![taker_coin(&c, p.fb, t_out)];
    let rearm = sign_batch(&c, b);
    c.push(&[&rearm]).await;
    let budget = e.merge_budget(10 * WHOLE).unwrap();
    let rearmed = IfdPairState { amount_left: 10 * WHOLE, custody: budget, ..waiting.clone() };
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobIfdPair(rearmed)));
    assert_eq!(c.hs.event_kinds(&cov), ["create", "fill", "rearm"]);
    assert_eq!(live_custodies(&c, &cov), vec![(tb, budget)], "the new B custody is the entry's custody");
    let at = custody_index(p, &rearm, &cov, tb.0, budget).expect("the new custody output");
    assert!(rearm.tx.outputs[at].covenant.as_ref().is_some_and(|o| o.covenant_id == tb.0));
    assert!(view(&c, &cov).custody.unwrap().ok);
    assert_eq!(c.hs.status(&exit), "filled");
    assert_eq!(c.hs.event_detail(&exit, "fill")["merged_into"], cov.to_hex());
}
