//! Regression: market queries bound their memory, and candles never lose old history to a row cap.
//!
//! `GET /v1/candles/{token}?from=0` used to load every fill of the token into memory (`limit` only trimmed the output), and the
//! 24 h stats scanned by event id without a time index. The fix capped every window read at the newest `MAX_FILL_ROWS` rows,
//! which also cut the charts: once a token had more fills than the cap, its 1d / 1h charts started at the 20 000th newest
//! fill and the older days disappeared (the TN10 soak, 2026-10-14). Now candles stream every fill of the window through the
//! `(kind, token, ts)` index and hold only the newest `limit` candles; the 24 h stats keep the row cap and say when it cut
//! the window (`window_truncated`).

use kob_executor::hex::Hash32;
use kob_executor::indexer::db::open_memory;
use kob_executor::indexer::reads::ReadCtx;
use kob_executor::indexer::{market, pairs};

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
fn candles_keep_every_fill_beyond_the_row_cap_and_stats_say_when_they_were_cut() {
    let n = market::MAX_FILL_ROWS as i64 * 3;
    let (conn, token) = seed(n);
    let tok = Hash32(token);
    let first_ts = 1_700_000_000_000i64;
    let newest_ts = first_ts + (n - 1) * 1_000;
    // limit 1: the newest candle only (the output stays bounded by `limit` however many fills the window holds)
    let one = market::candles(&conn, &tok, Some(3), "1m", 60_000, Some(0), None, 1).unwrap();
    assert_eq!(one.items.len(), 1, "one candle, the newest");
    assert_eq!(one.items[0].t, newest_ts.div_euclid(60_000) * 60_000);
    // the whole history: the first candle is the first fill's bucket, and every fill is counted once
    for (name, iv) in [("1h", 3_600_000i64), ("1d", 86_400_000)] {
        let all = market::candles(&conn, &tok, Some(3), name, iv, None, None, 1500).unwrap();
        assert_eq!(all.items.first().unwrap().t, first_ts.div_euclid(iv) * iv, "{name}: the chart starts at the first trade");
        assert_eq!(all.items.iter().map(|c| c.trades).sum::<u64>(), n as u64, "{name}: every trade is in a candle");
    }
    // a 1m chart over more fills than the cap: the oldest candles are there too
    let m = market::candles(&conn, &tok, Some(3), "1m", 60_000, Some(0), None, 1500).unwrap();
    assert_eq!(m.items.len(), ((newest_ts / 60_000) - (first_ts / 60_000) + 1) as usize);
    assert_eq!(m.items[0].t, first_ts.div_euclid(60_000) * 60_000);

    let ctx = ReadCtx { node_daa: None, settle_depth_daa: 100, now_unix: None };
    let s = market::stats(&conn, &ctx, &tok, Some(3)).unwrap();
    assert!(s.window_truncated, "the 24 h window holds {n} fills, more than one query reads");
    assert!(s.trades_24h <= market::MAX_FILL_ROWS as u64);
    let small = seed(50);
    let s = market::stats(&small.0, &ctx, &Hash32(small.1), Some(3)).unwrap();
    assert!(!s.window_truncated);
    assert_eq!(s.trades_24h, 50);
}

/// Fills of transactions with one to three fills on both sides (an older ask order, a newer bid order), prices moving, several
/// transactions per block time.
fn seed_mixed(conn: &rusqlite::Connection, token: [u8; 32], ask: [u8; 32], bid: [u8; 32], txs: i64, salt: i64) {
    for (cov, side, gdaa) in [(ask, 1i64, 1i64), (bid, 2, 2)] {
        conn.execute(
            "INSERT INTO orders (covenant_id, contract, template_hash, family, side, in_book, genesis_state, genesis_block, genesis_daa, listed, min_fill, scale, token_cov_id) \
             VALUES (?1, 'KobAsk', ?2, 1, ?3, 1, ?4, 1, ?5, 1, 1, 1000, ?6)",
            rusqlite::params![cov.to_vec(), vec![2u8; 32], side, vec![3u8; 8], gdaa, token.to_vec()],
        )
        .unwrap();
    }
    let mut st = conn
        .prepare(
            "INSERT INTO order_events (covenant_id, block_seq, daa, ts, txid, tx_pos, kind, token_cov_id, side, amount, price, closes, detail) \
             VALUES (?1, 1, ?2, ?3, ?4, 0, 'fill', ?5, ?6, ?7, ?8, 0, '{}')",
        )
        .unwrap();
    for i in 0..txs {
        let mut txid = vec![0u8; 32];
        txid[..8].copy_from_slice(&(i + salt).to_le_bytes());
        txid[31] = token[0];
        let ts = 1_700_000_000_000i64 + (i / 3) * 40_000;
        for k in 0..(1 + (i + salt) % 3) {
            let (cov, side) = if k % 2 == 0 { (ask, 1i64) } else { (bid, 2i64) };
            let price = 250_000_000 + ((i * 7919 + k * 31 + salt) % 997) * 100_000;
            st.execute(rusqlite::params![cov.to_vec(), ts / 100, ts, txid.clone(), token.to_vec(), side, 100 + (i % 13) * 10, price])
                .unwrap();
        }
    }
}

#[test]
fn streamed_candles_equal_the_candles_of_every_trade() {
    let conn = open_memory("testnet-10").unwrap();
    let token = [0x71u8; 32];
    seed_mixed(&conn, token, [5u8; 32], [6u8; 32], 900, 0);
    // the reference: every fill read into memory, grouped into trades, bucketed
    let rows: Vec<market::FillRow> = conn
        .prepare(
            "SELECT e.id, e.txid, e.ts, e.daa, e.side, e.amount, e.price, o.scale, o.genesis_daa FROM order_events e \
             JOIN orders o ON o.covenant_id = e.covenant_id WHERE e.kind = 'fill' AND e.token_cov_id = ?1",
        )
        .unwrap()
        .query_map(rusqlite::params![token.to_vec()], |r| {
            Ok(market::FillRow {
                id: r.get(0)?,
                txid: r.get(1)?,
                ts: r.get(2)?,
                daa: r.get(3)?,
                side: r.get(4)?,
                amount: r.get(5)?,
                price: r.get(6)?,
                scale: r.get(7)?,
                genesis_daa: r.get(8)?,
            })
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    let trades = market::trades_of(rows, 1_000);
    for (name, iv) in [("1m", 60_000i64), ("5m", 300_000), ("1h", 3_600_000)] {
        let want = market::candles_of(&trades, iv);
        let got = market::candles(&conn, &Hash32(token), Some(3), name, iv, Some(0), None, 1500).unwrap();
        let row = |t: i64, o: String, h: String, l: String, c: String, v: String, q: String, n: u64| {
            format!("{t} {o} {h} {l} {c} {v} {q} {n}")
        };
        let got_rows: Vec<String> = got
            .items
            .iter()
            .map(|c| row(c.t, c.o.clone(), c.h.clone(), c.l.clone(), c.c.clone(), c.volume.clone(), c.quote_volume.clone(), c.trades))
            .collect();
        let want_rows: Vec<String> = want
            .iter()
            .map(|c| {
                row(
                    c.t,
                    c.o.to_string(),
                    c.h.to_string(),
                    c.l.to_string(),
                    c.c.to_string(),
                    c.volume.to_string(),
                    c.quote_volume.to_string(),
                    c.trades,
                )
            })
            .collect();
        assert_eq!(got_rows, want_rows, "{name}");
        // the newest k candles alone are the last k of the whole series
        let k = want.len().min(7);
        let tail = market::candles(&conn, &Hash32(token), Some(3), name, iv, Some(0), None, k).unwrap();
        assert_eq!(
            tail.items.iter().map(|c| c.t).collect::<Vec<_>>(),
            want[want.len() - k..].iter().map(|c| c.t).collect::<Vec<_>>(),
            "{name}"
        );
    }
}

#[test]
fn pair_candles_with_a_small_limit_are_the_tail_of_the_whole_series() {
    let conn = open_memory("testnet-10").unwrap();
    let (a, b) = ([0x72u8; 32], [0x73u8; 32]);
    seed_mixed(&conn, a, [7u8; 32], [8u8; 32], 600, 0);
    // B trades less often and starts later: buckets of A alone carry B's last close forward
    seed_mixed(&conn, b, [9u8; 32], [10u8; 32], 120, 5_000);
    conn.execute("UPDATE order_events SET ts = ts + 600000 WHERE token_cov_id = ?1", rusqlite::params![b.to_vec()]).unwrap();
    for (name, iv) in [("1m", 60_000i64), ("5m", 300_000)] {
        let all = pairs::pair_candles(&conn, &Hash32(a), &Hash32(b), (Some(3), Some(3)), name, iv, Some(0), None, 1500).unwrap();
        let n = all.items.len();
        assert!(n > 12, "{name}: {n}");
        for k in [1usize, 5, 12] {
            let tail = pairs::pair_candles(&conn, &Hash32(a), &Hash32(b), (Some(3), Some(3)), name, iv, Some(0), None, k).unwrap();
            assert_eq!(
                serde_json::to_string(&tail.items).unwrap(),
                serde_json::to_string(&all.items[n - k..]).unwrap(),
                "{name} limit {k}"
            );
        }
    }
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
