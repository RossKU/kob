//! The x402 facilitator inside `kob-executor run`: finality from the indexer's acceptance tracking
//! ([`IndexedChain`]) instead of the merchant output staying in the UTXO set, the per-consumer watch
//! ownership of the ingest (the matcher never drops the facilitator's watches), and swap-and-pay quotes
//! from an indexer's book.

#[path = "matcher_common/mod.rs"]
mod mc;

use std::sync::Arc;
use std::time::{Duration, Instant};

use kob_executor::hex::Hash32;
use kob_executor::indexer::db::open_memory;
use kob_executor::indexer::ingest::Watcher;
use kob_executor::testkit::{noise_tx, Harness};
use kob_executor::x402::indexed::IndexedChain;
use kob_executor::x402::ledger::{Ledger, State};
use kob_executor::x402::quote::{quote, quote_legs, QuoteRequest, Take};
use kob_executor::x402::testutil::{Fixture, MERCHANT_KEY, START_MS};
use kob_protocol::state::AnyState;
use kob_x402::chain::{ChainView, FixedClock, Outpoint, OutputStatus, Tracked};
use kob_x402::testkit::{p2pk_spk, pubkey, MockChain};
use serde_json::Value;

/// A transaction as the indexer's node reports an accepted one (only its id matters to acceptance).
fn accepted(txid: [u8; 32], salt: u64) -> kob_executor::rpc::types::Tx {
    let mut t = noise_tx(salt, false);
    t.verbose_data.transaction_id = Hash32(txid);
    t
}

fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
    let t = Instant::now();
    while !f() {
        assert!(t.elapsed() < Duration::from_secs(10), "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn settlement_finality_and_reorgs_come_from_the_indexers_acceptance_tracking() {
    let hs = Harness::new(open_memory("testnet-10").unwrap(), None);
    hs.sync().await;
    let chain = Arc::new(MockChain::new());
    let view: Arc<dyn ChainView> = Arc::new(IndexedChain::new(chain.clone(), hs.ingest.clone()));
    let fx = Fixture::with_view(Arc::new(Ledger::in_memory()), chain.clone(), view.clone(), Arc::new(FixedClock::new(START_MS)));
    let coin = fx.fund(5 * 100_000_000);
    let (req, tx) = fx.payment(&[coin], 100_000_000, "indexed-settlement-0001", 7);
    let txid = tx.id().as_bytes();
    let merchant_out = Outpoint::new(txid, 0);
    let merchant_spk = p2pk_spk(&pubkey(MERCHANT_KEY));

    // settle in the background: it submits, then polls for finality
    let fac = fx.fac.clone();
    let settling = std::thread::spawn(move || fac.settle("open", &req));
    wait_until("the broadcast", || chain.submit_count() == 1);
    assert_eq!(view.tracked(&txid), Tracked::NotAccepted, "tracked from before the broadcast");

    // the node accepts it and the merchant spends the output at once: the UTXO observation alone
    // never sees the settlement as accepted
    chain.mine(1);
    chain.spend_externally(&merchant_out);
    assert_eq!(chain.output_status(&merchant_out, &merchant_spk).unwrap(), OutputStatus::Unknown);

    // the indexer's follower sees the accepting chain block
    let block = hs.node.push_block(vec![accepted(txid, 1)]);
    hs.sync().await;
    let daa = hs.node.chain().last().unwrap().daa;
    assert_eq!(hs.node.tip(), block);
    assert_eq!(view.tracked(&txid), Tracked::Accepted { block_daa_score: daa });

    let resp = tokio::task::spawn_blocking(move || settling.join().unwrap()).await.unwrap();
    assert!(resp.success, "{resp:?}");
    let ext: Value = resp.extensions.clone().unwrap();
    assert_eq!(ext["kob"]["acceptedDaaScore"], Value::String(daa.to_string()));
    let entry = fx.ledger.get(&kob_x402::wire::hex(&txid)).unwrap();
    assert_eq!((entry.state, entry.accepted_daa), (State::Accepted, Some(daa)));

    // reconcile: the merchant output is gone, but the tracker still places the transaction on the
    // selected chain: no reorg
    let r = fx.fac.reconcile();
    assert_eq!((r.checked, r.reorged), (1, 0), "{r:?}");

    // a reorg drops the accepting block: now the settlement is suspect
    hs.node.reorg(1, vec![vec![], vec![]]);
    hs.sync().await;
    assert_eq!(view.tracked(&txid), Tracked::NotAccepted);
    let r = fx.fac.reconcile();
    assert_eq!(r.reorged, 1, "{r:?}");
    assert_eq!(fx.ledger.get(&kob_x402::wire::hex(&txid)).unwrap().state, State::Ambiguous);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_matcher_never_drops_the_facilitators_watches() {
    let hs = Harness::new(open_memory("testnet-10").unwrap(), None);
    hs.sync().await;
    let (mine, theirs) = (Hash32([0x11; 32]), Hash32([0x22; 32]));
    {
        let mut ing = hs.ingest.lock().unwrap();
        ing.watch_tx_for(Watcher::Facilitator, mine);
        ing.watch_tx(theirs);
        // the matcher keeps nothing: only its own watch goes
        assert_eq!(ing.watched_acceptance(&[]), vec![]);
        assert_eq!(ing.acceptance_of(&mine), Some(None));
        assert_eq!(ing.acceptance_of(&theirs), None);
        // the matcher does not see the facilitator's watch as its own
        assert_eq!(ing.watched_acceptance(&[mine]), vec![(mine, None)]);
    }
    hs.node.push_block(vec![accepted(mine.0, 2)]);
    hs.sync().await;
    let mut ing = hs.ingest.lock().unwrap();
    let daa = hs.node.chain().last().unwrap().daa;
    assert_eq!(ing.acceptance_of(&mine).flatten().map(|(_, d)| d), Some(daa));
    ing.unwatch_tx_for(Watcher::Facilitator, &mine);
    assert_eq!(ing.acceptance_of(&mine), None);
}

#[test]
fn swap_quotes_include_kron_orders_and_a_route_may_cross_families() {
    use mc::*;
    let t = kob_protocol::artifacts::TemplateId::Kcc20Ref;
    let k = kob_protocol::artifacts::TemplateId::KronToken2433;
    // KRON bids (the pay token) and a KCC-20 ask (the merchant token), one book
    let kron_bids = vec![kron_order(&l_bid(1, bid(10, P250, k), 5 * WHOLE)), kron_order(&l_bid(2, bid(11, P260, k), 2 * WHOLE))];
    let kcc_ask = l_ask(3, ask(12, P255, 4 * WHOLE, t));
    let b = book(kron_bids.into_iter().chain([kcc_ask]).collect());
    let sell = QuoteRequest {
        token: kob_executor::testkit::TOKEN_COV_KRON,
        take: Take::SellToBids,
        amount: 4 * WHOLE,
        limit_price: None,
        exclude: vec![],
    };
    let legs = quote_legs(&b, &sell, UTC).unwrap();
    let got: Vec<(i64, i64)> = legs
        .iter()
        .map(|l| match &l.leg {
            kob_protocol::build::Leg::Bid { order, amount, .. } => (order.state.price, *amount),
            _ => panic!("a bid leg"),
        })
        .collect();
    assert_eq!(got, vec![(P260, 2 * WHOLE), (P250, 2 * WHOLE)], "best KRON bid first");
    // the KCC-20 book of the same covenant id namespace is separate: TOKEN's bids are not KRON's
    let other = QuoteRequest { token: TOKEN, ..sell.clone() };
    assert!(quote_legs(&b, &other, UTC).is_none());
    // a cross-family route: KRON sold into KRON bids, a KCC-20 token bought from its ask
    let buy = QuoteRequest { token: TOKEN, take: Take::BuyFromAsks, amount: 2 * WHOLE, limit_price: None, exclude: vec![] };
    let q = quote(&b, &[QuoteRequest { amount: 3 * WHOLE, ..sell }, buy], 88, UTC).unwrap();
    assert_eq!(q.orders.len(), 3);
    assert_eq!(q.orders.iter().filter(|o| o.kind() == Some(kob_x402::swap::LegKind::Bid)).count(), 2);
    assert_eq!(q.orders.iter().filter(|o| o.kind() == Some(kob_x402::swap::LegKind::Ask)).count(), 1);
}

#[test]
fn swap_quotes_come_from_the_book_best_price_first() {
    use mc::*;
    let t = kob_protocol::artifacts::TemplateId::Kcc20Ref;
    let mut bids =
        vec![l_bid(1, bid(10, P245, t), 2 * WHOLE), l_bid(2, bid(11, P260, t), WHOLE), l_bid(3, bid(12, P250, t), 5 * WHOLE)];
    // a decaying bid and a KRON-kind bid that names a KCC-20 program (no such order is valid) are never quoted
    let mut rising = bid(13, P260, t);
    rising.slope = 1;
    rising.price_end = P260 + 1_000;
    bids.push(l_bid(4, rising, 5 * WHOLE));
    let mut kron = l_bid(5, bid(14, P260 + 5, t), 5 * WHOLE);
    kron.order.state = AnyState::KobBidKron(match kron.order.state {
        AnyState::KobBid(s) => s,
        _ => unreachable!(),
    });
    bids.push(kron);
    let asks = vec![l_ask(6, ask(15, P255, 2 * WHOLE, t)), l_ask(7, ask(16, P250, WHOLE, t))];
    let b = book(bids.into_iter().chain(asks).collect());

    let sell = |n: i64, exclude: Vec<Outpoint>| QuoteRequest {
        token: TOKEN,
        take: Take::SellToBids,
        amount: n * WHOLE,
        limit_price: None,
        exclude,
    };
    let legs = quote_legs(&b, &sell(3, vec![]), UTC).unwrap();
    let got: Vec<(i64, i64)> = legs
        .iter()
        .map(|l| match &l.leg {
            kob_protocol::build::Leg::Bid { order, amount, .. } => (order.state.price, *amount),
            _ => panic!("a bid leg"),
        })
        .collect();
    assert_eq!(got, vec![(P260, WHOLE), (P250, 2 * WHOLE)], "best bid first, then the next level");
    // more than the book holds
    assert!(quote_legs(&b, &sell(9, vec![]), UTC).is_none());
    // an order lost in an order_conflict is skipped
    let lost = legs[0].outpoint();
    let again = quote_legs(&b, &sell(3, vec![lost]), UTC).unwrap();
    assert!(again.iter().all(|l| l.outpoint() != lost));
    // a limit price
    let limited = |n: i64| QuoteRequest { limit_price: Some(P250), ..sell(n, vec![]) };
    assert_eq!(quote_legs(&b, &limited(6), UTC).unwrap().iter().map(|l| l.amount()).sum::<i64>(), 6 * WHOLE);
    assert!(quote_legs(&b, &limited(7), UTC).is_none(), "the P245 bid is below the limit");

    // a two-token route: bids of the pay token, then asks of the merchant token (cheapest first)
    let buy = QuoteRequest { token: TOKEN, take: Take::BuyFromAsks, amount: 2 * WHOLE, limit_price: None, exclude: vec![] };
    let q = quote(&b, &[sell(1, vec![]), buy], 77, UTC).unwrap();
    assert_eq!(q.lock_time, 77);
    let prices: Vec<i64> = q
        .orders
        .iter()
        .map(|l| match &l.leg {
            kob_protocol::build::Leg::Bid { order, .. } => order.state.price,
            kob_protocol::build::Leg::Ask { order, .. } => order.state.price,
            _ => panic!(),
        })
        .collect();
    assert_eq!(prices, vec![P260, P250, P255]);
}
