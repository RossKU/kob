//! The matcher at the wallet's default order carrier (`DEFAULT_ORDER_CARRIER`, 1 KAS) on the widest token program, KaspaRocket's
//! 10 / 10 `KaspaRocketToken11458`: the orders are placed with 1 KAS on every covenant UTXO (the ask's order UTXO and custody, the
//! bid's `deliveryCarrier`), so a fill's token and order outputs are small cells whose KIP-9 storage mass (`4 × 10^12 / carrier`
//! each) is the limit that binds, not the relay fee or the compute mass. At 1 KAS every shape the matcher builds stays within the
//! 500,000 block storage limit and the full 10-delivery batches go out as one transaction; below it (0.5 KAS here) a first plan over
//! the limit is lowered (the engine scales the batch down by the overrun and plans again): fewer fills per transaction, never an
//! invalid or an unmined transaction.

mod common;

use std::collections::BTreeSet;

use common::*;
use kob_protocol::artifacts::TemplateId;
use kob_protocol::build::*;
use kob_protocol::defaults::DEFAULT_ORDER_CARRIER;
use kob_protocol::state::*;
use kob_protocol::tx::*;
use kobd::indexer::book;
use kobd::matcher::book::Clock;
use kobd::matcher::engine::{tick, EngineConfig, TickInput, TickReport};
use kobd::matcher::family::Families;
use kobd::matcher::wallet::LocalKeys;
use kobd::testkit::*;

const KR: TemplateId = TemplateId::KaspaRocketToken11458;

fn on_kr_ask(a: AskState) -> AskState {
    let (h, p, s) = tok_fields(KR);
    AskState { token_tpl_hash: h, tpl_prefix_len: p, tpl_suffix_len: s, refund_tip: rtip(KR), ..a }
}

fn on_kr_bid(b: BidState, delivery_carrier: u64) -> BidState {
    let (h, p, s) = tok_fields(KR);
    BidState {
        token_tpl_hash: h,
        tpl_prefix_len: p,
        tpl_suffix_len: s,
        refund_tip: rtip(KR),
        delivery_carrier: delivery_carrier as i64,
        ..b
    }
}

/// One tick over a book of `asks` (whole tokens held, each on its own maker) and `bids` (whole tokens wanted, each one fill),
/// every covenant UTXO of every order carrying `carrier`.
async fn tick_at(carrier: u64, asks: &[i64], bids: &[i64]) -> TickReport {
    let c = Ctx::new();
    let mut txs = vec![];
    for (i, held) in asks.iter().enumerate() {
        let maker = 1 + i as u8;
        let a = AskState { amount_left: held * WHOLE, ..on_kr_ask(ask(maker, P250)) };
        let mut req = c.w.create(AnyState::KobAsk(a), carrier, maker, held * WHOLE);
        req.token_carrier = carrier;
        txs.push(c.w.sign(&Action::CreateOrder(req)));
    }
    for (j, want) in bids.iter().enumerate() {
        let maker = 20 + j as u8;
        let b = on_kr_bid(bid(maker, P260), carrier);
        let escrow = b.escrow(want * WHOLE, 1).unwrap() as u64;
        txs.push(c.w.create_tx(AnyState::KobBid(b), escrow, maker, 0));
    }
    let refs: Vec<&SignedTx> = txs.iter().collect();
    c.push(&refs).await;
    let listed = { book::listed_orders(c.hs.ingest.lock().unwrap().conn()).unwrap() };
    let inp = TickInput {
        orders: listed,
        clock: Clock { daa: c.daa() + 5, utc: 1_790_694_000 },
        funding: vec![KeyUtxo {
            utxo: Utxo { transaction_id: [0xfa; 32], index: 0, amount: 50 * KAS, block_daa_score: 500, covenant_id: None },
            pubkey: pk(MATCHER),
        }],
        excluded: BTreeSet::new(),
    };
    let signer = LocalKeys { operator: pk(MATCHER), keys: (1..=40u8).map(|n| (pk(n), sk(n))).collect() };
    tick(&inp, &EngineConfig::default(), &Families::default(), &signer)
}

/// (fills, storage mass) of every prepared transaction.
fn shapes(r: &TickReport) -> Vec<(usize, u64)> {
    r.prepared.iter().map(|p| (p.plan.fills.len(), p.signed.fee.mass.storage)).collect()
}

fn assert_within_limits(r: &TickReport) {
    for p in &r.prepared {
        let m = p.signed.fee.mass;
        assert!(m.storage <= BLOCK_STORAGE_LIMIT, "storage {} over the block limit", m.storage);
        assert!(m.compute <= BLOCK_COMPUTE_LIMIT && m.transient <= BLOCK_TRANSIENT_LIMIT, "{m:?}");
        assert!(p.validation.is_some(), "every prepared transaction passed the engine");
    }
}

/// The default carrier is 1 KAS and the full 10-delivery batch of the widest program, one ask against ten bids, goes out as one
/// transaction of eleven fills at a storage mass well within the limit (measured 353,009 of 500,000).
#[tokio::test]
async fn the_full_ten_delivery_batch_fits_at_the_default_carrier() {
    assert_eq!(DEFAULT_ORDER_CARRIER, KAS);
    let r = tick_at(DEFAULT_ORDER_CARRIER, &[20], &[2; 10]).await;
    assert!(r.anomalies.is_empty(), "anomalies: {:?}", r.anomalies);
    assert_within_limits(&r);
    let s = shapes(&r);
    assert_eq!(s.len(), 1, "one transaction: {s:?}");
    assert_eq!(s[0].0, 11, "the ask and all ten bids fill: {s:?}");
    assert!(s[0].1 > BLOCK_STORAGE_LIMIT / 2 && s[0].1 <= BLOCK_STORAGE_LIMIT, "the batch is storage-heavy at 1 KAS: {s:?}");
}

/// The widest crossing the program takes (8 asks against 10 bids, 18 fills; 8 custody inputs, 10 deliveries) is one transaction
/// at 1 KAS, below the limit.
#[tokio::test]
async fn eight_asks_against_ten_bids_is_one_transaction_at_the_default_carrier() {
    let r = tick_at(DEFAULT_ORDER_CARRIER, &[10; 8], &[8; 10]).await;
    assert!(r.anomalies.is_empty(), "anomalies: {:?}", r.anomalies);
    assert_within_limits(&r);
    let s = shapes(&r);
    assert_eq!(s.len(), 1, "{s:?}");
    assert_eq!(s[0].0, 18, "{s:?}");
}

/// The partial-heavy shape: eight large asks that all stay open after one bid takes a slice of each (8 order continuations and 8
/// custody continuations, the cells that cost 40,000 storage mass apiece at 1 KAS). Whatever the plan is, no transaction goes over
/// the limit; a plan the builder could not mine is lowered by the engine, not submitted.
#[tokio::test]
async fn partial_heavy_batches_stay_within_the_storage_limit_at_the_default_carrier() {
    for (asks, bids) in [(vec![100i64; 8], vec![16i64]), (vec![100; 7], vec![14]), (vec![100; 8], vec![8, 8]), (vec![100; 3], vec![6])]
    {
        let r = tick_at(DEFAULT_ORDER_CARRIER, &asks, &bids).await;
        assert_within_limits(&r);
        println!("partial-heavy {} asks x {bids:?}: {:?} anomalies {}", asks.len(), shapes(&r), r.anomalies.len());
        assert!(!r.prepared.is_empty(), "the crossing is filled ({:?})", r.skipped);
    }
}

/// Below the default (0.5 KAS) the same ten-delivery batch is over the storage limit as planned: the engine lowers it (the batch
/// is scaled down by the overrun and planned again), so the book is filled in several transactions of fewer fills, each within
/// the limit.
#[tokio::test]
async fn a_batch_over_the_storage_limit_falls_back_to_fewer_fills() {
    let r = tick_at(KAS / 2, &[20], &[2; 10]).await;
    assert_within_limits(&r);
    let s = shapes(&r);
    println!("0.5 KAS: {s:?} anomalies {}", r.anomalies.len());
    assert!(s.len() >= 2, "the batch is split: {s:?}");
    assert!(s.iter().all(|(fills, _)| *fills < 11), "no transaction takes all eleven fills: {s:?}");
    assert!(
        r.anomalies.iter().any(|(_, _, why)| why.contains("storage mass") && why.contains("exceeds the block limit")),
        "the first plan is logged as over the storage limit: {:?}",
        r.anomalies
    );
    assert!(s.iter().map(|(f, _)| f).sum::<usize>() >= 10, "the book is still filled: {s:?}");
}

/// A taker's (wallet-built) buy that takes a slice of every one of `n` KaspaRocket asks (placed with the default 1 KAS on every
/// covenant UTXO) leaves each ask a continuation and a custody remainder, 2n small covenant outputs (`4 × 10^12 / carrier`
/// storage mass each). Five asks fit (455,668 of 500,000) and six or more are over the block limit: the builder refuses the
/// transaction before anything is signed, and its message names the storage mass and what to do (fewer orders, or a larger
/// carrier) instead of a "dust payout". Asks placed with 2 KAS carry the same buy. The matcher never plans such a
/// transaction (it shrinks the plan, test above); the 3 / 3 programs are capped at three asks by their token slots.
#[tokio::test]
async fn a_wallet_buy_from_many_partial_asks_is_refused_before_signing_with_a_clear_message() {
    let c = Ctx::new();
    // eight asks of ten tokens at each carrier, created in blocks (their UTXOs are then the batch's inputs)
    let mut made: Vec<(u64, Vec<(AskState, SignedTx)>)> = vec![];
    for carrier in [DEFAULT_ORDER_CARRIER, 2 * KAS] {
        let mut txs = vec![];
        for i in 0..8u8 {
            let maker = 1 + i + if carrier == DEFAULT_ORDER_CARRIER { 0 } else { 10 };
            let a = AskState { amount_left: 10 * WHOLE, ..on_kr_ask(ask(maker, P250)) };
            let mut req = c.w.create(AnyState::KobAsk(a.clone()), carrier, maker, 10 * WHOLE);
            req.token_carrier = carrier;
            txs.push((a, c.w.sign(&Action::CreateOrder(req))));
        }
        let refs: Vec<&SignedTx> = txs.iter().map(|t| &t.1).collect();
        c.push(&refs).await;
        made.push((carrier, txs));
    }
    let try_buy = |carrier: u64, n: usize| -> Result<u64, String> {
        let txs = &made.iter().find(|m| m.0 == carrier).unwrap().1;
        let legs: Vec<Leg> = txs
            .iter()
            .take(n)
            .map(|(a, create)| {
                let cov = c.w.cov(create, 0);
                let custody = c.w.token_at(create, 1, Kcc20State::custody(10 * WHOLE, cov.0, EXT));
                Leg::Ask { order: c.w.order(create, 0, a.clone()), custody, amount: WHOLE, t: None }
            })
            .collect();
        let mut b = batch(&c.w, c.daa(), legs);
        b.taker_token_carrier = carrier;
        build(&Action::Batch(b)).map(|t| t.fee.mass.storage).map_err(|e| e.to_string())
    };
    // measured: one partial ask adds about 83,000 of storage mass at 1 KAS (123,766 for one, 455,668 for five)
    let five = try_buy(DEFAULT_ORDER_CARRIER, 5).expect("five partial asks fit at the default carrier");
    assert!(five <= BLOCK_STORAGE_LIMIT && five > BLOCK_STORAGE_LIMIT * 4 / 5, "{five}");
    for n in [6, 7, 8] {
        let e = try_buy(DEFAULT_ORDER_CARRIER, n).expect_err("a buy from so many partial asks is over the storage limit at 1 KAS");
        assert!(e.contains("storage mass") && e.contains("exceeds the block limit"), "{e}");
        assert!(e.contains("fill fewer orders") && e.contains("larger carrier"), "{e}");
        assert!(!e.contains("dust payout"), "no single output is dust: {e}");
        // asks placed with a larger carrier carry the same buy
        let ok = try_buy(2 * KAS, n).unwrap_or_else(|e| panic!("{n} asks at 2 KAS: {e}"));
        assert!(ok <= BLOCK_STORAGE_LIMIT, "{ok}");
    }
}
