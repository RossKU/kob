//! Regression: market queries read a bounded number of rows.
//!
//! `GET /v1/candles/{token}?from=0` used to load every fill of the token into memory (`limit` only trimmed the output), and the
//! 24 h stats scanned by event id without a time index. Now: the newest `MAX_FILL_ROWS` rows of the window at most, served by a
//! `(kind, token, ts)` index, and a `window_truncated` flag when the cap cut a 24 h window.

use kob_executor::hex::Hash32;
use kob_executor::indexer::db::open_memory;
use kob_executor::indexer::market;
use kob_executor::indexer::reads::ReadCtx;

fn seed(n: i64) -> (rusqlite::Connection, [u8; 32]) {
    let conn = open_memory("testnet-10").unwrap();
    let token = [0x70u8; 32];
    conn.execute(
        "INSERT INTO orders (covenant_id, contract, template_hash, family, side, in_book, genesis_state, genesis_block, genesis_daa, listed, min_fill, scale) \
         VALUES (?1, 'KobAsk', ?2, 1, 1, 1, ?3, 1, 1, 1, 1000, 1000)",
        rusqlite::params![vec![1u8; 32], vec![2u8; 32], vec![3u8; 8]],
    )
    .unwrap();
    let tx = conn.unchecked_transaction().unwrap();
    {
        let mut st = tx
            .prepare(
                "INSERT INTO order_events (covenant_id, block_seq, daa, ts, txid, tx_pos, kind, token_cov_id, side, amount, price, closes, detail) \
                 VALUES (?1, 1, ?2, ?3, ?4, 0, 'fill', ?5, 1, 1000, 250000000, 0, '{}')",
            )
            .unwrap();
        for i in 0..n {
            let mut txid = vec![0u8; 32];
            txid[..8].copy_from_slice(&i.to_le_bytes());
            st.execute(rusqlite::params![vec![1u8; 32], i, 1_700_000_000_000i64 + i * 1_000, txid, token.to_vec()]).unwrap();
        }
    }
    tx.commit().unwrap();
    (conn, token)
}

#[test]
fn candles_from_zero_read_at_most_the_row_cap_and_stats_say_when_they_were_cut() {
    let n = market::MAX_FILL_ROWS as i64 * 3;
    let (conn, token) = seed(n);
    let tok = Hash32(token);
    let all = market::candles(&conn, &tok, Some(3), "1m", 60_000, Some(0), None, 1).unwrap();
    assert_eq!(all.items.len(), 1, "one candle, the newest");
    let newest_ts = 1_700_000_000_000i64 + (n - 1) * 1_000;
    assert_eq!(all.items[0].t, newest_ts.div_euclid(60_000) * 60_000);

    let ctx = ReadCtx { node_daa: None, settle_depth_daa: 100, now_unix: None };
    let s = market::stats(&conn, &ctx, &tok, Some(3)).unwrap();
    assert!(s.window_truncated, "the 24 h window holds {n} fills, more than one query reads");
    assert!(s.trades_24h <= market::MAX_FILL_ROWS as u64);
    let small = seed(50);
    let s = market::stats(&small.0, &ctx, &Hash32(small.1), Some(3)).unwrap();
    assert!(!s.window_truncated);
    assert_eq!(s.trades_24h, 50);
}

#[test]
fn the_window_query_uses_the_time_index() {
    let (conn, _t) = seed(10);
    let plan: Vec<String> = conn
        .prepare(
            "EXPLAIN QUERY PLAN SELECT e.id FROM order_events e WHERE e.kind = 'fill' AND e.token_cov_id = ?1 AND e.ts >= ?2 AND e.ts < ?3 ORDER BY e.ts DESC LIMIT 5",
        )
        .unwrap()
        .query_map(rusqlite::params![vec![0x70u8; 32], 0i64, 1i64], |r| r.get::<_, String>(3))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert!(plan.iter().any(|p| p.contains("order_events_fill_ts")), "{plan:?}");
}
