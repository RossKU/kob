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
use crate::testkit::{ask, bid, pk, tok_fields, EXT, KAS, SCALE, T3, TOKEN_COV, WHOLE};
use crate::tokens::TokenEntry;
use rusqlite::params;
use std::sync::atomic::{AtomicU64, Ordering};

/// Held by the tests that build large stores, and by the timed scale test (`reads_scale_tests`), so that its timings are
/// not taken while they load.
pub(super) static HEAVY: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub(super) fn heavy() -> std::sync::MutexGuard<'static, ()> {
    HEAVY.lock().unwrap_or_else(|e| e.into_inner())
}

/// The VM steps of `f` on `conn`: the progress handler is called at every VM instruction it checks
/// (`sqlite3_progress_handler` with N = 1), for every statement `f` runs.
pub(crate) fn steps<T>(conn: &Connection, f: impl FnOnce() -> T) -> (T, u64) {
    unsafe extern "C" fn tick(p: *mut std::ffi::c_void) -> std::ffi::c_int {
        // SAFETY: `p` is the counter of `steps`, which outlives the handler.
        unsafe { (*(p as *const AtomicU64)).fetch_add(1, Ordering::Relaxed) };
        0
    }
    let n = Box::new(AtomicU64::new(0));
    let t = std::time::Instant::now();
    // SAFETY: the handle is this connection's, and the handler is removed before the counter goes.
    unsafe { rusqlite::ffi::sqlite3_progress_handler(conn.handle(), 1, Some(tick), &*n as *const AtomicU64 as *mut std::ffi::c_void) };
    let r = f();
    // SAFETY: as above.
    unsafe { rusqlite::ffi::sqlite3_progress_handler(conn.handle(), 0, None, std::ptr::null_mut()) };
    let n = n.load(Ordering::Relaxed);
    WALLS.with(|w| w.borrow_mut().push((n, t.elapsed())));
    (r, n)
}

thread_local! {
    /// The wall time of each [`steps`] measurement of this thread, by its VM steps: [`flat`] prints the one of its 2 N
    /// read (R5C2-6). The time includes the progress handler, about a third more than without it.
    static WALLS: std::cell::RefCell<Vec<(u64, std::time::Duration)>> = Default::default();
}

/// The work of a read with 2 N attacker rows is that with N, within 5 % and a few hundred steps.
fn flat(name: &str, few: u64, many: u64) {
    let wall = WALLS.with(|w| w.borrow().iter().rev().find(|(n, _)| *n == many).map(|(_, d)| *d));
    eprintln!("{name}: {few} VM steps with N rows, {many} with 2 N, wall {wall:?} with 2 N");
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
            assert!(subqueries.contains(&what) || what.starts_with("(subquery"), "{name}: {step} in {p}");
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
             in_book, initial_amount, genesis_state, genesis_block, genesis_daa, listed, parent, quote_id, ext_id) \
             SELECT {cov}, ?4, ?5, 1, ?6, ?7, ?8, {SCALE}, 1, ?9 + i, 0, ?10, ?11, COALESCE(?12, x'00'), 1 + i, 1 + i, ?13, ?14, ?15, ?16 FROM n"
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
            quote,
            id(conn, &EXT)
        ],
    )
    .unwrap();
    // The order UTXO: a plain bid's escrow (1,000 KAS, so its buying power is its stated amount and more), else a carrier
    // of 0.2 KAS. The fill rules are the matcher's (R6C2a-2), so the outputs a fill builds clear KIP-9.
    let value: i64 = if matches!(s.contract, "KobBid" | "KobBidKron") { 100_000_000_000 } else { 20_000_000 };
    let utxo = "CAST(printf('u%s%030d', ?3, i) AS BLOB)";
    let join = format!("FROM n CROSS JOIN orders o ON o.covenant_id = {cov}");
    tx.execute(
        &format!(
            "{n} INSERT INTO order_utxos (txid, idx, order_id, value, spk, state, created_block, created_daa) \
             SELECT {utxo}, 0, o.id, ?5, x'00', ?4, 1, 1 {join}"
        ),
        params![from, to, s.prefix, s.state, value],
    )
    .unwrap();
    tx.execute(
        &format!(
            "{n} INSERT INTO order_state (order_id, status, filled_amount, remaining_amount, amount_exact, cur_txid, cur_idx, cur_value, \
             state_known, last_block, last_daa) SELECT o.id, 'open', 0, ?4, 1, {utxo}, 0, ?5, 1, 1, 1 {join}"
        ),
        params![from, to, s.prefix, s.remaining, value],
    )
    .unwrap();
    if s.custody {
        tx.execute(
            &format!(
                "{n} INSERT INTO token_utxos (txid, idx, token_id, order_id, amount, value, role, created_block, created_daa) \
                 SELECT {utxo}, 1, ?4, o.id, ?5, 10000000, 'custody', 1, 1 {join}"
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

/// A curve ask of two levels of 10 whole tokens, at 5 and 6 KAS per whole token (worth more than the listing minimum).
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
        level_size: 10 * WHOLE,
        total: 20 * WHOLE,
        amount_left: 20 * WHOLE,
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
    let _heavy = heavy();
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
            ("quotes sample", steps(&conn, || book_side_sorted(&conn, &ctx, &token, 1, Some(1), false).unwrap()).1),
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
        remaining: 20 * WHOLE,
        custody: true,
        listed: true,
        maker: pk(9),
    };
    let n = 12_000;
    let maintain = || crate::indexer::db::maintain_live_books(&conn, 1_000, None).unwrap();
    seed(&conn, &s, 0, n);
    maintain();
    let few = measure();
    seed(&conn, &s, n, 2 * n);
    maintain();
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
    assert_eq!(t[0].open_asks, (BOOK_ORDERS_MAX + 2 * BOOK_MOVING_MAX) as i64);
    // the rows of a book side are a prefix of `book_live`, read by its index
    let p = plan(
        &conn,
        &format!("SELECT o.id {} LEFT JOIN order_utxos u ON u.txid = s.cur_txid AND u.idx = s.cur_idx", book_from(1_000, None, true)),
        params![TOKEN_COV.to_vec(), 1],
    );
    assert!(p.contains("book_live_key"), "{p}");
    indexed("book side", &p, &["b"]);
}

/// Asks at the best price that a taker cannot fill now: `n` of them from `from`, covenant ids `prefix`, each a plain ask
/// of `state` with an exact custody, 0.50 KAS a whole token and up (below the honest asks of [`honest_book`]).
fn junk_asks(conn: &Connection, prefix: &str, state: AnyState, from: i64, n: i64) {
    let s = Seed {
        prefix,
        contract: "KobAsk",
        side: 1,
        token: TOKEN_COV,
        quote: None,
        parent: None,
        state: Some(state.encode()),
        price0: Some(50_000_000),
        remaining: 10 * WHOLE,
        custody: true,
        listed: true,
        maker: pk(7),
    };
    seed(conn, &s, from, from + n);
}

/// R4C2-2 (round 4, closed): orders that a taker cannot fill now take no place in a book read and cost it nothing. 12,000
/// asks not active yet, then 12,000 TWAP asks whose slice is not open, then 12,000 expired asks, all at a better limit
/// than every honest ask: the honest book is returned whole, not `truncated`, and the VM steps of the reads stay those of
/// the honest book alone (the rows are outside the index range read). Auctions at the best limit fill only their own
/// bound (`BOOK_MOVING_MAX`): the honest orders with a fixed quote are all still there.
#[test]
fn orders_that_cannot_be_filled_now_take_no_place_in_the_book() {
    let _heavy = heavy();
    let conn = open_memory("testnet-10").unwrap();
    honest_book(&conn);
    let maintain = || crate::indexer::db::maintain_live_books(&conn, 1_000, None).unwrap();
    maintain();
    let (token, ctx, list) = (Hash32(TOKEN_COV), ctx(), allowlist());
    let honest = serde_json::to_value(book(&conn, &ctx, &token, 20, true).unwrap()).unwrap();
    let honest_orders = serde_json::to_value(book(&conn, &ctx, &token, 20, false).unwrap()).unwrap();
    let measure = || -> Vec<(&'static str, u64)> {
        vec![
            ("book aggregate", steps(&conn, || book(&conn, &ctx, &token, 20, true).unwrap()).1),
            ("book orders", steps(&conn, || book(&conn, &ctx, &token, 20, false).unwrap()).1),
            ("depth", steps(&conn, || market::depth(&conn, &ctx, &token, Some(3), 20).unwrap()).1),
            ("stats", steps(&conn, || market::stats(&conn, &ctx, &token, Some(3)).unwrap()).1),
            ("tokens", steps(&conn, || token_list(&conn, &list).unwrap()).1),
        ]
    };
    let base = measure();
    let check = |what: &str| {
        let now = serde_json::to_value(book(&conn, &ctx, &token, 20, true).unwrap()).unwrap();
        assert_eq!(now, honest, "{what}: the honest levels, whole and not truncated");
        let now = serde_json::to_value(book(&conn, &ctx, &token, 20, false).unwrap()).unwrap();
        assert_eq!(now, honest_orders, "{what}: the honest orders");
        let stats = serde_json::to_value(market::stats(&conn, &ctx, &token, Some(3)).unwrap()).unwrap();
        assert!(stats.get("book_truncated").is_none(), "{what}: {stats}");
        assert_eq!(token_list(&conn, &list).unwrap()[0].open_asks, 10, "{what}");
        for ((name, b), (_, m)) in base.iter().zip(&measure()) {
            flat(&format!("{what}: {name}"), *b, *m);
        }
    };
    // not active yet: `activeFrom` far ahead (the column and the state)
    let later = AnyState::KobAsk(kob_protocol::state::AskState { active_from: 1 << 40, ..ask(7, 50_000_000) });
    junk_asks(&conn, "x", later, 0, 12_000);
    conn.execute("UPDATE orders SET active_from = 1099511627776 WHERE substr(covenant_id, 1, 1) = CAST('x' AS BLOB)", []).unwrap();
    maintain();
    check("12,000 asks not active yet");
    // a TWAP ask whose next slice opens `interval` after its UTXO (DAA 1)
    let twap = AnyState::KobAsk(kob_protocol::state::AskState { interval: 10_000, max_fill: WHOLE, ..ask(7, 50_000_000) });
    junk_asks(&conn, "y", twap, 0, 12_000);
    maintain();
    check("and 12,000 TWAP asks between slices");
    // expired: `expiryDaa` passed
    junk_asks(&conn, "z", AnyState::KobAsk(ask(7, 50_000_000)), 0, 12_000);
    conn.execute("UPDATE orders SET expiry_daa = 500 WHERE substr(covenant_id, 1, 1) = CAST('z' AS BLOB)", []).unwrap();
    maintain();
    check("and 12,000 expired asks");
    let open: Vec<(i64, i64)> = conn
        .prepare("SELECT open, COUNT(*) FROM book_live GROUP BY open ORDER BY open")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    // the asks not active before their expiry can never be filled (`ok` 0, R6C2a-2); the TWAP asks wait for their slice
    assert_eq!(open, vec![(0, 12_000), (1, 20), (2, 12_000), (3, 12_000)], "not yet / fillable / closed / never");

    // the slice of the TWAP asks opens at DAA 10,001: they are real asks at the best price then, and the commit at that
    // score lists them; the other junk stays out
    crate::indexer::db::maintain_live_books(&conn, 20_000, None).unwrap();
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM book_live WHERE open = 1", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 20 + 12_000);
    maintain();
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM book_live WHERE open = 1", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 20, "a revert of the DAA score moves them back");

    // auctions at the best limit: they fill their own bound only
    let decay = AnyState::KobAsk(kob_protocol::state::AskState { slope: 1, price_end: 40_000_000, ..ask(7, 50_000_000) });
    junk_asks(&conn, "w", decay, 0, 2_000);
    maintain();
    let now = serde_json::to_value(book(&conn, &ctx, &token, 2_000, false).unwrap()).unwrap();
    let ids: Vec<&str> = now["asks"].as_array().unwrap().iter().map(|o| o["covenant_id"].as_str().unwrap()).collect();
    for o in honest_orders["asks"].as_array().unwrap() {
        assert!(ids.contains(&o["covenant_id"].as_str().unwrap()), "an honest ask is still read");
    }
    assert_eq!(now["truncated"], true, "the auctions past their bound are cut");
}

/// R5A-1: a buy-first if-done entry pays its seller from the KAS it holds. 1,001 entries at a better price than every
/// honest bid, each stating 1,000 whole tokens and holding 0.2 KAS, can fill none: they take no place, the honest bids
/// are read whole and not `truncated`, and no entry is a level, the stats' best bid or a count.
#[test]
fn an_if_done_bid_without_its_escrow_takes_no_place() {
    let _heavy = heavy();
    let conn = open_memory("testnet-10").unwrap();
    honest_book(&conn);
    let maintain = || crate::indexer::db::maintain_live_books(&conn, 1_000, None).unwrap();
    maintain();
    let (token, ctx, list) = (Hash32(TOKEN_COV), ctx(), allowlist());
    let honest = serde_json::to_value(book(&conn, &ctx, &token, 20, true).unwrap()).unwrap();
    let honest_orders = serde_json::to_value(book(&conn, &ctx, &token, 20, false).unwrap()).unwrap();
    let entry = kob_protocol::state::IfdBidState { min_fill: 1_000 * WHOLE, ..crate::testkit::ifd_bid(7, 1_000) };
    assert!(entry.escrow().unwrap() > 20_000_000, "the seeded 0.2 KAS is far below its escrow");
    let s = Seed {
        prefix: "f",
        contract: "KobIfdBid",
        side: 2,
        token: TOKEN_COV,
        quote: None,
        parent: None,
        state: Some(AnyState::KobIfdBid(entry.clone()).encode()),
        price0: Some(entry.price),
        remaining: entry.amount_left,
        custody: false,
        listed: true,
        maker: pk(7),
    };
    seed(&conn, &s, 0, 1_001);
    maintain();
    assert_eq!(serde_json::to_value(book(&conn, &ctx, &token, 20, true).unwrap()).unwrap(), honest);
    assert_eq!(serde_json::to_value(book(&conn, &ctx, &token, 20, false).unwrap()).unwrap(), honest_orders);
    let stats = serde_json::to_value(market::stats(&conn, &ctx, &token, Some(3)).unwrap()).unwrap();
    assert!(stats.get("book_truncated").is_none(), "{stats}");
    assert_eq!(token_list(&conn, &list).unwrap()[0].open_bids, 10);
    let open: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM book_live bl JOIN orders o ON o.id = bl.order_id WHERE o.contract = 'KobIfdBid' AND bl.open = 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(open, 0);

    // one entry funded for three whole tokens: it is a bid of three, not of 1,000
    // three whole tokens, the carriers of one fill and a continuation KIP-9 accepts
    let three = entry.spend(3 * WHOLE, entry.price).unwrap()
        + entry.delivery_carrier
        + entry.exit_carrier
        + crate::matcher::candidate::COV_OUTPUT_MIN;
    let funded = kob_protocol::state::IfdBidState { min_fill: WHOLE, ..entry.clone() };
    let s = Seed { prefix: "g", state: Some(AnyState::KobIfdBid(funded.clone()).encode()), ..s };
    seed(&conn, &s, 0, 1);
    conn.execute("UPDATE order_utxos SET value = ?1 WHERE order_id = (SELECT id FROM orders WHERE substr(covenant_id, 1, 1) = CAST('g' AS BLOB))", [three])
        .unwrap();
    conn.execute("UPDATE order_state SET cur_value = ?1 WHERE order_id = (SELECT id FROM orders WHERE substr(covenant_id, 1, 1) = CAST('g' AS BLOB))", [three])
        .unwrap();
    conn.execute(
        "UPDATE book_live SET ok = NULL WHERE order_id = (SELECT id FROM orders WHERE substr(covenant_id, 1, 1) = CAST('g' AS BLOB))",
        [],
    )
    .unwrap();
    maintain();
    let now = serde_json::to_value(book(&conn, &ctx, &token, 20, false).unwrap()).unwrap();
    let best = &now["bids"][0];
    assert_eq!(best["contract"], "KobIfdBid");
    assert_eq!(best["fillable_now"], (3 * WHOLE).to_string());
}

/// R5C2-1: orders their own maker filled down to one base unit stay listed and fillable, but worth less than the listing
/// minimum to a taker they are dust: 1,200 such asks below every honest ask and 1,200 such bids above every honest bid take
/// none of the 1,000 places of a side. The honest orders are all read, the stats' best bid and ask and the sampled quote
/// are honest ones, the pair route has no dust, and the dust is read in its own range of 200 (`truncated` says the rest).
#[test]
fn dust_left_by_partial_fills_takes_no_place() {
    let _heavy = heavy();
    let conn = open_memory("testnet-10").unwrap();
    honest_book(&conn);
    let maintain = || crate::indexer::db::maintain_live_books(&conn, 1_000, None).unwrap();
    maintain();
    let (token, ctx) = (Hash32(TOKEN_COV), ctx());
    let honest_orders = serde_json::to_value(book(&conn, &ctx, &token, 20, false).unwrap()).unwrap();
    let honest_stats = serde_json::to_value(market::stats(&conn, &ctx, &token, Some(3)).unwrap()).unwrap();
    let a = AnyState::KobAsk(kob_protocol::state::AskState { amount_left: 1, min_fill: 1, ..ask(7, 50_000_000) });
    let s = Seed {
        prefix: "d",
        contract: "KobAsk",
        side: 1,
        token: TOKEN_COV,
        quote: None,
        parent: None,
        state: Some(a.encode()),
        price0: Some(50_000_000),
        remaining: 1,
        custody: true,
        listed: true,
        maker: pk(7),
    };
    seed(&conn, &s, 0, 1_200);
    let b = AnyState::KobBid(bid(7, 150_000_000));
    let s = Seed { prefix: "e", contract: "KobBid", side: 2, state: Some(b.encode()), price0: Some(150_000_000), custody: false, ..s };
    seed(&conn, &s, 0, 1_200);
    // the bids keep an escrow that buys about one base unit after their 10 KAS delivery carrier
    let tiny = 1_000_000_000 + 160_000;
    conn.execute("UPDATE order_utxos SET value = ?1 WHERE substr(txid, 1, 2) = CAST('ue' AS BLOB)", [tiny]).unwrap();
    conn.execute("UPDATE order_state SET cur_value = ?1 WHERE substr(cur_txid, 1, 2) = CAST('ue' AS BLOB)", [tiny]).unwrap();
    conn.execute("UPDATE order_state SET status = 'partial' WHERE remaining_amount = 1", []).unwrap();
    maintain();
    let dust: i64 = conn.query_row("SELECT COUNT(*) FROM book_live WHERE open = 1 AND auction = 2", [], |r| r.get(0)).unwrap();
    assert_eq!(dust, 2_400, "dust, fillable");
    // by default the book holds no dust and says how much it left out (R6B-L2)
    let plain = serde_json::to_value(book(&conn, &ctx, &token, 2_000, false).unwrap()).unwrap();
    assert_eq!(plain["asks"].as_array().unwrap().len() + plain["bids"].as_array().unwrap().len(), 20, "the honest orders only");
    assert_eq!(plain["dust_orders"], 2_400);
    assert!(plain.get("truncated").is_none(), "dust alone never cuts a book");
    let now = serde_json::to_value(book_with(&conn, &ctx, &token, 2_000, false, true).unwrap()).unwrap();
    for side in ["asks", "bids"] {
        let ids: Vec<&str> = now[side].as_array().unwrap().iter().map(|o| o["covenant_id"].as_str().unwrap()).collect();
        for o in honest_orders[side].as_array().unwrap() {
            assert!(ids.contains(&o["covenant_id"].as_str().unwrap()), "{side}: an honest order is still read");
        }
        assert_eq!(ids.len(), 10 + BOOK_MOVING_MAX, "{side}: the honest orders and the dust's own range");
    }
    assert!(now["asks"].as_array().unwrap().iter().any(|o| o["dust"] == true), "with include_dust the dust is there, marked");
    let stats = serde_json::to_value(market::stats(&conn, &ctx, &token, Some(3)).unwrap()).unwrap();
    for k in ["best_bid", "best_ask", "mid", "spread_bps"] {
        assert_eq!(stats[k], honest_stats[k], "{k}");
    }
    assert!(stats.get("book_truncated").is_none(), "{stats}");
    let best_ask = book_side_sorted(&conn, &ctx, &token, 1, Some(1), false).unwrap().0[0].0;
    assert_eq!(best_ask.0, 100_000_000, "the sampled quote is an honest ask");
}

/// R5C2-4: the rows a store's writer decided under other rules (an older build, other pinned templates, another listing
/// minimum) are all decided again, once; under the same rules nothing is reset.
#[test]
fn the_live_books_are_decided_again_when_their_rules_change() {
    use crate::indexer::db::{ensure_book_rules, maintain_live_books, meta_set};
    let conn = open_memory("testnet-10").unwrap();
    honest_book(&conn);
    maintain_live_books(&conn, 1_000, None).unwrap();
    let unset = || -> i64 { conn.query_row("SELECT COUNT(*) FROM book_live WHERE ok IS NULL", [], |r| r.get(0)).unwrap() };
    let dust = || -> i64 { conn.query_row("SELECT COUNT(*) FROM book_live WHERE auction = 2", [], |r| r.get(0)).unwrap() };
    assert_eq!((unset(), dust()), (0, 0));
    assert!(!ensure_book_rules(&conn, 100_000_000).unwrap(), "the rules this store was opened with");
    // an older build's rows
    meta_set(&conn, "book_live_rules", "1/0000000000000000/100000000").unwrap();
    assert!(ensure_book_rules(&conn, 100_000_000).unwrap());
    assert_eq!(unset(), 20);
    maintain_live_books(&conn, 1_000, None).unwrap();
    assert_eq!((unset(), dust()), (0, 0));
    // a listing minimum of 200 KAS: every honest ask (about 10 KAS) is now dust (below half of it); the bids, with 1,000
    // KAS of buying power each, are not
    assert!(ensure_book_rules(&conn, 20_000_000_000).unwrap());
    maintain_live_books(&conn, 1_000, None).unwrap();
    assert_eq!(dust(), 10);
    assert!(!ensure_book_rules(&conn, 20_000_000_000).unwrap());
}

/// R5C1-L3, R5C2-I3: an ask row with one custody and no stored amount makes the window's condition NULL. The writer reads
/// that as not fillable: the commit's maintenance succeeds and the row takes no place.
#[test]
fn a_null_in_the_window_condition_does_not_fail_the_commit() {
    let conn = open_memory("testnet-10").unwrap();
    let s = Seed {
        prefix: "n",
        contract: "KobAsk",
        side: 1,
        token: TOKEN_COV,
        quote: None,
        parent: None,
        state: Some(AnyState::KobAsk(ask(1, 100_000_000)).encode()),
        price0: Some(100_000_000),
        remaining: 10 * WHOLE,
        custody: true,
        listed: true,
        maker: pk(1),
    };
    seed(&conn, &s, 0, 1);
    conn.execute("UPDATE order_state SET remaining_amount = NULL", []).unwrap();
    crate::indexer::db::maintain_live_books(&conn, 1_000, None).unwrap();
    let open: i64 = conn.query_row("SELECT open FROM book_live", [], |r| r.get(0)).unwrap();
    assert_eq!(open, 3);
}

/// R5C2-3: a possibly-frozen flag that moves to the order's next UTXO (an upsert) sends the row back to the writer, so the
/// row does not keep `open = 1` and a place it then fails.
#[test]
fn a_moved_frozen_flag_takes_the_row_out_of_the_book() {
    let conn = open_memory("testnet-10").unwrap();
    let s = Seed {
        prefix: "q",
        contract: "KobAsk",
        side: 1,
        token: TOKEN_COV,
        quote: None,
        parent: None,
        state: Some(AnyState::KobAsk(ask(1, 100_000_000)).encode()),
        price0: Some(100_000_000),
        remaining: 10 * WHOLE,
        custody: true,
        listed: true,
        maker: pk(1),
    };
    seed(&conn, &s, 0, 1);
    let maintain = || crate::indexer::db::maintain_live_books(&conn, 1_000, None).unwrap();
    let open = || -> i64 { conn.query_row("SELECT open FROM book_live", [], |r| r.get(0)).unwrap() };
    maintain();
    assert_eq!(open(), 1);
    let cov: [u8; 32] = conn.query_row("SELECT covenant_id FROM orders", [], |r| r.get::<_, Vec<u8>>(0)).unwrap().try_into().unwrap();
    assert!(crate::indexer::flags::set(&conn, &cov, "test", 1).unwrap());
    maintain();
    assert_eq!(open(), 3, "flagged at its UTXO");
    // a fill moves the order to its next UTXO: the flag names the old one, so the row is fillable again
    let next = vec![0x42u8; 32];
    conn.execute("UPDATE order_utxos SET txid = ?1", [&next]).unwrap();
    conn.execute("UPDATE order_state SET cur_txid = ?1", [&next]).unwrap();
    conn.execute("UPDATE book_live SET ok = NULL", []).unwrap();
    maintain();
    assert_eq!(open(), 1);
    // the pre-simulation fails again and the flag moves to the new UTXO
    assert!(crate::indexer::flags::set(&conn, &cov, "test", 2).unwrap());
    maintain();
    assert_eq!(open(), 3, "flagged at its new UTXO");
}

/// R4C2-1, R4C2-5 and R3C2-1: one order's view (on the light lane), its events and a maker's order
/// page do not grow with its children, its live and spent strays, or a spent bought-custody history.
#[test]
fn the_order_view_does_not_grow_with_children_strays_or_spent_custody() {
    let _heavy = heavy();
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
    let t = std::time::Instant::now();
    let v = order(&conn, &ctx, &c).unwrap().unwrap();
    eprintln!("order view at its bound: {:?} wall (this build's profile)", t.elapsed());
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
    let _heavy = heavy();
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

/// R5C2-2: a block holding more of the owner's rows than one page reads (`HOLDINGS_SCAN_MAX`) is passed, not read
/// again: following `next_cursor` reaches the owner's holding of the token after it, and the paging ends.
#[test]
fn holdings_paging_moves_past_a_block_of_more_than_the_budget() {
    let conn = open_memory("testnet-10").unwrap();
    let (other, mine, owner) = (id(&conn, &[0x6c; 32]), id(&conn, &TOKEN_COV), [0x0f; 32]);
    let owner_id = id(&conn, &owner);
    crate::testkit::named_holdings_writable(&conn);
    // 10,001 live holdings of another token in block 5, then one of the token in block 6
    let n = "WITH RECURSIVE n(i) AS (SELECT ?1 UNION ALL SELECT i + 1 FROM n WHERE i < ?2 - 1)";
    let put = |from: i64, to: i64, token: i64, block: i64| {
        conn.execute(
            &format!(
                "{n} INSERT INTO token_holdings_named (txid, idx, token_id, program, family, owner_id, owner_kind, amount, value, state, role, \
                 created_block, created_daa) SELECT CAST(printf('h%031d', i) AS BLOB), 0, ?3, 'KCC20Ref', 1, ?4, 0, 5, 1, x'08', 'owned', ?5, ?5 FROM n"
            ),
            params![from, to, token, owner_id, block],
        )
        .unwrap();
    };
    put(0, HOLDINGS_SCAN_MAX as i64 + 1, other, 5);
    put(HOLDINGS_SCAN_MAX as i64 + 1, HOLDINGS_SCAN_MAX as i64 + 2, mine, 6);
    let f = HoldingFilter { owner: Some(owner.to_vec()), token: Some(Hash32(TOKEN_COV)), include_spent: false };
    let (mut cursor, mut found, mut pages) = (None, 0, 0);
    loop {
        let page = holdings(&conn, &ctx(), &f, 500, cursor.take()).unwrap();
        found += page.items.len();
        pages += 1;
        assert!(pages <= 5, "the paging does not advance");
        match page.next_cursor {
            Some(c) => cursor = parse_holding_cursor(&c),
            None => break,
        }
    }
    assert_eq!(found, 1, "the holding after the big block is reached");
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
                 created_block, created_daa, spent_block) SELECT CAST(printf('%08d%024d', (i * 104729) % 100000000, i) AS BLOB), 0, ?3, 'KCC20Ref', 1, ?4, 0, 5, 1, x'08', \
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
    let _heavy = heavy();
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
    // The rows the writer has not set yet (an older build's) are read row by row, within the bound.
    let n = pairs::PAIR_SCAN_MAX as i64 + 2_000;
    grow(0, n);
    let few = measure();
    grow(n, 2 * n);
    let many = measure();
    for ((name, f), (_, m)) in few.iter().zip(&many) {
        flat(name, *f, *m);
    }
    // Once set, these orders (no state the indexer knows) cannot be filled: they are outside the ranges read.
    crate::indexer::db::maintain_live_books(&conn, 1_000, None).unwrap();
    let open: i64 = conn.query_row("SELECT COUNT(*) FROM pair_live WHERE open = 1", [], |r| r.get(0)).unwrap();
    assert_eq!(open, 0);
    for ((name, f), (_, m)) in few.iter().zip(&measure()).filter(|((name, _), _)| *name != "pair fills") {
        assert!(m * 20 < *f, "{name}: {f} VM steps over unset rows, {m} once they are set");
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

/// R4C2-6: the keepers' read of the automatic creator locks due (`/v1/creator-locks?due=`) reads the locks still holding
/// (`kcc23_auto_live`), not every lock released before them; the triggers move a lock out when its output is spent.
#[test]
fn the_due_creator_locks_do_not_grow_with_released_ones() {
    let conn = open_memory("testnet-10").unwrap();
    crate::testkit::named_holdings_writable(&conn);
    let (token, owner) = (id(&conn, &TOKEN_COV), id(&conn, &[0x0f; 32]));
    let grow = |from: i64, to: i64, spent: bool| {
        let n = "WITH RECURSIVE n(i) AS (SELECT ?1 UNION ALL SELECT i + 1 FROM n WHERE i < ?2 - 1)";
        conn.execute(
            &format!(
                "{n} INSERT INTO token_holdings_named (txid, idx, token_id, program, family, owner_id, owner_kind, amount, value, state, role, \
                 created_block, created_daa) SELECT CAST(printf('%08d%024d', (i * 7919) % 100000000, i) AS BLOB), 0, ?3, 'KCC20Ref', 1, ?4, 0, 5, 1, x'08', 'owned', 2, 2 FROM n"
            ),
            params![from, to, token, owner],
        )
        .unwrap();
        conn.execute(
            &format!(
                "{n} INSERT INTO kcc23_auto_locks (until, covenant_id, genesis_block, genesis_txid, idx) \
                 SELECT 1000 + i, CAST(printf('m%031d', i) AS BLOB), 2, CAST(printf('%08d%024d', (i * 7919) % 100000000, i) AS BLOB), 0 FROM n"
            ),
            params![from, to],
        )
        .unwrap();
        if spent {
            // released: the locked output is spent
            conn.execute(
                &format!(
                    "{n} UPDATE token_holdings SET spent_block = 3 WHERE tx_id IN (SELECT t.id FROM n JOIN txs t \
                     ON substr(t.h, 1, 8) = substr(CAST(printf('%08d%024d', (i * 7919) % 100000000, i) AS BLOB), 1, 8) AND t.h = CAST(printf('%08d%024d', (i * 7919) % 100000000, i) AS BLOB))"
                ),
                params![from, to],
            )
            .unwrap();
        }
    };
    let live = |c: &Connection| -> i64 { c.query_row("SELECT COUNT(*) FROM kcc23_auto_live", [], |r| r.get(0)).unwrap() };
    grow(0, 5_000, true);
    grow(1_000_000, 1_000_010, false);
    assert_eq!(live(&conn), 10, "the released locks left");
    let few = steps(&conn, || crate::indexer::kcc23::due_locks(&conn, u64::MAX, 200).unwrap()).1;
    grow(5_000, 10_000, true);
    let many = steps(&conn, || crate::indexer::kcc23::due_locks(&conn, u64::MAX, 200).unwrap()).1;
    flat("creator-locks due", few, many);
    // the once-only fill gives the same rows
    conn.execute_batch("DELETE FROM kcc23_auto_live; DELETE FROM meta WHERE k = 'kcc23_auto_live';").unwrap();
    assert_eq!(crate::indexer::db::backfill_auto_live(&conn).unwrap(), Some(10));
    assert_eq!(live(&conn), 10);
}

/// R5C2-6: the pair candles take each shown bucket's volume from the writer's aggregates (`pair_volume`), one row each and no
/// fills: 210,000 then 420,000 pair fills in the ten shown minutes cost the same, and the volume is all of them, never cut.
#[test]
fn the_pair_candles_read_one_volume_row_a_bucket() {
    let _heavy = heavy();
    let conn = open_memory("testnet-10").unwrap();
    let (a, b) = ([0x5c; 32], [0x5d; 32]);
    let (aid, bid_) = (id(&conn, &a), id(&conn, &b));
    // a KAS trade of each token in each of the ten minutes, so that the ten buckets have a rate
    let s = Seed {
        prefix: "r",
        contract: "KobAsk",
        side: 1,
        token: a,
        quote: None,
        parent: None,
        state: None,
        price0: Some(100_000_000),
        remaining: 1,
        custody: false,
        listed: false,
        maker: pk(8),
    };
    seed(&conn, &s, 0, 1);
    seed(&conn, &Seed { prefix: "s", token: b, ..s }, 0, 1);
    let t0 = 1_700_000_000_000i64;
    for (prefix, tok) in [("r", aid), ("s", bid_)] {
        conn.execute(
            "WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i + 1 FROM n WHERE i < 9) \
             INSERT INTO order_events (order_id, block_seq, daa, ts, txid, tx_pos, kind, token_id, side, amount, price, closes, detail) \
             SELECT (SELECT id FROM orders WHERE substr(covenant_id, 1, 1) = CAST(?1 AS BLOB)), 1, 1, ?3 + i * 60000, CAST(printf('k%s%030d', ?1, i) AS BLOB), 0, \
             'fill', ?2, 1, 1000, 100000000, 0, '{}' FROM n",
            params![prefix, tok, t0],
        )
        .unwrap();
    }
    for t in [a, b] {
        crate::indexer::candles::rebuild(&conn, &Hash32(t), 1_000).unwrap();
    }
    let oid: i64 = conn.query_row("SELECT MIN(id) FROM orders", [], |r| r.get(0)).unwrap();
    let grow = |from: i64, to: i64| {
        conn.execute(
            "WITH RECURSIVE n(i) AS (SELECT ?1 UNION ALL SELECT i + 1 FROM n WHERE i < ?2 - 1) \
             INSERT INTO pair_fills (block_seq, daa, ts, txid, tx_pos, order_id, contract, base_id, quote_id, side, amount_a, amount_b, price, \
             a_scale, tip_kas, counterparty) SELECT 1, 1, ?3 + (i % 600000), CAST(printf('p%031d', i) AS BLOB), 0, ?4, 'KobPair', ?5, ?6, 1, 10, 10, \
             1000, 1000, 0, 'route' FROM n",
            params![from, to, t0, oid, aid, bid_],
        )
        .unwrap();
    };
    let read =
        || pairs::pair_candles(&conn, &Hash32(a), &Hash32(b), (Some(3), Some(3)), "1m", 60_000, Some(t0), Some(t0 + 600_000), 500);
    let n = 210_000;
    grow(0, n);
    crate::indexer::pair_volume::refresh(&conn).unwrap();
    let (v, few) = steps(&conn, || read().unwrap());
    assert_eq!(v.items.len(), 10);
    grow(n, 2 * n);
    crate::indexer::pair_volume::refresh(&conn).unwrap();
    let (v, many) = steps(&conn, || read().unwrap());
    flat("pair candles", few, many);
    let total: u64 = v.items.iter().map(|c| c.pair_fills).sum();
    assert_eq!(total, 2 * n as u64, "every pair fill of the window counts");
}

/// The logo lists (`GET /v1/logo-votes`, `GET /v1/logo-blocklist`) read one row per token, not one per vote or report:
/// 5,000 then 10,000 votes and reports on the same 100 tokens cost the same.
#[test]
fn the_logo_lists_do_not_grow_with_votes_or_reports() {
    let conn = open_memory("testnet-10").unwrap();
    conn.execute_batch(crate::logo_votes::SCHEMA).unwrap();
    conn.execute_batch(crate::logo_reports::SCHEMA).unwrap();
    for sql in [
        "INSERT INTO logo_vote_totals (covenant_id, up, down, updated_ms) SELECT CAST(printf('t%031d', i) AS BLOB), 1, 2, 1 FROM n",
        "INSERT INTO logo_blocklist (covenant_id, state, first_ms, hidden_ms, restored_ms, report_count, updated_ms) \
         SELECT CAST(printf('t%031d', i) AS BLOB), 'hidden', 1, 1, NULL, 1, 1 FROM n",
    ] {
        conn.execute(&format!("WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i + 1 FROM n WHERE i < 99) {sql}"), []).unwrap();
    }
    let grow = |from: i64, to: i64| {
        let n = "WITH RECURSIVE n(i) AS (SELECT ?1 UNION ALL SELECT i + 1 FROM n WHERE i < ?2 - 1)";
        conn.execute(
            &format!(
                "{n} INSERT INTO logo_votes (covenant_id, voter, vote, day, nonce, signature, created_ms, outcome) \
                 SELECT CAST(printf('t%031d', i % 100) AS BLOB), CAST(printf('v%031d', i) AS BLOB), 1, '2026-11-20', '0', zeroblob(64), 1, 'counted' FROM n"
            ),
            params![from, to],
        )
        .unwrap();
        conn.execute(
            &format!(
                "{n} INSERT INTO logo_reports (covenant_id, reason, source, ref, created_ms, outcome) \
                 SELECT CAST(printf('t%031d', i % 100) AS BLOB), 'other', 'web', NULL, 1, 'recorded' FROM n"
            ),
            params![from, to],
        )
        .unwrap();
    };
    let measure = || -> Vec<(&'static str, u64)> {
        vec![
            ("logo votes", steps(&conn, || crate::logo_votes::totals(&conn).unwrap()).1),
            ("logo blocklist", steps(&conn, || crate::logo_reports::blocklist(&conn).unwrap()).1),
        ]
    };
    grow(0, 5_000);
    let few = measure();
    grow(5_000, 10_000);
    let many = measure();
    for ((name, f), (_, m)) in few.iter().zip(&many) {
        flat(name, *f, *m);
    }
}

/// R5F-2, measured once (`cargo test -p kobd --lib first_open_time -- --ignored --nocapture`; `KOB_LIVE_ORDERS`, default
/// 100,000): the first open of this build over a store whose live-book tables an older build never filled. The fill and the
/// windows of every row run in one transaction; for comparison, 2,000 rows' windows set one commit each, as before.
#[test]
#[ignore]
fn first_open_time_of_a_store_of_live_orders() {
    let n: i64 = std::env::var("KOB_LIVE_ORDERS").ok().and_then(|v| v.parse().ok()).unwrap_or(100_000);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.sqlite3");
    {
        let conn = crate::indexer::db::open_writer(&path, "testnet-10").unwrap();
        let s = Seed {
            prefix: "l",
            contract: "KobAsk",
            side: 1,
            token: TOKEN_COV,
            quote: None,
            parent: None,
            state: Some(AnyState::KobAsk(ask(1, 100_000_000)).encode()),
            price0: Some(100_000_000),
            remaining: 10 * WHOLE,
            custody: true,
            listed: true,
            maker: pk(1),
        };
        seed(&conn, &s, 0, n);
        // as the previous build left it: no live-book rows, no rules
        conn.execute_batch(
            "DELETE FROM book_live; DELETE FROM pair_live; DELETE FROM meta WHERE k IN ('book_live', 'book_live_rules');",
        )
        .unwrap();
    }
    let t = std::time::Instant::now();
    let conn = crate::indexer::db::open_writer(&path, "testnet-10").unwrap();
    let first = t.elapsed();
    let set: i64 = conn.query_row("SELECT COUNT(*) FROM book_live WHERE ok IS NOT NULL", [], |r| r.get(0)).unwrap();
    assert_eq!(set, n);
    let t = std::time::Instant::now();
    drop(conn);
    let conn = crate::indexer::db::open_writer(&path, "testnet-10").unwrap();
    let again = t.elapsed();
    conn.execute("UPDATE book_live SET ok = NULL WHERE rowid IN (SELECT rowid FROM book_live LIMIT 2000)", []).unwrap();
    let t = std::time::Instant::now();
    crate::indexer::db::maintain_live_books(&conn, 1_000, None).unwrap();
    let autocommit = t.elapsed();
    eprintln!("first open over {n} live orders: {first:?}; a later open: {again:?}; 2,000 windows one commit each: {autocommit:?}");
}

/// R5C2-6, R6C2b-2: the worst case of a book read, with every range full of curve asks of 1,000 levels
/// (`CURVE_MAX_LEVELS`): the fixed range with N at a better price than every honest ask, the dust range with curves whose
/// reachable levels are worth less than half the listing minimum, and the pending range with curves that opened after the
/// store's last commit. N then 2 N of each cost the same VM steps, with and without `include_dust`; the wall time per read
/// is printed (`-- --nocapture`).
#[test]
fn worst_case_book_reads_over_1000_level_curves() {
    let _heavy = heavy();
    let conn = open_memory("testnet-10").unwrap();
    honest_book(&conn);
    let (hh, p, s) = tok_fields(T3);
    let levels = kob_protocol::state::CURVE_MAX_LEVELS as i64;
    let curve = |size: i64, active_from: i64| {
        AnyState::KobCurveAsk(kob_protocol::state::CurveAskState {
            maker: pk(9),
            token_cov_id: TOKEN_COV,
            token_tpl_hash: hh,
            tpl_prefix_len: p,
            tpl_suffix_len: s,
            scale: SCALE,
            min_fill: 1,
            tip: 0,
            active_from,
            level_size: size,
            total: levels * size,
            amount_left: levels * size,
            extension_commitment: EXT,
            table: (0..levels).map(|k| 50_000_000 + k * 10_000).collect(),
        })
    };
    let seedc = |prefix: &'static str, size: i64, active_from: i64| Seed {
        prefix,
        contract: "KobCurveAsk",
        side: 1,
        token: TOKEN_COV,
        quote: None,
        parent: None,
        state: Some(curve(size, active_from).encode()),
        price0: Some(50_000_000),
        remaining: levels * size,
        custody: true,
        listed: true,
        maker: pk(9),
    };
    // the store's last commit was at DAA 1,000, the node is at 2,000: the pending curves opened at 1,500
    let ctx = ReadCtx { node_daa: Some(2_000), settle_depth_daa: 100, now_unix: None };
    let token = Hash32(TOKEN_COV);
    let grow = |from: i64, to: i64, m_from: i64, m_to: i64| {
        seed(&conn, &seedc("k", WHOLE, 0), from, to);
        // each level worth 0.005 KAS: the 8 levels one fill can walk are worth less than half the listing minimum
        seed(&conn, &seedc("l", WHOLE / 100, 0), m_from, m_to);
        seed(&conn, &seedc("m", WHOLE, 1_500), m_from, m_to);
        conn.execute("UPDATE orders SET active_from = 1500 WHERE substr(covenant_id, 1, 1) = CAST('m' AS BLOB)", []).unwrap();
        conn.execute("UPDATE book_live SET ok = NULL", []).unwrap();
        crate::indexer::db::maintain_live_books(&conn, 1_000, None).unwrap();
    };
    let measure = || -> Vec<(&'static str, u64)> {
        vec![
            ("book orders depth 200, all ranges", steps(&conn, || book_with(&conn, &ctx, &token, 200, false, true).unwrap()).1),
            ("book levels depth 200, all ranges", steps(&conn, || book_with(&conn, &ctx, &token, 200, true, true).unwrap()).1),
            ("book orders depth 200", steps(&conn, || book(&conn, &ctx, &token, 200, false).unwrap()).1),
            ("depth 200, all ranges", steps(&conn, || market::depth_with(&conn, &ctx, &token, Some(3), 200, true).unwrap()).1),
            ("stats", steps(&conn, || market::stats(&conn, &ctx, &token, Some(3)).unwrap()).1),
        ]
    };
    let (n, m) = (BOOK_ORDERS_MAX as i64 + 200, BOOK_MOVING_MAX as i64 + 50);
    grow(0, n, 0, m);
    let classes: Vec<(i64, i64, i64)> = conn
        .prepare("SELECT open, auction, COUNT(*) FROM book_live GROUP BY open, auction ORDER BY open, auction")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert!(classes.contains(&(1, CLASS_DUST, BOOK_MOVING_MAX as i64 + 50)), "{classes:?}");
    assert!(classes.iter().any(|(open, _, n)| *open == 0 && *n == BOOK_MOVING_MAX as i64 + 50), "{classes:?}");
    let few = measure();
    grow(n, 2 * n, m, 2 * m);
    let many = measure();
    for ((name, f), (_, m)) in few.iter().zip(&many) {
        flat(name, *f, *m);
    }
    assert!(book(&conn, &ctx, &token, 200, false).unwrap().truncated);
}

/// R5C2-6: the pair reads over 20,001 fillable `KobPair` asks of one pair with a real state each and its exact custody (the
/// reads take each from its `pair_live` row as the writer decided it), printed with their wall time (`-- --nocapture`).
#[test]
fn worst_case_pair_reads_over_real_pair_orders() {
    use crate::testkit::rtip;
    let _heavy = heavy();
    let conn = open_memory("testnet-10").unwrap();
    let (th, p, s) = tok_fields(T3);
    let quote = [0x5a; 32];
    let x = kob_protocol::state::PairState {
        maker: pk(3),
        side: kob_protocol::state::SIDE_ASK,
        s_cov_id: TOKEN_COV,
        s_tpl_hash: th,
        s_pre: p,
        s_suf: s,
        s_family: 1,
        s_scale: 1000,
        t_cov_id: quote,
        t_tpl_hash: th,
        t_pre: p,
        t_suf: s,
        t_family: 1,
        t_ext: [0xee; 32],
        t_scale: 1000,
        min_fill: 1000,
        price: 2_000,
        tip: 0,
        tif: 0,
        active_from: 0,
        expiry_daa: 400_000_000,
        refund_tip: rtip(T3),
        delivery_carrier: 200_000_000,
        interval: 0,
        max_fill: 0,
        slope: 0,
        price_end: 0,
        decay_step: 0,
        amount_left: 10_000,
        custody: 10_000,
        s_ext: [0xee; 32],
        mode: 0,
        recv_threshold: 0,
        topup_tip: 0,
        bought: 0,
    };
    let st = AnyState::KobPair(x);
    assert!(crate::sanity::check(&st).is_ok());
    let sd = Seed {
        prefix: "r",
        contract: "KobPair",
        side: 1,
        token: TOKEN_COV,
        quote: Some(quote),
        parent: None,
        state: Some(st.encode()),
        price0: None,
        remaining: 10_000,
        custody: true,
        listed: true,
        maker: pk(3),
    };
    seed(&conn, &sd, 0, pairs::PAIR_SCAN_MAX as i64 + 1);
    crate::indexer::db::maintain_live_books(&conn, 1_000, None).unwrap();
    let open: i64 = conn.query_row("SELECT COUNT(*) FROM pair_live WHERE open = 1", [], |r| r.get(0)).unwrap();
    assert_eq!(open, pairs::PAIR_SCAN_MAX as i64 + 1, "every pair order is fillable");
    let ctx = ctx();
    let (base, q) = (Hash32(TOKEN_COV), Hash32(quote));
    for (name, f) in [
        ("pair book", &(|| steps(&conn, || pairs::pair_book(&conn, &ctx, &base, &q, 20).unwrap()).1) as &dyn Fn() -> u64),
        ("pairs of a token", &|| steps(&conn, || pairs::pairs(&conn, &ctx, Some(&base)).unwrap()).1),
    ] {
        let t = std::time::Instant::now();
        let n = f();
        eprintln!("{name}: {:?} wall, {n} VM steps", t.elapsed());
        // a ceiling of twice the measured steps (R7C2b-I4): the table in `api-bounds.md` cites them
        let ceiling = if name == "pair book" { 2 * 62_183 } else { 2 * 780_122 };
        assert!(n <= ceiling, "{name}: {n} VM steps, past {ceiling}");
    }
}

/// R5C2-4: a store whose trigger and index an earlier build created with another body gets this build's at open (`CREATE
/// ... IF NOT EXISTS` keeps the old one), in the open's transaction; under this build's bodies nothing is recreated.
#[test]
fn triggers_and_indexes_of_an_older_body_are_recreated() {
    let conn = open_memory("testnet-10").unwrap();
    let stored =
        |name: &str| -> String { conn.query_row("SELECT sql FROM sqlite_master WHERE name = ?1", [name], |r| r.get(0)).unwrap() };
    let (trigger, index) = (stored("book_live_flag_update"), stored("book_live_pending"));
    assert!(crate::indexer::db::refresh_schema_objects(&conn).unwrap().is_empty());
    conn.execute_batch(
        "DROP TRIGGER book_live_flag_update; CREATE TRIGGER book_live_flag_update AFTER UPDATE ON order_flags BEGIN SELECT 1; END; \
         DROP INDEX book_live_pending; CREATE INDEX book_live_pending ON book_live (token_id, side);",
    )
    .unwrap();
    let mut done = crate::indexer::db::refresh_schema_objects(&conn).unwrap();
    done.sort();
    assert_eq!(done, vec!["book_live_flag_update".to_string(), "book_live_pending".to_string()]);
    assert_eq!((stored("book_live_flag_update"), stored("book_live_pending")), (trigger, index));
    assert!(crate::indexer::db::refresh_schema_objects(&conn).unwrap().is_empty());
}

/// R5C2-6: a store an earlier build of schema 14 created has `book_live` and `pair_live` without the columns the reads take
/// now; the next open adds them, and the rules reset fills them.
#[test]
fn a_store_without_the_live_read_columns_gains_them_at_open() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.sqlite3");
    {
        let conn = crate::indexer::db::open_writer(&path, "testnet-10").unwrap();
        honest_book(&conn);
        conn.execute_batch(
            "ALTER TABLE book_live DROP COLUMN levels; ALTER TABLE pair_live DROP COLUMN q_origin; ALTER TABLE pair_live DROP COLUMN ask; \
             DELETE FROM meta WHERE k = 'book_live_rules';",
        )
        .unwrap();
    }
    let conn = crate::indexer::db::open_writer(&path, "testnet-10").unwrap();
    let levels: i64 = conn.query_row("SELECT COUNT(*) FROM book_live WHERE levels = 1 AND ok = 1", [], |r| r.get(0)).unwrap();
    assert_eq!(levels, 20);
    let cols: i64 = conn
        .query_row("SELECT COUNT(*) FROM pragma_table_info('pair_live') WHERE name IN ('ask', 'q_origin')", [], |r| r.get(0))
        .unwrap();
    assert_eq!(cols, 2);
}

/// The `book_live` class (`auction`) and `ok` of the orders whose covenant ids start with `prefix`.
fn class_of(conn: &Connection, prefix: &str) -> Vec<(i64, i64)> {
    conn.prepare(
        "SELECT COALESCE(bl.ok, -1), bl.auction FROM book_live bl JOIN orders o ON o.id = bl.order_id \
         WHERE substr(o.covenant_id, 1, length(?1)) = CAST(?1 AS BLOB) ORDER BY o.id",
    )
    .unwrap()
    .query_map([prefix], |r| Ok((r.get(0)?, r.get(1)?)))
    .unwrap()
    .map(|r| r.unwrap())
    .collect()
}

/// Sets the order UTXO's value of the orders whose covenant ids start with `prefix`.
fn set_value(conn: &Connection, prefix: &str, value: i64) {
    let ids = "(SELECT id FROM orders WHERE substr(covenant_id, 1, length(?1)) = CAST(?1 AS BLOB))";
    conn.execute(&format!("UPDATE order_utxos SET value = ?2 WHERE order_id IN {ids}"), params![prefix, value]).unwrap();
    conn.execute(&format!("UPDATE order_state SET cur_value = ?2 WHERE order_id IN {ids}"), params![prefix, value]).unwrap();
    conn.execute(&format!("UPDATE book_live SET ok = NULL WHERE order_id IN {ids}"), params![prefix]).unwrap();
}

/// R6C2a-2: orders whose covenant no fill can ever take (an ask whose tip is above its price; an ask and a bid whose
/// minimum fill is above their maximum fill; a curve of one cheap base unit and a remainder past its table) take no place:
/// 1,001 of each at a better price than the honest book leave it whole, not cut, and they never set the stats' touch or
/// the sampled quote.
#[test]
fn orders_no_fill_can_take_take_no_place() {
    use kob_protocol::state::{AskState, BidState, CurveAskState};
    let _heavy = heavy();
    let conn = open_memory("testnet-10").unwrap();
    honest_book(&conn);
    let maintain = || crate::indexer::db::maintain_live_books(&conn, 1_000, None).unwrap();
    maintain();
    let (token, ctx) = (Hash32(TOKEN_COV), ctx());
    let honest = serde_json::to_value(book(&conn, &ctx, &token, 20, false).unwrap()).unwrap();
    let honest_stats = serde_json::to_value(market::stats(&conn, &ctx, &token, Some(3)).unwrap()).unwrap();
    let base = Seed {
        prefix: "1",
        contract: "KobAsk",
        side: 1,
        token: TOKEN_COV,
        quote: None,
        parent: None,
        state: Some(AnyState::KobAsk(AskState { tip: 50_000_001, ..ask(7, 50_000_000) }).encode()),
        price0: Some(50_000_000),
        remaining: 10 * WHOLE,
        custody: true,
        listed: true,
        maker: pk(7),
    };
    seed(&conn, &base, 0, 1_001);
    let mm = AnyState::KobAsk(AskState { max_fill: 2 * WHOLE, min_fill: 3 * WHOLE, ..ask(7, 50_000_000) });
    seed(&conn, &Seed { prefix: "2", state: Some(mm.encode()), ..base }, 0, 1_001);
    let bid_mm = AnyState::KobBid(BidState { max_fill: 2 * WHOLE, min_fill: 3 * WHOLE, ..bid(7, 150_000_000) });
    let bids = Seed {
        prefix: "3",
        contract: "KobBid",
        side: 2,
        state: Some(bid_mm.encode()),
        price0: Some(150_000_000),
        custody: false,
        ..base
    };
    seed(&conn, &bids, 0, 1_001);
    let (hh, p, s) = tok_fields(T3);
    let one = AnyState::KobCurveAsk(CurveAskState {
        maker: pk(9),
        token_cov_id: TOKEN_COV,
        token_tpl_hash: hh,
        tpl_prefix_len: p,
        tpl_suffix_len: s,
        scale: SCALE,
        min_fill: 1,
        tip: 0,
        active_from: 0,
        level_size: 1,
        total: 10 * WHOLE,
        amount_left: 10 * WHOLE,
        extension_commitment: EXT,
        table: vec![50_000_000],
    });
    seed(&conn, &Seed { prefix: "4", contract: "KobCurveAsk", state: Some(one.encode()), ..base }, 0, 1_001);
    maintain();
    for prefix in ["1", "2", "3"] {
        assert!(class_of(&conn, prefix).iter().all(|(ok, _)| *ok == 0), "{prefix}: no fill can take it");
    }
    assert!(class_of(&conn, "4").iter().all(|(ok, class)| *ok == 0 || *class == CLASS_DUST), "td: one base unit is dust");
    let now = serde_json::to_value(book(&conn, &ctx, &token, 20, false).unwrap()).unwrap();
    assert_eq!((&now["asks"], &now["bids"]), (&honest["asks"], &honest["bids"]));
    assert!(now.get("truncated").is_none(), "{now}");
    let stats = serde_json::to_value(market::stats(&conn, &ctx, &token, Some(3)).unwrap()).unwrap();
    for k in ["best_bid", "best_ask", "mid", "spread_bps"] {
        assert_eq!(stats[k], honest_stats[k], "{k}");
    }
    assert_eq!(book_side_sorted(&conn, &ctx, &token, 1, Some(1), false).unwrap().0[0].0 .0, 100_000_000);
}

/// R6C2a-1: an ask of one base unit decaying from 2·10^13 sompi a token to 1 is valued at its floor, so it is dust from
/// the start: it never sets the stats' best ask, mid or spread, or the sampled quote.
#[test]
fn a_decayed_ask_of_dust_does_not_set_the_touch() {
    use kob_protocol::state::AskState;
    let conn = open_memory("testnet-10").unwrap();
    honest_book(&conn);
    let maintain = |daa| crate::indexer::db::maintain_live_books(&conn, daa, None).unwrap();
    maintain(1_000);
    let (token, ctx) = (Hash32(TOKEN_COV), ctx());
    let honest_stats = serde_json::to_value(market::stats(&conn, &ctx, &token, Some(3)).unwrap()).unwrap();
    let decay = AskState {
        amount_left: 1,
        min_fill: 1,
        price: 20_000_000_000_000,
        slope: 10_000_000_000_000,
        price_end: 1,
        decay_step: 10,
        tip: 0,
        ..ask(7, 0)
    };
    let s = Seed {
        prefix: "5",
        contract: "KobAsk",
        side: 1,
        token: TOKEN_COV,
        quote: None,
        parent: None,
        state: Some(AnyState::KobAsk(decay).encode()),
        price0: Some(20_000_000_000_000),
        remaining: 1,
        custody: true,
        listed: true,
        maker: pk(7),
    };
    seed(&conn, &s, 0, 1);
    maintain(1_000);
    let got = class_of(&conn, "5");
    assert!(got.iter().all(|(ok, class)| *ok == 0 || *class == CLASS_DUST), "{got:?}");
    let stats = serde_json::to_value(market::stats(&conn, &ctx, &token, Some(3)).unwrap()).unwrap();
    for k in ["best_ask", "mid", "spread_bps"] {
        assert_eq!(stats[k], honest_stats[k], "{k}");
    }
    assert_eq!(book_side_sorted(&conn, &ctx, &token, 1, Some(1), false).unwrap().0[0].0 .0, 100_000_000);
}

/// R6C2a-I1: a buy-first entry with no carriers can pay for its fill, but the delivery and the exit it builds would be
/// below KIP-9's minimum output, so no builder or executor fills it: it takes no place.
#[test]
fn an_entry_whose_fill_kip9_refuses_takes_no_place() {
    let conn = open_memory("testnet-10").unwrap();
    let entry = kob_protocol::state::IfdBidState { delivery_carrier: 0, exit_carrier: 0, ..crate::testkit::ifd_bid(7, 10) };
    let s = Seed {
        prefix: "6",
        contract: "KobIfdBid",
        side: 2,
        token: TOKEN_COV,
        quote: None,
        parent: None,
        state: Some(AnyState::KobIfdBid(entry.clone()).encode()),
        price0: Some(entry.price),
        remaining: entry.amount_left,
        custody: false,
        listed: true,
        maker: pk(7),
    };
    seed(&conn, &s, 0, 1);
    set_value(&conn, "6", entry.escrow().unwrap() + 100 * KAS as i64);
    crate::indexer::db::maintain_live_books(&conn, 1_000, None).unwrap();
    assert_eq!(class_of(&conn, "6")[0].0, 0);
}

/// R6C2a-I2: an order an honest builder (the web, `kob place`) places at the listing minimum is not dust, for every kind
/// in a KAS book: an ask of 1 KAS of tokens, a bid buying 1 KAS of tokens with its delivery carrier (1 KAS, the default),
/// a curve of 1 KAS over 4 levels, a buy-first and a sell-first entry with the escrow and prefund their builders compute.
#[test]
fn an_honest_minimum_order_is_not_dust() {
    use kob_protocol::state::{AskState, BidState, CurveAskState, IfdAskState, IfdBidState};
    let conn = open_memory("testnet-10").unwrap();
    let carrier = KAS as i64; // `defaults::DEFAULT_CARRIER`, the web's and the CLI's
    let mk = |prefix: &'static str, contract: &'static str, side: i64, state: AnyState, custody: bool| {
        let s = Seed {
            prefix,
            contract,
            side,
            token: TOKEN_COV,
            quote: None,
            parent: None,
            remaining: state.amount_left().unwrap_or(WHOLE),
            price0: crate::model::terms_of(&state).price,
            state: Some(state.encode()),
            custody,
            listed: true,
            maker: pk(7),
        };
        seed(&conn, &s, 0, 1);
    };
    let a = AskState { amount_left: WHOLE, min_fill: 1, tip: 0, ..ask(7, KAS as i64) };
    mk("7", "KobAsk", 1, AnyState::KobAsk(a), true);
    set_value(&conn, "7", carrier);
    let b = BidState { min_fill: WHOLE, delivery_carrier: carrier, tip: 0, ..bid(7, KAS as i64) };
    mk("8", "KobBid", 2, AnyState::KobBid(b.clone()), false);
    set_value(&conn, "8", b.escrow(WHOLE, 1).unwrap());
    let (hh, p, s) = tok_fields(T3);
    let c = CurveAskState {
        maker: pk(9),
        token_cov_id: TOKEN_COV,
        token_tpl_hash: hh,
        tpl_prefix_len: p,
        tpl_suffix_len: s,
        scale: SCALE,
        min_fill: 1,
        tip: 0,
        active_from: 0,
        level_size: WHOLE / 4,
        total: WHOLE,
        amount_left: WHOLE,
        extension_commitment: EXT,
        table: vec![KAS as i64; 4],
    };
    mk("9", "KobCurveAsk", 1, AnyState::KobCurveAsk(c), true);
    set_value(&conn, "9", carrier);
    let ib = IfdBidState {
        amount_left: WHOLE,
        min_fill: WHOLE,
        price: KAS as i64,
        tip: 0,
        delivery_carrier: carrier,
        exit_carrier: carrier,
        ..crate::testkit::ifd_bid(7, 1)
    };
    mk("A", "KobIfdBid", 2, AnyState::KobIfdBid(ib.clone()), false);
    set_value(&conn, "A", ib.escrow().unwrap());
    let ia = IfdAskState {
        amount_left: WHOLE,
        min_fill: WHOLE,
        price: KAS as i64,
        tip: 0,
        exit_carrier: carrier,
        ..crate::testkit::ifd_ask(7)
    };
    mk("B", "KobIfdAsk", 1, AnyState::KobIfdAsk(ia.clone()), true);
    set_value(&conn, "B", ia.escrow(carrier).unwrap());
    crate::indexer::db::maintain_live_books(&conn, 1_000, None).unwrap();
    for prefix in ["7", "8", "9", "A", "B"] {
        let got = class_of(&conn, prefix);
        assert_eq!(got.len(), 1, "{prefix}");
        assert!(got[0].0 == 1 && got[0].1 != CLASS_DUST, "{prefix}: {got:?} is fillable and not dust");
    }
}

/// R6C2b-1: a chart answers in whole buckets, so a window whose edges fall inside a day full of fills reads the day's
/// aggregate rows, never its fills: N then 2 N fills in the edge day cost the same VM steps, at `1d` and at `1h`.
#[test]
fn candle_edges_do_not_grow_with_the_fills_of_a_day() {
    let _heavy = heavy();
    let conn = open_memory("testnet-10").unwrap();
    honest_book(&conn);
    let token = Hash32(TOKEN_COV);
    let (oid, tid): (i64, i64) =
        (conn.query_row("SELECT id FROM orders WHERE contract = 'KobAsk' LIMIT 1", [], |r| r.get(0)).unwrap(), id(&conn, &TOKEN_COV));
    let (d0, day) = (1_700_006_400_000i64, 86_400_000i64);
    let grow = |from: i64, to: i64| {
        conn.execute(
            "WITH RECURSIVE n(i) AS (SELECT ?1 UNION ALL SELECT i + 1 FROM n WHERE i < ?2 - 1) \
             INSERT INTO order_events (order_id, block_seq, daa, ts, txid, tx_pos, kind, token_id, side, amount, price, closes, detail) \
             SELECT ?3, 1, 1, ?5 + (i * 1700) % ?6, CAST(printf('e%031d', i) AS BLOB), 0, 'fill', ?4, 1, 1000, 100000000, 0, '{}' FROM n",
            params![from, to, oid, tid, d0, day],
        )
        .unwrap();
        crate::indexer::candles::rebuild(&conn, &token, 1_000).unwrap();
    };
    let measure = || -> Vec<(&'static str, u64)> {
        vec![
            (
                "candles 1d, edges inside the day",
                steps(&conn, || {
                    market::candles(&conn, &token, Some(3), "1d", day, Some(d0 + 1), Some(d0 + 2 * day - 1), 1500).unwrap()
                })
                .1,
            ),
            (
                "candles 1h, from inside the day",
                steps(&conn, || {
                    market::candles(&conn, &token, Some(3), "1h", 3_600_000, Some(d0 + 1), Some(d0 + day - 1), 1500).unwrap()
                })
                .1,
            ),
        ]
    };
    let n = 50_000;
    grow(0, n);
    let few = measure();
    grow(n, 2 * n);
    let many = measure();
    for ((name, f), (_, m)) in few.iter().zip(&many) {
        flat(name, *f, *m);
    }
    let v = market::candles(&conn, &token, Some(3), "1d", day, Some(d0 + 1), Some(d0 + 2 * day - 1), 1500).unwrap();
    assert_eq!(v.items.len(), 1, "the edge day, whole");
    assert_eq!(v.items[0].trades, 2 * n as u64, "every fill of the day counts");
}

/// R6C2b-2: a status that is common but old, read in history order (`+s.status`), behind many newer orders of other
/// statuses: a page reads at most `ORDERS_SCAN_MAX` orders of the history and carries the cursor of where it stopped, so N
/// then 2 N newer orders cost the same, and following the cursor reaches the old ones.
#[test]
fn a_status_read_in_history_order_is_bounded() {
    let _heavy = heavy();
    let conn = open_memory("testnet-10").unwrap();
    let s = Seed {
        prefix: "z",
        contract: "KobAsk",
        side: 1,
        token: TOKEN_COV,
        quote: None,
        parent: None,
        state: Some(AnyState::KobAsk(ask(1, 100_000_000)).encode()),
        price0: Some(100_000_000),
        remaining: 10 * WHOLE,
        custody: false,
        listed: false,
        maker: pk(1),
    };
    // 2,000 old cancelled orders, then the newer ones, filled
    seed(&conn, &s, 0, 2_000);
    conn.execute("UPDATE order_state SET status = 'cancelled'", []).unwrap();
    let newer = Seed { prefix: "y", ..s };
    let grow = |from: i64, to: i64| {
        seed(&conn, &newer, from, to);
        conn.execute("UPDATE order_state SET status = 'filled' WHERE status = 'open'", []).unwrap();
    };
    let ctx = ctx();
    let f = OrderFilter { status: Some("cancelled".into()), ..Default::default() };
    let measure = || vec![("orders of an old status", steps(&conn, || orders(&conn, &ctx, &f, 50, None).unwrap()).1)];
    let n = 30_000;
    grow(2_000, 2_000 + n);
    let few = measure();
    grow(2_000 + n, 2_000 + 2 * n);
    let many = measure();
    for ((name, a), (_, b)) in few.iter().zip(&many) {
        flat(name, *a, *b);
    }
    // the pages go on through the history and reach the cancelled orders
    let (mut cursor, mut found, mut pages) = (None, 0, 0);
    loop {
        let page = orders(&conn, &ctx, &f, 50, cursor.take()).unwrap();
        found += page.items.len();
        pages += 1;
        assert!(pages <= 2_000 / 50 + 2 * n as usize / ORDERS_SCAN_MAX + 3, "the paging advances");
        match page.next_cursor {
            Some(c) => cursor = parse_order_cursor(&c),
            None => break,
        }
    }
    assert_eq!(found, 2_000);
}

/// R6C2b-1: a store that keeps no candle aggregates (a lean store) charts from the fills, at most `CANDLE_SCAN_ROWS_MAX` of
/// them, the newest, and says `truncated`: N then 2 N fills in the window cost the same VM steps.
#[test]
fn lean_candles_read_at_most_their_cap_of_fills() {
    let _heavy = heavy();
    let conn = open_memory("testnet-10").unwrap();
    honest_book(&conn);
    crate::indexer::candles::configure(&conn, true).unwrap();
    let token = Hash32(TOKEN_COV);
    let (oid, tid): (i64, i64) =
        (conn.query_row("SELECT id FROM orders WHERE contract = 'KobAsk' LIMIT 1", [], |r| r.get(0)).unwrap(), id(&conn, &TOKEN_COV));
    let (d0, day) = (1_700_006_400_000i64, 86_400_000i64);
    let grow = |from: i64, to: i64| {
        conn.execute(
            "WITH RECURSIVE n(i) AS (SELECT ?1 UNION ALL SELECT i + 1 FROM n WHERE i < ?2 - 1) \
             INSERT INTO order_events (order_id, block_seq, daa, ts, txid, tx_pos, kind, token_id, side, amount, price, closes, detail) \
             SELECT ?3, 1, 1, ?5 + (i * 1700) % ?6, CAST(printf('f%031d', i) AS BLOB), 0, 'fill', ?4, 1, 1000, 100000000, 0, '{}' FROM n",
            params![from, to, oid, tid, d0, day],
        )
        .unwrap();
    };
    let read = || market::candles(&conn, &token, Some(3), "1h", 3_600_000, Some(d0), Some(d0 + day), 1500).unwrap();
    let n = market::CANDLE_SCAN_ROWS_MAX as i64 + 5_000;
    grow(0, n);
    let few = steps(&conn, read).1;
    grow(n, 2 * n);
    let many = steps(&conn, read).1;
    flat("lean candles", few, many);
    assert!(read().truncated, "older fills of the window were left out");
}

/// R7C2b-2: a status read in history order reaches the old rows of its filter whatever newer orders of other tokens and
/// makers lie in front: 2,000 old cancelled orders of one token and maker behind 30,000 newer filled ones of another token
/// and maker, read by token, by maker, by both and by neither. Each filter's pages return its rows within its own budget.
#[test]
fn a_status_read_reaches_old_rows_under_every_filter() {
    let _heavy = heavy();
    let conn = open_memory("testnet-10").unwrap();
    let other = [0x5c; 32];
    let s = Seed {
        prefix: "z",
        contract: "KobAsk",
        side: 1,
        token: TOKEN_COV,
        quote: None,
        parent: None,
        state: Some(AnyState::KobAsk(ask(1, 100_000_000)).encode()),
        price0: Some(100_000_000),
        remaining: 10 * WHOLE,
        custody: false,
        listed: false,
        maker: pk(1),
    };
    seed(&conn, &s, 0, 2_000);
    conn.execute("UPDATE order_state SET status = 'cancelled'", []).unwrap();
    seed(&conn, &Seed { prefix: "y", token: other, maker: pk(2), ..s }, 2_000, 32_000);
    conn.execute("UPDATE order_state SET status = 'filled' WHERE status = 'open'", []).unwrap();
    let ctx = ctx();
    let filters = [
        ("token", OrderFilter { token: Some(Hash32(TOKEN_COV)), status: Some("cancelled".into()), ..Default::default() }, 0),
        ("maker", OrderFilter { maker: Some(pk(1).to_vec()), status: Some("cancelled".into()), ..Default::default() }, 0),
        (
            "maker and token",
            OrderFilter { maker: Some(pk(1).to_vec()), token: Some(Hash32(TOKEN_COV)), status: Some("cancelled".into()) },
            0,
        ),
        ("none", OrderFilter { status: Some("cancelled".into()), ..Default::default() }, 30_000 / ORDERS_SCAN_MAX + 1),
    ];
    for (name, f, extra) in filters {
        let (mut cursor, mut found, mut pages) = (None, 0, 0);
        loop {
            let page = orders(&conn, &ctx, &f, 50, cursor.take()).unwrap();
            found += page.items.len();
            pages += 1;
            assert!(pages <= 2_000 / 50 + 2 + extra, "{name}: {pages} pages and not done ({found} found)");
            match page.next_cursor {
                Some(c) => cursor = parse_order_cursor(&c),
                None => break,
            }
        }
        assert_eq!(found, 2_000, "{name}");
    }
}

/// R7C2b-1: a maker's own takes make no price and are never walked: the price reads range over the priced fills through
/// their index, so N then 2 N self-takes in front of one priced fill cost the same VM steps for the trades, the
/// statistics, a pair chart's carry (a window after them) and a lean store's chart.
#[test]
fn self_takes_do_not_grow_the_market_reads() {
    let _heavy = heavy();
    let conn = open_memory("testnet-10").unwrap();
    honest_book(&conn);
    let token = Hash32(TOKEN_COV);
    let (oid, tid): (i64, i64) =
        (conn.query_row("SELECT id FROM orders WHERE contract = 'KobAsk' LIMIT 1", [], |r| r.get(0)).unwrap(), id(&conn, &TOKEN_COV));
    let maker: Vec<u8> =
        conn.query_row("SELECT h FROM ids WHERE id = (SELECT maker_id FROM orders WHERE id = ?1)", [oid], |r| r.get(0)).unwrap();
    conn.execute("INSERT INTO scripts (id, spk) VALUES (77, ?1)", [[&[0x00, 0x00, 0x20][..], &maker[..], &[0xac][..]].concat()])
        .unwrap();
    let t0 = 1_700_006_400_000i64;
    let fill = |from: i64, to: i64, executor: Option<i64>| {
        conn.execute(
            "WITH RECURSIVE n(i) AS (SELECT ?1 UNION ALL SELECT i + 1 FROM n WHERE i < ?2 - 1) \
             INSERT INTO order_events (order_id, block_seq, daa, ts, txid, tx_pos, kind, token_id, side, amount, price, closes, detail, executor_id) \
             SELECT ?3, 1, 1, ?5 + i, CAST(printf('x%031d', i) AS BLOB), 0, 'fill', ?4, 1, 1000, 100000000, 0, '{}', ?6 FROM n",
            params![from, to, oid, tid, t0, executor],
        )
        .unwrap();
    };
    fill(0, 1, None);
    let priced: i64 =
        conn.query_row("SELECT COUNT(*) FROM order_events WHERE kind = 'fill' AND priced = 1", [], |r| r.get(0)).unwrap();
    let other = Hash32([0x5d; 32]);
    let ctx = ctx();
    let measure = || -> Vec<(&'static str, u64)> {
        vec![
            ("trades", steps(&conn, || market::trades(&conn, &ctx, &token, Some(3), None, 200).unwrap()).1),
            ("stats", steps(&conn, || market::stats(&conn, &ctx, &token, Some(3)).unwrap()).1),
            (
                "pair candles carry",
                steps(&conn, || {
                    pairs::pair_candles(&conn, &token, &other, (Some(3), Some(3)), "1m", 60_000, Some(t0 + 1_000_000_000), None, 10)
                        .unwrap()
                })
                .1,
            ),
        ]
    };
    let n = 30_000;
    fill(1, 1 + n, Some(77));
    let few = measure();
    fill(1 + n, 1 + 2 * n, Some(77));
    let many = measure();
    for ((name, a), (_, b)) in few.iter().zip(&many) {
        flat(name, *a, *b);
    }
    let still: i64 = conn.query_row("SELECT COUNT(*) FROM order_events WHERE kind = 'fill' AND priced = 1", [], |r| r.get(0)).unwrap();
    assert_eq!((priced, still), (1, 1), "a take its maker's own key paid for makes no price");
    // a lean store charts from the fills: the self-takes are not read either
    crate::indexer::candles::configure(&conn, true).unwrap();
    let lean =
        || steps(&conn, || market::candles(&conn, &token, Some(3), "1h", 3_600_000, Some(t0), Some(t0 + 86_400_000), 1500).unwrap()).1;
    let a = lean();
    fill(1 + 2 * n, 1 + 3 * n, Some(77));
    flat("lean candles", a, lean());
}

/// R7C2a-1 and R7C2a-2, the findings' own cases: a rising bid worth 1 KAS only at its cap and almost nothing now, and a
/// curve whose minimum fill bundles seven cheap units with one at an absurd price, never set the touch: the bid is valued
/// at the start of its window (dust), the curve is ranked at the mean price of its smallest valid fill (the bottom of
/// the side), and the stats' best bid, best ask, mid and spread are the honest book's.
#[test]
fn a_rising_bid_and_a_bundled_curve_do_not_set_the_touch() {
    use kob_protocol::state::{BidState, CurveAskState};
    let _heavy = heavy();
    let conn = open_memory("testnet-10").unwrap();
    honest_book(&conn);
    let maintain = || crate::indexer::db::maintain_live_books(&conn, 1_000, None).unwrap();
    maintain();
    let (token, ctx) = (Hash32(TOKEN_COV), ctx());
    let honest = serde_json::to_value(market::stats(&conn, &ctx, &token, Some(3)).unwrap()).unwrap();
    let rising = BidState {
        price: 990_000_000,
        price_end: 100_000_000_000,
        slope: 100_000_000_000 - 990_000_000,
        decay_step: 300_000_000,
        min_fill: 1,
        tip: 0,
        delivery_carrier: 10_000_000,
        ..bid(7, 990_000_000)
    };
    let s = Seed {
        prefix: "r",
        contract: "KobBid",
        side: 2,
        token: TOKEN_COV,
        quote: None,
        parent: None,
        state: Some(AnyState::KobBid(rising).encode()),
        price0: Some(990_000_000),
        remaining: 1,
        custody: false,
        listed: true,
        maker: pk(7),
    };
    seed(&conn, &s, 0, 1);
    set_value(&conn, "r", 120_000_000);
    let (hh, p, sf) = tok_fields(T3);
    let mut table = vec![50_000_000i64; 7];
    table.push(10_000_000_000_000_000);
    let bundled = AnyState::KobCurveAsk(CurveAskState {
        maker: pk(9),
        token_cov_id: TOKEN_COV,
        token_tpl_hash: hh,
        tpl_prefix_len: p,
        tpl_suffix_len: sf,
        scale: SCALE,
        min_fill: 8,
        tip: 0,
        active_from: 0,
        level_size: 1,
        total: 10 * WHOLE,
        amount_left: 10 * WHOLE,
        extension_commitment: EXT,
        table,
    });
    seed(
        &conn,
        &Seed {
            prefix: "q",
            contract: "KobCurveAsk",
            side: 1,
            state: Some(bundled.encode()),
            price0: Some(50_000_000),
            remaining: 10 * WHOLE,
            custody: true,
            ..s
        },
        0,
        1,
    );
    maintain();
    let bid_row = class_of(&conn, "r");
    assert!(bid_row.iter().all(|(ok, class)| *ok == 0 || *class == CLASS_DUST), "the rising bid: {bid_row:?}");
    let now = serde_json::to_value(market::stats(&conn, &ctx, &token, Some(3)).unwrap()).unwrap();
    for k in ["best_bid", "best_ask", "mid", "spread_bps"] {
        assert_eq!(now[k], honest[k], "{k}");
    }
    assert_eq!(book_side_sorted(&conn, &ctx, &token, 1, Some(1), false).unwrap().0[0].0 .0, 100_000_000, "the honest best ask");
}
