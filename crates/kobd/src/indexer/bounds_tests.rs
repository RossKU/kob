//! Measured bounds of the public read API (`docs/ops/api-bounds.md`, R4C2).
//!
//! Each test seeds the rows an attacker can add to a store cheaply (curve asks on one token,
//! children and strays of one order, spent holdings of one owner, fills of one token, pair orders
//! of one pair), runs the reads of the routes over them with N and then 2 N such rows, both past
//! the route's bound, and asserts that the SQLite VM steps of the read (every statement it runs,
//! counted by a progress handler) do not grow with N. The plans of the statements that serve the
//! routes are checked too: no scan of a table and no temporary B-tree over the rows that grow.
//!
//! The stores are in memory and built with set-based SQL, so a test runs in seconds.

use super::*;
use crate::indexer::db::open_memory;
use crate::indexer::{activity, market, pairs, quotes};
use crate::testkit::{ask, bid, pk, tok_fields, EXT, SCALE, T3, TOKEN_COV, WHOLE};
use crate::tokens::TokenEntry;
use rusqlite::params;
use std::sync::atomic::{AtomicU64, Ordering};

/// The VM steps of `f` on `conn`: the progress handler is called at every VM instruction it checks
/// (`sqlite3_progress_handler` with N = 1), for every statement `f` runs.
pub(crate) fn steps<T>(conn: &Connection, f: impl FnOnce() -> T) -> (T, u64) {
    unsafe extern "C" fn tick(p: *mut std::ffi::c_void) -> std::ffi::c_int {
        // SAFETY: `p` is the counter of `steps`, which outlives the handler.
        unsafe { (*(p as *const AtomicU64)).fetch_add(1, Ordering::Relaxed) };
        0
    }
    let n = Box::new(AtomicU64::new(0));
    // SAFETY: the handle is this connection's, and the handler is removed before the counter goes.
    unsafe { rusqlite::ffi::sqlite3_progress_handler(conn.handle(), 1, Some(tick), &*n as *const AtomicU64 as *mut std::ffi::c_void) };
    let r = f();
    // SAFETY: as above.
    unsafe { rusqlite::ffi::sqlite3_progress_handler(conn.handle(), 0, None, std::ptr::null_mut()) };
    (r, n.load(Ordering::Relaxed))
}

/// The work of a read with 2 N attacker rows is that with N, within 5 % and a few hundred steps.
fn flat(name: &str, few: u64, many: u64) {
    eprintln!("{name}: {few} VM steps with N rows, {many} with 2 N");
    assert!(many <= few + few / 20 + 500, "{name}: {few} VM steps with N rows, {many} with 2 N");
}

/// The query plan of `sql`, one line per step.
fn plan(conn: &Connection, sql: &str, args: impl rusqlite::Params) -> String {
    conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .unwrap()
        .query_map(args, |r| r.get::<_, String>(3))
        .unwrap()
        .map(|r| r.unwrap())
        .collect::<Vec<_>>()
        .join("; ")
}

/// No temporary B-tree, and no step that scans a table: a `SCAN` only of a named subquery.
fn indexed(name: &str, p: &str, subqueries: &[&str]) {
    assert!(!p.contains("TEMP B-TREE"), "{name}: {p}");
    for step in p.split("; ") {
        if let Some(what) = step.strip_prefix("SCAN ") {
            let what = what.split(' ').next().unwrap_or("");
            assert!(subqueries.contains(&what), "{name}: {step} in {p}");
        }
    }
}

fn ctx() -> ReadCtx {
    ReadCtx { node_daa: Some(1_000), settle_depth_daa: 100, now_unix: None }
}

fn id(conn: &Connection, h: &[u8]) -> i64 {
    crate::indexer::ids::intern(conn, h).unwrap()
}

/// Orders `from..to` of one shape, each with its order UTXO and state, and its exact custody when
/// `custody`. Covenant ids are `prefix` and the number, the price `price0 + i` (none for `None`).
struct Seed<'a> {
    prefix: &'a str,
    contract: &'a str,
    side: i64,
    token: [u8; 32],
    quote: Option<[u8; 32]>,
    parent: Option<[u8; 32]>,
    state: Option<Vec<u8>>,
    price0: Option<i64>,
    remaining: i64,
    custody: bool,
    listed: bool,
    maker: [u8; 32],
}

fn seed(conn: &Connection, s: &Seed, from: i64, to: i64) {
    let tpl = TemplateId::from_name(s.contract).unwrap();
    let template = id(conn, &kob_protocol::artifacts::pinned_hash(tpl));
    let (maker, token) = (id(conn, &s.maker), id(conn, &s.token));
    let quote = s.quote.map(|q| id(conn, &q));
    let n = "WITH RECURSIVE n(i) AS (SELECT ?1 UNION ALL SELECT i + 1 FROM n WHERE i < ?2 - 1)";
    let cov = "CAST(printf('%s%031d', ?3, i) AS BLOB)";
    let tx = conn.unchecked_transaction().unwrap();
    tx.execute(
        &format!(
            "{n} INSERT INTO orders (covenant_id, contract, template_id, family, side, maker_id, token_id, scale, min_fill, price, tip, \
             in_book, initial_amount, genesis_state, genesis_block, genesis_daa, listed, parent, quote_id) \
             SELECT {cov}, ?4, ?5, 1, ?6, ?7, ?8, {SCALE}, 1, ?9 + i, 0, ?10, ?11, COALESCE(?12, x'00'), 1 + i, 1 + i, ?13, ?14, ?15 FROM n"
        ),
        params![
            from,
            to,
            s.prefix,
            s.contract,
            template,
            s.side,
            maker,
            token,
            s.price0,
            crate::model::in_book(tpl) as i64,
            s.remaining,
            s.state,
            s.listed as i64,
            s.parent.map(|p| p.to_vec()),
            quote
        ],
    )
    .unwrap();
    let utxo = "CAST(printf('u%s%030d', ?3, i) AS BLOB)";
    let join = format!("FROM n CROSS JOIN orders o ON o.covenant_id = {cov}");
    tx.execute(
        &format!(
            "{n} INSERT INTO order_utxos (txid, idx, order_id, value, spk, state, created_block, created_daa) \
             SELECT {utxo}, 0, o.id, 20000000, x'00', ?4, 1, 1 {join}"
        ),
        params![from, to, s.prefix, s.state],
    )
    .unwrap();
    tx.execute(
        &format!(
            "{n} INSERT INTO order_state (order_id, status, filled_amount, remaining_amount, amount_exact, cur_txid, cur_idx, cur_value, \
             state_known, last_block, last_daa) SELECT o.id, 'open', 0, ?4, 1, {utxo}, 0, 20000000, 1, 1, 1 {join}"
        ),
        params![from, to, s.prefix, s.remaining],
    )
    .unwrap();
    if s.custody {
        tx.execute(
            &format!(
                "{n} INSERT INTO token_utxos (txid, idx, token_id, order_id, amount, value, role, created_block, created_daa) \
                 SELECT {utxo}, 1, ?4, o.id, ?5, 1000, 'custody', 1, 1 {join}"
            ),
            params![from, to, s.prefix, token, s.remaining],
        )
        .unwrap();
    }
    tx.commit().unwrap();
}

fn allowlist() -> TokenAllowlist {
    TokenAllowlist::from_entries([TokenEntry {
        ticker: "T".into(),
        covenant_id: Hash32(TOKEN_COV),
        template_hash: None,
        extension_commitment: None,
        decimals: Some(3),
        family: None,
        enabled: true,
        template_id: None,
        powers: vec![],
    }])
}

/// A curve ask of two levels of 10 base units, at 5 and 6 KAS per whole token.
fn curve() -> Vec<u8> {
    let (hh, p, s) = tok_fields(T3);
    AnyState::KobCurveAsk(kob_protocol::state::CurveAskState {
        maker: pk(9),
        token_cov_id: TOKEN_COV,
        token_tpl_hash: hh,
        tpl_prefix_len: p,
        tpl_suffix_len: s,
        scale: SCALE,
        min_fill: 1,
        tip: 0,
        active_from: 0,
        level_size: 10,
        total: 20,
        amount_left: 20,
        extension_commitment: EXT,
        table: vec![500_000_000, 600_000_000],
    })
    .encode()
}

/// An honest book: ten asks from 1.00 KAS and ten bids from 0.99 KAS down, ten whole tokens each.
fn honest_book(conn: &Connection) {
    for k in 0..10i64 {
        let a = AnyState::KobAsk(ask(1, 100_000_000 + k * 1_000_000)).encode();
        let s = Seed {
            prefix: "a",
            contract: "KobAsk",
            side: 1,
            token: TOKEN_COV,
            quote: None,
            parent: None,
            state: Some(a),
            price0: Some(100_000_000 + k * 1_000_000 - k),
            remaining: 10 * WHOLE,
            custody: true,
            listed: true,
            maker: pk(1),
        };
        seed(conn, &s, k, k + 1);
        let b = AnyState::KobBid(bid(2, 99_000_000 - k * 1_000_000)).encode();
        let s = Seed {
            prefix: "b",
            contract: "KobBid",
            side: 2,
            state: Some(b),
            price0: Some(99_000_000 - k * 1_000_000 - k),
            remaining: 10 * WHOLE,
            custody: false,
            maker: pk(2),
            ..s
        };
        seed(conn, &s, k, k + 1);
    }
}

/// R4C2-2: the KAS book (both views), the depth, the 24 h statistics, the token list's counts and the
/// KAS route of a pair book read a bounded prefix of the live book, however many curve asks are
/// listed on the token. The answer of the honest book in front of them does not change.
#[test]
fn the_book_reads_do_not_grow_with_listed_curve_asks() {
    let conn = open_memory("testnet-10").unwrap();
    honest_book(&conn);
    let (token, other) = (Hash32(TOKEN_COV), Hash32([0x5a; 32]));
    let (ctx, list) = (ctx(), allowlist());
    let honest = serde_json::to_value(book(&conn, &ctx, &token, 10, true).unwrap()).unwrap();
    let honest_orders = serde_json::to_value(book(&conn, &ctx, &token, 10, false).unwrap()).unwrap();
    let measure = || -> Vec<(&'static str, u64)> {
        vec![
            ("book aggregate", steps(&conn, || book(&conn, &ctx, &token, 20, true).unwrap()).1),
            ("book orders", steps(&conn, || book(&conn, &ctx, &token, 20, false).unwrap()).1),
            ("depth", steps(&conn, || market::depth(&conn, &ctx, &token, Some(3), 20).unwrap()).1),
            ("stats", steps(&conn, || market::stats(&conn, &ctx, &token, Some(3)).unwrap()).1),
            ("tokens", steps(&conn, || token_list(&conn, &list).unwrap()).1),
            ("pair book", steps(&conn, || pairs::pair_book(&conn, &ctx, &token, &other, 20).unwrap()).1),
            ("quotes sample", steps(&conn, || book_side_sorted(&conn, &ctx, &token, 1, Some(1)).unwrap()).1),
        ]
    };
    let s = Seed {
        prefix: "c",
        contract: "KobCurveAsk",
        side: 1,
        token: TOKEN_COV,
        quote: None,
        parent: None,
        state: Some(curve()),
        price0: Some(500_000_000),
        remaining: 20,
        custody: true,
        listed: true,
        maker: pk(9),
    };
    let n = BOOK_SCAN_MAX as i64 + 2_000;
    seed(&conn, &s, 0, n);
    let few = measure();
    seed(&conn, &s, n, 2 * n);
    let many = measure();
    for ((name, f), (_, m)) in few.iter().zip(&many) {
        flat(name, *f, *m);
    }
    // the honest levels in front are what they were, and the answer says that the side was cut
    let now = serde_json::to_value(book(&conn, &ctx, &token, 10, true).unwrap()).unwrap();
    assert_eq!((&now["asks"], &now["bids"]), (&honest["asks"], &honest["bids"]));
    assert_eq!(now["truncated"], true);
    assert!(honest.get("truncated").is_none(), "not sent for a whole book");
    let now = serde_json::to_value(book(&conn, &ctx, &token, 10, false).unwrap()).unwrap();
    assert_eq!((&now["asks"], &now["bids"]), (&honest_orders["asks"], &honest_orders["bids"]));
    assert_eq!(serde_json::to_value(market::stats(&conn, &ctx, &token, Some(3)).unwrap()).unwrap()["book_truncated"], true);
    // the count of a side says the bound, "this many or more"
    let t = token_list(&conn, &list).unwrap();
    assert_eq!(t[0].open_asks, BOOK_SCAN_MAX as i64);
    // the rows of a book side are a prefix of `book_live`, read by its index
    let p = plan(
        &conn,
        &format!("SELECT o.id {} LEFT JOIN order_utxos u ON u.txid = s.cur_txid AND u.idx = s.cur_idx", book_from()),
        params![TOKEN_COV.to_vec(), 1],
    );
    assert!(p.contains("book_live_key"), "{p}");
    indexed("book side", &p, &["b"]);
}

/// R4C2-1, R4C2-5 and R3C2-1: one order's view (on the light lane), its events and a maker's order
/// page do not grow with its children, its live and spent strays, or a spent bought-custody history.
#[test]
fn the_order_view_does_not_grow_with_children_strays_or_spent_custody() {
    let conn = open_memory("testnet-10").unwrap();
    let parent = AnyState::KobAsk(ask(3, 100_000_000)).encode();
    let p = Seed {
        prefix: "p",
        contract: "KobAsk",
        side: 1,
        token: TOKEN_COV,
        quote: None,
        parent: None,
        state: Some(parent),
        price0: Some(100_000_000),
        remaining: 10 * WHOLE,
        custody: true,
        listed: true,
        maker: pk(3),
    };
    seed(&conn, &p, 0, 1);
    let cov: [u8; 32] = format!("p{:031}", 0).into_bytes().try_into().unwrap();
    let pid: i64 = conn.query_row("SELECT id FROM orders WHERE covenant_id = ?1", [cov.to_vec()], |r| r.get(0)).unwrap();
    let token = id(&conn, &TOKEN_COV);
    let kids = Seed { prefix: "k", parent: Some(cov), state: None, listed: false, custody: false, maker: pk(4), ..p };
    let grow = |from: i64, to: i64| {
        seed(&conn, &kids, from, to);
        let n = "WITH RECURSIVE n(i) AS (SELECT ?1 UNION ALL SELECT i + 1 FROM n WHERE i < ?2 - 1)";
        for (role, spent) in [("stray", "NULL"), ("stray", "7"), ("bought", "7")] {
            conn.execute(
                &format!(
                    "{n} INSERT INTO token_utxos (txid, idx, token_id, order_id, amount, value, role, created_block, created_daa, spent_block) \
                     SELECT CAST(printf('s%s%s%028d', ?3, ?4, i) AS BLOB), 0, ?5, ?6, 1, 8000000, ?3, 2 + i, 2 + i, {spent} FROM n"
                ),
                params![from, to, role, spent, token, pid],
            )
            .unwrap();
        }
        conn.execute(
            &format!(
                "{n} INSERT INTO order_events (order_id, block_seq, daa, ts, txid, tx_pos, kind, token_id, side, amount, price, closes, detail) \
                 SELECT ?3, 1, 1, 1700000000000 + i, CAST(printf('e%031d', i) AS BLOB), 0, 'fill', ?4, 1, 1, 100000000, 0, '{{}}' FROM n"
            ),
            params![from, to, pid, token],
        )
        .unwrap();
    };
    let ctx = ctx();
    let c = Hash32(cov);
    let maker = OrderFilter { maker: Some(pk(3).to_vec()), token: None, status: None };
    let measure = || -> Vec<(&'static str, u64)> {
        vec![
            ("order view", steps(&conn, || order(&conn, &ctx, &c).unwrap().unwrap()).1),
            (
                "bought custody",
                steps(&conn, || live_token_utxos(&conn, &ctx, &c, crate::indexer::delivery::ROLE_BOUGHT, 2).unwrap()).1,
            ),
            ("order events", steps(&conn, || order_events(&conn, &ctx, &c, 100, None).unwrap()).1),
            ("orders of a maker", steps(&conn, || orders(&conn, &ctx, &maker, 50, None).unwrap()).1),
        ]
    };
    let n = VIEW_COUNT_MAX + 2_000;
    grow(0, n);
    let few = measure();
    grow(n, 2 * n);
    let many = measure();
    for ((name, f), (_, m)) in few.iter().zip(&many) {
        flat(name, *f, *m);
    }
    let v = order(&conn, &ctx, &c).unwrap().unwrap();
    assert_eq!((v.children_total, v.strays_total), (Some(VIEW_COUNT_MAX), Some(VIEW_COUNT_MAX)));
    let p = plan(
        &conn,
        &format!(
            "SELECT {TOKEN_UTXO_COLUMNS} {TOKEN_UTXO_FROM} WHERE t.order_id = (SELECT id FROM orders WHERE covenant_id = ?1) AND t.role = 'bought' \
             AND t.spent_block IS NULL ORDER BY t.created_block, t.txid, t.idx LIMIT ?2"
        ),
        params![cov.to_vec(), 2],
    );
    assert!(p.contains("token_utxos_bought_live"), "{p}");
    indexed("bought custody", &p, &[]);
}

/// The fills, trades, candles, quotes, activity and 24 h figures of a token do not grow with its
/// fills: a page, a window of buckets, or the stated row cap of the 24 h window
/// ([`market::MAX_FILL_ROWS`]).
#[test]
fn the_market_reads_do_not_grow_with_fills() {
    let conn = open_memory("testnet-10").unwrap();
    honest_book(&conn);
    let token = Hash32(TOKEN_COV);
    let (oid, tid): (i64, i64) =
        (conn.query_row("SELECT id FROM orders WHERE contract = 'KobAsk' LIMIT 1", [], |r| r.get(0)).unwrap(), id(&conn, &TOKEN_COV));
    // every fill in the same ten minutes: the buckets do not grow, the fills do
    let grow = |from: i64, to: i64| {
        conn.execute(
            "WITH RECURSIVE n(i) AS (SELECT ?1 UNION ALL SELECT i + 1 FROM n WHERE i < ?2 - 1) \
             INSERT INTO order_events (order_id, block_seq, daa, ts, txid, tx_pos, kind, token_id, side, amount, price, closes, detail) \
             SELECT ?3, 1, 1, 1700000000000 + (i % 600000), CAST(printf('f%031d', i) AS BLOB), 0, 'fill', ?4, 1, 1000, 100000000, 0, '{}' FROM n",
            params![from, to, oid, tid],
        )
        .unwrap();
        crate::indexer::candles::rebuild(&conn, &token, 1_000).unwrap();
    };
    let ctx = ctx();
    let measure = || -> Vec<(&'static str, u64)> {
        vec![
            ("fills", steps(&conn, || fills(&conn, &ctx, Some(&token), None, None, 50).unwrap()).1),
            ("trades", steps(&conn, || market::trades(&conn, &ctx, &token, Some(3), None, 50).unwrap()).1),
            ("candles", steps(&conn, || market::candles(&conn, &token, Some(3), "1m", 60_000, None, None, 500).unwrap()).1),
            ("quotes", steps(&conn, || quotes::quotes(&conn, &token, Some(3), "1m", 60_000, None, None, 500).unwrap()).1),
            ("stats", steps(&conn, || market::stats(&conn, &ctx, &token, Some(3)).unwrap()).1),
            ("activity", steps(&conn, || activity::activity(&conn, None).unwrap()).1),
        ]
    };
    let n = market::MAX_FILL_ROWS as i64 + 5_000;
    grow(0, n);
    let few = measure();
    grow(n, 2 * n);
    let many = measure();
    for ((name, f), (_, m)) in few.iter().zip(&many) {
        flat(name, *f, *m);
    }
}

/// R3C2-1 items 6 and 7: the strays (all, and a maker's) and an owner's holdings of one token do not
/// grow with the live strays of the store or the owner's other holdings: a page, or the stated
/// budgets ([`STRAYS_SCAN_MAX`], [`HOLDINGS_SCAN_MAX`]).
#[test]
fn strays_and_an_owners_holdings_of_a_token_do_not_grow() {
    let conn = open_memory("testnet-10").unwrap();
    let o = Seed {
        prefix: "o",
        contract: "KobAsk",
        side: 1,
        token: TOKEN_COV,
        quote: None,
        parent: None,
        state: None,
        price0: Some(1),
        remaining: 1,
        custody: false,
        listed: false,
        maker: pk(5),
    };
    seed(&conn, &o, 0, 1);
    let oid: i64 = conn.query_row("SELECT id FROM orders LIMIT 1", [], |r| r.get(0)).unwrap();
    let (token, owner) = (id(&conn, &TOKEN_COV), [0x0e; 32]);
    let owner_id = id(&conn, &owner);
    crate::testkit::named_holdings_writable(&conn);
    let grow = |from: i64, to: i64| {
        let n = "WITH RECURSIVE n(i) AS (SELECT ?1 UNION ALL SELECT i + 1 FROM n WHERE i < ?2 - 1)";
        conn.execute(
            &format!(
                "{n} INSERT INTO token_utxos (txid, idx, token_id, order_id, amount, value, role, created_block, created_daa) \
                 SELECT CAST(printf('t%031d', i) AS BLOB), 0, ?3, ?4, 1, 8000000, 'stray', 2 + i, 2 + i FROM n"
            ),
            params![from, to, token, oid],
        )
        .unwrap();
        conn.execute(
            &format!(
                "{n} INSERT INTO token_holdings_named (txid, idx, token_id, program, family, owner_id, owner_kind, amount, value, state, role, \
                 created_block, created_daa, spent_block) SELECT CAST(printf('h%031d', i) AS BLOB), 0, ?3, 'KCC20Ref', 1, ?4, 0, 5, 1, x'08', \
                 'owned', 2 + i, 2 + i, 3 + i FROM n"
            ),
            params![from, to, token, owner_id],
        )
        .unwrap();
    };
    let ctx = ctx();
    let elsewhere = Some(Hash32([0x6b; 32]));
    let spent = HoldingFilter { owner: Some(owner.to_vec()), token: elsewhere, include_spent: true };
    let live = HoldingFilter { include_spent: false, ..spent.clone() };
    let measure = || -> Vec<(&'static str, u64)> {
        vec![
            ("strays", steps(&conn, || strays(&conn, &ctx, None, 50).unwrap()).1),
            ("strays of a maker", steps(&conn, || strays(&conn, &ctx, Some(&[0x77u8; 32][..]), 50).unwrap()).1),
            ("token-utxos owner token spent", steps(&conn, || holdings(&conn, &ctx, &spent, 100, None).unwrap()).1),
            ("token-utxos owner token", steps(&conn, || holdings(&conn, &ctx, &live, 100, None).unwrap()).1),
        ]
    };
    let n = HOLDINGS_SCAN_MAX.max(STRAYS_SCAN_MAX) as i64 + 2_000;
    grow(0, n);
    let few = measure();
    grow(n, 2 * n);
    let many = measure();
    for ((name, f), (_, m)) in few.iter().zip(&many) {
        flat(name, *f, *m);
    }
    // a page the budget ended carries the cursor of the point it reached, and the next page goes on from it
    let page = holdings(&conn, &ctx, &spent, 100, None).unwrap();
    assert!(page.items.is_empty());
    let next = parse_holding_cursor(page.next_cursor.as_deref().unwrap()).unwrap();
    assert!(next.0 > 2, "{next:?}");
    let p = plan(
        &conn,
        "SELECT t.txid FROM token_utxos t INDEXED BY token_utxos_strays_new WHERE t.role = 'stray' AND t.spent_block IS NULL AND t.created_block >= ?1 \
         ORDER BY t.created_block DESC, t.txid, t.idx LIMIT 50",
        params![0],
    );
    assert!(p.contains("token_utxos_strays_new"), "{p}");
    indexed("strays", &p, &[]);
}

/// R4C2-2: the pair list, a pair book and the pair fills read the oldest `PAIR_SCAN_MAX` live pair
/// orders of the pair from `pair_live`, not every live order of the store.
#[test]
fn the_pair_reads_do_not_grow_with_pair_orders() {
    let conn = open_memory("testnet-10").unwrap();
    let (a, b) = (TOKEN_COV, [0x5b; 32]);
    let s = Seed {
        prefix: "q",
        contract: "KobPair",
        side: 1,
        token: a,
        quote: Some(b),
        parent: None,
        state: None,
        price0: None,
        remaining: 10,
        custody: false,
        listed: true,
        maker: pk(6),
    };
    let (ha, hb) = (Hash32(a), Hash32(b));
    let ctx = ctx();
    let measure = || -> Vec<(&'static str, u64)> {
        vec![
            ("pairs", steps(&conn, || pairs::pairs(&conn, &ctx, None).unwrap()).1),
            ("pairs of a token", steps(&conn, || pairs::pairs(&conn, &ctx, Some(&hb)).unwrap()).1),
            ("pair book", steps(&conn, || pairs::pair_book(&conn, &ctx, &ha, &hb, 20).unwrap()).1),
            ("pair fills", steps(&conn, || pairs::pair_fills(&conn, &ctx, &ha, &hb, None, 50).unwrap()).1),
        ]
    };
    let (aid, bid_) = (id(&conn, &a), id(&conn, &b));
    let grow = |from: i64, to: i64| {
        seed(&conn, &s, from, to);
        let oid: i64 = conn.query_row("SELECT MIN(id) FROM orders", [], |r| r.get(0)).unwrap();
        conn.execute(
            "WITH RECURSIVE n(i) AS (SELECT ?1 UNION ALL SELECT i + 1 FROM n WHERE i < ?2 - 1) \
             INSERT INTO pair_fills (block_seq, daa, ts, txid, tx_pos, order_id, contract, base_id, quote_id, side, amount_a, amount_b, price, \
             a_scale, tip_kas, counterparty) SELECT 1, 1, 1700000000000 + i, CAST(printf('g%031d', i) AS BLOB), 0, ?3, 'KobPair', ?4, ?5, 1, 10, 10, \
             1000, 1000, 0, 'route' FROM n",
            params![from, to, oid, aid, bid_],
        )
        .unwrap();
    };
    let n = pairs::PAIR_SCAN_MAX as i64 + 2_000;
    grow(0, n);
    let few = measure();
    grow(n, 2 * n);
    let many = measure();
    for ((name, f), (_, m)) in few.iter().zip(&many) {
        flat(name, *f, *m);
    }
}

/// The token events of one token and a declaration read by index, whatever the other tokens have.
#[test]
fn token_events_and_a_declaration_do_not_grow_with_other_tokens() {
    let conn = open_memory("testnet-10").unwrap();
    let grow = |from: i64, to: i64| {
        conn.execute(
            "WITH RECURSIVE n(i) AS (SELECT ?1 UNION ALL SELECT i + 1 FROM n WHERE i < ?2 - 1) \
             INSERT INTO token_events (token_cov_id, tpl_hash, ext_commit, kind, txid, block_seq, daa) \
             SELECT CAST(printf('v%031d', i) AS BLOB), NULL, NULL, 'seen', CAST(printf('w%031d', i) AS BLOB), 1, 1 FROM n",
            params![from, to],
        )
        .unwrap();
    };
    let t = Hash32([0x6c; 32]);
    let measure = || -> Vec<(&'static str, u64)> {
        vec![
            ("token events of a token", steps(&conn, || token_events(&conn, Some(&t), 50).unwrap()).1),
            ("token events", steps(&conn, || token_events(&conn, None, 50).unwrap()).1),
            ("declaration", steps(&conn, || crate::indexer::kcc23::proof_json(&conn, &t).unwrap()).1),
        ]
    };
    grow(0, 20_000);
    let few = measure();
    grow(20_000, 40_000);
    let many = measure();
    for ((name, f), (_, m)) in few.iter().zip(&many) {
        flat(name, *f, *m);
    }
}

/// R4C2-2: `book_live` and `pair_live` are exactly the live listed KAS-book and pair orders, as the live set read them
/// (`LIVE_FROM`), through inserts, a REPLACE of the state, status, listing, price and token changes, and deletes; and the
/// once-only fill (`db::backfill_book_live`) gives the same rows.
#[test]
fn the_live_books_follow_the_orders_and_their_states() {
    type Books = (Vec<(i64, i64, i64, String)>, Vec<(i64, i64, i64, i64)>);
    let reference = |conn: &Connection| -> Books {
        let book = conn
            .prepare(&format!(
                "SELECT o.id, o.token_id, o.side, printf('%.12e', CASE o.side WHEN 2 THEN -1.0 ELSE 1.0 END * o.price / MAX(COALESCE(o.scale, 1), 1)) \
                 {LIVE_FROM} WHERE s.status IN ('open','partial') AND o.listed = 1 AND o.in_book = 1 AND o.price IS NOT NULL \
                 AND o.token_id IS NOT NULL ORDER BY o.id"
            ))
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        let pair = conn
            .prepare(&format!(
                "SELECT o.id, o.token_id, o.quote_id, o.genesis_block {LIVE_FROM} WHERE s.status IN ('open','partial') AND o.listed = 1 \
                 AND o.quote_id IS NOT NULL AND o.token_id IS NOT NULL ORDER BY o.id"
            ))
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        (book, pair)
    };
    let kept = |conn: &Connection| -> Books {
        let book = conn
            .prepare("SELECT order_id, token_id, side, printf('%.12e', key) FROM book_live ORDER BY order_id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        let pair = conn
            .prepare("SELECT order_id, token_id, quote_id, genesis_block FROM pair_live ORDER BY order_id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        (book, pair)
    };
    for seed in 1..12u64 {
        let conn = super::tests::store(seed, 400);
        let want = reference(&conn);
        assert!(!want.0.is_empty() && !want.1.is_empty(), "seed {seed}: the store has live books");
        assert_eq!(kept(&conn), want, "seed {seed}: after the inserts");
        // the changes the writer makes: a REPLACE of the state, a status, the listing, a price, a token, deletes
        conn.execute_batch(
            "INSERT OR REPLACE INTO order_state SELECT order_id, CASE WHEN order_id % 3 = 0 THEN 'filled' ELSE 'partial' END, filled_amount, \
               remaining_amount, amount_exact, cur_txid, cur_idx, cur_value, state_known, last_block, last_daa FROM order_state WHERE order_id % 5 = 0; \
             UPDATE order_state SET status = 'open' WHERE order_id % 7 = 0; \
             UPDATE order_state SET status = 'cancelled' WHERE order_id % 11 = 0; \
             UPDATE orders SET listed = 1 - listed WHERE id % 4 = 1; \
             UPDATE orders SET price = price + 3, scale = 100 WHERE id % 6 = 2; \
             UPDATE orders SET in_book = 1 - in_book WHERE id % 9 = 4; \
             UPDATE orders SET token_id = NULL WHERE id % 13 = 3; \
             UPDATE orders SET quote_id = token_id WHERE id % 17 = 5; \
             DELETE FROM order_state WHERE order_id % 19 = 6; \
             DELETE FROM orders WHERE id % 23 = 7;",
        )
        .unwrap();
        let want = reference(&conn);
        assert_eq!(kept(&conn), want, "seed {seed}: after the changes");
        // the once-only fill gives the same rows
        conn.execute_batch("DELETE FROM book_live; DELETE FROM pair_live; DELETE FROM meta WHERE k = 'book_live';").unwrap();
        assert!(crate::indexer::db::backfill_book_live(&conn).unwrap().is_some());
        assert_eq!(kept(&conn), want, "seed {seed}: filled once");
        assert_eq!(crate::indexer::db::backfill_book_live(&conn).unwrap(), None, "once");
    }
}
