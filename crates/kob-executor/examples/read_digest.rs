//! Read-equivalence harness: runs the read API's queries over a database and prints, per query, its wall time and a digest of
//! its JSON answer, then a logical digest of every table. Run it with the build before a storage change on a database, and
//! with the build after on the migrated copy: the two outputs must carry the same digests (the timings are the cost of the
//! change).
//!
//!   read_digest <index.sqlite3> <registry tokens.json> [network]
//!
//! Output, one line per query: `label<TAB>ms<TAB>digest`. A table digest hashes every row in rowid order, with the stored state
//! columns decoded (`kob_*` functions, when the build has them), so a repacked database digests like the one it came from.
//! Run `... | sort -k1,1` on both outputs and compare the first and third columns.

use kob_executor::indexer::reads::{self, HoldingFilter, OrderFilter, ReadCtx};
use kob_executor::indexer::{book, db, market, pairs};
use kob_executor::tokens::TokenAllowlist;
use rusqlite::{params, Connection};
use serde::Serialize;
use std::time::Instant;

fn digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let d = Sha256::digest(bytes);
    d.iter().take(16).map(|b| format!("{b:02x}")).collect()
}

fn run<T: Serialize, E: std::fmt::Display>(label: &str, f: impl FnOnce() -> Result<T, E>) {
    let t = Instant::now();
    let r = f();
    let ms = t.elapsed().as_secs_f64() * 1000.0;
    match r {
        Ok(v) => println!("{label}\t{ms:.1}\t{}", digest(&serde_json::to_vec(&v).expect("serialise"))),
        Err(e) => println!("{label}\t{ms:.1}\tERR {e}"),
    }
}

fn blobs(conn: &Connection, sql: &str) -> Vec<Vec<u8>> {
    let mut st = conn.prepare(sql).expect("sample query");
    st.query_map([], |r| r.get::<_, Vec<u8>>(0)).expect("sample").map(|r| r.expect("row")).collect()
}

fn h32(b: &[u8]) -> kob_executor::hex::Hash32 {
    kob_executor::hex::Hash32::from_slice(b).expect("32 bytes")
}

fn has_fn(conn: &Connection, name: &str) -> bool {
    conn.query_row("SELECT COUNT(*) FROM pragma_function_list WHERE name = ?1", [name], |r| r.get::<_, i64>(0)).unwrap_or(0) > 0
}

/// Hash every row of `sql` (types and bytes, in the order the statement returns them).
fn table_digest(conn: &Connection, label: &str, sql: &str) {
    use sha2::{Digest, Sha256};
    let t = Instant::now();
    let mut h = Sha256::new();
    let mut n = 0u64;
    let mut st = conn.prepare(sql).unwrap_or_else(|e| panic!("{label}: {e}"));
    let cols = st.column_count();
    let mut rows = st.query([]).expect("query");
    while let Some(r) = rows.next().expect("row") {
        n += 1;
        for i in 0..cols {
            match r.get_ref(i).expect("col") {
                rusqlite::types::ValueRef::Null => h.update([0u8]),
                rusqlite::types::ValueRef::Integer(v) => {
                    h.update([1u8]);
                    h.update(v.to_le_bytes());
                }
                rusqlite::types::ValueRef::Real(v) => {
                    h.update([2u8]);
                    h.update(v.to_le_bytes());
                }
                rusqlite::types::ValueRef::Text(b) => {
                    h.update([3u8]);
                    h.update((b.len() as u64).to_le_bytes());
                    h.update(b);
                }
                rusqlite::types::ValueRef::Blob(b) => {
                    h.update([4u8]);
                    h.update((b.len() as u64).to_le_bytes());
                    h.update(b);
                }
            }
        }
    }
    let d: String = h.finalize().iter().take(16).map(|b| format!("{b:02x}")).collect();
    println!("table:{label}\t{:.1}\t{n} rows {d}", t.elapsed().as_secs_f64() * 1000.0);
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = std::path::PathBuf::from(args.get(1).expect("index.sqlite3"));
    let tokens_path = args.get(2).expect("tokens.json");
    let network = args.get(3).map(String::as_str).unwrap_or("testnet-10");
    let tokens = TokenAllowlist::load(std::path::Path::new(tokens_path)).expect("tokens");
    let conn = db::open_reader(&path, network).expect("open");
    let cursor = book::cursor_daa(&conn).expect("cursor");
    let ctx = ReadCtx { node_daa: Some(cursor), settle_depth_daa: 600, now_unix: Some(1_793_800_000) };
    let only_tables = std::env::var("ONLY_TABLES").is_ok();
    let skip_tables = std::env::var("SKIP_TABLES").is_ok();

    if !only_tables {
        // ---- samples (by ordered ids, so both builds pick the same ones whatever their indexes)
        let tokens_top = blobs(
            &conn,
            "SELECT token_cov_id FROM (SELECT token_cov_id, COUNT(*) c FROM orders WHERE token_cov_id IS NOT NULL GROUP BY 1 ORDER BY c DESC, token_cov_id LIMIT 12)",
        );
        let makers_top = blobs(
            &conn,
            "SELECT maker FROM (SELECT maker, COUNT(*) c FROM orders WHERE maker IS NOT NULL GROUP BY 1 ORDER BY c DESC, maker LIMIT 6)",
        );
        let owners_top =
            blobs(&conn, "SELECT owner FROM (SELECT owner, COUNT(*) c FROM token_holdings GROUP BY 1 ORDER BY c DESC, owner LIMIT 6)");
        let owners_mid = blobs(&conn, "SELECT owner FROM token_holdings WHERE rowid IN (777, 1500000, 3000000)");
        let live =
            blobs(&conn, "SELECT covenant_id FROM order_state WHERE status IN ('open','partial') ORDER BY covenant_id LIMIT 120");
        let mut ids: Vec<Vec<u8>> = live;
        for b in 0u16..256 {
            let mut lo = [0u8; 32];
            lo[0] = b as u8;
            if let Ok(id) =
                conn.query_row("SELECT covenant_id FROM orders WHERE covenant_id >= ?1 ORDER BY covenant_id LIMIT 1", [&lo[..]], |r| {
                    r.get::<_, Vec<u8>>(0)
                })
            {
                ids.push(id);
            }
        }
        let pair_rows: Vec<(Vec<u8>, Vec<u8>)> = {
            let mut st = conn
                .prepare("SELECT base_cov_id, quote_cov_id FROM pair_fills GROUP BY 1, 2 ORDER BY COUNT(*) DESC, 1, 2 LIMIT 4")
                .expect("pairs");
            st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).expect("p").map(|r| r.expect("r")).collect()
        };

        // ---- global reads
        run("health.counters", || reads::counters(&conn));
        run("tokens", || reads::token_list(&conn, &tokens));
        run("token_events", || reads::token_events(&conn, None, 50));
        run("strays.all", || reads::strays(&conn, &ctx, None, 50));
        run("fills.all", || reads::fills(&conn, &ctx, None, None, None, 50));
        run("fills.all.ask", || reads::fills(&conn, &ctx, None, Some(1), None, 50));
        run("pairs.all", || pairs::pairs(&conn, &ctx, None));
        run("book.snapshot", || book::snapshot(&conn, None));
        run("export_orders.live", || kob_executor::recover::export_orders(&conn, None, true));
        let mut cursor_o = None;
        for page in 0..4 {
            let r = reads::orders(&conn, &ctx, &OrderFilter::default(), 100, cursor_o.clone());
            let next = r.as_ref().ok().and_then(|p| p.next_cursor.clone());
            run(&format!("orders.all.p{page}"), || r);
            cursor_o = next.and_then(|c| reads::parse_order_cursor(&c));
            if cursor_o.is_none() {
                break;
            }
        }
        for st in reads::ORDER_STATUSES {
            run(&format!("orders.status.{st}"), || {
                reads::orders(&conn, &ctx, &OrderFilter { maker: None, token: None, status: Some(st.to_string()) }, 100, None)
            });
        }

        // ---- per token
        for (i, t) in tokens_top.iter().enumerate() {
            let tok = h32(t);
            let dec = reads::token_decimals(&conn, &tokens, &tok).ok().flatten();
            let l = format!("tok{i}");
            run(&format!("{l}.book.agg"), || reads::book(&conn, &ctx, &tok, 20, true));
            run(&format!("{l}.book.raw"), || reads::book(&conn, &ctx, &tok, 20, false));
            run(&format!("{l}.stats"), || market::stats(&conn, &ctx, &tok, dec));
            run(&format!("{l}.depth"), || market::depth(&conn, &ctx, &tok, dec, 50));
            run(&format!("{l}.trades"), || market::trades(&conn, &ctx, &tok, dec, None, 50));
            for (name, iv) in [("1m", 60_000i64), ("5m", 300_000), ("1h", 3_600_000), ("1d", 86_400_000)] {
                run(&format!("{l}.candles.{name}"), || market::candles(&conn, &tok, dec, name, iv, None, None, 1500));
            }
            run(&format!("{l}.fills"), || reads::fills(&conn, &ctx, Some(&tok), None, None, 50));
            run(&format!("{l}.orders.token"), || {
                reads::orders(&conn, &ctx, &OrderFilter { maker: None, token: Some(tok), status: None }, 100, None)
            });
            run(&format!("{l}.orders.token.active"), || {
                reads::orders(&conn, &ctx, &OrderFilter { maker: None, token: Some(tok), status: Some("active".into()) }, 100, None)
            });
            run(&format!("{l}.token_events"), || reads::token_events(&conn, Some(&tok), 50));
            run(&format!("{l}.proven_programs"), || reads::proven_programs(&conn, &tok.0));
            run(&format!("{l}.pairs"), || pairs::pairs(&conn, &ctx, Some(&tok)));
            for spent in [false, true] {
                let mut cur = None;
                for page in 0..3 {
                    let f = HoldingFilter { owner: None, token: Some(tok), include_spent: spent };
                    let r = reads::holdings(&conn, &ctx, &f, 100, cur.clone());
                    let next = r.as_ref().ok().and_then(|p| p.next_cursor.clone());
                    run(&format!("{l}.holdings.token.spent{spent}.p{page}"), || r);
                    cur = next.and_then(|c| reads::parse_holding_cursor(&c));
                    if cur.is_none() {
                        break;
                    }
                }
            }
        }
        for (i, (a, b)) in pair_rows.iter().enumerate() {
            let (a, b) = (h32(a), h32(b));
            let da = reads::token_decimals(&conn, &tokens, &a).ok().flatten();
            let db_ = reads::token_decimals(&conn, &tokens, &b).ok().flatten();
            run(&format!("pair{i}.book"), || pairs::pair_book(&conn, &ctx, &a, &b, 20));
            run(&format!("pair{i}.fills"), || pairs::pair_fills(&conn, &ctx, &a, &b, None, 50));
            run(&format!("pair{i}.candles"), || pairs::pair_candles(&conn, &a, &b, (da, db_), "1h", 3_600_000, None, None, 500));
        }

        // ---- per maker / owner
        for (i, m) in makers_top.iter().enumerate() {
            let mut cur = None;
            for page in 0..3 {
                let r =
                    reads::orders(&conn, &ctx, &OrderFilter { maker: Some(m.clone()), token: None, status: None }, 100, cur.clone());
                let next = r.as_ref().ok().and_then(|p| p.next_cursor.clone());
                run(&format!("maker{i}.orders.p{page}"), || r);
                cur = next.and_then(|c| reads::parse_order_cursor(&c));
                if cur.is_none() {
                    break;
                }
            }
            run(&format!("maker{i}.strays"), || reads::strays(&conn, &ctx, Some(m.as_slice()), 50));
            run(&format!("maker{i}.export"), || kob_executor::recover::export_orders(&conn, Some(m.as_slice()), false));
        }
        let mut owners = owners_top.clone();
        owners.extend(owners_mid);
        for (i, o) in owners.iter().enumerate() {
            let tok = tokens_top.first().map(|t| h32(t));
            for (spent, with_token) in [(false, false), (true, false), (false, true), (true, true)] {
                let token = if with_token { tok } else { None };
                let mut cur = None;
                for page in 0..3 {
                    let f = HoldingFilter { owner: Some(o.clone()), token, include_spent: spent };
                    let r = reads::holdings(&conn, &ctx, &f, 100, cur.clone());
                    let next = r.as_ref().ok().and_then(|p| p.next_cursor.clone());
                    run(&format!("owner{i}.holdings.spent{spent}.tok{with_token}.p{page}"), || r);
                    cur = next.and_then(|c| reads::parse_holding_cursor(&c));
                    if cur.is_none() {
                        break;
                    }
                }
            }
            if o.len() == 32 {
                let mut k = [0u8; 32];
                k.copy_from_slice(o);
                run(&format!("owner{i}.operator_tokens"), || book::operator_tokens(&conn, &k).map(|v| v.len()));
            }
        }

        // ---- single orders and their events
        for (i, id) in ids.iter().enumerate() {
            let id = h32(id);
            run(&format!("order{i}"), || reads::order(&conn, &ctx, &id));
            run(&format!("order{i}.events"), || reads::order_events(&conn, &ctx, &id, 100, None));
        }

        // ---- the maintained candle aggregates, raw
        run("candles.table", || -> Result<Vec<String>, rusqlite::Error> {
            let mut st = conn.prepare(
                "SELECT hex(token_cov_id), iv, t, o, h, l, c, volume, quote_volume, trades FROM candles ORDER BY token_cov_id, iv, t",
            )?;
            let rows = st.query_map(params![], |r| {
                Ok(format!(
                    "{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                    r.get::<_, String>(7)?,
                    r.get::<_, String>(8)?,
                    r.get::<_, i64>(9)?
                ))
            })?;
            rows.collect()
        });
    }

    // ---- logical digests of every table (stored states decoded)
    if !skip_tables {
        let ustate = if has_fn(&conn, "kob_ustate") {
            "kob_ustate(u.state, (SELECT o.genesis_state FROM orders o WHERE o.covenant_id = u.covenant_id))"
        } else {
            "u.state"
        };
        let hstate =
            if has_fn(&conn, "kob_hstate") { "kob_hstate(h.state, h.family, h.owner, h.owner_kind, h.amount)" } else { "h.state" };
        table_digest(
            &conn,
            "order_utxos",
            &format!("SELECT u.txid, u.idx, u.covenant_id, u.value, u.spk, {ustate}, u.created_block, u.created_daa, u.spent_block, u.spent_txid, u.spent_entry, u.spent_amount FROM order_utxos u ORDER BY u.rowid"),
        );
        table_digest(
            &conn,
            "token_holdings",
            &format!("SELECT h.txid, h.idx, h.token_cov_id, h.program, h.family, h.owner, h.owner_kind, h.amount, h.value, {hstate}, h.role, h.created_block, h.created_daa, h.spent_block, h.spent_txid FROM token_holdings h ORDER BY h.rowid"),
        );
        for t in [
            "orders",
            "order_events",
            "order_amends",
            "order_state",
            "token_utxos",
            "token_events",
            "rejects",
            "blocks",
            "candles",
            "candle_meta",
            "pair_fills",
            "order_flags",
            "counters",
            "kcc23_candidates",
            "kcc23_txs",
            "kcc23_declarations",
        ] {
            let exists: i64 = conn
                .query_row("SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1", [t], |r| r.get(0))
                .unwrap_or(0);
            if exists > 0 {
                // by the primary key where there is one that means something: rowids are renumbered by a vacuum, and a start-up re-derivation
                // of a row rewrites it at the end
                let order = match t {
                    "counters" => "name",
                    "candles" => "token_cov_id, iv, t",
                    "order_state" | "order_flags" => "covenant_id",
                    "candle_meta" => "token_cov_id",
                    "kcc23_candidates" | "kcc23_txs" => "txid",
                    "kcc23_declarations" => "covenant_id",
                    "token_utxos" => "txid, idx",
                    _ => "rowid",
                };
                table_digest(&conn, t, &format!("SELECT * FROM {t} ORDER BY {order}"));
            }
        }
        table_digest(&conn, "meta", "SELECT k, v FROM meta WHERE k NOT IN ('schema_version') AND k NOT LIKE 'storage%' ORDER BY k");
    }
}
