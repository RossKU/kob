//! The read paths that start from the live set or from an index in page order return exactly what the history-wide
//! queries they replaced returned (rows, order, pages, cursors): random stores, every filter, every page.

use super::*;
use crate::indexer::db::open_memory;
use rusqlite::params;

/// A small deterministic generator.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 17
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
    fn pick<T: Clone>(&mut self, v: &[T]) -> T {
        v[self.below(v.len() as u64) as usize].clone()
    }
    fn bytes(&mut self) -> Vec<u8> {
        (0..32).map(|_| self.next() as u8).collect()
    }
}

const STATUSES: [&str; 7] = ["open", "partial", "filled", "cancelled", "refunded", "killed", "closed"];

fn tok(n: u8) -> Vec<u8> {
    vec![n; 32]
}

/// A store of `n` orders over tokens 1..=3 (pair orders quote 1 or 2, some of A / A), makers 1..=3, empty and none, ties of
/// genesis blocks, every status (the current UTXO set exactly for `open` / `partial`, sometimes spent: a stale state),
/// custody rows (none, one, two, the wrong amount), frozen flags, token identities and holdings (ties of created blocks).
fn store(seed: u64, n: usize) -> Connection {
    let conn = open_memory("testnet-10").unwrap();
    let mut r = Rng(seed);
    let contracts = ["KobAsk", "KobBid", "KobAskKron", "KobCondAsk", "KobIfdBid", "KobPair"];
    let makers: Vec<Option<Vec<u8>>> = vec![Some(vec![1; 32]), Some(vec![2; 32]), Some(vec![3; 32]), Some(vec![]), None];
    for _ in 0..n {
        let cov = r.bytes();
        let contract = r.pick(&contracts);
        let token: Option<Vec<u8>> = r.pick(&[Some(tok(1)), Some(tok(2)), Some(tok(3)), None]);
        let quote: Option<Vec<u8>> = if r.below(3) == 0 { r.pick(&[Some(tok(1)), Some(tok(2)), token.clone()]) } else { None };
        let side = 1 + r.below(2) as i64;
        let price = r.below(50) as i64 + 1;
        let price = r.pick(&[Some(price), None]);
        let (genesis_out_block, genesis_daa) = (r.below(12) as i64, r.below(1000) as i64);
        let listed = (r.below(4) != 0) as i64;
        // the template the order was indexed under: the one this build pins for its contract, else (a database an older build
        // indexed) another one, left out of every read
        let pinned = TemplateId::from_name(contract).map(kob_protocol::artifacts::pinned_hash).unwrap();
        let template = if r.below(6) == 0 { vec![9u8; 32] } else { pinned.to_vec() };
        conn.execute(
            "INSERT INTO orders (covenant_id, contract, template_hash, family, side, maker, token_cov_id, token_tpl_hash, scale, price, \
             tif, expiry_daa, active_from, in_book, deadline, genesis_state, genesis_txid, genesis_out, genesis_block, genesis_daa, \
             listed, origin, quote_cov_id) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,0,?18,?19,?20,'chain',?21)",
            params![
                cov,
                contract,
                template,
                r.pick(&[1i64, 2]),
                side,
                r.pick(&makers),
                token,
                r.pick(&[Some(vec![7u8; 32]), Some(vec![8u8; 32]), None]),
                r.pick(&[Some(1i64), Some(100), Some(1000), None]),
                price,
                r.pick(&[Some(0i64), Some(1), None]),
                r.pick(&[None, Some(500i64), Some(5_000)]),
                r.pick(&[None, Some(0i64), Some(4_000)]),
                (r.below(3) != 0) as i64,
                r.pick(&[None, Some(10i64), Some(i64::MAX)]),
                vec![0u8; 8],
                r.bytes(),
                genesis_out_block,
                genesis_daa,
                listed,
                quote,
            ],
        )
        .unwrap();
        if r.below(10) == 0 {
            continue; // no state row
        }
        let status = r.pick(&STATUSES);
        let live = status == "open" || status == "partial";
        let remaining: Option<i64> = r.pick(&[None, Some(0), Some(10), Some(25)]);
        let (txid, idx) = (r.bytes(), r.below(3) as i64);
        conn.execute(
            "INSERT OR REPLACE INTO order_state (covenant_id, status, filled_amount, remaining_amount, amount_exact, cur_txid, cur_idx, \
             cur_value, state_known, last_block, last_daa) VALUES (?1,?2,0,?3,?4,?5,?6,1000,?7,1,?8)",
            params![
                cov,
                status,
                remaining,
                r.below(2) as i64,
                live.then(|| txid.clone()),
                live.then_some(idx),
                (r.below(5) != 0) as i64,
                r.below(1000) as i64
            ],
        )
        .unwrap();
        if live {
            // the current UTXO's row: unspent, spent (a stale state) or not stored
            if r.below(10) != 0 {
                conn.execute(
                    "INSERT INTO order_utxos (txid, idx, covenant_id, value, spk, state, created_block, created_daa, spent_block) \
                     VALUES (?1,?2,?3,1000,x'00',?4,1,?5,?6)",
                    params![
                        txid,
                        idx,
                        cov,
                        r.pick(&[Some(vec![1u8; 4]), None]),
                        r.below(1000) as i64,
                        if r.below(8) == 0 { Some(2i64) } else { None }
                    ],
                )
                .unwrap();
            }
            if r.below(10) == 0 {
                conn.execute("INSERT INTO order_flags VALUES (?1,?2,?3,'frozen',1)", params![cov, txid, idx]).unwrap();
            }
            // custody: none, one (right or wrong amount), two
            for _ in 0..r.pick(&[0u64, 1, 1, 1, 2]) {
                conn.execute(
                    "INSERT INTO token_utxos (txid, idx, token_cov_id, owner, amount, value, role, created_block, created_daa, spent_block) \
                     VALUES (?1,0,?2,?3,?4,1,?5,1,1,?6)",
                    params![
                        r.bytes(),
                        tok(1),
                        cov,
                        r.pick(&[remaining.unwrap_or(0), 10, 7]),
                        r.pick(&["custody", "custody", "stray"]),
                        if r.below(6) == 0 { Some(3i64) } else { None }
                    ],
                )
                .unwrap();
            }
        }
    }
    for _ in 0..12 {
        conn.execute(
            "INSERT INTO token_events (token_cov_id, tpl_hash, kind, txid, block_seq, daa) VALUES (?1,?2,'seen',?3,1,1)",
            params![tok(1 + r.below(5) as u8), r.pick(&[Some(vec![7u8; 32]), Some(vec![6u8; 32]), None]), r.bytes()],
        )
        .unwrap();
    }
    for _ in 0..n {
        conn.execute(
            "INSERT INTO token_holdings (txid, idx, token_cov_id, program, family, owner, owner_kind, amount, value, state, role, \
             created_block, created_daa, spent_block) VALUES (?1,?2,?3,'KCC20Ref_8x8',1,?4,0,5,1,x'00','owned',?5,1,?6)",
            params![
                r.bytes(),
                r.below(2) as i64,
                tok(1 + r.below(3) as u8),
                vec![1 + r.below(3) as u8; 32],
                r.below(15) as i64,
                if r.below(3) == 0 { None } else { Some(4i64) }
            ],
        )
        .unwrap();
    }
    conn
}

fn ctx() -> ReadCtx {
    ReadCtx { node_daa: Some(1_000), settle_depth_daa: 10, now_unix: Some(100) }
}

/// The orders list as it was: one query over the whole history, the token filter an OR of both columns.
fn old_orders(conn: &Connection, ctx: &ReadCtx, f: &OrderFilter, limit: usize, cursor: Option<(i64, Vec<u8>)>) -> Page<OrderView> {
    // never an order under a template this build does not pin (a rule the live reads have too)
    let mut conds: Vec<String> = vec![pinned_template().to_string()];
    let mut args: Vec<SqlValue> = Vec::new();
    if let Some(m) = &f.maker {
        args.push(SqlValue::Blob(m.clone()));
        conds.push(format!("o.maker = ?{}", args.len()));
    }
    if let Some(t) = &f.token {
        args.push(SqlValue::Blob(t.0.to_vec()));
        conds.push(format!("(o.token_cov_id = ?{n} OR o.quote_cov_id = ?{n})", n = args.len()));
    }
    if let Some(s) = &f.status {
        if s == "active" {
            conds.push("s.status IN ('open','partial')".to_string());
        } else {
            args.push(SqlValue::Text(s.clone()));
            conds.push(format!("s.status = ?{}", args.len()));
        }
    }
    if let Some((b, c)) = cursor {
        args.push(SqlValue::Integer(b));
        args.push(SqlValue::Blob(c));
        conds.push(format!("(o.genesis_block, o.covenant_id) < (?{}, ?{})", args.len() - 1, args.len()));
    }
    let where_ = if conds.is_empty() { String::new() } else { format!(" WHERE {}", conds.join(" AND ")) };
    args.push(SqlValue::Integer(limit as i64 + 1));
    let sql = format!("{ORDER_SELECT}{where_} ORDER BY o.genesis_block DESC, o.covenant_id DESC LIMIT ?{}", args.len());
    let mut st = conn.prepare(&sql).unwrap();
    let mut items = st
        .query_map(params_from_iter(args.iter()), |r| order_from_row(r, ctx))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    let next_cursor = if items.len() > limit {
        items.truncate(limit);
        items.last().map(|o| format!("{}:{}", o.genesis.block_seq, o.covenant_id))
    } else {
        None
    };
    if f.maker.is_some() {
        for v in items.iter_mut().filter(|v| v.status == "open" || v.status == "partial") {
            if let Ok(id) = Hash32::parse(&v.covenant_id) {
                attach_holdings(conn, ctx, &id, v).unwrap();
            }
        }
    }
    Page { items, next_cursor }
}

#[test]
fn the_orders_list_pages_exactly_as_the_history_wide_query_did() {
    for seed in [1u64, 2, 3] {
        let conn = store(seed, 400);
        let ctx = ctx();
        let mut pages = 0;
        for maker in [None, Some(vec![1u8; 32]), Some(vec![])] {
            for token in [None, Some(Hash32(tok(1).try_into().unwrap())), Some(Hash32(tok(3).try_into().unwrap()))] {
                for status in [None, Some("active"), Some("open"), Some("filled"), Some("closed"), Some("cancelled")] {
                    // small pages read the history in order, large ones the live statuses (both strategies)
                    for limit in [1usize, 7, 60, 500] {
                        let f = OrderFilter { maker: maker.clone(), token, status: status.map(String::from) };
                        let mut cursor = None;
                        loop {
                            let new = orders(&conn, &ctx, &f, limit, cursor.clone()).unwrap();
                            let old = old_orders(&conn, &ctx, &f, limit, cursor.clone());
                            assert_eq!(
                                serde_json::to_value(&new).unwrap(),
                                serde_json::to_value(&old).unwrap(),
                                "seed {seed} {f:?} limit {limit} cursor {cursor:?}"
                            );
                            pages += 1;
                            match new.next_cursor {
                                Some(c) => cursor = parse_order_cursor(&c),
                                None => break,
                            }
                        }
                    }
                }
            }
        }
        assert!(pages > 1_000, "{pages}");
    }
}

#[test]
fn an_order_under_a_template_this_build_does_not_pin_is_in_no_read() {
    let mut would_list = 0;
    for seed in [1u64, 2, 3] {
        let conn = store(seed, 400);
        let ctx = ctx();
        let all: Vec<(Vec<u8>, Vec<u8>)> = conn
            .prepare("SELECT covenant_id, template_hash FROM orders")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        let unpinned: Vec<String> = all.iter().filter(|(_, t)| t[..] == [9u8; 32]).map(|(c, _)| hex::encode(c)).collect();
        assert!(!unpinned.is_empty() && unpinned.len() < all.len(), "seed {seed}: both kinds of rows");
        // the single-order view: not found
        for (cov, tpl) in &all {
            let found = order(&conn, &ctx, &Hash32(cov.clone().try_into().unwrap())).unwrap();
            assert_eq!(found.is_some(), tpl[..] != [9u8; 32], "seed {seed} order {}", hex::encode(cov));
        }
        // the orders list, every page of every filter
        let f = OrderFilter::default();
        let listed = orders(&conn, &ctx, &f, 10_000, None).unwrap().items;
        assert_eq!(listed.len(), all.len() - unpinned.len(), "seed {seed}");
        for status in ["active", "open", "partial", "filled", "cancelled"] {
            let f = OrderFilter { status: Some(status.into()), ..OrderFilter::default() };
            for limit in [1usize, 500] {
                let page = orders(&conn, &ctx, &f, limit, None).unwrap();
                assert!(page.items.iter().all(|o| !unpinned.contains(&o.covenant_id)), "seed {seed} {status}");
            }
        }
        // the books: an unpinned live order the book would list otherwise is left out
        let live = fillable_now(1_000, Some(100));
        for token in [1u8, 2, 3] {
            let t = Hash32(tok(token).try_into().unwrap());
            for side in [1i64, 2] {
                would_list += rows(
                    &conn,
                    &format!(
                        "SELECT o.covenant_id {LIVE_FROM} LEFT JOIN order_utxos u ON u.txid = s.cur_txid AND u.idx = s.cur_idx \
                         WHERE {BOOK_WHERE} AND {CUSTODY_OK} AND {live} AND o.template_hash = x'{}'",
                        hex::encode(&[9u8; 32])
                    ),
                    &[SqlValue::Blob(tok(token)), SqlValue::Integer(side)],
                )
                .len();
            }
            let b = book(&conn, &ctx, &t, 1_000, false).unwrap();
            for side in [&b.asks, &b.bids] {
                let BookSide::Orders(v) = side else { unreachable!() };
                assert!(v.iter().all(|o| !unpinned.contains(&o.covenant_id)), "seed {seed} token {token}");
            }
        }
    }
    assert!(would_list > 0, "the stores have unpinned orders the book's other conditions admit");
}

/// Rows of `sql` with every column as a value.
fn rows(conn: &Connection, sql: &str, args: &[SqlValue]) -> Vec<Vec<SqlValue>> {
    let mut st = conn.prepare(sql).unwrap();
    let n = st.column_count();
    st.query_map(params_from_iter(args.iter()), |r| (0..n).map(|i| r.get::<_, SqlValue>(i)).collect())
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}

fn sorted(mut v: Vec<Vec<SqlValue>>) -> Vec<String> {
    let mut s: Vec<String> = v.drain(..).map(|r| format!("{r:?}")).collect();
    s.sort();
    s
}

const OLD_FROM: &str = "FROM orders o JOIN order_state s ON s.covenant_id = o.covenant_id";

#[test]
fn the_books_counts_and_live_orders_read_from_the_live_set_are_the_same_rows() {
    for seed in [4u64, 5, 6] {
        let conn = store(seed, 400);
        let live = fillable_now(1_000, Some(100));
        for token in [1u8, 2, 3] {
            for side in [1i64, 2] {
                let args3 = [SqlValue::Blob(tok(token)), SqlValue::Integer(side), SqlValue::Integer(5)];
                // the per-order book (ordered, limited) and the levels' rows
                for (sel, tail) in [
                    (
                        "SELECT o.covenant_id, o.price, s.remaining_amount, u.state",
                        " ORDER BY o.price, o.genesis_block, o.covenant_id LIMIT ?3",
                    ),
                    ("SELECT o.price, COALESCE(o.scale, 1), s.remaining_amount, s.amount_exact", ""),
                ] {
                    let q = |from: &str| {
                        format!(
                            "{sel} {from} LEFT JOIN order_utxos u ON u.txid = s.cur_txid AND u.idx = s.cur_idx \
                             WHERE {BOOK_WHERE} AND {CUSTODY_OK} AND {live}{tail}"
                        )
                    };
                    let args = if tail.is_empty() { &args3[..2] } else { &args3[..] };
                    let (old, new) = (rows(&conn, &q(OLD_FROM), args), rows(&conn, &q(LIVE_FROM), args));
                    assert_eq!(sorted(old.clone()), sorted(new.clone()), "seed {seed} token {token} side {side}");
                    if !tail.is_empty() {
                        assert_eq!(format!("{old:?}"), format!("{new:?}"), "order: seed {seed} token {token} side {side}");
                    }
                }
            }
            // the reads themselves run on it
            let t = Hash32(tok(token).try_into().unwrap());
            book(&conn, &ctx(), &t, 20, true).unwrap();
            book(&conn, &ctx(), &t, 20, false).unwrap();
            crate::indexer::market::stats(&conn, &ctx(), &t, None).unwrap();
        }
        // the token list's counts
        let counts = |from: &str| {
            format!(
                "SELECT o.token_cov_id, o.side, COUNT(*) {from} WHERE o.listed = 1 AND o.in_book = 1 AND s.status IN ('open','partial') \
                 AND s.state_known = 1 AND o.token_cov_id IS NOT NULL AND {NOT_FROZEN} AND {HAS_AMOUNT} AND {CUSTODY_OK} \
                 GROUP BY o.token_cov_id, o.side"
            )
        };
        assert_eq!(sorted(rows(&conn, &counts(OLD_FROM), &[])), sorted(rows(&conn, &counts(LIVE_FROM), &[])));
        // the matcher's live orders (listed and not), in the book's order
        for listed in [0i64, 1] {
            let old = format!(
                "SELECT o.covenant_id, u.txid, u.idx FROM orders o JOIN order_state s ON s.covenant_id = o.covenant_id \
                 JOIN order_utxos u ON u.txid = s.cur_txid AND u.idx = s.cur_idx \
                 WHERE o.listed = ?1 AND s.state_known = 1 AND u.spent_block IS NULL AND u.state IS NOT NULL AND {} \
                 ORDER BY o.genesis_block, o.covenant_id",
                pinned_template()
            );
            let new = super::super::book::live_orders_sql().replacen("SELECT ", "SELECT o.covenant_id, u.txid, u.idx, ", 1);
            let new: Vec<Vec<SqlValue>> =
                rows(&conn, &new, &[SqlValue::Integer(listed)]).into_iter().map(|r| r[..3].to_vec()).collect();
            let old = rows(&conn, &old, &[SqlValue::Integer(listed)]);
            assert!(!old.is_empty());
            assert_eq!(format!("{old:?}"), format!("{new:?}"), "seed {seed} listed {listed}");
        }
    }
}

#[test]
fn the_open_token_list_reads_the_rows_the_union_did() {
    for seed in [7u64, 8] {
        let conn = store(seed, 300);
        // more distinct (token, program, family) rows of listed orders
        for t in 10u8..40 {
            conn.execute(
                "INSERT INTO orders (covenant_id, contract, template_hash, family, side, token_cov_id, token_tpl_hash, in_book, \
                 genesis_state, genesis_block, genesis_daa, listed, origin) VALUES (?1,'KobAsk',?2,?3,1,?4,?5,1,x'00',1,1,1,'chain')",
                params![vec![t; 32], vec![9u8; 32], 1 + (t % 2) as i64, tok(t % 7), vec![t % 3; 32]],
            )
            .unwrap();
        }
        let old = "SELECT DISTINCT token_cov_id, token_tpl_hash, family FROM orders WHERE listed = 1 AND token_cov_id IS NOT NULL \
                   AND token_tpl_hash IS NOT NULL \
                   UNION SELECT DISTINCT o.quote_cov_id, e.tpl_hash, 0 FROM orders o JOIN token_events e ON e.token_cov_id = o.quote_cov_id \
                   WHERE o.listed = 1 AND o.quote_cov_id IS NOT NULL AND e.tpl_hash IS NOT NULL \
                   UNION SELECT DISTINCT token_cov_id, tpl_hash, 0 FROM token_events WHERE tpl_hash IS NOT NULL \
                   ORDER BY 1, 3 DESC LIMIT ?1";
        for max in [1usize, 2, 3, 5, 8, 13, 5_000] {
            let want: Vec<(Vec<u8>, Vec<u8>, i64)> = conn
                .prepare(old)
                .unwrap()
                .query_map([max as i64], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            assert_eq!(open_token_rows(&conn, max).unwrap(), want, "seed {seed} max {max}");
        }
        assert!(open_token_rows(&conn, 5_000).unwrap().len() > 13);
    }
}

#[test]
fn the_holdings_pages_are_the_same_through_the_live_indexes() {
    let conn = store(9, 600);
    let ctx = ctx();
    for owner in [None, Some(vec![1u8; 32]), Some(vec![2u8; 32])] {
        for token in [None, Some(Hash32(tok(1).try_into().unwrap()))] {
            if owner.is_none() && token.is_none() {
                continue;
            }
            for spent in [false, true] {
                for limit in [1usize, 4, 100] {
                    let f = HoldingFilter { owner: owner.clone(), token, include_spent: spent };
                    let mut cursor = None;
                    let mut seen = vec![];
                    loop {
                        let p = holdings(&conn, &ctx, &f, limit, cursor.clone()).unwrap();
                        seen.extend(p.items.iter().map(|h| (h.txid.clone(), h.index)));
                        match p.next_cursor {
                            Some(c) => cursor = parse_holding_cursor(&c),
                            None => break,
                        }
                    }
                    // the oracle: every matching row, oldest first
                    let mut sql = "SELECT txid, idx FROM token_holdings WHERE 1".to_string();
                    let mut args = vec![];
                    if let Some(o) = &owner {
                        args.push(SqlValue::Blob(o.clone()));
                        sql.push_str(&format!(" AND owner = ?{}", args.len()));
                    }
                    if let Some(t) = &token {
                        args.push(SqlValue::Blob(t.0.to_vec()));
                        sql.push_str(&format!(" AND token_cov_id = ?{}", args.len()));
                    }
                    if !spent {
                        sql.push_str(" AND spent_block IS NULL");
                    }
                    sql.push_str(" ORDER BY created_block, txid, idx");
                    let want: Vec<(String, i64)> = rows(&conn, &sql, &args)
                        .into_iter()
                        .map(|r| match (&r[0], &r[1]) {
                            (SqlValue::Blob(t), SqlValue::Integer(i)) => (hex::encode(t), *i),
                            _ => unreachable!(),
                        })
                        .collect();
                    assert!(!want.is_empty());
                    assert_eq!(seen, want, "{f:?} limit {limit}");
                }
            }
        }
    }
}

#[test]
fn the_kept_counters_follow_inserts_replacements_and_deletes() {
    let conn = store(10, 300);
    let check = |conn: &Connection| {
        let kept = counters(conn).unwrap();
        let counted = counted(conn, kept.last_block_seq, kept.last_block_daa).unwrap();
        assert_eq!(serde_json::to_value(&kept).unwrap(), serde_json::to_value(&counted).unwrap());
    };
    check(&conn);
    // a refresh replaces the state row with another status; a revert deletes orders, states, events and rejects
    conn.execute("UPDATE order_state SET status = 'filled' WHERE rowid % 3 = 0", []).unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO order_state (covenant_id, status, filled_amount, amount_exact, state_known, last_block, last_daa) \
         SELECT covenant_id, 'killed', 0, 0, 1, 1, 1 FROM order_state WHERE rowid % 5 = 0",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO order_events (covenant_id, block_seq, daa, ts, txid, tx_pos, kind) SELECT covenant_id, 3, 1, 1, x'00', 0, \
         CASE WHEN rowid % 2 = 0 THEN 'fill' ELSE 'cancel' END FROM orders",
        [],
    )
    .unwrap();
    conn.execute("INSERT INTO rejects (block_seq, txid, reason) VALUES (3, x'00', 'x'), (4, x'01', 'y')", []).unwrap();
    conn.execute("UPDATE orders SET listed = 1 - listed WHERE rowid % 4 = 0", []).unwrap();
    check(&conn);
    conn.execute("DELETE FROM order_events WHERE rowid % 3 = 0", []).unwrap();
    conn.execute("DELETE FROM rejects WHERE block_seq = 3", []).unwrap();
    conn.execute("DELETE FROM order_state WHERE rowid % 7 = 0", []).unwrap();
    conn.execute("DELETE FROM orders WHERE rowid % 6 = 0", []).unwrap();
    check(&conn);
    // a database whose counters were never counted (an earlier build) counts them once at start-up
    conn.execute_batch("DELETE FROM counters; DELETE FROM meta WHERE k = 'counters';").unwrap();
    assert!(kept_counters(&conn).unwrap().is_none());
    check(&conn);
    assert!(crate::indexer::db::backfill_counters(&conn).unwrap());
    assert!(!crate::indexer::db::backfill_counters(&conn).unwrap());
    assert!(kept_counters(&conn).unwrap().is_some());
    check(&conn);
}

/// `GET /v1/orders` with no filter on a large store whose newest genesis block holds most of the orders (a bulk import, a
/// rebase, a burst): a page is read in index order and stops at its end, where the plain `(genesis_block)` index made SQLite
/// fetch and sort every order of that block (10 s and a 504 on a 770,000-order store). The plan has no sort, the page and the
/// next ones are the newest orders in order, and a page costs milliseconds.
#[test]
fn the_unfiltered_orders_list_reads_a_page_in_index_order_whatever_one_block_holds() {
    let conn = open_memory("testnet-10").unwrap();
    let pinned = TemplateId::from_name("KobAsk").map(kob_protocol::artifacts::pinned_hash).unwrap().to_vec();
    conn.execute_batch("BEGIN").unwrap();
    let mut st = conn
        .prepare(
            "INSERT INTO orders (covenant_id, contract, template_hash, family, side, maker, token_cov_id, price, in_book, genesis_state, \
             genesis_block, genesis_daa, listed, origin) VALUES (?1,'KobAsk',?2,1,1,?3,?4,5,1,x'00',?5,?5,1,'chain')",
        )
        .unwrap();
    let mut s2 = conn
        .prepare("INSERT INTO order_state (covenant_id, status, filled_amount, state_known, last_block, last_daa) VALUES (?1,'filled',0,1,1,1)")
        .unwrap();
    let n = 240_000i64;
    for i in 0..n {
        let mut cov = vec![0u8; 32];
        cov[..8].copy_from_slice(&(i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15).to_be_bytes());
        // 200,000 orders in the newest block (1,000,000), the rest spread over older ones
        let block = if i < 200_000 { 1_000_000 } else { 1_000 + (i - 200_000) / 3 };
        st.execute(params![cov, pinned, vec![(i % 50) as u8; 32], vec![(i % 7) as u8; 32], block]).unwrap();
        s2.execute(params![cov]).unwrap();
    }
    drop(st);
    drop(s2);
    conn.execute_batch("COMMIT").unwrap();

    let sql = format!("{ORDER_SELECT} WHERE {} ORDER BY o.genesis_block DESC, o.covenant_id DESC LIMIT 51", pinned_template());
    let mut p = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
    let plan: Vec<String> = p.query_map([], |r| r.get::<_, String>(3)).unwrap().map(|x| x.unwrap()).collect();
    assert!(plan.iter().any(|l| l.contains("orders_genesis_page")), "{plan:?}");
    assert!(!plan.iter().any(|l| l.contains("TEMP B-TREE")), "no sort of a block: {plan:?}");

    let f = OrderFilter::default();
    let t = std::time::Instant::now();
    let first = orders(&conn, &ctx(), &f, 50, None).unwrap();
    let took = t.elapsed();
    assert_eq!(first.items.len(), 50);
    assert!(took < std::time::Duration::from_secs(2), "a page took {took:?}");
    println!("unfiltered page of a 240,000-order store with 200,000 orders in one block: {took:?}");
    // the newest block, covenant ids descending; the cursor continues exactly there
    let ids: Vec<&str> = first.items.iter().map(|o| o.covenant_id.as_str()).collect();
    assert!(ids.windows(2).all(|w| w[0] > w[1]), "descending by covenant id inside one block");
    assert!(first.items.iter().all(|o| o.genesis.block_seq == 1_000_000));
    let second = orders(&conn, &ctx(), &f, 50, first.next_cursor.as_deref().and_then(parse_order_cursor)).unwrap();
    assert!(second.items[0].covenant_id.as_str() < ids[49]);
    // the same rows as the reference query
    let old = old_orders(&conn, &ctx(), &f, 50, None);
    assert_eq!(serde_json::to_value(&first).unwrap(), serde_json::to_value(&old).unwrap());
}
