//! The per-order and per-request reads stay fast on a store of the size the soak reached (audit 2026-10-30 I9): one token with
//! over a million holdings (almost all spent), a token with hundreds of thousands of orders and no holding at all (a bid-only
//! token), thousands of placement events, thousands of tokens, and order covenant ids that were sent thousands of stray token
//! outputs. The store is a file (the upgrade path creates the indexes on it: `open_writer`), built with set-based SQL.
//!
//! `KOB_SCALE_ROWS` sets the holdings of the big token (default 200,000 so that CI stays quick; the soak-sized run is
//! `KOB_SCALE_ROWS=1100000 cargo test -p kob-executor --lib reads_scale -- --nocapture`). The time limits are generous multiples of
//! what the indexed reads take (sub-millisecond to a few milliseconds); the scans they replaced took 40 ms to 18 s.

use super::*;
use crate::indexer::db::open_writer;
use std::time::{Duration, Instant};

const T0: [u8; 32] = [0x70; 32]; // the big token: holdings, orders
const T1: [u8; 32] = [0x71; 32]; // the bid-only token: orders and events, no holding
const P8: kob_protocol::artifacts::TemplateId = kob_protocol::artifacts::TemplateId::Kcc20Ref8x8;

fn rows() -> usize {
    std::env::var("KOB_SCALE_ROWS").ok().and_then(|v| v.parse().ok()).unwrap_or(200_000)
}

fn tpl_hash() -> Vec<u8> {
    kob_protocol::artifacts::token_template(P8).hash.to_vec()
}

fn open_list() -> TokenAllowlist {
    let p = kob_protocol::profile::of(P8).unwrap();
    TokenAllowlist::from_entries([]).with_strict_templates([(
        Hash32::parse(&p.template_hash).unwrap(),
        crate::tokens::StrictTemplate { id: p.registry_id.clone(), family: kob_protocol::Family::Kcc20, powers: vec![] },
    )])
}

/// The indexes this fix adds: dropped after the load so that opening the store again is the upgrade of a store that lacks them.
const NEW_INDEXES: [&str; 6] = [
    "token_holdings_token_program",
    "token_holdings_token_page",
    "token_holdings_owner_page",
    "token_utxos_custody_live",
    "orders_token_tpl",
    "token_events_token_tpl",
];

fn load(path: &std::path::Path, n: usize) {
    let conn = open_writer(path, "testnet-10").unwrap();
    let pinned = kob_protocol::artifacts::pinned_hash(TemplateId::KobBid).to_vec();
    let pinned_ask = kob_protocol::artifacts::pinned_hash(TemplateId::KobAsk).to_vec();
    let tx = conn.unchecked_transaction().unwrap();
    // the big token's holdings: one owner holds a tenth of them, 19 in 20 are spent
    tx.execute(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?1) \
         INSERT INTO token_holdings (txid, idx, token_cov_id, program, family, owner, owner_kind, amount, value, state, role, \
                                     created_block, created_daa, spent_block, spent_txid) \
         SELECT randomblob(32), 0, ?2, ?3, 1, CASE WHEN i % 10 = 0 THEN x'aa00000000000000000000000000000000000000000000000000000000000000' \
                ELSE randomblob(32) END, 0, 5, 100000000, randomblob(112), 'owned', i / 100 + 1, i, \
                CASE WHEN i % 20 = 0 THEN NULL ELSE i / 100 + 2 END, NULL FROM n",
        rusqlite::params![n as i64, &T0[..], P8.name()],
    )
    .unwrap();
    // orders: the big token has n / 3 resting-less filled ones; the bid-only token n / 3; 2,000 tokens have 20 listed ones each
    let orders = |token: Vec<u8>, count: usize, contract: &str, template: &[u8], side: i64, status: &str| {
        tx.execute(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?1) \
             INSERT INTO orders (covenant_id, contract, template_hash, family, side, maker, token_cov_id, token_tpl_hash, scale, price, \
                                 in_book, genesis_state, genesis_block, genesis_daa, listed, origin) \
             SELECT randomblob(32), ?2, ?3, 1, ?4, randomblob(32), ?5, ?6, 1000, 1000, 1, randomblob(8), i / 50 + 1, i, 1, 'chain' FROM n",
            rusqlite::params![count as i64, contract, template, side, token, tpl_hash()],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO order_state (covenant_id, status, filled_amount, remaining_amount, amount_exact, state_known, last_block, last_daa) \
             SELECT covenant_id, ?1, 0, 0, 1, 1, 1, 1 FROM orders WHERE token_cov_id = ?2 AND covenant_id NOT IN (SELECT covenant_id FROM order_state)",
            rusqlite::params![status, &token],
        )
        .unwrap();
    };
    orders(T0.to_vec(), n / 3, "KobBid", &pinned, 2, "filled");
    orders(T1.to_vec(), n / 3, "KobBid", &pinned, 2, "filled");
    // 2,000 other tokens, 20 orders each (bids), open
    tx.execute(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 40000) \
         INSERT INTO orders (covenant_id, contract, template_hash, family, side, maker, token_cov_id, token_tpl_hash, scale, price, \
                             in_book, genesis_state, genesis_block, genesis_daa, listed, origin) \
         SELECT randomblob(32), 'KobBid', ?1, 1, 2, randomblob(32), CAST(printf('%032d', i % 2000 + 1000) AS BLOB), ?2, 1000, 1000, 1, \
                randomblob(8), i, i, 1, 'chain' FROM n",
        rusqlite::params![pinned, tpl_hash()],
    )
    .unwrap();
    // placement events of the bid-only token: a fresh commitment each (never pruned)
    tx.execute(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?1) \
         INSERT INTO token_events (token_cov_id, tpl_hash, ext_commit, kind, txid, block_seq, daa) \
         SELECT ?2, ?3, randomblob(32), 'seen', randomblob(32), i, i FROM n",
        rusqlite::params![(n / 15).max(1000) as i64, &T1[..], tpl_hash()],
    )
    .unwrap();
    // 60 live asks of the big token; two of their ids were sent 20,000 stray outputs each
    for k in 0..60i64 {
        let cov = vec![
            0xc0 + (k % 8) as u8,
            k as u8,
            1,
            2,
            3,
            4,
            5,
            6,
            7,
            8,
            9,
            10,
            11,
            12,
            13,
            14,
            15,
            16,
            17,
            18,
            19,
            20,
            21,
            22,
            23,
            24,
            25,
            26,
            27,
            28,
            29,
            30,
        ];
        tx.execute(
            "INSERT INTO orders (covenant_id, contract, template_hash, family, side, maker, token_cov_id, token_tpl_hash, scale, price, \
                                 in_book, genesis_state, genesis_block, genesis_daa, listed, origin) \
             VALUES (?1, 'KobAsk', ?2, 1, 1, x'01', ?3, ?4, 1000, 1000, 1, x'00', 1, 1, 1, 'chain')",
            rusqlite::params![cov, pinned_ask, &T0[..], tpl_hash()],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO order_state (covenant_id, status, filled_amount, remaining_amount, amount_exact, cur_txid, cur_idx, cur_value, \
                                      state_known, last_block, last_daa) VALUES (?1, 'open', 0, 10, 1, randomblob(32), 0, 1000, 1, 1, 1)",
            [&cov],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO token_utxos (txid, idx, token_cov_id, owner, amount, value, role, created_block, created_daa) \
             VALUES (randomblob(32), 0, ?1, ?2, 10, 1, 'custody', 1, 1)",
            rusqlite::params![&T0[..], cov],
        )
        .unwrap();
        if k < 2 {
            tx.execute(
                "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 20000) \
                 INSERT INTO token_utxos (txid, idx, token_cov_id, owner, amount, value, role, created_block, created_daa) \
                 SELECT randomblob(32), 0, ?1, ?2, 1, 1, 'stray', 1, 1 FROM n",
                rusqlite::params![&T0[..], cov],
            )
            .unwrap();
        }
    }
    tx.commit().unwrap();
    for ix in NEW_INDEXES {
        conn.execute_batch(&format!("DROP INDEX IF EXISTS {ix}")).unwrap();
    }
    if std::env::var("KOB_SCALE_NO_ANALYZE").is_err() {
        conn.execute_batch("ANALYZE").unwrap();
    }
}

fn timed<T>(what: &str, limit: Duration, f: impl FnOnce() -> T) -> T {
    let t = Instant::now();
    let r = f();
    let d = t.elapsed();
    eprintln!("scale  {what:<46} {d:>10.3?}");
    assert!(
        d <= limit || std::env::var("KOB_SCALE_BASELINE").is_ok(),
        "{what} took {d:?} (limit {limit:?}) on a store of {} holdings",
        rows()
    );
    r
}

#[test]
fn the_per_order_and_per_request_reads_stay_fast_on_a_store_of_soak_size() {
    let dir = std::env::temp_dir().join(format!("kob-scale-{}-{}", std::process::id(), rows()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("index.db");
    let n = rows();
    let t = Instant::now();
    load(&path, n);
    eprintln!("scale  loaded {n} holdings and the orders in {:.1?}", t.elapsed());
    // the upgrade: open_writer creates the indexes the store lacks
    let t = Instant::now();
    let conn = open_writer(&path, "testnet-10").unwrap();
    let upgrade = t.elapsed();
    eprintln!("scale  UPGRADE (index creation at open) of {n} holdings: {upgrade:.2?}");
    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    eprintln!("scale  database file {} MB", size / 1_000_000);
    let tokens = open_list();
    let ctx = ReadCtx { node_daa: Some(10_000), settle_depth_daa: 10, now_unix: Some(1_800_000_000) };
    let (t0, t1) = (Hash32(T0), Hash32(T1));
    let tpl = Hash32(<[u8; 32]>::try_from(tpl_hash()).unwrap());
    let fast = Duration::from_millis(50);
    let ms = |x: u64| Duration::from_millis(x);

    // 1. the proven programs (twice per evaluated order, once per claimed token in the list)
    let proven = timed("proven_programs (big token)", fast, || proven_programs(&conn, &T0).unwrap());
    assert_eq!(proven, vec![tpl.0.to_vec()]);
    assert!(timed("proven_programs (token without holdings)", fast, || proven_programs(&conn, &T1).unwrap()).is_empty());
    // 2. the standard scale an order is listed at, for a token with holdings and for a bid-only token (the ingest path per order)
    timed("open_standard_scale (big token)", fast, || open_standard_scale(&conn, &t0, Some(&tpl)).unwrap());
    timed("open_standard_scale (bid-only token)", fast, || open_standard_scale(&conn, &t1, Some(&tpl)).unwrap());
    // 3. the market views' decimals (trades, candles, stats, depth)
    timed("token_decimals (big token)", fast, || token_decimals(&conn, &tokens, &t0).unwrap());
    timed("token_decimals (bid-only token)", fast, || token_decimals(&conn, &tokens, &t1).unwrap());
    // 4. the open token list (recomputed every generation)
    let list = timed("token_list (2,002 open tokens)", ms(4_000), || token_list(&conn, &tokens).unwrap());
    assert!(list.len() >= 2_000, "{}", list.len());
    // 5. the holdings pages with the spent ones: by token, by the owner of a tenth of them, by both
    let f_tok = HoldingFilter { owner: None, token: Some(t0), include_spent: true };
    let f_own = HoldingFilter {
        owner: Some(vec![0xaa, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
        token: None,
        include_spent: true,
    };
    let page = timed("holdings?token&spent=true (page 1)", fast, || holdings(&conn, &ctx, &f_tok, 100, None).unwrap());
    assert_eq!(page.items.len(), 100);
    timed("holdings?token&spent=true (next page)", fast, || {
        holdings(&conn, &ctx, &f_tok, 100, page.next_cursor.as_deref().and_then(parse_holding_cursor)).unwrap()
    });
    timed("holdings?owner&spent=true (page 1)", fast, || holdings(&conn, &ctx, &f_own, 100, None).unwrap());
    let both = HoldingFilter { token: Some(t0), ..f_own.clone() };
    timed("holdings?owner&token&spent=true", fast, || holdings(&conn, &ctx, &both, 100, None).unwrap());
    // 6. the book of the big token: every live ask checks its custody (two of them have 20,000 strays)
    let b = timed("book (60 live asks, 2 with 20,000 strays)", ms(500), || book(&conn, &ctx, &t0, 100, false).unwrap());
    assert!(matches!(b.asks, BookSide::Orders(ref o) if o.len() == 60), "all 60 asks are liquidity");
    // 7. the placement identities of a bid-only token (the stats and trades read them)
    timed("market_scale (bid-only token)", fast, || market_scale(&conn, &T1).unwrap());
    drop(conn);
    let _ = std::fs::remove_dir_all(&dir);
}
