//! The activity totals (`GET /v1/stats`) through the real write path: a fill of a real transaction records its fee and the script
//! that funded it (the record log's payer section), a reorg takes both back out of the totals, the same transaction accepted again
//! puts them back, a rebuild from the record log gives the same totals, and a log of frames written before payers were recorded
//! replays with every figure but the executors (unknown, not counted).

mod common;

use common::*;
use kob_protocol::build::*;
use kob_protocol::state::*;
use kob_protocol::tx::{FeeOptions, KeyUtxo, SignedTx};
use kobd::indexer::activity::{self, ActivityView};
use kobd::indexer::db::open_memory;
use kobd::indexer::recordlog::{self, RecordLog};
use kobd::testkit::*;
use rusqlite::Connection;

fn totals(conn: &Connection) -> ActivityView {
    activity::activity(conn, None).unwrap()
}

fn live(c: &Ctx) -> ActivityView {
    totals(c.hs.ingest.lock().unwrap().conn())
}

/// (orders, fills, traders, executors, fees paid)
fn figures(v: &ActivityView) -> (i64, i64, i64, i64, String) {
    (v.orders, v.fills, v.traders, v.executors, v.fees_paid.clone())
}

#[tokio::test]
async fn a_fill_counts_its_fee_and_executor_and_a_reorg_takes_them_back() {
    let dir = tempfile::tempdir().unwrap();
    let (log, _) = RecordLog::open(dir.path(), 1 << 20, 0).unwrap();
    let c = Ctx::with(Harness::new(open_memory("testnet-10").unwrap(), Some(log)));
    let a = ask(MAKER_A, P250);
    let create = c.w.create_tx(AnyState::KobAsk(a.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    assert_eq!(figures(&live(&c)), (1, 0, 1, 0, "0".into()));
    let placed = c.hs.snapshot();

    // the taker's batch fills 4 whole tokens at the order's tip: 4 x TIP sompi to the filler, funded by the taker's coin
    let custody = c.w.token_at(&create, 1, Kcc20State::custody(10 * WHOLE, cov.0, EXT));
    let leg = Leg::Ask { order: c.w.order(&create, 0, a.clone()), custody, amount: 4 * WHOLE, t: None };
    let fill = c.w.sign(&Action::Batch(batch(&c.w, c.daa(), vec![leg])));
    c.push(&[&fill]).await;
    let fee = activity::fill_fee(Some(4 * WHOLE), Some(TIP), Some(SCALE)).unwrap();
    assert_eq!(fee, 4 * TIP);
    assert_eq!(figures(&live(&c)), (1, 1, 1, 1, fee.to_string()));
    assert_eq!(c.hs.query::<i64>("SELECT fee FROM order_events WHERE kind = 'fill'", []), fee);
    // the executor is the script of the taker's funding input
    let spk: Vec<u8> = c.hs.query("SELECT s.spk FROM order_events e JOIN scripts s ON s.id = e.executor_id WHERE e.kind = 'fill'", []);
    let funding = fill.tx.inputs.iter().find(|i| i.utxo.covenant_id.is_none()).expect("a funding input");
    assert_eq!(kobd::hex::encode(&spk), funding.utxo.script_public_key);
    // a window that starts after the fill counts nothing but what it holds
    let after: i64 = c.hs.query("SELECT MAX(ts) + 60000 FROM order_events", []);
    let w = activity::activity(c.hs.ingest.lock().unwrap().conn(), Some(after)).unwrap();
    assert_eq!(figures(&w), (0, 0, 0, 0, "0".into()));

    // the fill's block is reorged out: the totals are the ones before it, row for row
    c.hs.node.reorg(1, vec![vec![]]);
    c.hs.sync().await;
    assert_eq!(figures(&live(&c)), (1, 0, 1, 0, "0".into()));
    let rows =
        |s: &str| s.lines().filter(|l| l.starts_with("activity ") || l.starts_with("actor ")).map(str::to_string).collect::<Vec<_>>();
    assert_eq!(rows(&c.hs.snapshot()), rows(&placed));

    // accepted again in the next block
    c.w.include(&c.hs.node, &[&fill]);
    c.hs.sync().await;
    let filled = live(&c);
    assert_eq!(figures(&filled), (1, 1, 1, 1, fee.to_string()));
    let snap = c.hs.snapshot();
    drop(c);

    // a database rebuilt from the record log has the same totals and rows
    let logged = recordlog::read_all(dir.path()).unwrap();
    let rebuilt = |records: Vec<_>| {
        let mut ing =
            kobd::indexer::ingest::Ingest::new(open_memory("testnet-10").unwrap(), processor(), None).with_config(wide_window());
        ing.init_cursor(&kobd::indexer::ingest::Cursor { hash: logged.records[0].1.start, daa: 0 }).unwrap();
        ing.replay(records).unwrap();
        ing
    };
    let ing = rebuilt(logged.records.clone());
    assert_eq!(totals(ing.conn()), filled);
    assert_eq!(snapshot(ing.conn()), snap);

    // the same log as a build before payers wrote it: every figure but the executors, who are unknown
    let mut old = logged.records.clone();
    for (_, b) in old.iter_mut() {
        for t in b.blocks.iter_mut().flat_map(|b| b.txs.iter_mut()) {
            t.payer = None;
            t.in_value = None;
        }
        // such a frame is format 2 (no payer section, no declaration)
        assert_eq!(b.encode(1)[..2], [0, recordlog::FORMAT]);
    }
    let ing = rebuilt(old);
    let v = totals(ing.conn());
    assert_eq!(figures(&v), (1, 1, 1, 0, fee.to_string()));
    assert_eq!(
        ing.conn().query_row("SELECT COUNT(*) FROM order_events WHERE executor_id IS NOT NULL", [], |r| r.get::<_, i64>(0)).unwrap(),
        0
    );
    // a frame with a payer and input values is format 5, with a payer alone format 4
    assert!(logged.records.iter().any(|(_, b)| b.encode(1)[..2] == [0, recordlog::FORMAT_IN_VALUE]));
    let mut payer_only = logged.records.clone();
    for (_, b) in payer_only.iter_mut() {
        for t in b.blocks.iter_mut().flat_map(|b| b.txs.iter_mut()) {
            t.in_value = None;
        }
    }
    assert!(payer_only.iter().any(|(_, b)| b.encode(1)[..2] == [0, recordlog::FORMAT_PAYER]));
}

/// (executor income, its transactions, unknown ones, miner fees, their transactions, unknown ones, tips paid)
fn earned(v: &ActivityView) -> (String, i64, i64, String, i64, i64, String) {
    (
        v.executor_income.clone(),
        v.executor_income_txs,
        v.executor_income_unknown_txs,
        v.miner_fees.clone(),
        v.miner_fee_txs,
        v.miner_fee_unknown_txs,
        v.tips_paid.clone(),
    )
}

/// An ask and a bid cross in one executor's transaction (a partial fill of both): what the transaction leaves its sender is the bid's
/// all-in spend minus the ask's all-in proceeds, hand-computed (4 whole tokens, scale 1 000): 4 x (2.60 + 0.001 tip) KAS released by the
/// bid against 4 x (2.50 - 0.001 tip) KAS paid to the ask's maker = 0.408 KAS, of which 0.008 KAS are the two tips (the only figure the
/// old `fees_paid` saw). The network fee is the transaction's own; a reorg takes all of it back, a rebuild from the record log gives it
/// again, and a log of records without input values (before 2026-11-12) leaves the fee unknown and counted as such.
#[tokio::test]
async fn a_crossing_pays_its_executor_the_spread_and_the_tips_and_its_fee_to_the_miners() {
    let dir = tempfile::tempdir().unwrap();
    let (log, _) = RecordLog::open(dir.path(), 1 << 20, 0).unwrap();
    let c = Ctx::with(Harness::new(open_memory("testnet-10").unwrap(), Some(log)));
    let (a, b) = (ask(MAKER_A, P250), bid(MAKER_B, P260));
    let create_a = c.w.create_tx(AnyState::KobAsk(a.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    let create_b = c.w.create_tx(AnyState::KobBid(b.clone()), b.escrow(10 * WHOLE, 3).unwrap() as u64, MAKER_B, 0);
    c.push(&[&create_a, &create_b]).await;
    let placed = live(&c);
    assert_eq!(earned(&placed), ("0".into(), 0, 0, "0".into(), 0, 0, "0".into()), "placements are the makers' own");
    let cov_a = c.w.cov(&create_a, 0);
    let custody = c.w.token_at(&create_a, 1, Kcc20State::custody(10 * WHOLE, cov_a.0, EXT));
    let legs = vec![
        Leg::Ask { order: c.w.order(&create_a, 0, a.clone()), custody, amount: 4 * WHOLE, t: None },
        Leg::Bid { order: c.w.order(&create_b, 0, b.clone()), amount: 4 * WHOLE, t: None },
    ];
    let fill = c.w.sign(&Action::Batch(batch(&c.w, c.daa(), legs)));
    c.push(&[&fill]).await;
    let income = 4 * (P260 + TIP) - 4 * (P250 - TIP);
    assert_eq!(income, 40_800_000);
    let fee = fill.fee.fee as i64;
    assert!(fee > 0);
    let v = live(&c);
    assert_eq!(earned(&v), (income.to_string(), 1, 0, fee.to_string(), 1, 0, (8 * TIP).to_string()));
    assert_eq!((v.executor_net.clone(), v.executor_net_txs), ((income - fee).to_string(), 1));
    assert_eq!(v.fees_paid, v.tips_paid, "the old name is the tip sum");
    assert!(c.check_income(&fill), "the builder's own figures agree");

    // reorged out and accepted again
    c.hs.node.reorg(1, vec![vec![]]);
    c.hs.sync().await;
    assert_eq!(earned(&live(&c)), earned(&placed));
    assert_eq!(c.hs.query::<i64>("SELECT COUNT(*) FROM executor_txs", []), 0);
    c.w.include(&c.hs.node, &[&fill]);
    c.hs.sync().await;
    let filled = live(&c);
    assert_eq!(earned(&filled), earned(&v));
    let snap = c.hs.snapshot();
    drop(c);

    let logged = recordlog::read_all(dir.path()).unwrap();
    let rebuilt = |records: Vec<_>| {
        let mut ing =
            kobd::indexer::ingest::Ingest::new(open_memory("testnet-10").unwrap(), processor(), None).with_config(wide_window());
        ing.init_cursor(&kobd::indexer::ingest::Cursor { hash: logged.records[0].1.start, daa: 0 }).unwrap();
        ing.replay(records).unwrap();
        ing
    };
    let ing = rebuilt(logged.records.clone());
    assert_eq!(totals(ing.conn()), filled);
    assert_eq!(snapshot(ing.conn()), snap);
    assert!(logged.records.iter().any(|(_, b)| b.encode(1)[..2] == [0, recordlog::FORMAT_IN_VALUE]));
    // the same log without input values: the income stays known, the fee is unknown and counted as such
    let mut old = logged.records.clone();
    for (_, b) in old.iter_mut() {
        for t in b.blocks.iter_mut().flat_map(|b| b.txs.iter_mut()) {
            t.in_value = None;
        }
    }
    let v = totals(rebuilt(old).conn());
    assert_eq!(earned(&v), (income.to_string(), 1, 0, "0".into(), 0, 1, (8 * TIP).to_string()));
    assert_eq!((v.executor_net.as_str(), v.executor_net_txs), ("0", 0), "net only where both are known");
}

/// A keeper's refund of an expired ask keeps the order's refund tip: the order UTXO and its custody carrier go back to the maker less it.
#[tokio::test]
async fn a_keepers_refund_earns_its_refund_tip() {
    let c = Ctx::new();
    let a = AskState { expiry_daa: c.daa() as i64 + 2_000, ..ask(MAKER_A, P250) };
    let create = c.w.create_tx(AnyState::KobAsk(a.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    let custody = c.w.token_at(&create, 1, Kcc20State::custody(10 * WHOLE, cov.0, EXT));
    let state = AnyState::KobAsk(a.clone());
    let due = state.refund_due(c.w.utxo(&create, 0).block_daa_score as i64).unwrap();
    let refund = c.w.sign(&Action::RefundOrder(RefundOrder {
        prefund: None,
        order: c.w.order(&create, 0, state),
        foreign: vec![],
        custody: Some(custody),
        lock_time: due as u64,
        funding: vec![c.w.coin(KEEPER, 10)],
        change: Some(pk(KEEPER)),
        fee: FeeOptions::default(),
    }));
    c.push(&[&refund]).await;
    assert_eq!(c.hs.status(&cov), "refunded");
    // the maker's output at the order's index: both carriers less the tip
    assert_eq!(refund.tx.outputs[0].value as i64, 2 * CARRIER as i64 - a.refund_tip);
    let v = live(&c);
    assert_eq!(earned(&v), (a.refund_tip.to_string(), 1, 0, refund.fee.fee.to_string(), 1, 0, "0".into()));
    assert!(c.check_income(&refund));
}

/// A lean store prunes the closed orders and their history, but not the totals: all time is everything it ever indexed (the orders,
/// fills, makers, executors, tips, income and fees).
#[tokio::test]
async fn the_lean_pruning_does_not_take_back_the_totals() {
    let c = Ctx::with(Harness::lean(8));
    let a = ask(MAKER_A, P250);
    let create = c.w.create_tx(AnyState::KobAsk(a.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    let custody = c.w.token_at(&create, 1, Kcc20State::custody(10 * WHOLE, cov.0, EXT));
    let leg = Leg::Ask { order: c.w.order(&create, 0, a.clone()), custody, amount: 10 * WHOLE, t: None };
    let fill = c.w.sign(&Action::Batch(batch(&c.w, c.daa(), vec![leg])));
    c.push(&[&fill]).await;
    assert_eq!(c.hs.status(&cov), "filled");
    let before = live(&c);
    assert_eq!((before.orders, before.fills, before.traders, before.executors), (1, 1, 1, 1));
    // the taker bought the tokens for itself: an own trade, not counted in the income (its network fee is)
    assert_eq!((before.executor_income_txs, before.executor_income_own_trade_txs, before.miner_fee_txs), (0, 1, 1));
    let rows =
        |c: &Ctx| (c.hs.query::<i64>("SELECT COUNT(*) FROM orders", []), c.hs.query::<i64>("SELECT COUNT(*) FROM executor_txs", []));
    assert_eq!(rows(&c), (1, 1));
    // the reorg window passes: the filled order and the executor's transaction are pruned
    c.hs.node.push_empty(20);
    c.hs.sync().await;
    assert_eq!(rows(&c), (0, 0), "pruned");
    let after = live(&c);
    assert_eq!(ActivityView { ts: before.ts, ..after.clone() }, before, "every total as before");
    assert_eq!(c.hs.query::<i64>("SELECT COUNT(*) FROM meta WHERE k = 'pruning'", []), 0, "the key never outlives the pruning");
}

/// The network fee of a record without the node's input values (a frame written before 2026-11-12) is still known when every input
/// spends an output the store recorded: here a second crossing funded by the first one's change (its other inputs are the orders and
/// the custody). The first crossing's funding came from outside KOB: its fee stays unknown. Live, the node's values are used and the
/// recorded ones must agree with them (the processor asserts it); a reorg of the second block un-spends the change, and its replay spends
/// it again.
#[tokio::test]
async fn a_fee_is_resolved_from_the_recorded_outputs_when_the_record_has_no_input_values() {
    let dir = tempfile::tempdir().unwrap();
    let (log, _) = RecordLog::open(dir.path(), 1 << 20, 0).unwrap();
    let c = Ctx::with(Harness::new(open_memory("testnet-10").unwrap(), Some(log)));
    let (a, b) = (ask(MAKER_A, P250), bid(MAKER_B, P260));
    let create_a = c.w.create_tx(AnyState::KobAsk(a.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    let create_b = c.w.create_tx(AnyState::KobBid(b.clone()), b.escrow(10 * WHOLE, 3).unwrap() as u64, MAKER_B, 0);
    c.push(&[&create_a, &create_b]).await;
    let (cov_a, cov_b) = (c.w.cov(&create_a, 0), c.w.cov(&create_b, 0));
    let custody = c.w.token_at(&create_a, 1, Kcc20State::custody(10 * WHOLE, cov_a.0, EXT));
    let legs = vec![
        Leg::Ask { order: c.w.order(&create_a, 0, a.clone()), custody, amount: 4 * WHOLE, t: None },
        Leg::Bid { order: c.w.order(&create_b, 0, b.clone()), amount: 4 * WHOLE, t: None },
    ];
    let fill = c.w.sign(&Action::Batch(batch(&c.w, c.daa(), legs)));
    c.push(&[&fill]).await;
    // the second crossing spends the first one's change
    let change = fill.fee.change_output.expect("a change output") as usize;
    let a6 = AskState { amount_left: 6 * WHOLE, ..a.clone() };
    let custody = c.w.token_at(&fill, find_custody(&fill, &cov_a, 6 * WHOLE).unwrap(), Kcc20State::custody(6 * WHOLE, cov_a.0, EXT));
    let legs = vec![
        Leg::Ask { order: c.w.order(&fill, find_cov(&fill, &cov_a).unwrap(), a6), custody, amount: 2 * WHOLE, t: None },
        Leg::Bid { order: c.w.order(&fill, find_cov(&fill, &cov_b).unwrap(), b.clone()), amount: 2 * WHOLE, t: None },
    ];
    let mut second = batch(&c.w, c.daa(), legs);
    second.funding = vec![KeyUtxo { utxo: c.w.utxo(&fill, change), pubkey: pk(TAKER) }];
    let fill2 = c.w.sign(&Action::Batch(second));
    c.push(&[&fill2]).await;
    let (fee1, fee2) = (fill.fee.fee as i64, fill2.fee.fee as i64);
    let income = 6 * (P260 + TIP) - 6 * (P250 - TIP);
    let v = live(&c);
    assert_eq!(earned(&v), (income.to_string(), 2, 0, (fee1 + fee2).to_string(), 2, 0, (12 * TIP).to_string()));
    // the second block reorged out and in again: the change it spent is unspent, then spent again
    c.hs.node.reorg(1, vec![vec![]]);
    c.hs.sync().await;
    let spent = |c: &Ctx| c.hs.query::<i64>("SELECT COUNT(*) FROM recorded_outputs WHERE spent_block IS NOT NULL", []);
    assert_eq!(spent(&c), 0);
    c.w.include(&c.hs.node, &[&fill2]);
    c.hs.sync().await;
    assert_eq!(spent(&c), 1, "the first crossing's change");
    assert_eq!(earned(&live(&c)), earned(&v));
    drop(c);

    let logged = recordlog::read_all(dir.path()).unwrap();
    let mut old = logged.records.clone();
    for (_, b) in old.iter_mut() {
        for t in b.blocks.iter_mut().flat_map(|b| b.txs.iter_mut()) {
            t.in_value = None;
        }
    }
    let mut ing = kobd::indexer::ingest::Ingest::new(open_memory("testnet-10").unwrap(), processor(), None).with_config(wide_window());
    ing.init_cursor(&kobd::indexer::ingest::Cursor { hash: logged.records[0].1.start, daa: 0 }).unwrap();
    ing.replay(old).unwrap();
    // the second fee is resolved (and is the node's), the first is not
    let r = totals(ing.conn());
    assert_eq!(earned(&r), (income.to_string(), 2, 0, fee2.to_string(), 1, 1, (12 * TIP).to_string()));
    let fees: Vec<Option<i64>> = {
        let mut st = ing.conn().prepare("SELECT miner_fee FROM executor_txs ORDER BY id").unwrap();
        let v = st.query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect();
        v
    };
    assert_eq!(fees, [None, Some(fee2)]);
}

/// (counted income, counted transactions, own trades, unknown) of the totals
fn counted(v: &ActivityView) -> (String, i64, i64, i64) {
    (v.executor_income.clone(), v.executor_income_txs, v.executor_income_own_trade_txs, v.executor_income_unknown_txs)
}

/// The own-trade flag of the transaction's `executor_txs` row.
fn own_trade(c: &Ctx, t: &SignedTx) -> i64 {
    c.hs.query("SELECT own_trade FROM executor_txs WHERE txid = ?1", [&t.tx.id[..]])
}

/// "Paid to executors" is what traders paid executors for matching and keeping: a transaction in which the sender trades for itself is
/// an own trade, not counted (its network fee is). An inventory buy (the sender takes an ask's tokens into its own output) and an
/// inventory sale (the sender's own tokens go into a bid's delivery) are own trades; a crossing of two traders' orders and a keeper's
/// refund count. The sender filling its own ask and keeping the tokens is an own trade too (the tokens come to it); its income (the
/// price it paid itself, less nothing) is kept on the row but never in the totals.
#[tokio::test]
async fn an_executor_trading_for_itself_is_not_counted_as_paid_to_it() {
    let c = Ctx::new();
    // a trader's ask and bid, and an ask of the executor's own key (TAKER)
    let (a, b) = (ask(MAKER_A, P250), bid(MAKER_B, P260));
    let create_a = c.w.create_tx(AnyState::KobAsk(a.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    let create_b = c.w.create_tx(AnyState::KobBid(b.clone()), b.escrow(10 * WHOLE, 3).unwrap() as u64, MAKER_B, 0);
    let own = ask(TAKER, P250);
    let create_own = c.w.create_tx(AnyState::KobAsk(own.clone()), CARRIER, TAKER, 10 * WHOLE);
    c.push(&[&create_a, &create_b, &create_own]).await;
    let (cov_a, cov_own) = (c.w.cov(&create_a, 0), c.w.cov(&create_own, 0));

    // 1. a pure match: the traders' ask and bid cross (4 whole tokens): counted, 4 x (2.601 - 2.499) KAS
    let custody = c.w.token_at(&create_a, 1, Kcc20State::custody(10 * WHOLE, cov_a.0, EXT));
    let legs = vec![
        Leg::Ask { order: c.w.order(&create_a, 0, a.clone()), custody, amount: 4 * WHOLE, t: None },
        Leg::Bid { order: c.w.order(&create_b, 0, b.clone()), amount: 4 * WHOLE, t: None },
    ];
    let matched = c.w.sign(&Action::Batch(batch(&c.w, c.daa(), legs)));
    c.push(&[&matched]).await;
    let spread = 4 * (P260 + TIP) - 4 * (P250 - TIP);
    assert_eq!(own_trade(&c, &matched), 0);
    assert_eq!(counted(&live(&c)), (spread.to_string(), 1, 0, 0));

    // 2. an inventory buy: the executor takes 2 whole tokens of the trader's ask into its own token output
    let a6 = AskState { amount_left: 6 * WHOLE, ..a.clone() };
    let custody =
        c.w.token_at(&matched, find_custody(&matched, &cov_a, 6 * WHOLE).unwrap(), Kcc20State::custody(6 * WHOLE, cov_a.0, EXT));
    let leg = Leg::Ask { order: c.w.order(&matched, find_cov(&matched, &cov_a).unwrap(), a6), custody, amount: 2 * WHOLE, t: None };
    let bought = c.w.sign(&Action::Batch(batch(&c.w, c.daa(), vec![leg])));
    c.push(&[&bought]).await;
    assert_eq!(own_trade(&c, &bought), 1);
    let v = live(&c);
    assert_eq!(counted(&v), (spread.to_string(), 1, 1, 0), "not counted: the executor paid for tokens it keeps");
    assert_eq!(v.miner_fee_txs, 2, "its network fee is counted");

    // 3. an inventory sale: the executor's own tokens go into the trader's bid (6 whole tokens left to buy)
    let cov_b = c.w.cov(&create_b, 0);
    let mut sale = batch(
        &c.w,
        c.daa(),
        vec![Leg::Bid { order: c.w.order(&matched, find_cov(&matched, &cov_b).unwrap(), b.clone()), amount: 2 * WHOLE, t: None }],
    );
    sale.taker_tokens = vec![c.w.token(TAKER, 2 * WHOLE)];
    let sold = c.w.sign(&Action::Batch(sale));
    c.push(&[&sold]).await;
    assert_eq!(own_trade(&c, &sold), 1);
    assert_eq!(counted(&live(&c)), (spread.to_string(), 1, 2, 0));

    // 4. the executor fills its own ask and keeps the tokens: an own trade (the tokens come to it)
    let custody = c.w.token_at(&create_own, 1, Kcc20State::custody(10 * WHOLE, cov_own.0, EXT));
    let leg = Leg::Ask { order: c.w.order(&create_own, 0, own.clone()), custody, amount: 3 * WHOLE, t: None };
    let self_fill = c.w.sign(&Action::Batch(batch(&c.w, c.daa(), vec![leg])));
    c.push(&[&self_fill]).await;
    assert_eq!(own_trade(&c, &self_fill), 1);
    assert_eq!(counted(&live(&c)), (spread.to_string(), 1, 3, 0));

    // 5. a keeper's refund of an expired ask of a trader: counted (the refund tip)
    let e = AskState { expiry_daa: c.daa() as i64 + 2_000, ..ask(MAKER_B, P245) };
    let create_e = c.w.create_tx(AnyState::KobAsk(e.clone()), CARRIER, MAKER_B, 10 * WHOLE);
    c.push(&[&create_e]).await;
    let cov_e = c.w.cov(&create_e, 0);
    let state = AnyState::KobAsk(e.clone());
    let due = state.refund_due(c.w.utxo(&create_e, 0).block_daa_score as i64).unwrap();
    let refund = c.w.sign(&Action::RefundOrder(RefundOrder {
        prefund: None,
        order: c.w.order(&create_e, 0, state),
        foreign: vec![],
        custody: Some(c.w.token_at(&create_e, 1, Kcc20State::custody(10 * WHOLE, cov_e.0, EXT))),
        lock_time: due as u64,
        funding: vec![c.w.coin(KEEPER, 10)],
        change: Some(pk(KEEPER)),
        fee: FeeOptions::default(),
    }));
    c.push(&[&refund]).await;
    assert_eq!(own_trade(&c, &refund), 0);
    let v = live(&c);
    assert_eq!(counted(&v), ((spread + e.refund_tip).to_string(), 2, 3, 0));
    assert_eq!(v.miner_fee_txs, 5, "every network fee, own trades too");
    // the net is over the counted transactions only
    let fees: i64 = [&matched, &refund].iter().map(|t| t.fee.fee as i64).sum();
    assert_eq!((v.executor_net.clone(), v.executor_net_txs), ((spread + e.refund_tip - fees).to_string(), 2));
}

/// A fill's tip is at most the KAS the fill moved for its order, and an order whose state fails the sanity check records none.
///
/// `KobAsk` requires only `price >= tip` and pays its maker `ceil(n x (price - tip) / scale)`. Asks at `price = tip` filled one base
/// unit each pay their makers nothing but their carriers, and their terms named a tip of 2^60 (sane) or 2.9e18 (not sane) sompi per
/// fill. Eight such fills in one minute summed the bucket's fees past 64 bits, into a float, which a revert then took back inexactly.
#[tokio::test]
async fn a_tip_is_at_most_the_kas_its_fill_moved_and_an_insane_order_records_none() {
    let c = Ctx::with(kobd::testkit::Harness::new(open_memory("testnet-10").unwrap(), None));
    let (sane, insane) = (1i64 << 60, activity::MAX_FEE);
    let tops = [sane, sane, sane, sane, sane, sane, sane, sane, insane];
    let asks: Vec<AskState> = tops
        .iter()
        .map(|&top| AskState { price: top, tip: top, scale: 1, min_fill: 1, amount_left: 1, ..ask(MAKER_A, P250) })
        .collect();
    let creates: Vec<SignedTx> = asks.iter().map(|a| c.w.create_tx(AnyState::KobAsk(a.clone()), CARRIER, MAKER_A, 1)).collect();
    c.push(&creates.iter().collect::<Vec<_>>()).await;
    let leg = |k: usize| {
        let cov = c.w.cov(&creates[k], 0);
        let custody = c.w.token_at(&creates[k], 1, Kcc20State::custody(1, cov.0, EXT));
        Leg::Ask { order: c.w.order(&creates[k], 0, asks[k].clone()), custody, amount: 1, t: None }
    };
    // A transaction of the token's program spends at most 3 token inputs: three batches of three asks, in one block.
    let fills = [0, 3, 6].map(|k| c.w.sign(&Action::Batch(batch(&c.w, c.daa(), (k..k + 3).map(leg).collect()))));
    c.push(&fills.iter().collect::<Vec<_>>()).await;
    let mut paid = 0i64;
    for (k, create) in creates.iter().enumerate() {
        let cov = c.w.cov(create, 0);
        let fee: Option<i64> = c.hs.query(
            "SELECT fee FROM order_events WHERE kind = 'fill' AND order_id = (SELECT id FROM orders WHERE covenant_id = ?1)",
            [&cov.0[..]],
        );
        // What the order moved: the KAS of its UTXO (a sold-out ask has no continuation), or its payout if that is more.
        let fill = &fills[k / 3];
        let input = fill.tx.inputs.iter().position(|i| i.transaction_id == create.tx.id && i.index == 0).expect("the order's input");
        let moved = (CARRIER as i64).max(fill.tx.outputs[input].value as i64);
        if tops[k] == sane {
            assert_eq!(fee, Some(moved), "ask {k}: the tip its terms name ({sane}) is bounded by the KAS the fill moved");
            paid += moved;
        } else {
            assert_eq!(fee, None, "ask {k}: an order that fails the sanity check records no tip");
        }
    }
    assert!(paid < 1 << 40);
    let v = live(&c);
    assert_eq!((v.fills, v.tips_paid.clone()), (tops.len() as i64, paid.to_string()));
    let real: i64 = c.hs.query("SELECT COUNT(*) FROM activity WHERE typeof(fees) != 'integer'", []);
    assert_eq!(real, 0, "every bucket's fees stay an integer");
    // A reorg takes the tips back exactly.
    c.hs.node.reorg(1, vec![vec![]]);
    c.hs.sync().await;
    let v = live(&c);
    assert_eq!((v.fills, v.tips_paid), (0, "0".to_string()));
}
