//! Audit 2026-10-30 I3 / I4 regressions: the candles window never overflows, and an order view bounds its strays.

use kob_executor::hex::Hash32;
use kob_executor::indexer::db::open_memory;
use kob_executor::indexer::reads::{ReadCtx, MAX_VIEW_STRAYS};
use kob_executor::indexer::{market, pairs, reads};
use kob_executor::testkit::*;

mod common;

#[test]
fn candles_with_an_extreme_to_do_not_overflow() {
    let conn = open_memory("testnet-10").unwrap();
    let tok = Hash32(TOKEN_COV);
    for to in [i64::MIN, i64::MIN + 1, i64::MAX, 0, -1] {
        let r = market::candles(&conn, &tok, Some(3), "1d", 86_400_000, None, Some(to), 1500).unwrap();
        assert!(r.items.is_empty());
        let p =
            pairs::pair_candles(&conn, &tok, &Hash32([0x71; 32]), (Some(3), Some(3)), "1d", 86_400_000, None, Some(to), 1500).unwrap();
        assert!(p.items.is_empty());
    }
    // an inverted window (`from` after `to`) is empty too
    let r = market::candles(&conn, &tok, Some(3), "1d", 86_400_000, Some(10), Some(5), 10).unwrap();
    assert!(r.items.is_empty());
}

#[tokio::test]
async fn an_order_view_bounds_its_strays() {
    let c = common::Ctx::new();
    let a = ask(MAKER_A, 250_000_000);
    let create = c.w.create_tx(kob_protocol::state::AnyState::KobAsk(a.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    let ctx = ReadCtx { node_daa: Some(c.daa()), settle_depth_daa: 100, now_unix: None };
    let seed = |n: usize| {
        let g = c.hs.ingest.lock().unwrap();
        let tx = g.conn().unchecked_transaction().unwrap();
        {
            let mut st = tx
                .prepare(
                    "INSERT OR IGNORE INTO token_utxos (txid, idx, token_cov_id, owner, amount, value, role, created_block, created_daa) \
                     VALUES (?1, 0, ?2, ?3, 1, 8000000, 'stray', 1, 1)",
                )
                .unwrap();
            for i in 0..n {
                let mut txid = vec![0xEEu8; 32];
                txid[..8].copy_from_slice(&(i as u64).to_le_bytes());
                st.execute(rusqlite::params![txid, TOKEN_COV.to_vec(), cov.0.to_vec()]).unwrap();
            }
        }
        tx.commit().unwrap();
    };
    let view = || {
        let g = c.hs.ingest.lock().unwrap();
        reads::order(g.conn(), &ctx, &cov).unwrap().unwrap()
    };
    // within the bound: all of them, no total
    seed(MAX_VIEW_STRAYS);
    let v = view();
    assert_eq!((v.strays.as_ref().unwrap().len(), v.strays_total), (MAX_VIEW_STRAYS, None));
    // beyond it: the first MAX_VIEW_STRAYS and the count
    seed(3_000);
    let v = view();
    assert_eq!((v.strays.as_ref().unwrap().len(), v.strays_total), (MAX_VIEW_STRAYS, Some(3_000)));
    assert!(serde_json::to_vec(&v).unwrap().len() < 200_000);
    // the custody is still found beside them
    assert!(v.custody.as_ref().unwrap().ok);
}
