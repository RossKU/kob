//! `kobd db migrate`: a store written by an earlier build (schema 5: states whole, no counters, the indexes of that time; schema 7: packed
//! states, 32-byte ids in every row; schema 8: integer ids in the child tables, blobs in the orders; schema 9: no fees, executors or
//! activity totals) is rebuilt as the current schema (integer ids in the orders too, the fees of the fills, the totals) in one run, and no
//! read changes: every table digests the same
//! with its ids joined back and its states decoded, the API reads return the same JSON, the event ids and sequences are kept, a
//! store this build refuses is left as it is, and the migrated store goes on being indexed.

mod common;

use common::*;
use kob_protocol::build::*;
use kob_protocol::state::*;
use kob_protocol::tx::*;
use kobd::config::IndexerConfig;
use kobd::hex::Hash32;
use kobd::indexer::reads::{self, HoldingFilter, OrderFilter};
use kobd::indexer::{book, db, packed, storage};
use kobd::testkit::*;
use rusqlite::{Connection, OpenFlags};
use sha2::{Digest, Sha256};
use std::path::Path;

/// The schema of the last build that stored 32-byte ids in every row (schema 7), as a fixture.
const SCHEMA7: &str = include_str!("fixtures/schema7.sql");
/// The schema of the build before the orders named their maker, token, templates and quote token by id (schema 8), as a fixture.
const SCHEMA8: &str = include_str!("fixtures/schema8.sql");
/// The schema of the build before the fills kept their fee and executor and the activity totals were kept (schema 9), as a fixture.
const SCHEMA9: &str = include_str!("fixtures/schema9.sql");

/// Which earlier layout [`legacy_store`] writes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Legacy {
    /// States whole, no row counters, the indexes of that time, 32-byte ids everywhere.
    V5,
    /// Packed states, 32-byte ids everywhere.
    V7,
    /// Integer ids in the child tables, 32-byte ids in the orders.
    V8,
    /// Integer ids everywhere; no fee or executor on the fills, no activity totals.
    V9,
}

fn cancel(c: &Ctx, order: OrderUtxo<AnyState>, custody: Option<TokenUtxo>, strays: Vec<TokenUtxo>) -> SignedTx {
    c.w.sign(&Action::CancelOrder(CancelOrder {
        retired: None,
        prefund: None,
        order,
        custody,
        foreign: vec![],
        strays,
        tokens: vec![],
        funding: vec![c.w.coin(MAKER_A, 10)],
        change: None,
        replace: None,
        lock_time: 0,
        records: vec![],
        fee: FeeOptions::default(),
    }))
}

/// A store with orders that were placed and left, partly filled, swept and cancelled, and the token holdings they moved.
async fn flows() -> Ctx {
    let mut c = Ctx::new();
    scenario(&mut c, |_| {}).await;
    c
}

/// [`flows`] on a given context; `mid` runs once the first ask and a stray to it are indexed and before anything spends them (it may
/// replace the context's indexer: the chain goes on from the node).
async fn scenario(c: &mut Ctx, mid: impl FnOnce(&mut Ctx)) {
    // an ask that is partly filled twice and then cancelled with a stray swept
    let a = ask(MAKER_A, P250);
    let create = c.w.create_tx(AnyState::KobAsk(a.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    let stray = c.w.stray(cov, 10 * WHOLE, TAKER);
    c.push(&[&stray]).await;
    mid(c);
    let custody = c.w.token_at(&create, 1, Kcc20State::custody(10 * WHOLE, cov.0, EXT));
    let leg = Leg::Ask { order: c.w.order(&create, 0, a.clone()), custody, amount: 4 * WHOLE, t: None };
    let fill = c.w.sign(&Action::Batch(batch(&c.w, c.daa(), vec![leg])));
    c.push(&[&fill]).await;
    let a6 = AskState { amount_left: 6 * WHOLE, ..a };
    let oi = find_cov(&fill, &cov).unwrap();
    let custody = c.w.token_at(&fill, find_custody(&fill, &cov, 6 * WHOLE).unwrap(), Kcc20State::custody(6 * WHOLE, cov.0, EXT));
    let stray_utxo =
        c.w.token_at(&stray, find_custody(&stray, &cov, 10 * WHOLE).unwrap(), Kcc20State::custody(10 * WHOLE, cov.0, EXT));
    let sweep = cancel(c, c.w.order(&fill, oi, AnyState::KobAsk(a6)), Some(custody), vec![stray_utxo]);
    c.push(&[&sweep]).await;
    // a bid placed and cancelled, an ask left open
    let b0 = bid(MAKER_B, P245);
    let create_b = c.w.create_tx(AnyState::KobBid(b0.clone()), b0.escrow(4 * WHOLE, 2).unwrap() as u64, MAKER_B, 0);
    c.push(&[&create_b]).await;
    let a2 = ask(MAKER_A, P250);
    let open = c.w.create_tx(AnyState::KobAsk(a2), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&open]).await;
}

/// Every row of every table, ids joined back to their 32 bytes and states decoded, hashed per table. The same text reads a schema 8 store.
fn table_digests(conn: &Connection) -> Vec<(String, u64, String)> {
    let gen = "kob_gstate(o.genesis_state, o_mk.h, o_tk.h, o_tp.h, o_qt.h)";
    let ids = " LEFT JOIN ids o_mk ON o_mk.id = o.maker_id LEFT JOIN ids o_tk ON o_tk.id = o.token_id LEFT JOIN ids o_tp ON o_tp.id = o.token_tpl_id LEFT JOIN ids o_qt ON o_qt.id = o.quote_id LEFT JOIN ids o_tm ON o_tm.id = o.template_id LEFT JOIN ids o_ex ON o_ex.id = o.ext_id ";
    let mut queries: Vec<(&str, String)> = vec![
        (
            "orders",
            format!("SELECT o.covenant_id, o.contract, o_tm.h, o.family, o.side, o_mk.h, o_tk.h, o_tp.h, o_ex.h, o.scale, o.min_fill, o.price, o.tip, o.tif, o.expiry_daa, o.active_from, o.in_book, o.budget_rate, o.reserve, o.initial_amount, o.deadline, {gen}, o.genesis_txid, o.genesis_out, o.genesis_block, o.genesis_daa, o.parent, o.listed, o.unlisted_reason, o.origin, o_qt.h FROM orders o{ids}ORDER BY o.covenant_id"),
        ),
        (
            "order_utxos",
            format!("SELECT u.txid, u.idx, o.covenant_id, u.value, u.spk, kob_ustate(u.state, {gen}), u.created_block, u.created_daa, u.spent_block, u.spent_txid, u.spent_entry, u.spent_amount FROM order_utxos u JOIN orders o ON o.id = u.order_id{ids}ORDER BY u.txid, u.idx"),
        ),
        (
            "order_events",
            "SELECT e.id, o.covenant_id, e.block_seq, e.daa, e.ts, e.txid, e.tx_pos, e.kind, t.h, e.side, e.amount, e.price, e.payout, e.closes, kob_dtext(e.detail) FROM order_events e JOIN orders o ON o.id = e.order_id LEFT JOIN ids t ON t.id = e.token_id ORDER BY e.id".to_string(),
        ),
        (
            "order_state",
            "SELECT o.covenant_id, s.status, s.filled_amount, s.remaining_amount, s.amount_exact, s.cur_txid, s.cur_idx, s.cur_value, s.state_known, s.last_block, s.last_daa FROM order_state s JOIN orders o ON o.id = s.order_id ORDER BY o.covenant_id".to_string(),
        ),
        (
            "order_amends",
            "SELECT a.id, o.covenant_id, a.block_seq, a.price, a.tip, a.tif, a.expiry_daa, a.active_from, a.deadline, a.listed, a.unlisted_reason, a.budget_rate, a.reserve, a.min_fill FROM order_amends a JOIN orders o ON o.id = a.order_id ORDER BY a.id".to_string(),
        ),
        (
            "token_utxos",
            "SELECT u.txid, u.idx, t.h, o.covenant_id, u.amount, u.value, u.role, u.created_block, u.created_daa, u.spent_block, u.spent_txid FROM token_utxos u JOIN ids t ON t.id = u.token_id JOIN orders o ON o.id = u.order_id ORDER BY u.txid, u.idx".to_string(),
        ),
        (
            "token_holdings",
            "SELECT h.txid, h.idx, t.h, h.program, h.family, w.h, h.owner_kind, h.amount, h.value, kob_hstate(h.state, h.family, w.h, h.owner_kind, h.amount), h.role, h.created_block, h.created_daa, h.spent_block, h.spent_txid FROM token_holdings h JOIN ids t ON t.id = h.token_id JOIN ids w ON w.id = h.owner_id ORDER BY h.txid, h.idx".to_string(),
        ),
        ("counters", "SELECT name, n FROM counters WHERE n != 0 ORDER BY name".to_string()),
        (
            "holding_programs",
            "SELECT t.h, p.program, p.n FROM holding_programs p JOIN ids t ON t.id = p.token_id ORDER BY t.h, p.program".to_string(),
        ),
        (
            "meta",
            "SELECT k, v FROM meta WHERE k NOT IN ('schema_version') ORDER BY k".to_string(),
        ),
    ];
    for t in [
        "blocks",
        "token_events",
        "rejects",
        "candles",
        "candle_meta",
        "candle_dirty",
        "order_flags",
        "pair_fills",
        "kcc23_candidates",
        "kcc23_txs",
        "kcc23_declarations",
        "logo_blocklist",
        "logo_reports",
    ] {
        queries.push((t, format!("SELECT * FROM {t} ORDER BY 1, 2")));
    }
    queries
        .iter()
        .map(|(name, sql)| {
            let mut h = Sha256::new();
            let mut n = 0u64;
            let mut st = conn.prepare(sql).unwrap_or_else(|e| panic!("{name}: {e}"));
            let cols = st.column_count();
            let mut rows = st.query([]).unwrap();
            while let Some(r) = rows.next().unwrap() {
                n += 1;
                for i in 0..cols {
                    h.update(format!("{:?}|", r.get_ref(i).unwrap()).as_bytes());
                }
            }
            (name.to_string(), n, format!("{:x}", h.finalize()))
        })
        .collect()
}

/// The API reads that carry the ids and the packed columns, as JSON.
fn reads_json(conn: &Connection) -> Vec<String> {
    let ctx = reads::ReadCtx { node_daa: Some(1_000_000), settle_depth_daa: 100, now_unix: None };
    let mut out = vec![];
    let ids: Vec<Vec<u8>> = {
        let mut st = conn.prepare("SELECT covenant_id FROM orders ORDER BY covenant_id").unwrap();
        st.query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect()
    };
    assert!(ids.len() >= 3);
    for id in &ids {
        let h = Hash32::from_slice(id).unwrap();
        out.push(serde_json::to_string(&reads::order(conn, &ctx, &h).unwrap()).unwrap());
        out.push(serde_json::to_string(&reads::order_events(conn, &ctx, &h, 100, None).unwrap()).unwrap());
    }
    out.push(serde_json::to_string(&reads::orders(conn, &ctx, &OrderFilter::default(), 100, None).unwrap()).unwrap());
    out.push(serde_json::to_string(&reads::book(conn, &ctx, &Hash32(TOKEN_COV), 10, false).unwrap()).unwrap());
    out.push(serde_json::to_string(&reads::strays(conn, &ctx, None, 10).unwrap()).unwrap());
    out.push(serde_json::to_string(&reads::fills(conn, &ctx, None, None, None, 100).unwrap()).unwrap());
    for spent in [false, true] {
        let f = HoldingFilter { owner: None, token: Some(Hash32(TOKEN_COV)), include_spent: spent };
        out.push(serde_json::to_string(&reads::holdings(conn, &ctx, &f, 100, None).unwrap()).unwrap());
    }
    // the holdings of an owner: the owner of one holding of the store
    let owner: Option<Vec<u8>> = conn
        .query_row("SELECT w.h FROM token_holdings h JOIN ids w ON w.id = h.owner_id ORDER BY h.txid, h.idx LIMIT 1", [], |r| r.get(0))
        .ok();
    if let Some(o) = owner {
        let f = HoldingFilter { owner: Some(o), token: None, include_spent: true };
        out.push(serde_json::to_string(&reads::holdings(conn, &ctx, &f, 100, None).unwrap()).unwrap());
    }
    out.push(serde_json::to_string(&book::snapshot(conn, None).unwrap()).unwrap());
    out
}

fn open_ro(path: &Path) -> Connection {
    let c = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    packed::register(&c).unwrap();
    c
}

/// Bytes of the packed columns: order UTXO states, holding states, genesis states, event details.
fn state_bytes(conn: &Connection) -> (i64, i64, i64, i64) {
    conn.query_row(
        "SELECT (SELECT SUM(LENGTH(state)) FROM order_utxos), (SELECT SUM(LENGTH(state)) FROM token_holdings), (SELECT SUM(LENGTH(genesis_state)) FROM orders), (SELECT SUM(LENGTH(detail)) FROM order_events)",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )
    .unwrap()
}

fn config(dir: &Path) -> IndexerConfig {
    let mut cfg = IndexerConfig { data_dir: dir.to_path_buf(), network: "testnet-10".into(), ..Default::default() };
    cfg.records.enabled = false;
    cfg
}

/// The columns of `table` of the schema-7 database `conn` (all of them exist in the schema 8 table, bar the ids).
fn cols(conn: &Connection, table: &str) -> Vec<String> {
    let mut st = conn.prepare("SELECT name FROM pragma_table_info(?1, 'main') ORDER BY cid").unwrap();
    st.query_map([table], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect()
}

/// Writes `dest` as the store an earlier build would have left of the current store `src` (see [`Legacy`]). The event sequence is moved
/// 50 above its last id, as a reverted tail leaves it.
fn legacy_store(src: &Path, dest: &Path, kind: Legacy) {
    let whole = kind == Legacy::V5;
    for s in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{s}", dest.display()));
    }
    let w = Connection::open(dest).unwrap();
    packed::register(&w).unwrap();
    w.execute_batch(match kind {
        Legacy::V9 => SCHEMA9,
        Legacy::V8 => SCHEMA8,
        _ => SCHEMA7,
    })
    .unwrap();
    // the tables the executor adds to the same file (token-logo reports), which no chain rebuilds
    w.execute_batch(kobd::logo_reports::SCHEMA).unwrap();
    w.execute("ATTACH DATABASE ?1 AS cur", [src.to_string_lossy().as_ref()]).unwrap();
    let list = |t: &str, f: &dyn Fn(&str) -> Option<String>| -> (String, String) {
        let names = cols(&w, t);
        let sel = names.iter().map(|c| f(c).unwrap_or_else(|| format!("x.{c}"))).collect::<Vec<_>>().join(", ");
        (names.join(", "), sel)
    };
    // the ids of an order, joined back to their 32 bytes
    let order_ids = " LEFT JOIN cur.ids tm ON tm.id = x.template_id LEFT JOIN cur.ids mk ON mk.id = x.maker_id LEFT JOIN cur.ids tk ON tk.id = x.token_id LEFT JOIN cur.ids tp ON tp.id = x.token_tpl_id LEFT JOIN cur.ids ex ON ex.id = x.ext_id LEFT JOIN cur.ids qt ON qt.id = x.quote_id ";
    let genesis_whole = "kob_gstate(x.genesis_state, mk.h, tk.h, tp.h, qt.h)".to_string();
    // the genesis state of the order a row of a child table belongs to (`x.order_id`), decoded
    let go = "(SELECT kob_gstate(g.genesis_state, gm.h, gt.h, gp.h, gq.h) FROM cur.orders g LEFT JOIN cur.ids gm ON gm.id = g.maker_id LEFT JOIN cur.ids gt ON gt.id = g.token_id LEFT JOIN cur.ids gp ON gp.id = g.token_tpl_id LEFT JOIN cur.ids gq ON gq.id = g.quote_id WHERE g.id = x.order_id)".to_string();
    let run = |sql: String| {
        w.execute(&sql, []).unwrap_or_else(|e| panic!("{e}: {sql}"));
    };
    if kind == Legacy::V9 {
        // the tables of schema 9 are those of today without the new columns: every row as it is, by the names of schema 9
        for t in ["ids", "orders", "order_utxos", "order_events", "order_state", "order_amends", "token_utxos", "token_holdings"] {
            let names = cols(&w, t).join(", ");
            run(format!("INSERT INTO {t} ({names}) SELECT {names} FROM cur.{t} ORDER BY rowid"));
        }
    }
    let (c, s) = if kind == Legacy::V9 {
        (String::new(), String::new())
    } else {
        list("orders", &|c| match c {
            "template_hash" => Some("tm.h".to_string()),
            "maker" => Some("mk.h".to_string()),
            "token_cov_id" => Some("tk.h".to_string()),
            "token_tpl_hash" => Some("tp.h".to_string()),
            "ext_commit" => Some("ex.h".to_string()),
            "quote_cov_id" => Some("qt.h".to_string()),
            "genesis_state" if whole => Some(genesis_whole.clone()),
            _ => None,
        })
    };
    if kind != Legacy::V9 {
        run(format!("INSERT INTO orders ({c}) SELECT {s} FROM cur.orders x{order_ids}ORDER BY x.id"));
    }
    if kind == Legacy::V8 {
        // the children keep the integers of the current store: its ids come with them
        run("INSERT INTO ids SELECT * FROM cur.ids".to_string());
        for t in ["order_utxos", "order_events", "order_state", "order_amends", "token_utxos", "token_holdings"] {
            let names = cols(&w, t).join(", ");
            run(format!("INSERT INTO {t} ({names}) SELECT {names} FROM cur.{t} ORDER BY rowid"));
        }
    }
    if kind != Legacy::V8 && kind != Legacy::V9 {
        let (c, s) = list("order_utxos", &|c| match c {
            "covenant_id" => Some("o.covenant_id".to_string()),
            "state" if whole => Some(format!("kob_ustate(x.state, {go})")),
            _ => None,
        });
        run(format!(
            "INSERT INTO order_utxos ({c}) SELECT {s} FROM cur.order_utxos x JOIN cur.orders o ON o.id = x.order_id ORDER BY x.rowid"
        ));
        let (c, s) = list("order_events", &|c| match c {
            "covenant_id" => Some("o.covenant_id".to_string()),
            "token_cov_id" => Some("t.h".to_string()),
            "detail" if whole => Some("kob_dtext(x.detail)".to_string()),
            _ => None,
        });
        run(format!("INSERT INTO order_events ({c}) SELECT {s} FROM cur.order_events x JOIN cur.orders o ON o.id = x.order_id LEFT JOIN cur.ids t ON t.id = x.token_id ORDER BY x.id"));
        let (c, s) = list("order_state", &|c| (c == "covenant_id").then(|| "o.covenant_id".to_string()));
        run(format!(
        "INSERT INTO order_state ({c}) SELECT {s} FROM cur.order_state x JOIN cur.orders o ON o.id = x.order_id ORDER BY x.order_id"
    ));
        let (c, s) = list("order_amends", &|c| (c == "covenant_id").then(|| "o.covenant_id".to_string()));
        run(format!(
            "INSERT INTO order_amends ({c}) SELECT {s} FROM cur.order_amends x JOIN cur.orders o ON o.id = x.order_id ORDER BY x.id"
        ));
        let (c, s) = list("token_utxos", &|c| match c {
            "token_cov_id" => Some("t.h".to_string()),
            "owner" => Some("o.covenant_id".to_string()),
            _ => None,
        });
        run(format!("INSERT INTO token_utxos ({c}) SELECT {s} FROM cur.token_utxos x JOIN cur.ids t ON t.id = x.token_id JOIN cur.orders o ON o.id = x.order_id ORDER BY x.rowid"));
        let (c, s) = list("token_holdings", &|c| match c {
            "token_cov_id" => Some("t.h".to_string()),
            "owner" => Some("w.h".to_string()),
            "state" if whole => Some("kob_hstate(x.state, x.family, w.h, x.owner_kind, x.amount)".to_string()),
            _ => None,
        });
        run(format!("INSERT INTO token_holdings ({c}) SELECT {s} FROM cur.token_holdings x JOIN cur.ids t ON t.id = x.token_id JOIN cur.ids w ON w.id = x.owner_id ORDER BY x.rowid"));
    }
    for t in [
        "blocks",
        "token_events",
        "rejects",
        "candles",
        "candle_meta",
        "candle_dirty",
        "order_flags",
        "pair_fills",
        "kcc23_candidates",
        "kcc23_txs",
        "kcc23_declarations",
        "logo_blocklist",
        "logo_reports",
    ] {
        let names = cols(&w, t).join(", ");
        run(format!("INSERT INTO {t} ({names}) SELECT {names} FROM cur.{t} ORDER BY 1"));
    }
    // (the activity totals came with schema 10)
    run("INSERT INTO meta (k, v) SELECT k, v FROM cur.meta WHERE k NOT IN ('schema_version', 'activity')".to_string());
    if whole {
        // schema 5: no row counters, no program counts, the indexes of that time
        w.execute_batch(
            "DROP TRIGGER holding_programs_insert; DROP TRIGGER holding_programs_delete; DROP TRIGGER holding_programs_update; \
             DROP TABLE holding_programs; DELETE FROM meta WHERE k IN ('holding_programs', 'counters'); \
             DROP INDEX order_events_fills; DROP INDEX order_events_fill_ts; \
             CREATE INDEX order_events_fills ON order_events (kind, token_cov_id, id); \
             CREATE INDEX order_events_fill_ts ON order_events (kind, token_cov_id, ts); \
             CREATE INDEX token_holdings_owner ON token_holdings (owner, token_cov_id); \
             CREATE INDEX token_holdings_token_program ON token_holdings (token_cov_id, program);",
        )
        .unwrap();
        let triggers: Vec<String> = {
            let mut st = w.prepare("SELECT name FROM sqlite_master WHERE type = 'trigger' AND name LIKE 'counters_%'").unwrap();
            st.query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect()
        };
        for t in triggers {
            w.execute_batch(&format!("DROP TRIGGER {t}")).unwrap();
        }
        w.execute_batch("DROP TABLE counters").unwrap();
    }
    // the sequences of the autoincrement tables, as the source had them, the events' ahead of their last id
    w.execute_batch(
        "UPDATE sqlite_sequence SET seq = (SELECT seq FROM cur.sqlite_sequence c WHERE c.name = sqlite_sequence.name) \
         WHERE name IN (SELECT name FROM cur.sqlite_sequence); \
         UPDATE sqlite_sequence SET seq = seq + 50 WHERE name = 'order_events';",
    )
    .unwrap();
    w.execute(
        "INSERT INTO meta (k, v) VALUES ('schema_version', ?1) ON CONFLICT(k) DO UPDATE SET v = excluded.v",
        [match kind {
            Legacy::V5 => "5",
            Legacy::V7 => "7",
            Legacy::V8 => "8",
            Legacy::V9 => "9",
        }],
    )
    .unwrap();
    w.execute_batch("DETACH DATABASE cur").unwrap();
}

fn sequence(conn: &Connection, table: &str) -> i64 {
    conn.query_row("SELECT seq FROM sqlite_sequence WHERE name = ?1", [table], |r| r.get(0)).unwrap()
}

/// `VACUUM INTO` a file of the store the flows ran on.
fn snapshot_file(c: &Ctx, path: &Path) {
    let g = c.hs.ingest.lock().unwrap();
    // rows no chain gives back (token-logo reports and the blocklist, crate::logo_reports): a migration must carry them
    g.conn()
        .execute_batch(
            "INSERT OR IGNORE INTO logo_blocklist (covenant_id, state, first_ms, hidden_ms, restored_ms, report_count, updated_ms)                  VALUES (zeroblob(32), 'hidden', 1000, 2000, NULL, 2, 2000);              INSERT OR IGNORE INTO logo_reports (id, covenant_id, reason, source, ref, created_ms, outcome)                  VALUES (7, zeroblob(32), 'other', 'web', NULL, 1000, 'hidden'), (9, zeroblob(32), 'illegal', 'github', 5, 2000, 'recorded');",
        )
        .unwrap();
    g.conn().execute("VACUUM INTO ?1", [path.to_string_lossy().as_ref()]).unwrap();
}

#[tokio::test]
async fn migrate_rebuilds_a_schema_5_store_as_the_current_schema_and_no_read_changes() {
    let c = flows().await;
    let dir = tempfile::tempdir().unwrap();
    let cur = dir.path().join("current.sqlite3");
    snapshot_file(&c, &cur);
    let path = dir.path().join("index.sqlite3");
    legacy_store(&cur, &path, Legacy::V5);
    std::fs::remove_file(&cur).unwrap();
    let digests_before = {
        let g = c.hs.ingest.lock().unwrap();
        table_digests(g.conn())
    };
    assert!(digests_before.iter().find(|d| d.0 == "order_utxos").unwrap().1 >= 4);
    let (utxo_whole, holding_whole, genesis_whole, detail_whole) = state_bytes(&open_ro(&path));
    let seq_before = sequence(&open_ro(&path), "order_events");

    let cfg = config(dir.path());
    let mut lines = vec![];
    let rep = storage::migrate(&cfg, storage::Options::default(), |l| lines.push(l.to_string())).unwrap();
    assert_eq!(rep.from_schema, 5, "{lines:?}");
    assert_eq!(rep.rows("orders"), digests_before.iter().find(|d| d.0 == "orders").unwrap().1);
    assert!(rep.ids > 0);
    let old = rep.kept_old.clone().expect("rebuilt");
    assert!(old.exists() && old.to_string_lossy().ends_with("index.sqlite3.before-migrate"));
    assert!(!dir.path().join("index.sqlite3.migrating").exists());

    let after = open_ro(&path);
    let (utxo_after, holding_after, genesis_after, detail_after) = state_bytes(&after);
    assert!(utxo_after < utxo_whole / 2, "{utxo_after} vs {utxo_whole}");
    assert!(holding_after < holding_whole / 2, "{holding_after} vs {holding_whole}");
    assert!(genesis_after < genesis_whole / 2, "{genesis_after} vs {genesis_whole}");
    assert!(detail_after < detail_whole, "{detail_after} vs {detail_whole}");
    // every table reads the same: ids joined back, states decoded (the counters were counted afresh, and equal what the writer kept)
    assert_eq!(table_digests(&after), digests_before);
    assert_eq!(db::meta_get(&after, "schema_version").unwrap().as_deref(), Some(db::SCHEMA_VERSION.to_string().as_str()));
    assert_eq!(after.query_row::<String, _, _>("PRAGMA integrity_check", [], |r| r.get(0)).unwrap(), "ok");
    // the sequence of the events is kept (an id is never reused), and the event ids are the old ones
    assert_eq!(sequence(&after, "order_events"), seq_before);
    // the fills-only indexes of schema.sql, none of the indexes of schema 5
    let index_sql = |name: &str| -> Option<String> {
        after.query_row("SELECT sql FROM sqlite_master WHERE type = 'index' AND name = ?1", [name], |r| r.get(0)).ok()
    };
    for name in ["order_events_fills", "order_events_fill_ts"] {
        assert!(index_sql(name).unwrap().contains("WHERE kind = 'fill'"), "{name}");
    }
    assert!(index_sql("token_holdings_owner").is_none() && index_sql("token_holdings_token_program").is_none());
    // the programs of the holdings are what a count of the holdings says
    let counted: i64 = after.query_row("SELECT SUM(n) FROM holding_programs", [], |r| r.get(0)).unwrap();
    let real: i64 = after.query_row("SELECT COUNT(*) FROM token_holdings", [], |r| r.get(0)).unwrap();
    assert!(real > 0);
    assert_eq!(counted, real);
    // the ids are what the rows name: every id is used, none twice
    let (ids, distinct): (i64, i64) =
        after.query_row("SELECT COUNT(*), COUNT(DISTINCT h) FROM ids", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!(ids, distinct);

    // the API reads: the migrated store answers what the in-memory store the flows ran on answers
    let migrated = reads_json(&after);
    let original = {
        let g = c.hs.ingest.lock().unwrap();
        reads_json(g.conn())
    };
    assert_eq!(migrated.len(), original.len());
    for (i, (m, o)) in migrated.iter().zip(&original).enumerate() {
        if m != o {
            let at = m.chars().zip(o.chars()).position(|(x, y)| x != y).unwrap_or(0);
            let dir = std::env::temp_dir();
            std::fs::write(dir.join("sm_migrated.json"), m).unwrap();
            std::fs::write(dir.join("sm_original.json"), o).unwrap();
            panic!("read {i} differs at {at} (both written to {})", dir.display());
        }
    }
    drop(after);

    // a second run changes nothing (and keeps the first run's rollback file out of the way)
    std::fs::remove_file(&old).unwrap();
    let again = storage::migrate(&cfg, storage::Options::default(), |_| {}).unwrap();
    assert_eq!(again.from_schema, db::SCHEMA_VERSION);
    assert!(again.tables.is_empty() && again.kept_old.is_none(), "{again:?}");
    assert_eq!(table_digests(&open_ro(&path)), digests_before);
}

#[tokio::test]
async fn a_schema_8_store_is_migrated_in_the_middle_of_a_chain_and_the_indexer_goes_on_from_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.sqlite3");
    let mut c = Ctx::with(Harness::new(db::open_writer(&path, "testnet-10").unwrap(), None));
    let (cfg, p) = (config(dir.path()), path.clone());
    let d = dir.path().to_path_buf();
    scenario(&mut c, move |c| {
        let node = c.hs.node.clone();
        let cur = d.join("current.sqlite3");
        snapshot_file(c, &cur);
        // the indexer lets go of the file (a placeholder store in its place), the file becomes the store of schema 7 and is migrated
        c.hs = Harness::new(db::open_memory("testnet-10").unwrap(), None);
        legacy_store(&cur, &p, Legacy::V8);
        std::fs::remove_file(&cur).unwrap();
        let rep = storage::migrate(&cfg, storage::Options::default(), |_| {}).unwrap();
        assert_eq!(rep.from_schema, 8);
        let w = db::open_writer(&p, "testnet-10").unwrap();
        // the next event id is above every id the old store gave out (the sequence was ahead of the last event)
        assert!(sequence(&w, "order_events") >= 50);
        c.hs = Harness::with_node(node, w, None, wide_window());
    })
    .await;
    let control = flows().await;
    let migrated = {
        let g = c.hs.ingest.lock().unwrap();
        (table_digests(g.conn()), reads_json(g.conn()))
    };
    let straight = {
        let g = control.hs.ingest.lock().unwrap();
        (table_digests(g.conn()), reads_json(g.conn()))
    };
    // the rows the chain added after the migration are the rows it adds to a store that was never migrated, except the ids of the events
    // (the migrated sequence skipped 50)
    // (the logo rows were put into the migrated store only)
    let no_logo = |v: &Vec<(String, u64, String)>| -> Vec<(String, u64, String)> {
        v.iter().filter(|d| !d.0.starts_with("logo_")).cloned().collect()
    };
    let (m0, s0) = (no_logo(&migrated.0), no_logo(&straight.0));
    let strip = |v: &Vec<(String, u64, String)>| -> Vec<(String, u64)> { v.iter().map(|d| (d.0.clone(), d.1)).collect() };
    assert_eq!(strip(&m0), strip(&s0));
    for ((name, _, a), (_, _, b)) in m0.iter().zip(&s0) {
        // the event ids differ by the skipped sequence, and the candles' position in the events (meta) follows them
        if !matches!(name.as_str(), "order_events" | "meta") {
            assert_eq!(a, b, "{name}");
        }
    }
    assert_eq!(migrated.1.len(), straight.1.len());
    let g = c.hs.ingest.lock().unwrap();
    assert_eq!(g.conn().query_row::<String, _, _>("PRAGMA integrity_check", [], |r| r.get(0)).unwrap(), "ok");
    // the events the chain added after the migration carry ids above the old sequence
    let high: i64 = g.conn().query_row("SELECT COUNT(*) FROM order_events WHERE id > 50", [], |r| r.get(0)).unwrap();
    assert!(high >= 1);
}

#[tokio::test]
async fn an_old_layout_store_is_refused_untouched_and_a_row_the_integers_cannot_hold_stops_the_migration() {
    let c = flows().await;
    let dir = tempfile::tempdir().unwrap();
    let cur = dir.path().join("current.sqlite3");
    snapshot_file(&c, &cur);
    let path = dir.path().join("index.sqlite3");
    legacy_store(&cur, &path, Legacy::V7);
    std::fs::remove_file(&cur).unwrap();

    // this build opens no store of an older layout, for writing or for reading, and says what to run
    for e in [db::open_writer(&path, "testnet-10").err().unwrap(), db::open_reader(&path, "testnet-10").err().unwrap()] {
        let msg = e.to_string();
        assert!(msg.contains("kobd db migrate") && msg.contains("schema 7"), "{msg}");
    }

    // an event of an order that has no row: the integer layout cannot hold it, so nothing is migrated and nothing is changed
    {
        let w = Connection::open(&path).unwrap();
        w.execute(
            "INSERT INTO order_events (covenant_id, block_seq, daa, ts, txid, tx_pos, kind, closes) VALUES (?1, 1, 1, 1, ?1, 0, 'cancel', 0)",
            [vec![0xEEu8; 32]],
        )
        .unwrap();
    }
    let before = std::fs::read(&path).unwrap();
    let cfg = config(dir.path());
    let err = storage::migrate(&cfg, storage::Options::default(), |_| {}).unwrap_err().to_string();
    assert!(err.contains("order_events") && err.contains("untouched"), "{err}");
    assert!(!dir.path().join("index.sqlite3.migrating").exists(), "the half-built file is removed");
    assert!(!dir.path().join("index.sqlite3.before-migrate").exists());
    assert_eq!(std::fs::read(&path).unwrap(), before, "the old store is byte for byte what it was");
}

/// A store of whole values (what an earlier build wrote) goes on being indexed by this build: the rows the flow adds are packed, the ones
/// it spends are whole, and every read is what the same flow gives on a store that was packed throughout.
#[tokio::test]
async fn a_whole_store_mixes_with_the_packed_rows_this_build_adds_and_reads_the_same() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.sqlite3");
    let mut c = Ctx::with(Harness::new(db::open_writer(&path, "testnet-10").unwrap(), None));
    let p = path.clone();
    scenario(&mut c, move |_| {
        // everything indexed so far is made whole, as an earlier build left it
        let w = Connection::open(&p).unwrap();
        packed::register(&w).unwrap();
        w.execute_batch(
            "UPDATE order_utxos SET state = kob_ustate(state, (SELECT kob_gstate(g.genesis_state, (SELECT h FROM ids WHERE id = g.maker_id), (SELECT h FROM ids WHERE id = g.token_id), (SELECT h FROM ids WHERE id = g.token_tpl_id), (SELECT h FROM ids WHERE id = g.quote_id)) FROM orders g WHERE g.id = order_utxos.order_id)); \
             UPDATE orders SET genesis_state = kob_gstate(genesis_state, (SELECT h FROM ids WHERE id = orders.maker_id), (SELECT h FROM ids WHERE id = orders.token_id), (SELECT h FROM ids WHERE id = orders.token_tpl_id), (SELECT h FROM ids WHERE id = orders.quote_id)); \
             UPDATE order_events SET detail = kob_dtext(detail); \
             UPDATE token_holdings SET state = kob_hstate(state, family, (SELECT h FROM ids WHERE id = token_holdings.owner_id), owner_kind, amount);",
        )
        .unwrap();
    })
    .await;
    let control = flows().await;
    let mixed = {
        let g = c.hs.ingest.lock().unwrap();
        reads_json(g.conn())
    };
    let packed_throughout = {
        let g = control.hs.ingest.lock().unwrap();
        reads_json(g.conn())
    };
    assert_eq!(mixed, packed_throughout);
    // both kinds are in the store
    let g = c.hs.ingest.lock().unwrap();
    let (whole, short): (i64, i64) = g
        .conn()
        .query_row("SELECT SUM(LENGTH(genesis_state) > 100), SUM(LENGTH(genesis_state) <= 100) FROM orders", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert!(whole >= 1 && short >= 1, "{whole} whole and {short} packed genesis states");
    assert_eq!(table_digests(g.conn()), table_digests(control.hs.ingest.lock().unwrap().conn()));
}

#[tokio::test]
async fn an_unknown_table_stops_the_migration_and_a_lean_store_stays_lean() {
    let c = flows().await;
    let dir = tempfile::tempdir().unwrap();
    let cur = dir.path().join("current.sqlite3");
    snapshot_file(&c, &cur);
    let path = dir.path().join("index.sqlite3");
    legacy_store(&cur, &path, Legacy::V7);
    std::fs::remove_file(&cur).unwrap();
    // a table the old store has and this build does not would be lost: the migration says so and builds nothing
    Connection::open(&path).unwrap().execute_batch("CREATE TABLE mystery (x)").unwrap();
    let err = storage::migrate(&config(dir.path()), storage::Options::default(), |_| {}).unwrap_err().to_string();
    assert!(err.contains("mystery") && err.contains("untouched"), "{err}");
    assert!(!dir.path().join("index.sqlite3.migrating").exists());
    Connection::open(&path).unwrap().execute_batch("DROP TABLE mystery").unwrap();

    // a lean store (incremental auto-vacuum) is rebuilt as one
    let mut lean = config(dir.path());
    lean.lean = true;
    let rep = storage::migrate(&lean, storage::Options::default(), |_| {}).unwrap();
    assert_eq!(rep.from_schema, 7);
    let after = open_ro(&path);
    let auto_vacuum: i64 = after.query_row("PRAGMA auto_vacuum", [], |r| r.get(0)).unwrap();
    assert_eq!(auto_vacuum, 2);
    let original = {
        let g = c.hs.ingest.lock().unwrap();
        table_digests(g.conn())
    };
    // a lean store keeps no candle aggregates (the writer empties them and records that: `candles::configure`); every other table is as it was
    let kept = |v: Vec<(String, u64, String)>| -> Vec<(String, u64, String)> {
        v.into_iter().filter(|d| !matches!(d.0.as_str(), "candles" | "candle_meta" | "meta")).collect()
    };
    assert_eq!(kept(table_digests(&after)), kept(original));
}

fn amend_req(c: &Ctx, at: &SignedTx, idx: usize, a: &AskState, amended: AskState) -> AmendOrder {
    AmendOrder {
        order: c.w.order(at, idx, AnyState::KobAsk(a.clone())),
        amended: AnyState::KobAsk(amended),
        value: None,
        funding: vec![],
        change: None,
        lock_time: 0,
        deadline: None,
        records: vec![],
        fee: FeeOptions::default(),
    }
}

/// Every event's fee, by event id.
fn fees(conn: &Connection) -> Vec<(i64, String, Option<i64>)> {
    let mut st = conn.prepare("SELECT id, kind, fee FROM order_events ORDER BY id").unwrap();
    st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap().map(|r| r.unwrap()).collect()
}

/// An activity bucket: (iv, t, fills, fees, orders).
type Bucket = (i64, i64, i64, i64, i64);

/// The activity buckets and the makers (by key, with their newest block) the totals keep.
fn kept_totals(conn: &Connection) -> (Vec<Bucket>, Vec<(Vec<u8>, i64)>) {
    let mut st = conn.prepare("SELECT iv, t, fills, fees, orders FROM activity ORDER BY iv, t").unwrap();
    let buckets =
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))).unwrap().map(|r| r.unwrap()).collect();
    let mut st =
        conn.prepare("SELECT i.h, a.last_block FROM actors a JOIN ids i ON i.id = a.actor_id WHERE a.kind = 1 ORDER BY i.h").unwrap();
    let makers = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(|r| r.unwrap()).collect();
    (buckets, makers)
}

/// A schema 9 store (no fees on its fills, no executors, no activity totals) is rebuilt as schema 10 with the fees and the totals the
/// store that indexed the same chain itself has: a fill is paid at the tip the order had at the fill, though an amend raised it later.
/// Only the executors are unknown (the history recorded no payer): none is counted. The migrated store goes on being indexed, and the
/// fills after the migration count their executor.
#[tokio::test]
async fn a_schema_9_store_gets_the_fees_and_totals_of_a_fresh_store_and_no_executors() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.sqlite3");
    let mut c = Ctx::with(Harness::new(db::open_writer(&path, "testnet-10").unwrap(), None));
    scenario(&mut c, |_| {}).await;
    // an ask filled, its tip tripled by an amend, filled again
    let a = ask(MAKER_B, P250);
    let create = c.w.create_tx(AnyState::KobAsk(a.clone()), CARRIER, MAKER_B, 10 * WHOLE);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    let custody = c.w.token_at(&create, 1, Kcc20State::custody(10 * WHOLE, cov.0, EXT));
    let leg = Leg::Ask { order: c.w.order(&create, 0, a.clone()), custody, amount: 2 * WHOLE, t: None };
    let fill = c.w.sign(&Action::Batch(batch(&c.w, c.daa(), vec![leg])));
    c.push(&[&fill]).await;
    let a8 = AskState { amount_left: 8 * WHOLE, ..a.clone() };
    let oi = find_cov(&fill, &cov).unwrap();
    let custody = c.w.token_at(&fill, find_custody(&fill, &cov, 8 * WHOLE).unwrap(), Kcc20State::custody(8 * WHOLE, cov.0, EXT));
    let amended = AskState { tip: 3 * a.tip, ..a8.clone() };
    let am = c.w.sign(&Action::AmendOrder(amend_req(&c, &fill, oi, &a8, amended.clone())));
    c.push(&[&am]).await;
    let leg = Leg::Ask { order: c.w.order(&am, 0, amended.clone()), custody, amount: 3 * WHOLE, t: None };
    let fill2 = c.w.sign(&Action::Batch(batch(&c.w, c.daa(), vec![leg])));
    c.push(&[&fill2]).await;
    assert_eq!(c.hs.event_kinds(&cov), ["create", "fill", "amend", "fill"]);

    let since: i64 = c.hs.query("SELECT MAX(ts) - 60000 FROM order_events", []);
    let fresh = {
        let g = c.hs.ingest.lock().unwrap();
        let totals = |s| kobd::indexer::activity::activity(g.conn(), s).unwrap();
        (fees(g.conn()), kept_totals(g.conn()), totals(None), table_digests(g.conn()), totals(Some(since)))
    };
    // the fills of the scenario (4 whole at the tip) and of this ask (2 whole at the tip, 3 at three times it)
    let paid: Vec<i64> = fresh.0.iter().filter(|e| e.1 == "fill").map(|e| e.2.unwrap()).collect();
    assert_eq!(paid, [4 * TIP, 2 * TIP, 9 * TIP]);
    assert_eq!(fresh.2.fees_paid, (15 * TIP).to_string());
    assert_eq!((fresh.2.fills, fresh.2.executors), (3, 1), "the taker funded every fill");

    // the indexer lets go of the file, the file becomes the store of schema 9 and is migrated
    let node = c.hs.node.clone();
    let cur = dir.path().join("current.sqlite3");
    snapshot_file(&c, &cur);
    c.hs = Harness::new(db::open_memory("testnet-10").unwrap(), None);
    legacy_store(&cur, &path, Legacy::V9);
    std::fs::remove_file(&cur).unwrap();
    {
        let old = open_ro(&path);
        assert!(old.prepare("SELECT fee FROM order_events").is_err(), "a schema 9 store has no fees");
    }
    let mut lines = vec![];
    let rep = storage::migrate(&config(dir.path()), storage::Options::default(), |l| lines.push(l.to_string())).unwrap();
    assert_eq!(rep.from_schema, 9, "{lines:?}");
    let after = open_ro(&path);
    assert_eq!(fees(&after), fresh.0, "every fill's fee, at the tip in force at the fill");
    assert_eq!(kept_totals(&after), fresh.1);
    let v = kobd::indexer::activity::activity(&after, None).unwrap();
    assert_eq!(v, kobd::indexer::activity::ActivityView { executors: 0, ..fresh.2.clone() }, "every figure but the executors");
    let known: i64 = after.query_row("SELECT COUNT(*) FROM order_events WHERE executor_id IS NOT NULL", [], |r| r.get(0)).unwrap();
    assert_eq!(known, 0);
    // a window: the same
    let w = kobd::indexer::activity::activity(&after, Some(since)).unwrap();
    assert_eq!(w, kobd::indexer::activity::ActivityView { executors: 0, ..fresh.4.clone() });
    assert!(fresh.4.fills >= 1);
    // every other table reads as it did (the logo rows were put into the file only)
    let chain = |v: Vec<(String, u64, String)>| -> Vec<(String, u64, String)> {
        v.into_iter().filter(|d| !d.0.starts_with("logo_")).collect()
    };
    assert_eq!(chain(table_digests(&after)), chain(fresh.3));
    drop(after);

    // the indexer goes on from the migrated store: a fill after it counts its executor
    let w = db::open_writer(&path, "testnet-10").unwrap();
    c.hs = Harness::with_node(node, w, None, wide_window());
    let leg = Leg::Ask {
        order: c.w.order(&fill2, find_cov(&fill2, &cov).unwrap(), AskState { amount_left: 5 * WHOLE, ..amended.clone() }),
        custody: c.w.token_at(&fill2, find_custody(&fill2, &cov, 5 * WHOLE).unwrap(), Kcc20State::custody(5 * WHOLE, cov.0, EXT)),
        amount: WHOLE,
        t: None,
    };
    let fill3 = c.w.sign(&Action::Batch(batch(&c.w, c.daa(), vec![leg])));
    c.push(&[&fill3]).await;
    let v = {
        let g = c.hs.ingest.lock().unwrap();
        kobd::indexer::activity::activity(g.conn(), None).unwrap()
    };
    assert_eq!((v.fills, v.executors, v.fees_paid), (4, 1, (18 * TIP).to_string()));
    // the totals stay what the rows say (`testkit::assert_read_invariants`)
    let _ = c.hs.snapshot();
}

/// A lean schema 9 store (incremental auto-vacuum) is upgraded as one, and a second run finds nothing to do.
#[tokio::test]
async fn a_lean_schema_9_store_stays_lean() {
    let c = flows().await;
    let dir = tempfile::tempdir().unwrap();
    let cur = dir.path().join("current.sqlite3");
    snapshot_file(&c, &cur);
    let path = dir.path().join("index.sqlite3");
    legacy_store(&cur, &path, Legacy::V9);
    std::fs::remove_file(&cur).unwrap();
    Connection::open(&path).unwrap().execute_batch("PRAGMA auto_vacuum = 2; VACUUM;").unwrap();
    let mut lean = config(dir.path());
    lean.lean = true;
    let rep = storage::migrate(&lean, storage::Options::default(), |_| {}).unwrap();
    assert_eq!(rep.from_schema, 9);
    assert_eq!(rep.rows("orders"), 3);
    let after = open_ro(&path);
    assert_eq!(after.query_row::<i64, _, _>("PRAGMA auto_vacuum", [], |r| r.get(0)).unwrap(), 2);
    assert_eq!(after.query_row::<String, _, _>("PRAGMA integrity_check", [], |r| r.get(0)).unwrap(), "ok");
    let original = {
        let g = c.hs.ingest.lock().unwrap();
        kobd::indexer::activity::activity(g.conn(), None).unwrap()
    };
    assert_eq!(
        kobd::indexer::activity::activity(&after, None).unwrap(),
        kobd::indexer::activity::ActivityView { executors: 0, ..original }
    );
    drop(after);
    std::fs::remove_file(rep.kept_old.unwrap()).unwrap();
    let again = storage::migrate(&lean, storage::Options::default(), |_| {}).unwrap();
    assert_eq!(again.from_schema, db::SCHEMA_VERSION);
    assert!(again.kept_old.is_none());
}
