//! `index migrate`: a store written whole by an earlier build (states in full, the fills-only indexes absent,
//! `token_holdings_owner` present) is repacked, its indexes swapped and the file compacted, and no read changes: every table
//! digests the same with its states decoded, the API reads return the same JSON, and a second run changes nothing.

mod common;

use common::*;
use kob_executor::config::IndexerConfig;
use kob_executor::hex::Hash32;
use kob_executor::indexer::reads::{self, HoldingFilter, OrderFilter};
use kob_executor::indexer::{book, db, packed, storage};
use kob_executor::testkit::*;
use kob_protocol::build::*;
use kob_protocol::state::*;
use kob_protocol::tx::*;
use rusqlite::{Connection, OpenFlags};
use sha2::{Digest, Sha256};

fn cancel(c: &Ctx, order: OrderUtxo<AnyState>, custody: Option<TokenUtxo>, strays: Vec<TokenUtxo>) -> SignedTx {
    c.w.sign(&Action::CancelOrder(CancelOrder {
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
    let c = Ctx::new();
    scenario(&c, || {}).await;
    c
}

/// [`flows`] on a given context; `mid` runs once the first ask and a stray to it are indexed and before anything spends them.
async fn scenario(c: &Ctx, mid: impl FnOnce()) {
    // an ask that is partly filled twice and then cancelled with a stray swept
    let a = ask(MAKER_A, P250);
    let create = c.w.create_tx(AnyState::KobAsk(a.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    let stray = c.w.stray(cov, 10 * WHOLE, TAKER);
    c.push(&[&stray]).await;
    mid();
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

/// Every row of the tables the migration touches or reads, states decoded, hashed per table.
fn table_digests(conn: &Connection) -> Vec<(String, u64, String)> {
    let queries = [
        (
            "order_utxos",
            "SELECT txid, idx, covenant_id, value, spk, kob_ustate(state, (SELECT kob_gstate(g.genesis_state, g.maker, g.token_cov_id, g.token_tpl_hash, g.quote_cov_id) FROM orders g WHERE g.covenant_id = order_utxos.covenant_id)), created_block, created_daa, spent_block, spent_txid, spent_entry, spent_amount FROM order_utxos ORDER BY txid, idx",
        ),
        (
            "token_holdings",
            "SELECT txid, idx, token_cov_id, program, family, owner, owner_kind, amount, value, kob_hstate(state, family, owner, owner_kind, amount), role, created_block, created_daa, spent_block, spent_txid FROM token_holdings ORDER BY txid, idx",
        ),
        ("orders", "SELECT * FROM orders ORDER BY covenant_id"),
        ("order_events", "SELECT * FROM order_events ORDER BY id"),
        ("order_state", "SELECT * FROM order_state ORDER BY covenant_id"),
        ("token_utxos", "SELECT * FROM token_utxos ORDER BY txid, idx"),
        ("counters", "SELECT * FROM counters ORDER BY name"),
    ];
    queries
        .iter()
        .map(|(name, sql)| {
            let mut h = Sha256::new();
            let mut n = 0u64;
            // the packed columns of `SELECT *`, decoded
            let sql = &match *name {
                "orders" => sql.replace(
                    "SELECT *",
                    &format!(
                        "SELECT {}",
                        columns_with(
                            conn,
                            "orders",
                            "genesis_state",
                            "kob_gstate(genesis_state, maker, token_cov_id, token_tpl_hash, quote_cov_id)"
                        )
                    ),
                ),
                "order_events" => {
                    sql.replace("SELECT *", &format!("SELECT {}", columns_with(conn, "order_events", "detail", "kob_dtext(detail)")))
                }
                _ => sql.to_string(),
            };
            let mut st = conn.prepare(sql).unwrap();
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

/// The columns of a table in order, `column` replaced by `expr`.
fn columns_with(conn: &Connection, table: &str, column: &str, expr: &str) -> String {
    let mut st = conn.prepare(&format!("SELECT name FROM pragma_table_info('{table}')")).unwrap();
    let names: Vec<String> = st.query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect();
    names.iter().map(|n| if n == column { expr.to_string() } else { n.clone() }).collect::<Vec<_>>().join(", ")
}

/// The API reads that carry the packed columns, as JSON.
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
    for spent in [false, true] {
        let f = HoldingFilter { owner: None, token: Some(Hash32(TOKEN_COV)), include_spent: spent };
        out.push(serde_json::to_string(&reads::holdings(conn, &ctx, &f, 100, None).unwrap()).unwrap());
    }
    out.push(serde_json::to_string(&book::snapshot(conn, None).unwrap()).unwrap());
    out
}

fn open_ro(path: &std::path::Path) -> Connection {
    let c = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    packed::register(&c).unwrap();
    c
}

/// Bytes of the packed columns: order UTXO states, holding states, genesis states, event details.
fn state_bytes(conn: &Connection) -> (i64, i64, i64, i64) {
    conn.query_row(
        "SELECT (SELECT SUM(LENGTH(state)) FROM order_utxos), (SELECT SUM(LENGTH(state)) FROM token_holdings),                 (SELECT SUM(LENGTH(genesis_state)) FROM orders), (SELECT SUM(LENGTH(detail)) FROM order_events)",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )
    .unwrap()
}

fn index_sql(conn: &Connection, name: &str) -> Option<String> {
    conn.query_row("SELECT sql FROM sqlite_master WHERE type = 'index' AND name = ?1", [name], |r| r.get(0)).ok()
}

#[tokio::test]
async fn migrate_repacks_a_whole_store_and_no_read_changes() {
    let c = flows().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.sqlite3");
    {
        let g = c.hs.ingest.lock().unwrap();
        g.conn().execute("VACUUM INTO ?1", [path.to_string_lossy().as_ref()]).unwrap();
    }
    // the store as an earlier build left it: every state whole, the indexes of schema 5, schema version 5
    {
        let w = Connection::open(&path).unwrap();
        packed::register(&w).unwrap();
        w.execute_batch(
            "UPDATE order_utxos SET state = kob_ustate(state, (SELECT kob_gstate(g.genesis_state, g.maker, g.token_cov_id, g.token_tpl_hash, g.quote_cov_id) FROM orders g WHERE g.covenant_id = order_utxos.covenant_id)); \
             UPDATE orders SET genesis_state = kob_gstate(genesis_state, maker, token_cov_id, token_tpl_hash, quote_cov_id); \
             UPDATE order_events SET detail = kob_dtext(detail); \
             UPDATE token_holdings SET state = kob_hstate(state, family, owner, owner_kind, amount); \
             DROP INDEX order_events_fills; DROP INDEX order_events_fill_ts; \
             CREATE INDEX order_events_fills ON order_events (kind, token_cov_id, id); \
             CREATE INDEX order_events_fill_ts ON order_events (kind, token_cov_id, ts); \
             CREATE INDEX token_holdings_owner ON token_holdings (owner, token_cov_id); \
             CREATE INDEX token_holdings_token_program ON token_holdings (token_cov_id, program); \
             DROP TRIGGER holding_programs_insert; DROP TRIGGER holding_programs_delete; DROP TRIGGER holding_programs_update; \
             DROP TABLE holding_programs; DELETE FROM meta WHERE k = 'holding_programs'; \
             UPDATE meta SET v = '5' WHERE k = 'schema_version';",
        )
        .unwrap();
    }
    let whole = open_ro(&path);
    let (utxo_whole, holding_whole, genesis_whole, detail_whole) = state_bytes(&whole);
    let digests_before = table_digests(&whole);
    assert!(digests_before.iter().find(|d| d.0 == "order_utxos").unwrap().1 >= 4);
    drop(whole);

    let mut cfg = IndexerConfig { data_dir: dir.path().to_path_buf(), network: "testnet-10".into(), ..Default::default() };
    cfg.records.enabled = false;
    let mut lines = vec![];
    let rep = storage::migrate(&cfg, storage::Options::default(), |l| lines.push(l.to_string())).unwrap();
    assert!(rep.changed("order_utxos.state") > 0 && rep.changed("token_holdings.state") > 0, "{rep:?}");
    assert!(rep.changed("orders.genesis_state") > 0 && rep.changed("order_events.detail") > 0, "{rep:?}");
    assert_eq!(rep.indexes_dropped, ["token_holdings_owner", "token_holdings_token_program"]);
    assert_eq!(rep.indexes_swapped, ["order_events_fills", "order_events_fill_ts"]);
    let old = rep.kept_old.clone().expect("compacted");
    assert!(old.exists() && old.to_string_lossy().ends_with("index.sqlite3.before-migrate"));

    let after = open_ro(&path);
    let (utxo_after, holding_after, genesis_after, detail_after) = state_bytes(&after);
    assert!(utxo_after < utxo_whole / 2, "{utxo_after} vs {utxo_whole}");
    assert!(holding_after < holding_whole / 2, "{holding_after} vs {holding_whole}");
    assert!(genesis_after < genesis_whole / 2, "{genesis_after} vs {genesis_whole}");
    assert!(detail_after < detail_whole, "{detail_after} vs {detail_whole}");
    assert_eq!(table_digests(&after), digests_before, "every table reads the same, its states decoded");
    for name in ["order_events_fills", "order_events_fill_ts"] {
        assert!(index_sql(&after, name).unwrap().contains("WHERE kind = 'fill'"), "{name}");
    }
    assert!(index_sql(&after, "token_holdings_owner").is_none() && index_sql(&after, "token_holdings_token_program").is_none());
    // the programs of the holdings, counted at the start of the migration, are what a count of the holdings says
    let counted: Vec<(Vec<u8>, String, i64)> = {
        let mut st = after.prepare("SELECT token_cov_id, program, n FROM holding_programs ORDER BY 1, 2").unwrap();
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap().map(|r| r.unwrap()).collect()
    };
    let real: Vec<(Vec<u8>, String, i64)> = {
        let mut st = after.prepare("SELECT token_cov_id, program, COUNT(*) FROM token_holdings GROUP BY 1, 2 ORDER BY 1, 2").unwrap();
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap().map(|r| r.unwrap()).collect()
    };
    assert!(!real.is_empty());
    assert_eq!(counted, real);
    assert_eq!(db::meta_get(&after, "schema_version").unwrap().as_deref(), Some("7"));
    assert_eq!(after.query_row::<String, _, _>("PRAGMA integrity_check", [], |r| r.get(0)).unwrap(), "ok");

    // the API reads: the migrated store answers what the in-memory store the flows ran on answers
    let migrated = reads_json(&after);
    let original = {
        let g = c.hs.ingest.lock().unwrap();
        reads_json(g.conn())
    };
    assert_eq!(migrated, original);
    drop(after);

    // a second run changes nothing (and keeps the first run's rollback file out of the way)
    std::fs::remove_file(&old).unwrap();
    let again = storage::migrate(&cfg, storage::Options::default(), |_| {}).unwrap();
    assert!(again.columns.iter().all(|c| c.changed == 0), "{again:?}");
    assert!(again.indexes_swapped.is_empty() && again.indexes_dropped.is_empty());
    assert_eq!(table_digests(&open_ro(&path)), digests_before);
}

#[tokio::test]
async fn a_store_written_by_this_build_is_packed_already_and_the_migration_changes_nothing() {
    let c = flows().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.sqlite3");
    {
        let g = c.hs.ingest.lock().unwrap();
        let conn = g.conn();
        // the writer packs: an order that was only placed has the few bytes of its genesis state's delta, never the 300
        let (n, short): (i64, i64) = conn
            .query_row(
                "SELECT COUNT(*), SUM(LENGTH(u.state) < LENGTH((SELECT kob_gstate(o.genesis_state, o.maker, o.token_cov_id, o.token_tpl_hash, o.quote_cov_id) FROM orders o WHERE o.covenant_id = u.covenant_id)) / 2) FROM order_utxos u",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(n >= 4 && short >= 1, "{n} {short}");
        // a holding is shorter than its 112 whole bytes (an extension commitment is the part it keeps)
        let holdings_packed: i64 =
            conn.query_row("SELECT COUNT(*) FROM token_holdings WHERE LENGTH(state) < 100", [], |r| r.get(0)).unwrap();
        assert!(holdings_packed >= 1);
        conn.execute("VACUUM INTO ?1", [path.to_string_lossy().as_ref()]).unwrap();
    }
    let before = table_digests(&open_ro(&path));
    let mut cfg = IndexerConfig { data_dir: dir.path().to_path_buf(), network: "testnet-10".into(), ..Default::default() };
    cfg.records.enabled = false;
    let rep = storage::migrate(&cfg, storage::Options { compact: false, compress_log: false }, |_| {}).unwrap();
    assert!(rep.columns.iter().all(|c| c.changed == 0), "{rep:?}");
    assert!(rep.indexes_swapped.is_empty() && rep.indexes_dropped.is_empty());
    assert_eq!(table_digests(&open_ro(&path)), before);
}

/// A store of whole values (what an earlier build wrote) goes on being indexed by this build: the rows the flow adds are packed, the ones
/// it spends are whole, and every read is what the same flow gives on a store that was packed throughout.
#[tokio::test]
async fn a_whole_store_mixes_with_the_packed_rows_this_build_adds_and_reads_the_same() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.sqlite3");
    let c = Ctx::with(Harness::new(db::open_writer(&path, "testnet-10").unwrap(), None));
    scenario(&c, || {
        // everything indexed so far is made whole, as an earlier build left it
        let w = Connection::open(&path).unwrap();
        packed::register(&w).unwrap();
        w.execute_batch(
            "UPDATE order_utxos SET state = kob_ustate(state, (SELECT kob_gstate(g.genesis_state, g.maker, g.token_cov_id, g.token_tpl_hash, g.quote_cov_id) FROM orders g WHERE g.covenant_id = order_utxos.covenant_id));              UPDATE orders SET genesis_state = kob_gstate(genesis_state, maker, token_cov_id, token_tpl_hash, quote_cov_id);              UPDATE order_events SET detail = kob_dtext(detail);              UPDATE token_holdings SET state = kob_hstate(state, family, owner, owner_kind, amount);",
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
