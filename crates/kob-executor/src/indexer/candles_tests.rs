//! The maintained candle aggregates answer exactly what the fills do (the reference path, `market::scan_candles`): random histories
//! with multi-fill transactions, several scales, reverts, a changed price basis and windows that start or end inside a bucket; and
//! the same over a history of a million fills with the timings of every interval.

use super::*;
use crate::indexer::db::{open_memory, open_writer};
use crate::indexer::market::{candles as api_candles, scan_candles};
use crate::tokens::TokenAllowlist;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 17
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

const PINNED: &str = "KobBid";

fn token(n: u8) -> Hash32 {
    Hash32([n; 32])
}

fn add_order(conn: &Connection, cov: u8, tok: u8, scale: i64, genesis_block: i64, genesis_daa: i64) {
    let pinned = kob_protocol::artifacts::pinned_hash(kob_protocol::artifacts::TemplateId::KobBid).to_vec();
    conn.execute(
        "INSERT INTO orders (covenant_id, contract, template_hash, family, side, maker, token_cov_id, scale, price, in_book, genesis_state, \
         genesis_block, genesis_daa, listed, origin) VALUES (?1, ?2, ?3, 1, 2, x'01', ?4, ?5, 1000, 1, x'00', ?6, ?7, 1, 'chain')",
        params![vec![cov; 32], PINNED, pinned, vec![tok; 32], scale, genesis_block, genesis_daa],
    )
    .unwrap();
}

/// One transaction of `fills` fills (order, side, amount, price) at `ts`, in block `seq`, marking the buckets as the processor does.
fn add_tx(conn: &Connection, txid: u64, seq: i64, ts: i64, tok: u8, fills: &[(u8, i64, i64, i64)]) {
    for (cov, side, amount, price) in fills {
        conn.execute(
            "INSERT INTO order_events (covenant_id, block_seq, daa, ts, txid, tx_pos, kind, token_cov_id, side, amount, price, closes) \
             VALUES (?1, ?2, ?3, ?4, ?5, 0, 'fill', ?6, ?7, ?8, ?9, 0)",
            params![vec![*cov; 32], seq, ts / 100, ts, txid.to_be_bytes().repeat(4), vec![tok; 32], side, amount, price],
        )
        .unwrap();
        mark(conn, &[tok; 32], ts).unwrap();
    }
}

fn revert(conn: &Connection, seq: i64) {
    mark_block(conn, seq).unwrap();
    conn.execute("DELETE FROM order_events WHERE block_seq = ?1", [seq]).unwrap();
}

/// The aggregates equal the reference over `[from, to)`; `None` from the aggregates is allowed only where the reference path is the
/// designed answer (a window inside one bucket).
fn same(conn: &Connection, tokens: &TokenAllowlist, tok: &Hash32, iv: i64, from: i64, to: i64, keep: usize) {
    let basis = token_basis(conn, tokens, tok).unwrap();
    let want = scan_candles(conn, tok, basis, iv, from, to, keep).unwrap();
    match read(conn, tok, basis, iv, from, to, keep).unwrap() {
        Some(got) => assert_eq!(got, want, "iv {iv} window [{from}, {to}) keep {keep}"),
        None => {
            assert!(to - from < 2 * iv || meta_basis(conn, &tok.0).unwrap() != Some(basis), "no answer for [{from}, {to}) at {iv}");
        }
    }
}

#[test]
fn random_histories_reverts_and_windows_match_the_reference() {
    let tokens = TokenAllowlist::from_entries([]);
    for seed in 1..=6u64 {
        let conn = open_memory("testnet-10").unwrap();
        let mut r = Rng(seed * 7919);
        // two tokens; token 1 has orders of two scales, so a fill converts
        add_order(&conn, 1, 1, 100_000_000, 1, 10);
        add_order(&conn, 2, 1, 1_000, 2, 20);
        add_order(&conn, 3, 2, 1_000_000, 1, 30);
        let mut ts = 1_700_000_000_000i64 + r.below(60_000) as i64;
        let mut seq = 0i64;
        let mut live_blocks: Vec<i64> = vec![];
        let mut txid = 0u64;
        for step in 0..260 {
            if r.below(9) == 0 && !live_blocks.is_empty() {
                // a reorg of the newest block (and sometimes the one before)
                for _ in 0..1 + r.below(2) {
                    if let Some(s) = live_blocks.pop() {
                        revert(&conn, s);
                    }
                }
            } else {
                seq += 1;
                live_blocks.push(seq);
                // time moves forward by anything from 0 ms (same bucket, same ts) to a few days
                ts += match r.below(6) {
                    0 => 0,
                    1 => r.below(2_000) as i64,
                    2 => r.below(120_000) as i64,
                    3 => r.below(7_200_000) as i64,
                    4 => r.below(172_800_000) as i64,
                    _ => 59_999,
                };
                for _ in 0..1 + r.below(3) {
                    txid += 1;
                    let tok = 1 + r.below(2) as u8;
                    let n = 1 + r.below(4);
                    let fills: Vec<(u8, i64, i64, i64)> = (0..n)
                        .map(|_| {
                            let cov = if tok == 1 { 1 + r.below(2) as u8 } else { 3 };
                            (cov, 1 + r.below(2) as i64, 1 + r.below(900) as i64, 500 + r.below(300) as i64)
                        })
                        .collect();
                    add_tx(&conn, txid, seq, ts, tok, &fills);
                }
            }
            refresh(&conn, &tokens).unwrap();
            let dirty: i64 = conn.query_row("SELECT COUNT(*) FROM candle_dirty", [], |r| r.get(0)).unwrap();
            assert_eq!(dirty, 0);
            if step % 4 == 0 {
                for t in [token(1), token(2)] {
                    for iv in IVS {
                        let end = ts + 1;
                        // aligned windows ending at the newest bucket, and arbitrary (unaligned) ones
                        let aligned_end = (ts.div_euclid(iv) + 1) * iv;
                        for keep in [1usize, 3, 500] {
                            same(&conn, &tokens, &t, iv, aligned_end - iv * 20, aligned_end, keep);
                            same(&conn, &tokens, &t, iv, 0, aligned_end, keep);
                            let from = ts - r.below(4_000_000_000) as i64;
                            same(&conn, &tokens, &t, iv, from, end, keep);
                            same(&conn, &tokens, &t, iv, from, from + r.below(10 * iv as u64) as i64, keep);
                        }
                        same(&conn, &tokens, &t, iv, ts + 1, ts + 5, 3);
                        same(&conn, &tokens, &t, iv, 5, 4, 3);
                    }
                }
            }
        }
        // the whole history, by interval: the API view equals the reference too
        for t in [token(1), token(2)] {
            for (name, iv) in [("1m", IVS[0]), ("5m", IVS[1]), ("1h", IVS[2]), ("1d", IVS[3])] {
                let basis = token_basis(&conn, &tokens, &t).unwrap();
                let view = api_candles(&conn, &t, None, name, iv, None, None, 500).unwrap();
                let newest: Option<i64> = conn
                    .query_row("SELECT MAX(ts) FROM order_events WHERE kind = 'fill' AND token_cov_id = ?1", [t.0.to_vec()], |r| {
                        r.get(0)
                    })
                    .unwrap();
                let end = newest.map(|n| (n.div_euclid(iv) + 1) * iv).unwrap_or(0);
                let want = scan_candles(&conn, &t, basis, iv, end.saturating_sub(iv * 500), end, 500).unwrap();
                assert_eq!(view.items.len(), want.len());
                for (v, w) in view.items.iter().zip(&want) {
                    assert_eq!(
                        (v.t, &v.o, &v.h, &v.l, &v.c),
                        (w.t, &w.o.to_string(), &w.h.to_string(), &w.l.to_string(), &w.c.to_string())
                    );
                    assert_eq!((&v.volume, &v.quote_volume, v.trades), (&w.volume.to_string(), &w.quote_volume.to_string(), w.trades));
                }
            }
        }
    }
}

/// A token whose price basis changed (its first order is now one of another scale, as when its decimals become known) is answered
/// from the fills until its next fill rebuilds the rows at the new basis; the numbers are the new basis's either way.
#[test]
fn a_changed_basis_is_never_answered_from_rows_of_the_old_one() {
    let tokens = TokenAllowlist::from_entries([]);
    let conn = open_memory("testnet-10").unwrap();
    add_order(&conn, 1, 1, 1_000, 5, 50);
    let t0 = 1_700_000_000_000i64;
    for i in 0..30u64 {
        add_tx(&conn, i + 1, 1 + i as i64, t0 + i as i64 * 61_000, 1, &[(1, 1, 7 * (i as i64 + 1), 1_234 + i as i64)]);
    }
    refresh(&conn, &tokens).unwrap();
    let t = token(1);
    let old = token_basis(&conn, &tokens, &t).unwrap();
    assert_eq!(old, 1_000);
    assert_eq!(meta_basis(&conn, &t.0).unwrap(), Some(1_000));
    same(&conn, &tokens, &t, IVS[1], t0 - 1_000_000, t0 + 4_000_000, 500);
    // an order of another scale, older than the first: the basis is now its scale
    add_order(&conn, 2, 1, 100_000_000, 1, 10);
    let new = token_basis(&conn, &tokens, &t).unwrap();
    assert_eq!(new, 100_000_000);
    assert!(read(&conn, &t, new, IVS[1], t0 - 1_000_000, t0 + 4_000_000, 500).unwrap().is_none(), "the old rows do not answer");
    same(&conn, &tokens, &t, IVS[1], t0 - 1_000_000, t0 + 4_000_000, 500);
    // the next fill rebuilds the token at the new basis
    add_tx(&conn, 99, 99, t0 + 40 * 61_000, 1, &[(1, 1, 5, 1_300)]);
    refresh(&conn, &tokens).unwrap();
    assert_eq!(meta_basis(&conn, &t.0).unwrap(), Some(new));
    for iv in IVS {
        same(&conn, &tokens, &t, iv, t0 - 10 * iv, t0 + 100 * iv, 500);
        assert!(read(&conn, &t, new, iv, t0.div_euclid(iv) * iv - 10 * iv, t0.div_euclid(iv) * iv + 100 * iv, 500).unwrap().is_some());
    }
}

/// A lean store keeps no aggregates and reads from the fills.
#[test]
fn a_lean_store_keeps_no_aggregates() {
    let dir = tempfile::tempdir().unwrap();
    let conn = open_writer(&dir.path().join("lean.db"), "testnet-10").unwrap();
    assert!(enabled(&conn));
    add_order(&conn, 1, 1, 1_000, 1, 1);
    add_tx(&conn, 1, 1, 1_700_000_000_000, 1, &[(1, 1, 5, 1_000)]);
    refresh(&conn, &TokenAllowlist::from_entries([])).unwrap();
    assert_eq!(conn.query_row::<i64, _, _>("SELECT COUNT(*) FROM candles", [], |r| r.get(0)).unwrap(), 4);
    configure(&conn, true).unwrap();
    assert!(!enabled(&conn));
    assert_eq!(conn.query_row::<i64, _, _>("SELECT COUNT(*) FROM candles", [], |r| r.get(0)).unwrap(), 0);
    mark(&conn, &[1; 32], 5).unwrap();
    assert_eq!(conn.query_row::<i64, _, _>("SELECT COUNT(*) FROM candle_dirty", [], |r| r.get(0)).unwrap(), 0);
    let t = token(1);
    assert!(read(&conn, &t, 1_000, IVS[0], 0, 1 << 50, 5).unwrap().is_none());
}

/// A million fills (about 330,000 transactions of one token over eleven days): the backfill builds the rows, every interval's chart is
/// the reference's byte for byte, and each takes well under 100 ms. `KOB_CANDLE_FILLS` sets the size (default 150,000 so that CI is
/// quick; the soak-sized run is `KOB_CANDLE_FILLS=1000000 cargo test -p kob-executor --lib candles::tests::a_large -- --nocapture`).
#[test]
fn a_large_history_is_charted_in_milliseconds_and_equals_the_reference() {
    use std::time::Instant;
    let n: usize = std::env::var("KOB_CANDLE_FILLS").ok().and_then(|v| v.parse().ok()).unwrap_or(150_000);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.db");
    let conn = open_writer(&path, "testnet-10").unwrap();
    // 300 orders over two scales for one token; the fills: three per transaction, one transaction a second
    let t0 = 1_700_000_000_000i64;
    conn.execute(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?1) \
         INSERT INTO order_events (covenant_id, block_seq, daa, ts, txid, tx_pos, kind, token_cov_id, side, amount, price, closes) \
         SELECT CAST(printf('%032d', i % 300) AS BLOB), i / 300 + 1, i, ?2 + (i / 3) * 1000, CAST(printf('%032d', i / 3) AS BLOB), 0, 'fill', \
                ?3, 1 + (i % 2), 1 + (i * 7919) % 5000, 1000 + (i * 104729) % 700, 0 FROM n",
        params![n as i64, t0, vec![1u8; 32]],
    )
    .unwrap();
    // the orders the fills name (their covenant ids are the zero-padded numbers above)
    let pinned = kob_protocol::artifacts::pinned_hash(kob_protocol::artifacts::TemplateId::KobBid).to_vec();
    conn.execute(
        "WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i + 1 FROM n WHERE i < 299) \
         INSERT INTO orders (covenant_id, contract, template_hash, family, side, maker, token_cov_id, scale, price, in_book, genesis_state, \
                             genesis_block, genesis_daa, listed, origin) \
         SELECT CAST(printf('%032d', i) AS BLOB), 'KobBid', ?1, 1, 2, x'01', ?2, CASE WHEN i % 2 = 0 THEN 100000000 ELSE 1000 END, 1000, 1, x'00', \
                1, 10 + i, 1, 'chain' FROM n",
        params![pinned, vec![1u8; 32]],
    )
    .unwrap();
    let tokens = TokenAllowlist::from_entries([]);
    let t = Instant::now();
    let report = backfill(&conn, &tokens).unwrap();
    eprintln!("candles  backfill of {n} fills: {:?} ({} token, {} rows)", t.elapsed(), report.tokens, report.rows);
    assert_eq!(report.tokens, 1);
    assert_eq!(backfill(&conn, &tokens).unwrap(), Backfill::default(), "a second run has nothing to do");
    let tok = token(1);
    let basis = token_basis(&conn, &tokens, &tok).unwrap();
    for (name, iv) in [("1m", IVS[0]), ("5m", IVS[1]), ("1h", IVS[2]), ("1d", IVS[3])] {
        let t = Instant::now();
        let view = api_candles(&conn, &tok, None, name, iv, None, None, 500).unwrap();
        let fast = t.elapsed();
        let t = Instant::now();
        let end = {
            let newest: i64 = conn.query_row("SELECT MAX(ts) FROM order_events", [], |r| r.get(0)).unwrap();
            (newest.div_euclid(iv) + 1) * iv
        };
        let want = scan_candles(&conn, &tok, basis, iv, end - iv * 500, end, 500).unwrap();
        let scan = t.elapsed();
        eprintln!("candles  {name:>2}: {fast:>10.2?} from the aggregates, {scan:>10.2?} from the fills ({} candles)", want.len());
        assert!(fast < std::time::Duration::from_millis(100), "{name} took {fast:?}");
        assert_eq!(view.items.len(), want.len());
        for (v, w) in view.items.iter().zip(&want) {
            assert_eq!(
                (v.t, v.o.as_str(), v.h.as_str(), v.l.as_str(), v.c.as_str(), v.volume.as_str(), v.quote_volume.as_str(), v.trades),
                (
                    w.t,
                    w.o.to_string().as_str(),
                    w.h.to_string().as_str(),
                    w.l.to_string().as_str(),
                    w.c.to_string().as_str(),
                    w.volume.to_string().as_str(),
                    w.quote_volume.to_string().as_str(),
                    w.trades
                )
            );
        }
        // the whole history, and an unaligned window
        same(&conn, &tokens, &tok, iv, 0, end, 1_000_000);
        same(&conn, &tokens, &tok, iv, t0 + 12_345, t0 + 3 * iv + 777, 500);
    }
    // the 24 h statistics read the newest 20,000 fills of the window at most
    let ctx = reads::ReadCtx { node_daa: Some(1 << 40), settle_depth_daa: 10, now_unix: None };
    let t = Instant::now();
    let stats = crate::indexer::market::stats(&conn, &ctx, &tok, None).unwrap();
    let took = t.elapsed();
    eprintln!("candles  24h stats: {took:?} ({} trades in the window)", stats.trades_24h);
    assert!(took < std::time::Duration::from_secs(1), "stats took {took:?}");
    // keeping up: a new transaction updates the buckets in a few queries
    let t = Instant::now();
    let ts = t0 + (n as i64 / 3 + 5) * 1000;
    add_tx(&conn, 9_000_000, 9_000, ts, 1, &[(1, 1, 10, 1_500), (3, 2, 4, 1_400)]);
    refresh(&conn, &tokens).unwrap();
    eprintln!("candles  one new transaction: {:?}", t.elapsed());
    for iv in IVS {
        same(&conn, &tokens, &tok, iv, ts - 3 * iv, ts + 10 * iv, 500);
    }
    // a reorg of that block
    revert(&conn, 9_000);
    refresh(&conn, &tokens).unwrap();
    for iv in IVS {
        same(&conn, &tokens, &tok, iv, ts - 3 * iv, ts + 10 * iv, 500);
    }
}

/// A rollback to an earlier build (which writes fills without maintaining the aggregates) and a second upgrade: the stale rows are
/// discarded and rebuilt at the start, never served.
#[test]
fn fills_written_by_a_build_that_does_not_maintain_the_aggregates_are_rebuilt_at_the_next_start() {
    let tokens = TokenAllowlist::from_entries([]);
    let conn = open_memory("testnet-10").unwrap();
    add_order(&conn, 1, 1, 1_000, 1, 1);
    let t0 = 1_700_000_000_000i64;
    for i in 0..10u64 {
        add_tx(&conn, i + 1, 1 + i as i64, t0 + i as i64 * 30_000, 1, &[(1, 1, 5 + i as i64, 1_000 + i as i64)]);
    }
    refresh(&conn, &tokens).unwrap();
    let tok = token(1);
    assert_eq!(backfill(&conn, &tokens).unwrap(), Backfill::default(), "up to date: nothing to do");
    // the earlier build: a new fill and a reverted one, no aggregates touched
    conn.execute("DELETE FROM order_events WHERE block_seq = 10", []).unwrap();
    conn.execute(
        "INSERT INTO order_events (covenant_id, block_seq, daa, ts, txid, tx_pos, kind, token_cov_id, side, amount, price, closes)          VALUES (?1, 11, 11, ?2, ?3, 0, 'fill', ?4, 1, 99, 5000, 0)",
        params![vec![1u8; 32], t0 + 700_000, vec![0xabu8; 32], vec![1u8; 32]],
    )
    .unwrap();
    // the second upgrade: the stale rows are dropped and rebuilt
    let report = backfill(&conn, &tokens).unwrap();
    assert_eq!(report.tokens, 1);
    for iv in IVS {
        same(&conn, &tokens, &tok, iv, t0 - 10 * iv, t0 + 100 * iv, 500);
        assert!(read(&conn, &tok, 1_000, iv, t0.div_euclid(iv) * iv - 10 * iv, t0.div_euclid(iv) * iv + 100 * iv, 500)
            .unwrap()
            .is_some());
    }
    assert_eq!(backfill(&conn, &tokens).unwrap(), Backfill::default());
}
