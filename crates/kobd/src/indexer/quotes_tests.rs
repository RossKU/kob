//! The top-of-book sampling: change-only rows, empty sides, retention, and the bucketed read.

use super::*;
use crate::indexer::db::open_memory;
use crate::indexer::ids::intern;

const TOKEN: u8 = 0x11;
const MIN: i64 = 60_000;
/// A minute well after the epoch.
const T0: i64 = 1_790_000_040_000;

fn cid(n: u8) -> Vec<u8> {
    vec![n; 32]
}

fn ctx() -> ReadCtx {
    ReadCtx { node_daa: Some(1_000), settle_depth_daa: 0, now_unix: Some(100) }
}

/// A live listed plain order of scale 1 000 at `price` sompi per 1 000 base units, 10 000 left.
fn order(c: &Connection, id: u8, side: i64, price: i64) {
    let tpl = kob_protocol::artifacts::pinned_hash(if side == 1 {
        kob_protocol::artifacts::TemplateId::KobAsk
    } else {
        kob_protocol::artifacts::TemplateId::KobBid
    });
    c.execute(
        "INSERT INTO orders (covenant_id, contract, template_id, family, side, maker_id, token_id, scale, min_fill, price, tip, \
         expiry_daa, in_book, initial_amount, genesis_state, genesis_block, genesis_daa, listed) \
         VALUES (?1, ?2, ?3, 1, ?4, ?5, ?6, 1000, 1000, ?7, 0, 100000000, 1, 10000, x'00', 1, 100, 1)",
        params![
            cid(id),
            if side == 1 { "KobAsk" } else { "KobBid" },
            intern(c, &tpl).unwrap(),
            side,
            intern(c, &cid(0xaa)).unwrap(),
            intern(c, &cid(TOKEN)).unwrap(),
            price
        ],
    )
    .unwrap();
    c.execute(
        "INSERT INTO order_state (order_id, status, filled_amount, remaining_amount, amount_exact, cur_txid, cur_idx, cur_value, \
         state_known, last_block, last_daa) VALUES ((SELECT id FROM orders WHERE covenant_id = ?1), 'open', 0, 10000, 1, ?2, 0, 1000, 1, 1, 900)",
        params![cid(id), cid(id + 100)],
    )
    .unwrap();
    if side == 1 {
        c.execute(
            "INSERT INTO token_utxos (txid, idx, token_id, order_id, amount, value, role, created_block, created_daa) \
             VALUES (?1, 1, (SELECT id FROM ids WHERE h = ?2), (SELECT id FROM orders WHERE covenant_id = ?3), 10000, 1000, 'custody', 1, 900)",
            params![cid(id + 100), cid(TOKEN), cid(id)],
        )
        .unwrap();
    }
}

fn close(c: &Connection, id: u8) {
    c.execute("UPDATE order_state SET status = 'filled' WHERE order_id = (SELECT id FROM orders WHERE covenant_id = ?1)", [cid(id)])
        .unwrap();
}

type Row = (i64, Option<i64>, Option<i64>, Option<i64>, Option<i64>);

fn rows(c: &Connection) -> Vec<Row> {
    c.prepare("SELECT t, bid_price, bid_scale, ask_price, ask_scale FROM book_quotes ORDER BY t")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

#[test]
fn a_row_only_when_a_side_changes_and_an_empty_side_is_null() {
    let c = open_memory("testnet-10").unwrap();
    // No book: nothing to write.
    assert_eq!(sample(&c, &ctx(), T0).unwrap(), 0);
    order(&c, 1, 1, 300_000);
    // One side only: the bid is null. Seconds into the minute round down.
    assert_eq!(sample(&c, &ctx(), T0 + 5_000).unwrap(), 1);
    assert_eq!(rows(&c), vec![(T0, None, None, Some(300_000), Some(1000))]);
    // Unchanged for minutes: no rows.
    for m in 1..5 {
        assert_eq!(sample(&c, &ctx(), T0 + m * MIN).unwrap(), 0);
    }
    // A better ask and a bid in one minute: one row.
    order(&c, 2, 1, 290_000);
    order(&c, 3, 2, 200_000);
    assert_eq!(sample(&c, &ctx(), T0 + 5 * MIN).unwrap(), 1);
    // A worse ask behind the best one changes nothing.
    order(&c, 4, 1, 310_000);
    assert_eq!(sample(&c, &ctx(), T0 + 6 * MIN).unwrap(), 0);
    // The book empties: a row of two nulls, then nothing more.
    for id in [1, 2, 3, 4] {
        close(&c, id);
    }
    assert_eq!(sample(&c, &ctx(), T0 + 7 * MIN).unwrap(), 1);
    assert_eq!(sample(&c, &ctx(), T0 + 8 * MIN).unwrap(), 0);
    assert_eq!(
        rows(&c),
        vec![
            (T0, None, None, Some(300_000), Some(1000)),
            (T0 + 5 * MIN, Some(200_000), Some(1000), Some(290_000), Some(1000)),
            (T0 + 7 * MIN, None, None, None, None),
        ]
    );
    // A clock that stepped back writes nothing.
    order(&c, 5, 2, 210_000);
    assert_eq!(sample(&c, &ctx(), T0 + 2 * MIN).unwrap(), 0);
}

#[test]
fn rows_older_than_the_retention_go_and_an_unchanged_book_is_written_again() {
    let c = open_memory("testnet-10").unwrap();
    order(&c, 1, 1, 300_000);
    sample(&c, &ctx(), T0).unwrap();
    order(&c, 2, 2, 200_000);
    sample(&c, &ctx(), T0 + MIN).unwrap();
    assert_eq!(rows(&c).len(), 2);
    // At the edge of the retention nothing goes.
    sample(&c, &ctx(), T0 + RETENTION_MS).unwrap();
    assert_eq!(rows(&c).len(), 2);
    // Past it the first row goes; the book did not change, so the second stays the newest.
    assert_eq!(sample(&c, &ctx(), T0 + RETENTION_MS + MIN).unwrap(), 1);
    assert_eq!(rows(&c).iter().map(|r| r.0).collect::<Vec<_>>(), vec![T0 + MIN]);
    // Past both: the old row goes and the book is written again at the minute.
    let t = T0 + RETENTION_MS + 2 * MIN;
    assert_eq!(sample(&c, &ctx(), t).unwrap(), 2);
    assert_eq!(rows(&c), vec![(t, Some(200_000), Some(1000), Some(300_000), Some(1000))]);
    // A token whose book emptied and whose rows aged out leaves nothing behind.
    close(&c, 1);
    close(&c, 2);
    sample(&c, &ctx(), t + MIN).unwrap();
    sample(&c, &ctx(), t + MIN + RETENTION_MS + MIN).unwrap();
    assert_eq!(rows(&c), vec![]);
}

#[test]
fn the_read_keeps_the_last_value_of_each_bucket_and_carries_the_value_in_force() {
    let c = open_memory("testnet-10").unwrap();
    let put = |t: i64, bid: Option<i64>, ask: Option<i64>| {
        c.execute(
            "INSERT INTO book_quotes VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![cid(TOKEN), t, bid, bid.map(|_| 1000), ask, ask.map(|_| 1000)],
        )
        .unwrap();
    };
    let h = 3_600_000;
    let base = T0 - T0 % h;
    put(base, Some(100), Some(200));
    put(base + MIN, Some(110), Some(200));
    put(base + h + 2 * MIN, None, Some(190));
    put(base + 3 * h, Some(120), Some(180));
    let token = Hash32([TOKEN; 32]);
    let items = |from: Option<i64>, to: Option<i64>, limit: usize| -> Vec<(i64, Option<String>, Option<String>)> {
        // Decimals 3: the basis is the scale, 1 000, so a price reads as stored.
        quotes(&c, &token, Some(3), "1h", h, from, to, limit).unwrap().items
    };
    let s = |v: i64| Some(v.to_string());
    // The whole history: the last row of each hour, empty hours omitted.
    assert_eq!(items(None, None, 500), vec![(base, s(110), s(200)), (base + h, None, s(190)), (base + 3 * h, s(120), s(180))]);
    // From the third hour: the value in force first, at that hour.
    assert_eq!(items(Some(base + 2 * h), None, 500), vec![(base + 2 * h, None, s(190)), (base + 3 * h, s(120), s(180))]);
    // `to` bounds the window; `limit` keeps the newest.
    assert_eq!(items(None, Some(base + 2 * h), 500), vec![(base, s(110), s(200)), (base + h, None, s(190))]);
    assert_eq!(items(None, None, 1), vec![(base + 3 * h, s(120), s(180))]);
    // At another basis the prices convert: decimals 2 make the basis 100.
    let v = quotes(&c, &token, Some(2), "1h", h, None, None, 1).unwrap();
    assert_eq!((v.price_basis.as_str(), &v.items[0].1), ("100", &s(12)));
    // An unknown token has none.
    assert!(quotes(&c, &Hash32([9; 32]), Some(3), "1h", h, None, None, 500).unwrap().items.is_empty());
}
